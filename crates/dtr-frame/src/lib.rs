//! The window, the surface, and the egui loop — one crate, so that a second
//! frontend can open a window without re-solving the traps.
//!
//! `crates/dtr` used to own all of this inside one 7 858-line `app.rs`. The
//! desktop's file picker is going to be a second process (`dtr-portal`) and the
//! manager a second window, and every one of those needs the same winit +
//! wgpu + egui-plumbing: a surface that survives being lost, a DPI factor taken
//! from the window rather than a ratio of two physical sizes, the viewport
//! commands that make a client-side caption draggable, the repaint that egui
//! asks for and the loop that has to carry it, and a drop order that does not
//! segfault on close. Re-inventing those once per frontend is how they get
//! re-broken; they live here instead.
//!
//! # The seam
//!
//! A [`Frame`] is one window: winit's window, the wgpu instance/adapter/device/
//! queue and the configured surface, one [`egui::Context`], one `egui_winit`
//! state and one `egui_wgpu` renderer. It knows nothing about terminals, panes,
//! tabs, a palette or a file listing — the application owns those.
//!
//! Each frame the application supplies two things:
//!
//! * the **scene** ([`Scene`]): what is drawn into the surface *under* egui —
//!   for dtr the terminal's grid. It can prepare buffers and draw one or two
//!   render passes; a frontend with no underlay implements the methods with
//!   empty bodies and returns a background colour.
//! * the **draw closure**: `&mut egui::Ui`, the root of this window's egui
//!   pass. Panels, cards, overlays — the application's business.
//!
//! The [`Frame`] then does the parts that are the same for every frontend:
//! tessellation, texture upload, the viewport-command round trip, the render
//! passes, present, a surface capture if one was asked for, and the drop-safe
//! order of the wgpu resources.
//!
//! [`FrameLoop`] owns the winit event loop and a [`Windows`] collection — a
//! `Vec` of [`Frame`]s, one `egui::Context` each, so the second window is not a
//! second event loop. An application implements [`App`]; the loop forwards
//! winit's callbacks and enforces the deadline discipline that
//! `set_control_flow`'s sticky state makes easy to get wrong.
//!
//! # Traps this crate exists to keep solved
//!
//! * **DPI** — [`Frame::scale`] is `window.scale_factor()`. `inner_size()` is
//!   already physical in winit 0.30, so dividing a surface width by it is
//!   always 1.0 and every point measurement would reserve half the space it
//!   needs on a HiDPI screen.
//! * **Surface loss** — a `Lost`/`Outdated` swapchain is reconfigured and the
//!   frame is skipped; a timeout or an occluded window is not an error.
//! * **`process_viewport_commands`** — `StartDrag`, `BeginResize`, `Maximized`
//!   and friends arrive in `FullOutput::viewport_output` and do nothing unless
//!   they are processed; egui only knows the maximised state because
//!   `update_viewport_info` tells it every frame (`docs/chrome.md` §8).
//! * **Repaint carrying** — after a frame, the loop must ask
//!   `Context::has_requested_repaint()` and request another redraw; on Wayland
//!   a redraw asked for *inside* a redraw waits for the compositor's frame
//!   callback, so a wake flag (see `Repaint` in the application) turns into a
//!   deadline. [`Frame::render`] does the egui half, the application keeps the
//!   wake half because it owns the reasons.
//! * **Never feed `RedrawRequested` to egui** — [`Frame::feed_event`] ignores
//!   it, so a caller that feeds every event cannot reintroduce the spin.
//! * **Drop order** — see [`Frame`]'s fields. Wrong order segfaults on close.
//! * **`textures_delta.clear()`** — egui panics if a delta is dropped
//!   unapplied; [`Frame::render`] applies then clears it.
//!
//! `docs/frame.md` is the standalone-consumer guide: the smallest program that
//! opens a window and draws one label.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};

/// The wgpu backends this process may use.
///
/// `WGPU_BACKEND` overrides the set, which is how a broken driver stack (or
/// Wine, which finds dx12 first and then cannot compile egui's shader through
/// FXC) is worked around without a rebuild.
pub fn instance() -> wgpu::Instance {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    if let Some(backends) = wgpu::Backends::from_env() {
        descriptor.backends = backends;
    }
    wgpu::Instance::new(descriptor)
}

