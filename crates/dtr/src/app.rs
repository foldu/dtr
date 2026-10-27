//! Window, GPU and egui plumbing.
//!
//! Deliberately not `eframe` (PLAN D6): we need winit events for the terminal
//! as well as for egui, and we want to redraw on bytes rather than on egui's
//! animation clock.

use std::path::PathBuf;
use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

use crate::palette::{Action, Palette};
use crate::ui::Look;
use crate::pane::{Frame, Pane, Transport};
use crate::ui::Status;

pub enum UserEvent {
    Frame,
}

/// Everything a fresh window needs that is decided on the command line.
pub struct Startup {
    pub transport: Transport,
    pub send: Vec<u8>,
    pub log: Option<(PathBuf, crate::serial::LogFormat)>,
    pub store: Option<Arc<dtr_store::Store>>,
    /// Open the palette on start (also how it gets into screenshots).
    pub palette: bool,
    pub look: Look,
    pub theme: Option<crate::theme::Theme>,
}

/// Render one frame offscreen and write it to `path`, without a window.
pub fn capture_headless(
    startup: Startup,
    path: &std::path::Path,
    width: u32,
    height: u32,
    delay: std::time::Duration,
) -> anyhow::Result<()> {
    let label = startup.transport.describe();
    let serial = matches!(startup.transport, Transport::Serial(_));
    let pane = Pane::spawn(
        startup.transport,
        startup.log.clone(),
        startup.theme.clone(),
        || {},
    )?;
    if !startup.send.is_empty() {
        pane.write(startup.send.clone());
    }

    // Give the session a moment to produce output (a serial device's prompt),
    // then take the newest frame rather than the first one.
    std::thread::sleep(delay);
    let mut frame = pane.take_frame();
    if frame.is_none() {
        // The pane may still be opening a port; wait for the first frame, but
        // not longer than it takes for the pane to report that it failed.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if let Some(err) = pane.error() {
                anyhow::bail!("{err}");
            }
            if let Some(ready) = pane.take_frame() {
                frame = Some(ready);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    crate::screenshot::capture(
        frame.as_ref(),
        path,
        width,
        height,
        &label,
        pane.stats(),
        startup.palette,
        serial,
        &startup.look,
    )
}

/// One place that decides which backends wgpu may use.
///
/// `WGPU_BACKEND` overrides the set, which is how a broken driver stack (or
/// Wine, which finds dx12 first and then cannot compile egui's shader through
/// FXC) is worked around without a rebuild.
pub(crate) fn instance() -> wgpu::Instance {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    if let Some(backends) = wgpu::Backends::from_env() {
        descriptor.backends = backends;
    }
    wgpu::Instance::new(descriptor)
}

pub fn run(startup: Startup) -> anyhow::Result<()> {
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy: EventLoopProxy<UserEvent> = event_loop.create_proxy();
    let label = startup.transport.describe();
    let serial = matches!(startup.transport, Transport::Serial(_));

    let pane = Pane::spawn(startup.transport, startup.log.clone(), startup.theme.clone(), {
        let proxy = proxy.clone();
        move || {
            let _ = proxy.send_event(UserEvent::Frame);
        }
    })?;
    if !startup.send.is_empty() {
        pane.write(startup.send.clone());
    }

    let mut app = App {
        pane,
        frame: None,
        gpu: None,
        modifiers: ModifiersState::empty(),
        label,
        meter: Meter::new(),
        fatal: None,
        quit: false,
        palette: Palette::new(startup.palette),
        look: startup.look.clone(),
        theme: startup.theme.clone(),
        proxy,
        store: startup.store,
        log: startup.log,
        serial,
        echo: false,
    };
    event_loop.run_app(&mut app)?;
    match app.fatal {
        Some(err) => anyhow::bail!("{err}"),
        None => Ok(()),
    }
}

struct App {
    pane: Pane,
    frame: Option<Frame>,
    gpu: Option<Gpu>,
    modifiers: ModifiersState,
    label: String,
    meter: Meter,
    /// Set when the pane fails; `run` returns it once the loop has stopped.
    fatal: Option<String>,
    quit: bool,
    palette: Palette,
    proxy: EventLoopProxy<UserEvent>,
    store: Option<Arc<dtr_store::Store>>,
    log: Option<(PathBuf, crate::serial::LogFormat)>,
    /// Whether this session has modem lines, for the status strip.
    serial: bool,
    echo: bool,
    look: Look,
    theme: Option<crate::theme::Theme>,
}

impl App {
    /// Swap the session under the window. Dropping the old pane closes its
    /// channels, which ends its thread and takes a shell process with it.
    fn start(&mut self, transport: Transport) {
        let proxy = self.proxy.clone();
        match Pane::spawn(transport.clone(), self.log.clone(), self.theme.clone(), move || {
            let _ = proxy.send_event(UserEvent::Frame);
        }) {
            Ok(pane) => {
                self.pane = pane;
                self.label = transport.describe();
                self.serial = matches!(transport, Transport::Serial(_));
                self.frame = None;
            }
            Err(err) => {
                tracing::error!(%err, "could not start the session");
                self.fatal = Some(format!("{err}"));
            }
        }
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::Connect(port) => {
                let remembered = self
                    .store
                    .as_ref()
                    .and_then(|store| store.serial(&port).ok().flatten())
                    .unwrap_or_default();
                self.start(Transport::Serial(crate::serial::Config {
                    port,
                    baud: remembered.baud,
                    cr_to_crlf: remembered.cr_to_crlf,
                    auto_reconnect: true,
                }));
            }
            Action::Shell(shell) => {
                let program = (!shell.is_empty()).then_some(shell);
                self.start(Transport::Shell(crate::pty::Config { program }));
            }
            Action::Replay(path) => self.start(Transport::Replay(PathBuf::from(path))),
            Action::Demo => self.start(Transport::Demo),
            Action::Ssh(host) => {
                // No SSH transport yet; saying so is better than doing nothing.
                tracing::warn!(%host, "ssh sessions are not implemented yet");
            }
            Action::Quit => self.quit = true,
        }
    }
}