/// The adapter wgpu picks when nobody asked for one by name.
///
/// `HighPerformance` is the default: this is a terminal someone stares at, and
/// asking for the low-power adapter hands it the integrated GPU of a machine
/// that has a real one sitting there. `[gpu] power` can say otherwise.
fn request_default_adapter(
    instance: &wgpu::Instance,
    surface: &wgpu::Surface<'static>,
    power_preference: wgpu::PowerPreference,
) -> anyhow::Result<wgpu::Adapter> {
    Ok(pollster::block_on(instance.request_adapter(
        &wgpu::RequestAdapterOptions {
            power_preference,
            compatible_surface: Some(surface),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        },
    ))?)
}

/// The adapter whose name contains `pattern`, case-insensitively.
///
/// Matching by name is crude, but it is the only handle wgpu offers that a
/// person can type: "nvidia", "radeon", "arc". Every adapter that cannot
/// present to this window is skipped, so a match cannot brick the window.
fn pick_adapter(
    instance: &wgpu::Instance,
    surface: &wgpu::Surface<'static>,
    pattern: &str,
) -> Option<wgpu::Adapter> {
    let needle = pattern.to_lowercase();
    let mut names = Vec::new();
    let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
    for adapter in adapters {
        let info = adapter.get_info();
        names.push(info.name.clone());
        if info.name.to_lowercase().contains(&needle) && adapter.is_surface_supported(surface) {
            return Some(adapter);
        }
    }
    tracing::debug!(?names, "adapters seen");
    None
}

/// What is drawn into the surface underneath egui.
///
/// dtr implements this with its grid renderer: [`prepare`](Scene::prepare)
/// writes the frame's vertex data, [`draw`](Scene::draw) is the clear-and-draw
/// pass, and [`draw_above`](Scene::draw_above) is the same pass after the clear
/// with `Load`, before egui's own draw — for dtr that is the images that asked
/// to be over the text. A frontend with nothing underneath returns the
/// background colour and does nothing else.
///
/// The order inside [`Frame::render`] is: `prepare`, then the grid pass
/// (`clear_color` + `draw`), then a second pass (`Load` + `draw_above` + egui).
pub trait Scene {
    /// The colour the first pass clears to, in the surface's own encoding.
    fn clear_color(&self) -> wgpu::Color;
    /// Upload whatever the frame needs, once, before either render pass.
    fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, size: (u32, u32));
    /// Draw the underlay into a pass that was begun with [`clear_color`].
    ///
    /// [`clear_color`]: Scene::clear_color
    fn draw(&mut self, pass: &mut wgpu::RenderPass<'_>);
    /// Draw anything that belongs over the underlay but under egui.
    fn draw_above(&mut self, pass: &mut wgpu::RenderPass<'_>);
}

/// One window asked for by a frontend.
///
/// The defaults are a decorated 900 × 620 window called `window`; every
/// frontend but a test sets them. `on_created` is the hook for platform work
/// that needs the native handle before the first frame (dtr subclasses the
/// window procedure there so Windows 11 offers snap layouts over its painted
/// maximise button).
pub struct WindowOptions {
    pub title: String,
    /// Logical points: what the window is asked for, before DPI.
    pub inner_size: (f32, f32),
    /// Whether the window manager paints the frame. `false` is a client-side
    /// frame; a compositor may still refuse on Wayland, which is why this is
    /// read once rather than switched later.
    pub decorations: bool,
    pub icon: Option<winit::window::Icon>,
    /// Wayland `app_id` / X11 `WM_CLASS` as `(name, general)`. Without it X11
    /// derives `WM_CLASS` from `argv[0]` and Wayland gets no `app_id` at all,
    /// so a running window is a second, iconless application in the dock.
    pub app_id: Option<(String, String)>,
    /// Whether the platform's input method may open a composition. A terminal
    /// wants it; a file picker's search field gets it from egui's own text
    /// field instead.
    pub ime_allowed: bool,
    /// Windows only: ask for a shadow on an undecorated window. A frameless
    /// window with no shadow reads as a rectangle pasted onto the desktop.
    pub undecorated_shadow: bool,
    /// Adapter name substring to prefer, e.g. `"nvidia"`.
    pub adapter_hint: Option<String>,
    /// What wgpu is asked for when no name wins: `HighPerformance` (the default)
    /// for the discrete GPU, `LowPower` for the integrated one, `None` to let
    /// wgpu decide. `[gpu] power`.
    pub power_preference: wgpu::PowerPreference,
    /// Called with the created window, before the [`Frame`] exists. The handle
    /// is the loop's `Arc`, cloned for the callback: platform work that needs
    /// the native handle (dtr subclasses the window procedure here) can keep it
    /// if it wants, and the loop still owns the window's lifetime.
    #[allow(clippy::type_complexity)]
    pub on_created: Option<Box<dyn FnOnce(Arc<Window>)>>,
}

impl Default for WindowOptions {
    fn default() -> Self {
        Self {
            title: String::from("window"),
            inner_size: (900.0, 620.0),
            decorations: true,
            icon: None,
            app_id: None,
            ime_allowed: false,
            undecorated_shadow: false,
            adapter_hint: None,
            power_preference: wgpu::PowerPreference::HighPerformance,
            on_created: None,
        }
    }
}

/// One window: its winit window, its surface, its wgpu device and its egui
/// context.
///
/// **The fields drop in declaration order and that order is load-bearing.**
/// Anything holding a texture, buffer or pipeline must go before the device
/// that owns them, the surface must go before the window it is a view of, and
/// the window must go last. Getting it wrong segfaults on close rather than
/// failing loudly, which is why it is written down. An application's own
/// `.wgpu` resources (dtr's grid) belong to it, not here, and must be dropped
/// before the [`Frame`]: the [`App`] is dropped before the [`Windows`] that
/// owns the frames, which is what makes that true for every frontend.
pub struct Frame {
    // Load-bearing order: GPU resources, then the device that owns them, then
    // the surface, then the window it came from.
    egui_renderer: egui_wgpu::Renderer,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    /// One context per window. Two windows with one context would share a
    /// texture atlas and a style, and an `egui::Id` collision would be two
    /// windows disagreeing about which one a popup belongs to.
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    adapter_name: String,
    /// The window's last reported size, until the next drawn frame configures
    /// the surface for it. A drag reports many; only the last one is drawn.
    pending_size: Option<winit::dpi::PhysicalSize<u32>>,
    /// The title the window was last told, so a frame that changes nothing does
    /// not talk to the window manager — which on Wayland is a round trip.
    title: String,
    window: Arc<Window>,
}

/// What one drawn frame cost, for a debug overlay or a log line.
#[derive(Debug, Clone, Copy)]
pub struct FrameTiming {
    /// CPU work up to and including `submit`, in milliseconds.
    pub cpu_ms: f32,
    /// How long `present` waited, when the caller asked to measure it. A
    /// vsynced frame waiting for the compositor is not work, so it is measured
    /// beside the CPU time rather than inside it (`docs/windows.md` §5.1).
    pub present_ms: Option<f32>,
}

/// What a caller tells [`Frame::render`] about this frame.
pub struct RenderOptions<'a> {
    /// How many frames this window has drawn; the first few and every 120th
    /// are logged at debug, enough to see a wedged surface without drowning a
    /// 60 Hz log.
    pub frames_drawn: u64,
    /// When the run started, for the one-time "first frame" log.
    pub started: Instant,
    /// Write a PNG of this frame's surface, before `present`, when set. This is
    /// the only picture of what the *window* draws, as opposed to the
    /// offscreen capture path (`docs/windows.md` §2).
    pub capture: Option<&'a Path>,
    /// Whether to time `present` for the debug overlay. Off means the `Instant`
    /// is not even read.
    pub measure_present: bool,
}

impl Frame {
    /// Configure a surface for `window` and build the egui side of it.
    ///
    /// The window is already created (see [`Windows::open`]): the adapter is
    /// chosen for the surface it has to present to, so the surface comes first.
    pub fn new(
        window: Arc<Window>,
        adapter_hint: Option<&str>,
        power_preference: wgpu::PowerPreference,
    ) -> anyhow::Result<Self> {
        let size = window.inner_size();
        let instance = instance();
        let surface = instance.create_surface(Arc::clone(&window))?;

        let adapter = match adapter_hint {
            Some(pattern) => match pick_adapter(&instance, &surface, pattern) {
                Some(adapter) => adapter,
                None => {
                    tracing::warn!(pattern, "no adapter matched; falling back");
                    request_default_adapter(&instance, &surface, power_preference)?
                }
            },
            None => request_default_adapter(&instance, &surface, power_preference)?,
        };
        tracing::info!(
            adapter = %adapter.get_info().name,
            backend = ?adapter.get_info().backend,
            "GPU adapter"
        );

        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("dtr-frame"),
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
        // One frame in the compositor's queue, not two.
        //
        // On DX12 — the backend a Windows machine gets — wgpu waits for a
        // swapchain buffer inside `get_current_texture`, on this thread, through
        // the frame-latency waitable object (`wgpu-hal`'s DX12 surface). A depth
        // of two lets the CPU run a frame ahead, so a keystroke's echo can sit
        // behind a whole refresh before it is even drawn; one is the lowest the
        // API allows and a terminal draws at most one frame per vblank, so
        // nothing is lost in throughput. Vulkan ignores the field, so Linux is
        // unchanged.
        config.desired_maximum_frame_latency = 1;
        // Copyable so the window can photograph itself: that is the only way to
        // verify what the *window* draws, as opposed to what the offscreen
        // capture path draws — and the two have disagreed before.
        config.usage = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC;
        // egui prefers a non-sRGB target and handles the conversion itself.
        if let Some(format) = surface
            .get_capabilities(&adapter)
            .formats
            .iter()
            .copied()
            .find(|f| {
                matches!(
                    f,
                    wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
                )
            })
        {
            config.format = format;
        }
        surface.configure(&device, &config);

        let egui_ctx = egui::Context::default();
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
        let adapter_name = adapter.get_info().name.clone();
        tracing::info!(
            format = ?config.format,
            width = config.width,
            height = config.height,
            present_mode = ?config.present_mode,
            adapter = %adapter_name,
            "window surface"
        );

        Ok(Self {
            egui_renderer,
            surface,
            device,
            queue,
            config,
            egui_ctx,
            egui_state,
            adapter_name,
            pending_size: None,
            title: String::new(),
            window,
        })
    }