/// Byte rate over a short window. Cheap, and it would look wrong if it updated
/// on every frame, so it only resamples a few times a second.
struct Meter {
    at: std::time::Instant,
    rx: u64,
    tx: u64,
    rx_rate: f64,
    tx_rate: f64,
}

impl Meter {
    fn new() -> Self {
        Self {
            at: std::time::Instant::now(),
            rx: 0,
            tx: 0,
            rx_rate: 0.0,
            tx_rate: 0.0,
        }
    }

    fn sample(&mut self, rx: u64, tx: u64) {
        let seconds = self.at.elapsed().as_secs_f64();
        if seconds < 0.25 {
            return;
        }
        self.rx_rate = rx.saturating_sub(self.rx) as f64 / seconds;
        self.tx_rate = tx.saturating_sub(self.tx) as f64 / seconds;
        self.rx = rx;
        self.tx = tx;
        self.at = std::time::Instant::now();
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }

        let attrs = Window::default_attributes()
            .with_title("dtr")
            .with_inner_size(LogicalSize::new(900.0, 620.0));
        let window = match event_loop.create_window(attrs) {
            Ok(window) => Arc::new(window),
            Err(err) => {
                tracing::error!(%err, "create window");
                event_loop.exit();
                return;
            }
        };

        match Gpu::new(window, &self.look) {
            Ok(gpu) => {
                gpu.window.request_redraw();
                self.gpu = Some(gpu);
            }
            Err(err) => {
                tracing::error!(%err, "initialise GPU");
                event_loop.exit();
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, _event: UserEvent) {
        if let Some(err) = self.pane.error() {
            tracing::error!(%err, "pane failed");
            self.fatal = Some(err);
            event_loop.exit();
            return;
        }
        if let Some(frame) = self.pane.take_frame() {
            self.frame = Some(frame);
        }
        if let Some(gpu) = &self.gpu {
            gpu.window.request_redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };

        // egui sees every event; the terminal will get its own copy later.
        let response = gpu.egui_state.on_window_event(&gpu.window, &event);
        if response.repaint {
            gpu.window.request_redraw();
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::KeyboardInput { event, .. } => {
                let palette_key = self.modifiers.control_key()
                    && self.modifiers.shift_key()
                    && matches!(
                        event.logical_key,
                        winit::keyboard::Key::Character(ref character) if character.eq_ignore_ascii_case("p")
                    );
                let chord = |character: &str| {
                    self.modifiers.control_key()
                        && self.modifiers.shift_key()
                        && matches!(
                            event.logical_key,
                            winit::keyboard::Key::Character(ref typed) if typed.eq_ignore_ascii_case(character)
                        )
                        && event.state == winit::event::ElementState::Pressed
                };
                if chord("d") {
                    let level = !self.pane.stats().dtr();
                    self.pane.send(crate::pane::Command::Dtr(level));
                } else if chord("r") {
                    let level = !self.pane.stats().rts();
                    self.pane.send(crate::pane::Command::Rts(level));
                } else if chord("b") {
                    self.pane.send(crate::pane::Command::Break);
                } else if chord("c") {
                    // No mouse selection yet: copy the visible screen.
                    self.pane.send(crate::pane::Command::Copy);
                } else if chord("v") {
                    // The clipboard arrives as an egui paste event, which the
                    // render pass collects; nothing to do here.
                } else if chord("e") {
                    self.echo = !self.echo;
                    self.pane.send(crate::pane::Command::Echo(self.echo));
                } else if palette_key && event.state == winit::event::ElementState::Pressed {
                    self.palette.toggle();
                } else if !self.palette.open {
                    // The palette owns the keyboard while it is open.
                    if let Some(bytes) = crate::input::encode(&event, self.modifiers) {
                        self.pane.write(bytes);
                    }
                }
            }
            WindowEvent::Resized(size) => gpu.resize(size),
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(y),
                    winit::event::MouseScrollDelta::PixelDelta(position) => position.y / 20.0,
                };
                if lines != 0.0 {
                    // Wheel up shows older lines, which the terminal calls a
                    // negative delta.
                    self.pane
                        .send(crate::pane::Command::Scroll(-lines.round() as isize));
                }
            }
            WindowEvent::RedrawRequested => {
                let stats = self.pane.stats().clone();
                let (rx, tx) = (stats.rx(), stats.tx());
                self.meter.sample(rx, tx);
                let status = Status {
                    label: &self.label,
                    rx,
                    tx,
                    rx_rate: self.meter.rx_rate,
                    tx_rate: self.meter.tx_rate,
                    connected: stats.connected(),
                    modem: self
                        .serial
                        .then(|| (stats.dtr(), stats.rts())),
                };
                let mut action = None;
                let mut copied_text = self.pane.take_copied();
                let palette_open = self.palette.open;
                if let Err(err) = gpu.render(
                    self.frame.as_ref(),
                    &status,
                    &mut self.palette,
                    &mut action,
                    &self.pane,
                    palette_open,
                    &mut copied_text,
                    &self.look,
                ) {
                    tracing::error!(%err, "render");
                }
                if let Some(action) = action {
                    self.apply(action);
                }
                if self.quit {
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }
}

struct Gpu {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
}

impl Gpu {
    fn new(window: Arc<Window>, look: &Look) -> anyhow::Result<Self> {
        let size = window.inner_size();
        let instance = instance();
        let surface = instance.create_surface(Arc::clone(&window))?;

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))?;
        tracing::info!(adapter = %adapter.get_info().name, "GPU adapter");

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("dtr"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        }))?;

        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or_else(|| anyhow::anyhow!("adapter cannot present to this surface"))?;
        config.present_mode = wgpu::PresentMode::AutoVsync;
        // egui prefers a non-sRGB target and handles the conversion itself.
        if let Some(format) = surface
            .get_capabilities(&adapter)
            .formats
            .iter()
            .copied()
            .find(|f| matches!(f, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm))
        {
            config.format = format;
        }
        surface.configure(&device, &config);

        let egui_ctx = egui::Context::default();
        look.install(&egui_ctx);
        egui_ctx.set_visuals(egui::Visuals::dark());

        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            window.theme(),
            Some(2048),
        );

        let egui_renderer = egui_wgpu::Renderer::new(
            &device,
            config.format,
            egui_wgpu::RendererOptions::default(),
        );

        Ok(Self {
            window,
            surface,
            device,
            queue,
            config,
            egui_ctx,
            egui_state,
            egui_renderer,
        })
    }

    fn resize(&mut self, size: winit::dpi::PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
        self.window.request_redraw();
    }

    fn render(
        &mut self,
        frame: Option<&Frame>,
        status: &Status,
        palette: &mut Palette,
        action: &mut Option<Action>,
        pane: &Pane,
        palette_open: bool,
        copied_text: &mut Option<String>,
        look: &Look,
    ) -> anyhow::Result<()> {
        let output = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture)
            | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => texture,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            // Nothing to draw into this instant; try again on the next wake.
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Validation => return Ok(()),
        };

        let raw_input = self.egui_state.take_egui_input(&self.window);
        let mut full_output = self.egui_ctx.run_ui(raw_input, |ui| {
            egui::Panel::bottom("dtr-status")
                .frame(
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgb(0x14, 0x18, 0x16))
                        .inner_margin(egui::Margin::symmetric(8, 4)),
                )
                .show(ui, |ui| crate::ui::status_bar(ui, status, look));
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(egui::Color32::BLACK))
                .show(ui, |ui| match frame {
                    Some(frame) => crate::ui::draw(ui, frame, look),
                    None => {
                        ui.label("waiting for the pane…");
                    }
                });
                if let Some(chosen) = palette.ui(ui) {
                    *action = Some(chosen);
                }
                // Clipboard in and out. egui hands paste over as an event; the
                // pane hands copied text back through a slot it fills itself.
                let pasted: Vec<String> = ui
                    .ctx()
                    .input(|input| {
                        input
                            .events
                            .iter()
                            .filter_map(|event| match event {
                                egui::Event::Paste(text) => Some(text.clone()),
                                _ => None,
                            })
                            .collect()
                    });
                for text in pasted {
                    if !palette_open {
                        pane.send(crate::pane::Command::Paste(text.into_bytes()));
                    }
                }
                if let Some(text) = copied_text.take() {
                    ui.ctx().copy_text(text);
                }
        });

        self.egui_state
            .handle_platform_output(&self.window, full_output.platform_output);

        let pixels_per_point = full_output.pixels_per_point;
        let clipped = self
            .egui_ctx
            .tessellate(full_output.shapes, pixels_per_point);

        for (id, deltas) in &full_output.textures_delta.set {
            for delta in deltas {
                self.egui_renderer
                    .update_texture(&self.device, &self.queue, *id, delta);
            }
        }

        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point,
        };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("dtr") });
        let user_cmds = self.egui_renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            &clipped,
            &screen,
        );

        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("egui"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            self.egui_renderer.render(&mut pass, &clipped, &screen);
        }

        self.queue
            .submit(user_cmds.into_iter().chain(std::iter::once(encoder.finish())));
        self.queue.present(output);

        for id in &full_output.textures_delta.free {
            self.egui_renderer.free_texture(id);
        }
        // TexturesDelta panics if dropped with unapplied entries.
        full_output.textures_delta.clear();
        Ok(())
    }
}