    /// This window's egui context. One per window, deliberately.
    pub fn context(&self) -> &egui::Context {
        &self.egui_ctx
    }

    /// The window itself, for the platform work egui does not do: a native
    /// dialog's parent, an IME cursor area, a pointer shape, a monitor's
    /// refresh rate.
    pub fn window(&self) -> &Arc<Window> {
        &self.window
    }

    /// The winit window id, as the event loop reports it.
    pub fn id(&self) -> WindowId {
        self.window.id()
    }

    /// The wgpu device, for an application's own GPU resources (dtr's grid).
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// The wgpu queue, for an application's own uploads.
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// The surface's texture format, which an application's pipeline must match.
    pub fn format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    /// The surface's configured size, in physical pixels: what the last drawn
    /// frame laid out against.
    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Backing-store pixels per logical point, for chrome measurements.
    ///
    /// This is the DPI factor, **not** a ratio of two physical sizes: winit
    /// 0.30 reports `inner_size` in physical pixels already, so dividing the
    /// surface width by it is always 1.0 — and everything that multiplied by
    /// the resulting scale reserved half the space it needed on HiDPI.
    pub fn scale(&self) -> f32 {
        self.window.scale_factor() as f32
    }

    /// The adapter wgpu picked, for a debug overlay that names the GPU which
    /// produced the numbers it shows.
    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    /// Ask the compositor for a frame.
    pub fn request_redraw(&self) {
        self.window.request_redraw();
    }

    /// Feed one window event to egui.
    ///
    /// `RedrawRequested` is deliberately ignored: fed to egui, it asks for
    /// another frame and the loop runs at the refresh rate forever. Everything
    /// else egui sees. The return value is egui's "this needs a repaint", which
    /// the caller may or may not act on — a caller that also requests a redraw
    /// for pointer moves must still feed the event through here.
    pub fn feed_event(&mut self, event: &WindowEvent) -> bool {
        if matches!(event, WindowEvent::RedrawRequested) {
            return false;
        }
        self.egui_state.on_window_event(&self.window, event).repaint
    }

    /// Remember a new window size, without touching the swapchain yet.
    ///
    /// `WindowEvent::Resized` only records the size: a window drag on Windows
    /// delivers one `WM_SIZE` per mouse move and every `surface.configure` waits
    /// for the present queue to go idle (wgpu's DX12 backend does), so a
    /// configure per event is a vblank wait per event. [`Frame::render`]
    /// configures once, at the top of the frame that draws the new size.
    pub fn resize(&mut self, size: winit::dpi::PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.pending_size = Some(size);
    }

    /// Push the last reported window size into the swapchain, once per drawn
    /// frame.
    ///
    /// Call this at the top of a redraw, before asking [`Frame::size`] for the
    /// layout: the frame that reads the new size is also the frame that draws
    /// it. [`Frame::render`] calls it too, so a caller that forgets cannot draw
    /// at the old size; calling it twice in one frame is a no-op.
    pub fn configure_pending_resize(&mut self) {
        let Some(size) = self.pending_size.take() else {
            return;
        };
        if size.width == self.config.width && size.height == self.config.height {
            return;
        }
        self.config.width = size.width;
        self.config.height = size.height;
        let began = Instant::now();
        self.surface.configure(&self.device, &self.config);
        tracing::debug!(
            width = size.width,
            height = size.height,
            us = began.elapsed().as_micros() as u64,
            "surface resized"
        );
    }

    /// Keep the OS title in step with whatever names this window.
    ///
    /// With a client-side frame the strip is the only place a session's name is
    /// written, so this is what the taskbar and Alt-Tab show. `set_title` is a
    /// window-manager round trip on Wayland, hence the comparison — a frame
    /// that changes nothing does not talk to the window manager.
    pub fn set_title(&mut self, title: &str) {
        if self.title == title {
            return;
        }
        self.window.set_title(title);
        self.title = title.to_owned();
    }

    /// Draw, present, and photograph when asked.
    ///
    /// This is the whole per-frame contract: `scene` draws the underlay, `draw`
    /// draws egui, and the frame does tessellation, texture upload, the
    /// viewport-command round trip, the render passes, present and an optional
    /// surface capture. `Ok(None)` means the surface was lost or busy and
    /// nothing was drawn — not an error.
    pub fn render(
        &mut self,
        scene: &mut impl Scene,
        options: RenderOptions<'_>,
        draw: impl FnMut(&mut egui::Ui),
    ) -> anyhow::Result<Option<FrameTiming>> {
        // Defensive: a caller that skipped `configure_pending_resize` draws at
        // the size it just reported, not the one before it.
        self.configure_pending_resize();

        // The first few frames at debug, then every 120th: enough to see a
        // wedged surface without drowning the log at 60 Hz.
        let loud = options.frames_drawn < 3 || options.frames_drawn % 120 == 0;
        let began = Instant::now();
        let output = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture)
            | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => texture,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                if loud {
                    tracing::warn!("surface outdated; reconfiguring");
                }
                self.surface.configure(&self.device, &self.config);
                return Ok(None);
            }
            // Nothing to draw into this instant; try again on the next wake.
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Validation => return Ok(None),
        };

        // egui talks to the window through `ViewportCommand`s — start a drag,
        // begin a resize, maximise, minimise — and it only knows what the window
        // is doing (focused? maximised?) if it is told every frame. Both halves
        // are easy to leave out by hand: without `update_viewport_info` the
        // maximised flag is always `None`, so the restore glyph never appears,
        // and without `process_viewport_commands` every command is silently
        // dropped, which is a title bar that does not move and a window that
        // does not resize on Windows.
        let mut viewport_info = egui::ViewportInfo::default();
        egui_winit::update_viewport_info(&mut viewport_info, &self.egui_ctx, &self.window, false);
        let mut raw_input = self.egui_state.take_egui_input(&self.window);
        raw_input
            .viewports
            .insert(egui::ViewportId::ROOT, viewport_info.clone());
        let mut full_output = self.egui_ctx.run_ui(raw_input, draw);

        self.egui_state
            .handle_platform_output(&self.window, full_output.platform_output);

        // The other half of the viewport conversation: drag, resize, maximise,
        // minimise, title and decorations all arrive here as commands, and a
        // backend that drops them leaves the window's own caption dead.
        let mut actions_requested = Vec::new();
        if let Some(output) = full_output.viewport_output.remove(&egui::ViewportId::ROOT) {
            if !output.commands.is_empty() {
                // A caption press that does nothing is easiest to see here: the
                // command is either in this list or the frame never asked.
                tracing::debug!(commands = ?output.commands, "viewport commands");
            }
            egui_winit::process_viewport_commands(
                &self.egui_ctx,
                &mut viewport_info,
                output.commands,
                &self.window,
                &mut actions_requested,
            );
        }
        for action in actions_requested {
            // This crate handles the window and nothing else, so an action here
            // is a request the application does not make. Log it rather than
            // pretend it happened.
            tracing::debug!(?action, "egui asked for an action the window does not take");
        }

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
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("dtr-frame"),
            });
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

        scene.prepare(
            &self.device,
            &self.queue,
            (self.config.width, self.config.height),
        );
        if options.frames_drawn == 0 {
            // Startup is the one cost a user feels directly: the window appears
            // when this line is logged, and everything before it is what they
            // stared at.
            tracing::info!(
                elapsed_ms = options.started.elapsed().as_millis(),
                "first frame"
            );
        }

        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("underlay"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(scene.clear_color()),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            scene.draw(&mut pass);
        }

        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("egui"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            // Keep the underlay underneath.
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            // Images that asked to be over the text, before egui: a menu or a
            // card is still above the picture, and `--capture` photographs the
            // same pass.
            scene.draw_above(&mut pass);
            self.egui_renderer.render(&mut pass, &clipped, &screen);
        }

        self.queue.submit(
            user_cmds
                .into_iter()
                .chain(std::iter::once(encoder.finish())),
        );
        if let Some(path) = options.capture {
            if let Err(err) = self.write_capture(&output, path) {
                tracing::warn!(%err, path = %path.display(), "could not capture the window");
            }
        }
        // Everything above is our work; `present` waits for the compositor and
        // would otherwise be logged as a slow frame at 120 Hz. That is exactly
        // the gap `docs/windows.md` §5.1 names: `cpu_ms` stops here, so the
        // overlay measures the wait beside it — and only when the overlay is
        // on, so a window nobody is measuring pays no `Instant` for a number
        // nobody reads.
        let cpu_ms = began.elapsed().as_secs_f32() * 1000.0;
        let present_began = options.measure_present.then(Instant::now);
        self.queue.present(output);
        let present_ms = present_began.map(|began| began.elapsed().as_secs_f32() * 1000.0);

        for id in &full_output.textures_delta.free {
            self.egui_renderer.free_texture(id);
        }
        // TexturesDelta panics if dropped with unapplied entries.
        full_output.textures_delta.clear();
        Ok(Some(FrameTiming { cpu_ms, present_ms }))
    }

    /// Copy the frame that was just drawn into a PNG.
    ///
    /// The surface is configured `COPY_SRC` for this: an offscreen capture of
    /// the same state is not the same picture, and twice now the difference
    /// mattered (a menu that only drew in the window, a font that only broke
    /// there).
    fn write_capture(&self, output: &wgpu::SurfaceTexture, path: &Path) -> anyhow::Result<()> {
        let (width, height) = (self.config.width, self.config.height);
        // Copy rows must be a multiple of 256 bytes.
        let unpadded = width * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = unpadded.div_ceil(align) * align;

        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dtr-frame-capture"),
            size: u64::from(padded) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("dtr-frame-capture"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &output.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));

        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        });

        let mut pixels = Vec::with_capacity((unpadded * height) as usize);
        {
            let Ok(view) = slice.get_mapped_range() else {
                anyhow::bail!("could not map the capture buffer");
            };
            for row in 0..height {
                let start = (row * padded) as usize;
                let end = start + unpadded as usize;
                pixels.extend_from_slice(&view[start..end]);
            }
        }
        buffer.unmap();

        // The surface is BGRA; PNG wants RGBA.
        if self.config.format == wgpu::TextureFormat::Bgra8Unorm {
            for pixel in pixels.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
        }
        write_png(path, width, height, &pixels)?;
        tracing::info!(path = %path.display(), width, height, "captured the window");
        Ok(())
    }
}

/// Ask Windows for the window's system menu, at a point in egui points.
///
/// The menu is the one a native caption shows on a right-click or on its
/// title-bar icon, and winit exposes exactly that as `show_window_menu`.
/// Windows only, so everything else compiles to nothing.
pub fn show_system_menu(window: &Window, at: egui::Pos2) {
    #[cfg(target_os = "windows")]
    {
        let scale = window.scale_factor() as f32;
        if let Ok(inner) = window.inner_position() {
            window.show_window_menu(winit::dpi::PhysicalPosition::new(
                inner.x + (at.x * scale) as i32,
                inner.y + (at.y * scale) as i32,
            ));
        }
    }
    #[cfg(not(target_os = "windows"))]
    let _ = (window, at);
}

/// Write RGBA pixels to `path` as a PNG, the same file format the offscreen
/// capture path produces.
pub fn write_png(
    path: &std::path::Path,
    width: u32,
    height: u32,
    pixels: &[u8],
) -> anyhow::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut writer = std::io::BufWriter::new(file);
    let mut png = png::Encoder::new(&mut writer, width, height);
    png.set_color(png::ColorType::Rgba);
    png.set_depth(png::BitDepth::Eight);
    let mut png = png.write_header()?;
    png.write_image_data(pixels)?;
    png.finish()?;
    Ok(())
}

/// The windows one application has open.
///
/// A `Vec`, not one window: the manager is a second window over the same event
/// loop, and a frontend that can only make one has to grow this later — which
/// is the change this crate exists to avoid. Each [`Frame`] has its own
/// [`egui::Context`].
#[derive(Default)]
pub struct Windows {
    frames: Vec<Frame>,
}

impl Windows {
    /// Create a window and configure it.
    ///
    /// The returned `Frame` is borrowed from the collection: an application
    /// uses it for this callback only, and reaches its windows later through
    /// [`Windows::get_mut`] or the event loop, exactly as winit's event
    /// callbacks dictate.
    pub fn open(
        &mut self,
        event_loop: &ActiveEventLoop,
        options: WindowOptions,
    ) -> anyhow::Result<&mut Frame> {
        let WindowOptions {
            title,
            inner_size,
            decorations,
            icon,
            app_id,
            ime_allowed,
            undecorated_shadow,
            adapter_hint,
            power_preference,
            on_created,
        } = options;

        let attrs = Window::default_attributes()
            .with_title(title)
            .with_decorations(decorations)
            .with_window_icon(icon)
            .with_inner_size(winit::dpi::LogicalSize::new(inner_size.0, inner_size.1));
        #[cfg(target_os = "windows")]
        let attrs = if undecorated_shadow {
            use winit::platform::windows::WindowAttributesExtWindows as _;
            attrs.with_undecorated_shadow(true)
        } else {
            attrs
        };
        #[cfg(target_os = "linux")]
        let attrs = match &app_id {
            Some((name, general)) => {
                use winit::platform::wayland::WindowAttributesExtWayland as _;
                attrs.with_name(name.clone(), general.clone())
            }
            None => attrs,
        };
        #[cfg(not(target_os = "linux"))]
        let _ = &app_id;
        #[cfg(not(target_os = "windows"))]
        let _ = undecorated_shadow;

        let window = Arc::new(event_loop.create_window(attrs)?);
        if ime_allowed {
            // A terminal is where CJK is typed, and without this the platform
            // never offers its input method: no composition, no candidate
            // window, and a pinyin keyboard types latin letters at the shell.
            window.set_ime_allowed(true);
        }
        if let Some(on_created) = on_created {
            on_created(Arc::clone(&window));
        }
        let frame = Frame::new(window, adapter_hint.as_deref(), power_preference)?;
        self.frames.push(frame);
        Ok(self.frames.last_mut().expect("just pushed"))
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn get_mut(&mut self, id: WindowId) -> Option<&mut Frame> {
        self.frames.iter_mut().find(|frame| frame.id() == id)
    }

    /// The first window, for an application that has exactly one.
    pub fn primary(&mut self) -> Option<&mut Frame> {
        self.frames.first_mut()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Frame> {
        self.frames.iter_mut()
    }

    /// Close one window, dropping its surface and device.
    ///
    /// Returns whether it existed. An application that closes its last window
    /// usually wants `event_loop.exit()` too; this crate does not decide that.
    pub fn close(&mut self, id: WindowId) -> bool {
        let before = self.frames.len();
        self.frames.retain(|frame| frame.id() != id);
        self.frames.len() != before
    }
}

/// The earliest deadline an application wants, collected once per iteration.
///
/// `set_control_flow` is **sticky state, not a per-iteration timeout**:
/// `WaitUntil(now)` left in place with nothing else on idle iterations makes
/// every `poll` time out and the loop spin at 55 000 iterations a second. So an
/// application does not call `set_control_flow` at all: it calls
/// [`Deadlines::soon`] for every deadline it has, and [`FrameLoop`] sets
/// exactly one value afterwards — the earliest, or `Wait`.
#[derive(Default)]
pub struct Deadlines {
    soonest: Option<Instant>,
}

impl Deadlines {
    /// Nothing due.
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask to be woken at `at`, keeping the earliest of every request.
    pub fn soon(&mut self, at: Instant) {
        self.soonest = Some(match self.soonest {
            Some(current) => current.min(at),
            None => at,
        });
    }

    /// The earliest deadline asked for this iteration. `None` means `Wait`.
    pub fn earliest(&self) -> Option<Instant> {
        self.soonest
    }

    fn apply(&mut self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(match self.soonest.take() {
            Some(at) => ControlFlow::WaitUntil(at),
            None => ControlFlow::Wait,
        });
    }
}

/// What a frontend implements: the application callbacks, minus winit's
/// bookkeeping.
///
/// The methods mirror [`ApplicationHandler`], except that the windows are this
/// crate's collection and the control flow is collected rather than set:
///
/// * `window_event` receives the [`Frame`] for the window the event names, so a
///   frontend never passes a `WindowId` around to find its own surface.
/// * `about_to_wait` receives a [`Deadlines`] to fill in; the loop applies it.
///   An implementation that says nothing waits, which is the idle default.
pub trait App {
    type Event: 'static;

    /// Called once at startup and again after a suspend. Create the windows
    /// here; `windows.is_empty()` is what tells a resume from the first call.
    fn resumed(&mut self, event_loop: &ActiveEventLoop, windows: &mut Windows);

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window: &mut Frame,
        event: WindowEvent,
    );

    fn user_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        windows: &mut Windows,
        event: Self::Event,
    ) {
        let _ = (event_loop, windows, event);
    }

    /// Collect this iteration's deadlines. The loop sets the control flow
    /// afterwards; do not call `set_control_flow` anywhere else.
    fn about_to_wait(
        &mut self,
        event_loop: &ActiveEventLoop,
        windows: &mut Windows,
        deadlines: &mut Deadlines,
    ) {
        let _ = (event_loop, windows, deadlines);
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: winit::event::StartCause) {
        let _ = (event_loop, cause);
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop, windows: &mut Windows) {
        let _ = (event_loop, windows);
    }

    /// Once, when the event loop has ended and before the application — and
    /// therefore its GPU resources — is dropped. Report a fatal error here to
    /// turn it into the process's exit status.
    fn finished(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

/// The winit event loop and the windows it owns.
///
/// One loop for every window: two loops cannot share an `egui::Context` and
/// winit allows only one per thread anyway. A frontend that needs a second
/// window opens it through [`Windows::open`] from `resumed` or a user event;
/// the loop routes the window's events to [`App::window_event`] with that
/// window's [`Frame`].
pub struct FrameLoop<E: 'static> {
    event_loop: EventLoop<E>,
}

impl<E: 'static> FrameLoop<E> {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            event_loop: EventLoop::<E>::with_user_event().build()?,
        })
    }

    /// A handle workers and panes can send events through.
    pub fn proxy(&self) -> EventLoopProxy<E> {
        self.event_loop.create_proxy()
    }

    /// Run until the loop ends.
    ///
    /// The [`App`] is dropped **before** its windows — and therefore before the
    /// devices that own its wgpu resources — because that is the field order of
    /// the runner below. A frontend does not need to arrange its own drop order
    /// for that half of the rule.
    pub fn run<A: App<Event = E>>(self, app: A) -> anyhow::Result<()> {
        let Self { event_loop } = self;
        let mut runner = Runner {
            // Drop order: `app` first, `windows` (the devices) after it.
            app,
            windows: Windows::default(),
        };
        event_loop.run_app(&mut runner)?;
        runner.app.finished()
    }
}

/// The [`ApplicationHandler`] adapter. Not public: a frontend talks to
/// [`FrameLoop`].
struct Runner<A: App> {
    // Load-bearing order — the application, and the GPU resources it may hold,
    // drop before the windows and their devices.
    app: A,
    windows: Windows,
}

impl<A: App> ApplicationHandler<A::Event> for Runner<A> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.app.resumed(event_loop, &mut self.windows);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.windows.get_mut(window_id) else {
            return;
        };
        self.app.window_event(event_loop, window, event);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: A::Event) {
        self.app.user_event(event_loop, &mut self.windows, event);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Exactly one `set_control_flow` per iteration, from the earliest
        // deadline the application asked for — see [`Deadlines`].
        let mut deadlines = Deadlines::new();
        self.app
            .about_to_wait(event_loop, &mut self.windows, &mut deadlines);
        deadlines.apply(event_loop);
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: winit::event::StartCause) {
        self.app.new_events(event_loop, cause);
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        self.app.suspended(event_loop, &mut self.windows);
    }
}

#[cfg(test)]
mod tests {
    use super::Deadlines;
    use std::time::{Duration, Instant};

    /// The earliest of however many deadlines wins, and nothing asked for means
    /// `Wait` — the two halves of the sticky-control-flow rule.
    #[test]
    fn the_earliest_deadline_wins() {
        let now = Instant::now();
        let mut deadlines = Deadlines::new();
        assert_eq!(deadlines.earliest(), None, "nothing asked for waits");
        deadlines.soon(now + Duration::from_millis(50));
        deadlines.soon(now + Duration::from_millis(10));
        deadlines.soon(now + Duration::from_millis(30));
        assert_eq!(deadlines.earliest(), Some(now + Duration::from_millis(10)));
    }
}
