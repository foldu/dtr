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

use crate::explorer::{Explorer, PaneView as FilePaneView};
use crate::palette::{Action, Palette};
use crate::pane::{Frame, Pane as TermPane, Transport};
use crate::ui::Look;
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
    /// A cell range to show selected on the first frame (screenshots).
    pub select: Option<(usize, usize)>,
    /// Lines to scroll into the scrollback before capturing (screenshots).
    pub scroll: i64,
    /// Draw the capture as a split, to review the layout (screenshots).
    pub split: Option<crate::layout::Axis>,
    /// Open a file pane beside the initial session, at this directory.
    pub files: Option<PathBuf>,
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
    let pane = TermPane::spawn(
        startup.transport,
        startup.log.clone(),
        startup.theme.clone(),
        || {},
    )?;
    if !startup.send.is_empty() {
        pane.write(startup.send.clone());
    }
    if startup.scroll != 0 {
        // After the output has arrived: scrolling an empty screen clamps to
        // zero, and the result looks like a scroll that did nothing.
        std::thread::sleep(std::time::Duration::from_millis(u64::from(delay.as_millis() as u64 / 2)));
        pane.send(crate::pane::Command::Scroll(-startup.scroll as isize));
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

    // The file pane is static for a capture: open it next to the session and
    // let the screenshot path lay the two out side by side.
    let files = startup
        .files
        .as_ref()
        .map(|path| Explorer::new(Some(path.clone())));

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
        startup.select,
        startup.split,
        files.as_ref(),
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

    let pane = TermPane::spawn(startup.transport, startup.log.clone(), startup.theme.clone(), {
        let proxy = proxy.clone();
        move || {
            let _ = proxy.send_event(UserEvent::Frame);
        }
    })?;
    if !startup.send.is_empty() {
        pane.write(startup.send.clone());
    }

    let mut app = App {
        tabs: vec![Tab::new(
            1,
            Pane::Terminal(Session {
                pane,
                frame: None,
                label,
                serial,
                echo: false,
                selection: startup.select,
                meter: Meter::new(),
            }),
        )],
        active: 0,
        next_leaf: 2,
        gpu: None,
        modifiers: ModifiersState::empty(),
        fatal: None,
        quit: false,
        palette: Palette::new(startup.palette),
        look: startup.look.clone(),
        theme: startup.theme.clone(),
        proxy,
        store: startup.store,
        log: startup.log,
        dragging: false,
        cursor_position: None,
        frames_drawn: 0,
    };
    // `--files` for screenshots and for starting next to a listing; the pane
    // gets focus, so the arrow keys work immediately.
    if let Some(path) = startup.files.clone() {
        app.split_files(Some(path));
    }
    event_loop.run_app(&mut app)?;
    match app.fatal {
        Some(err) => anyhow::bail!("{err}"),
        None => Ok(()),
    }
}

/// The next tab after wrapping; `count < 2` has nowhere to go.
fn cycled(active: usize, count: usize, forward: bool) -> usize {
    if count < 2 {
        return active;
    }
    if forward {
        (active + 1) % count
    } else {
        (active + count - 1) % count
    }
}

/// Where the selection lands after closing the active tab, or `None` when that
/// was the last one.
fn closed(active: usize, count: usize) -> Option<usize> {
    if count <= 1 {
        return None;
    }
    Some(active.min(count - 2))
}

/// Translate the keys a file pane understands. Named arrows are
/// keyboard-layout independent; `h`, `r`, `g` and `G` are the vim-ish ones the
/// brief asks for. Modifiers are filtered by the caller.
fn explorer_key(event: &winit::event::KeyEvent) -> Option<crate::explorer::Key> {
    use winit::keyboard::{Key, NamedKey};

    if event.state != winit::event::ElementState::Pressed {
        return None;
    }
    match &event.logical_key {
        Key::Named(NamedKey::ArrowUp) => Some(crate::explorer::Key::Up),
        Key::Named(NamedKey::ArrowDown) => Some(crate::explorer::Key::Down),
        Key::Named(NamedKey::ArrowLeft) => Some(crate::explorer::Key::Left),
        Key::Named(NamedKey::Enter) => Some(crate::explorer::Key::Enter),
        Key::Named(NamedKey::Backspace) => Some(crate::explorer::Key::Left),
        Key::Character(text) => match text.as_str() {
            "h" => Some(crate::explorer::Key::Left),
            "r" => Some(crate::explorer::Key::Reload),
            "g" => Some(crate::explorer::Key::Top),
            "G" => Some(crate::explorer::Key::Bottom),
            _ => None,
        },
        _ => None,
    }
}

/// One open session. Tabs are just these, in order; only the active one is
/// drawn, but the others keep running and keep their own scrollback, selection
/// and statistics.
struct Session {
    pane: TermPane,
    frame: Option<Frame>,
    label: String,
    /// Whether this session has modem lines, for the status strip.
    serial: bool,
    echo: bool,
    /// Cell indices of a mouse selection: `(anchor, head)`.
    selection: Option<(usize, usize)>,
    meter: Meter,
}

/// What a leaf contains. The layout only knows leaf ids; this is the map entry
/// that says what the id *is*, which is why a widget pane is a new variant here
/// rather than a change to the split tree.
enum Pane {
    Terminal(Session),
    Files(Explorer),
}

impl Pane {
    fn terminal(&self) -> Option<&Session> {
        match self {
            Pane::Terminal(session) => Some(session),
            Pane::Files(_) => None,
        }
    }

    fn terminal_mut(&mut self) -> Option<&mut Session> {
        match self {
            Pane::Terminal(session) => Some(session),
            Pane::Files(_) => None,
        }
    }

    fn explorer(&self) -> Option<&Explorer> {
        match self {
            Pane::Files(explorer) => Some(explorer),
            Pane::Terminal(_) => None,
        }
    }
}

/// One tab: a layout of panes, the panes themselves, and which one has focus.
///
/// The layout knows nothing about what a pane *is*; `leaves` is the map that
/// decides, which is how a file-explorer pane becomes a new entry in this map
/// rather than a change to the geometry.
struct Tab {
    layout: crate::layout::Layout,
    leaves: std::collections::BTreeMap<crate::layout::LeafId, Pane>,
    focus: crate::layout::LeafId,
}

impl Tab {
    fn new(id: crate::layout::LeafId, pane: Pane) -> Self {
        let mut leaves = std::collections::BTreeMap::new();
        leaves.insert(id, pane);
        Self {
            layout: crate::layout::Layout::leaf(id),
            leaves,
            focus: id,
        }
    }

    fn focused_terminal(&self) -> Option<&Session> {
        self.leaves.get(&self.focus).and_then(Pane::terminal)
    }

    fn focused_terminal_mut(&mut self) -> Option<&mut Session> {
        let focus = self.focus;
        self.leaves.get_mut(&focus).and_then(Pane::terminal_mut)
    }

    fn focused_explorer(&self) -> Option<&Explorer> {
        self.leaves.get(&self.focus).and_then(Pane::explorer)
    }

    fn focused_explorer_mut(&mut self) -> Option<&mut Explorer> {
        let focus = self.focus;
        match self.leaves.get_mut(&focus) {
            Some(Pane::Files(explorer)) => Some(explorer),
            _ => None,
        }
    }

    /// The first terminal leaf in tree order, which is where a file pane puts
    /// the path the user picked. `None` when the tab is all files.
    fn first_terminal(&self) -> Option<crate::layout::LeafId> {
        self.layout
            .leaves()
            .into_iter()
            .find(|id| matches!(self.leaves.get(id), Some(Pane::Terminal(_))))
    }
}

struct App {
    tabs: Vec<Tab>,
    active: usize,
    next_leaf: crate::layout::LeafId,
    gpu: Option<Gpu>,
    modifiers: ModifiersState,
    /// Set when the pane fails; `run` returns it once the loop has stopped.
    fatal: Option<String>,
    quit: bool,
    palette: Palette,
    proxy: EventLoopProxy<UserEvent>,
    store: Option<Arc<dtr_store::Store>>,
    log: Option<(PathBuf, crate::serial::LogFormat)>,
    look: Look,
    dragging: bool,
    cursor_position: Option<(f64, f64)>,
    theme: Option<crate::theme::Theme>,
    frames_drawn: u64,
}

impl App {
    /// The focused terminal, when the focused pane is one. A file pane makes
    /// this `None`; callers that are terminal-only must handle that.
    fn session(&self) -> Option<&Session> {
        self.tabs[self.active].focused_terminal()
    }

    fn session_mut(&mut self) -> Option<&mut Session> {
        self.tabs[self.active].focused_terminal_mut()
    }

    fn focused_explorer(&self) -> Option<&Explorer> {
        self.tabs[self.active].focused_explorer()
    }

    fn focused_explorer_mut(&mut self) -> Option<&mut Explorer> {
        self.tabs[self.active].focused_explorer_mut()
    }

    /// Open a session in a new tab and switch to it. The palette's actions all
    /// come here: a terminal that replaced its session every time you connected
    /// somewhere would lose the one you were looking at.
    fn open(&mut self, transport: Transport) {
        let proxy = self.proxy.clone();
        let spawned = TermPane::spawn(
            transport.clone(),
            self.log.clone(),
            self.theme.clone(),
            move || {
                let _ = proxy.send_event(UserEvent::Frame);
            },
        );
        match spawned {
            Ok(pane) => {
                let session = Session {
                    pane,
                    frame: None,
                    label: transport.describe(),
                    serial: matches!(transport, Transport::Serial(_)),
                    echo: false,
                    selection: None,
                    meter: Meter::new(),
                };
                let id = self.next_leaf;
                self.next_leaf += 1;
                self.tabs.push(Tab::new(id, Pane::Terminal(session)));
                self.active = self.tabs.len() - 1;
            }
            Err(err) => {
                tracing::error!(%err, "could not start the session");
                self.fatal = Some(format!("{err}"));
            }
        }
    }

    /// Close the active tab. The last tab closes the window, because a terminal
    /// with no sessions has nothing to show.
    /// Close the focused pane; when it is the last pane in its tab, close the
    /// tab; when it is the last tab, close the window.
    fn close_active(&mut self) {
        let tab = &mut self.tabs[self.active];
        if tab.leaves.len() > 1 && tab.layout.remove(tab.focus) {
            let gone = tab.focus;
            tab.leaves.remove(&gone);
            // Focus the first remaining pane: predictable, and never the one
            // whose geometry just changed under the pointer.
            if let Some(first) = tab.layout.leaves().first().copied() {
                tab.focus = first;
            }
            return;
        }

        match closed(self.active, self.tabs.len()) {
            Some(active) => {
                self.tabs.remove(self.active);
                self.active = active;
            }
            // The last tab takes the window with it: a terminal with no
            // sessions has nothing to show.
            None => self.quit = true,
        }
    }

    /// Split the focused pane and give the new one its own session.
    fn split(&mut self, axis: crate::layout::Axis, transport: Transport) {
        let proxy = self.proxy.clone();
        let spawned = TermPane::spawn(
            transport.clone(),
            self.log.clone(),
            self.theme.clone(),
            move || {
                let _ = proxy.send_event(UserEvent::Frame);
            },
        );
        let pane = match spawned {
            Ok(pane) => pane,
            Err(err) => {
                tracing::error!(%err, "could not start the split");
                return;
            }
        };

        let id = self.next_leaf;
        self.next_leaf += 1;
        let tab = &mut self.tabs[self.active];
        let target = tab.focus;
        if !tab.layout.split(target, axis, id, false) {
            return;
        }
        tab.leaves.insert(
            id,
            Pane::Terminal(Session {
                pane,
                frame: None,
                label: transport.describe(),
                serial: matches!(transport, Transport::Serial(_)),
                echo: false,
                selection: None,
                meter: Meter::new(),
            }),
        );
        tab.focus = id;
    }

    /// Split the focused pane and put a file manager in the new half. Side by
    /// side is the useful default: a listing next to the command line it feeds.
    fn split_files(&mut self, path: Option<PathBuf>) {
        let id = self.next_leaf;
        self.next_leaf += 1;
        let tab = &mut self.tabs[self.active];
        let target = tab.focus;
        if !tab
            .layout
            .split(target, crate::layout::Axis::Horizontal, id, false)
        {
            return;
        }
        tab.leaves
            .insert(id, Pane::Files(Explorer::new(path)));
        tab.focus = id;
    }

    /// Put a file path on the first terminal in this tab's command line. With
    /// no terminal in the tab there is nowhere to put it, so the pane is inert
    /// rather than destructive.
    fn insert_path_into_terminal(&mut self, path: PathBuf) {
        let tab = &self.tabs[self.active];
        let Some(id) = tab.first_terminal() else {
            return;
        };
        if let Some(Pane::Terminal(session)) = self.tabs[self.active].leaves.get(&id) {
            session
                .pane
                .write(path.to_string_lossy().into_owned().into_bytes());
        }
    }

    fn cycle(&mut self, forward: bool) {
        self.active = cycled(self.active, self.tabs.len(), forward);
    }

    /// The panes to draw, with the rectangle each one occupies.
    ///
    /// The status strip is subtracted first: panes live above it, and a split
    /// that ran under the chrome would hide its own last row.
    /// `screen`, `scale` and `metrics` come from the caller because the GPU is
    /// taken out of `self` while it draws: reading them back through `self`
    /// returned nothing, and the window rendered an empty screen with no error.
    fn pane_views<'a>(
        &self,
        tab: &'a Tab,
        metrics: crate::render::Metrics,
        screen: (f32, f32),
        scale: f32,
    ) -> Vec<(crate::layout::LeafId, crate::render::PaneView<'a>)> {
        let status_height = crate::ui::STATUS_HEIGHT * scale;
        let area = crate::layout::Rect::new(
            0.0,
            0.0,
            screen.0,
            (screen.1 - status_height).max(0.0),
        );

        tab.layout
            .rects(area, crate::ui::SPLIT_GAP * scale)
            .into_iter()
            .map(|(id, rect)| {
                let _ = metrics;
                let (frame, selection, cursor) = match tab.leaves.get(&id) {
                    Some(Pane::Terminal(session)) => (
                        session.frame.as_ref(),
                        session.selection,
                        id == tab.focus,
                    ),
                    // A widget pane occupies its rectangle in the grid pass
                    // with nothing to draw; egui paints it afterwards.
                    Some(Pane::Files(_)) => (None, None, false),
                    None => (None, None, false),
                };
                (
                    id,
                    crate::render::PaneView {
                        rect,
                        frame,
                        selection,
                        cursor,
                    },
                )
            })
            .collect()
    }

    /// Which pane is under a physical pixel position, if any.
    fn pane_at(&self, x: f64, y: f64) -> Option<crate::layout::LeafId> {
        let gpu = self.gpu.as_ref()?;
        let tab = &self.tabs[self.active];
        let screen = (gpu.config.width as f32, gpu.config.height as f32);
        self.pane_views(tab, gpu.metrics(), screen, gpu.scale())
            .into_iter()
            .find(|(_, pane)| pane.rect.contains(x as f32, y as f32))
            .map(|(id, _)| id)
    }

    /// Which cell is under a physical pixel position, if any.
    ///
    /// The grid starts at the window origin: the status bar is a separate panel
    /// at the bottom, and the grid is not offset.
    fn cell_at(&self, x: f64, y: f64) -> Option<usize> {
        let gpu = self.gpu.as_ref()?;
        let metrics = gpu.metrics();
        if metrics.cell_width <= 0.0 || metrics.cell_height <= 0.0 {
            return None;
        }
        let session = self.session()?;
        let frame = session.frame.as_ref()?;
        let cols = u32::from(frame.cols);
        let rows = u32::from(frame.rows);
        if cols == 0 || rows == 0 {
            return None;
        }
        // Relative to the focused pane: a split moves the grid's origin.
        let tab = &self.tabs[self.active];
        let gpu = self.gpu.as_ref()?;
        let screen = (gpu.config.width as f32, gpu.config.height as f32);
        let origin = self
            .pane_views(tab, metrics, screen, gpu.scale())
            .into_iter()
            .find(|(id, _)| *id == tab.focus)
            .map(|(_, pane)| (pane.rect.x, pane.rect.y))?;
        let col = ((x as f32 - origin.0) / metrics.cell_width).floor().max(0.0) as u32;
        let row = ((y as f32 - origin.1) / metrics.cell_height).floor().max(0.0) as u32;
        if col >= cols || row >= rows {
            return None;
        }
        Some((row * cols + col) as usize)
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::Connect(port) => {
                let remembered = self
                    .store
                    .as_ref()
                    .and_then(|store| store.serial(&port).ok().flatten())
                    .unwrap_or_default();
                self.open(Transport::Serial(crate::serial::Config {
                    port,
                    baud: remembered.baud,
                    cr_to_crlf: remembered.cr_to_crlf,
                    auto_reconnect: true,
                }));
            }
            Action::Theme(name) => match crate::theme::load(&name) {
                Ok(theme) => {
                    // The chrome and the terminals have to move together, or
                    // the window shows one theme's text on another theme's
                    // surface. Every terminal in the tab gets it, including the
                    // ones a file pane pushed out of focus.
                    self.look = crate::ui::Look::themed(
                        self.look.size,
                        self.look.path.clone(),
                        Some(&theme),
                    );
                    let tab = &mut self.tabs[self.active];
                    for pane in tab.leaves.values_mut() {
                        if let Pane::Terminal(session) = pane {
                            session
                                .pane
                                .send(crate::pane::Command::SetTheme(Some(theme.clone())));
                        }
                    }
                }
                Err(err) => tracing::warn!(%err, "theme"),
            },
            Action::Files(path) => {
                let path = path.filter(|path| !path.is_empty()).map(PathBuf::from);
                self.split_files(path);
            }
            Action::Shell(shell) => {
                let program = (!shell.is_empty()).then_some(shell);
                self.open(Transport::Shell(crate::pty::Config { program }));
            }
            Action::Replay(path) => self.open(Transport::Replay(PathBuf::from(path))),
            Action::Demo => self.open(Transport::Demo),
            Action::Ssh(host) => {
                // Delegating to the system's ssh client: it already knows about
                // the config file, agent forwarding, jump hosts and known_hosts,
                // and re-implementing those is a project, not a feature.
                self.open(Transport::Shell(crate::pty::Config {
                    program: Some(format!("ssh -t {host}")),
                }));
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
        // Every terminal pane publishes frames, not only the focused one: a
        // file pane can have focus while the terminal beside it is still live.
        for tab in &mut self.tabs {
            for pane in tab.leaves.values_mut() {
                if let Pane::Terminal(session) = pane {
                    if let Some(frame) = session.pane.take_frame() {
                        session.frame = Some(frame);
                    }
                }
            }
        }
        if let Some(err) = self.session().and_then(|session| session.pane.error()) {
            tracing::error!(%err, "pane failed");
            self.fatal = Some(err);
            event_loop.exit();
            return;
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
                // Every key event, before any decision: if a character arrives
                // twice this is where you see it.
                tracing::debug!(
                    key = ?event.logical_key,
                    state = ?event.state,
                    text = ?event.text,
                    repeat = event.repeat,
                    "key"
                );
                let palette_key = self.modifiers.control_key()
                    && self.modifiers.shift_key()
                    && matches!(
                        event.logical_key,
                        winit::keyboard::Key::Character(ref character) if character.eq_ignore_ascii_case("p")
                    );
                // Scrollback: Shift+PageUp/Down a page at a time, Shift+Home/End
                // to the ends. These are the terminal conventions, and they need
                // the grid height, which the frame knows. With a file pane
                // focused there is no terminal to scroll.
                let page = self
                    .session()
                    .and_then(|session| session.frame.as_ref())
                    .map_or(10, |frame| frame.rows.saturating_sub(1)) as isize;
                let scroll_key = self.modifiers.shift_key()
                    && event.state == winit::event::ElementState::Pressed
                    && match event.logical_key {
                        winit::keyboard::Key::Named(winit::keyboard::NamedKey::PageUp) => {
                            if let Some(session) = self.session() {
                                session.pane.send(crate::pane::Command::Scroll(-page));
                            }
                            true
                        }
                        winit::keyboard::Key::Named(winit::keyboard::NamedKey::PageDown) => {
                            if let Some(session) = self.session() {
                                session.pane.send(crate::pane::Command::Scroll(page));
                            }
                            true
                        }
                        winit::keyboard::Key::Named(winit::keyboard::NamedKey::Home) => {
                            if let Some(session) = self.session() {
                                session
                                    .pane
                                    .send(crate::pane::Command::ScrollEdge { top: true });
                            }
                            true
                        }
                        winit::keyboard::Key::Named(winit::keyboard::NamedKey::End) => {
                            if let Some(session) = self.session() {
                                session
                                    .pane
                                    .send(crate::pane::Command::ScrollEdge { top: false });
                            }
                            true
                        }
                        _ => false,
                    };

                // Splits: the new pane gets a shell, because a serial port
                // cannot be opened twice and "split this port" is meaningless.
                let split_key = self.modifiers.control_key()
                    && self.modifiers.shift_key()
                    && event.state == winit::event::ElementState::Pressed
                    && match &event.logical_key {
                        winit::keyboard::Key::Character(text) => match text.as_str() {
                            "\\" => {
                                self.split(
                                    crate::layout::Axis::Horizontal,
                                    Transport::Shell(crate::pty::Config::default()),
                                );
                                true
                            }
                            "-" | "_" => {
                                self.split(
                                    crate::layout::Axis::Vertical,
                                    Transport::Shell(crate::pty::Config::default()),
                                );
                                true
                            }
                            _ => false,
                        },
                        _ => false,
                    };

                let tab_switch = self.modifiers.control_key()
                    && event.state == winit::event::ElementState::Pressed
                    && match &event.logical_key {
                        winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab) => {
                            self.cycle(!self.modifiers.shift_key());
                            true
                        }
                        winit::keyboard::Key::Character(text) if self.modifiers.alt_key() => {
                            match text.chars().next().and_then(|c| c.to_digit(10)) {
                                Some(0) => {
                                    self.active = self.tabs.len() - 1;
                                    true
                                }
                                Some(digit) if digit as usize <= self.tabs.len() => {
                                    self.active = digit as usize - 1;
                                    true
                                }
                                _ => false,
                            }
                        }
                        _ => false,
                    };

                let chord = |character: &str| {
                    self.modifiers.control_key()
                        && self.modifiers.shift_key()
                        && matches!(
                            event.logical_key,
                            winit::keyboard::Key::Character(ref typed) if typed.eq_ignore_ascii_case(character)
                        )
                        && event.state == winit::event::ElementState::Pressed
                };
                if scroll_key || tab_switch || split_key {
                    // Already handled above.
                } else if chord("w") {
                    // Closing a pane is layout, not terminal work: it works
                    // whichever kind of pane has focus.
                    self.close_active();
                } else if self.session().is_none() {
                    // A file pane has focus. It gets navigation keys and the
                    // palette chord; everything terminal-only is ignored.
                    if palette_key && event.state == winit::event::ElementState::Pressed {
                        self.palette.toggle();
                    } else if !self.palette.open
                        && !self.modifiers.control_key()
                        && !self.modifiers.alt_key()
                    {
                        if let Some(key) = explorer_key(&event) {
                            let chosen = self
                                .focused_explorer_mut()
                                .and_then(|explorer| explorer.handle(key));
                            if let Some(path) = chosen {
                                self.insert_path_into_terminal(path);
                            }
                        }
                    }
                    // File navigation changes what egui paints, not what the
                    // terminal emits, so ask for the frame explicitly.
                    if let Some(gpu) = self.gpu.as_ref() {
                        gpu.window.request_redraw();
                    }
                } else if chord("d") {
                    if let Some(session) = self.session() {
                        let level = !session.pane.stats().dtr();
                        session.pane.send(crate::pane::Command::Dtr(level));
                    }
                } else if chord("r") {
                    if let Some(session) = self.session() {
                        let level = !session.pane.stats().rts();
                        session.pane.send(crate::pane::Command::Rts(level));
                    }
                } else if chord("b") {
                    if let Some(session) = self.session() {
                        session.pane.send(crate::pane::Command::Break);
                    }
                } else if chord("c") {
                    if let Some(session) = self.session() {
                        match session.selection {
                            Some((start, end)) => session
                                .pane
                                .send(crate::pane::Command::CopyRange(start, end)),
                            // No selection: copy the visible screen, which is what a
                            // terminal's "copy all" means.
                            None => session.pane.send(crate::pane::Command::Copy),
                        }
                    }
                } else if chord("v") {
                    // The clipboard arrives as an egui paste event, which the
                    // render pass collects; nothing to do here.
                } else if chord("e") {
                    if let Some(session) = self.session_mut() {
                        session.echo = !session.echo;
                        session.pane.send(crate::pane::Command::Echo(session.echo));
                    }
                } else if palette_key && event.state == winit::event::ElementState::Pressed {
                    self.palette.toggle();
                } else if !self.palette.open {
                    // The palette owns the keyboard while it is open.
                    if let Some(bytes) = crate::input::encode(&event, self.modifiers) {
                        if let Some(session) = self.session() {
                            session.pane.write(bytes);
                        }
                    }
                }
            }
            WindowEvent::Resized(size) => gpu.resize(size),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_position = Some((position.x, position.y));
                if self.dragging {
                    if let Some(cell) = self.cell_at(position.x, position.y) {
                        if let Some(session) = self.session_mut() {
                            if let Some((anchor, _)) = session.selection {
                                session.selection = Some((anchor, cell));
                            }
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button == winit::event::MouseButton::Left {
                    match state {
                        winit::event::ElementState::Pressed => {
                            // Clicking a pane focuses it, which is what makes a
                            // split usable at all: keys follow the pointer.
                            if let Some((x, y)) = self.cursor_position {
                                if let Some(id) = self.pane_at(x, y) {
                                    self.tabs[self.active].focus = id;
                                }
                            }
                            // A click without a drag is not a selection until
                            // the pointer moves; the anchor waits here. A file
                            // pane has no cells to select.
                            let anchor = self
                                .cursor_position
                                .and_then(|(x, y)| self.cell_at(x, y))
                                .map(|cell| (cell, cell));
                            if let Some(session) = self.session_mut() {
                                session.selection = anchor;
                            }
                            self.dragging = self.session().is_some();
                        }
                        winit::event::ElementState::Released => {
                            self.dragging = false;
                        }
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(y),
                    winit::event::MouseScrollDelta::PixelDelta(position) => position.y / 20.0,
                };
                if lines != 0.0 {
                    // Wheel up shows older lines, which the terminal calls a
                    // negative delta. Only a focused terminal scrolls.
                    if let Some(session) = self.session() {
                        session
                            .pane
                            .send(crate::pane::Command::Scroll(-lines.round() as isize));
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                // Taken out for the draw: the renderer needs `&mut gpu` while
                // the frame needs `&self`, and one borrow cannot be split
                // across a field and the whole struct.
                let mut gpu = self.gpu.take();
                if let Some(gpu) = gpu.as_mut() {
                // The status strip follows the focused pane. A file pane has
                // no byte counters, and saying so is more honest than showing
                // the terminal beside it as if it were focused.
                let mut label = String::from("files");
                let (mut rx, mut tx) = (0u64, 0u64);
                let (mut rx_rate, mut tx_rate) = (0.0f64, 0.0f64);
                let mut connected = true;
                let mut scrollback = 0u64;
                let mut modem = None;
                if let Some(session) = self.session() {
                    let stats = session.pane.stats().clone();
                    label = session.label.clone();
                    (rx, tx) = (stats.rx(), stats.tx());
                    connected = stats.connected();
                    scrollback = stats.scrollback();
                    modem = session.serial.then(|| (stats.dtr(), stats.rts()));
                    if let Some(session) = self.session_mut() {
                        session.meter.sample(rx, tx);
                    }
                    if let Some(session) = self.session() {
                        rx_rate = session.meter.rx_rate;
                        tx_rate = session.meter.tx_rate;
                    }
                } else if let Some(explorer) = self.focused_explorer() {
                    label = explorer.title();
                }
                let tabs = (self.active + 1, self.tabs.len());
                let status = Status {
                    label: &label,
                    tabs,
                    rx,
                    tx,
                    rx_rate,
                    tx_rate,
                    connected,
                    scrollback,
                    modem,
                };
                let mut action = None;
                let mut copied_text = self
                    .session_mut()
                    .and_then(|session| session.pane.take_copied());
                let palette_open = self.palette.open;
                // Split the borrows explicitly: the palette is `&mut` while the
                // frame, pane and look are `&`, and taking them out one by one
                // is what makes that legal.
                let mut palette = std::mem::replace(&mut self.palette, Palette::new(false));
                let tab = &self.tabs[self.active];
                let screen = (gpu.config.width as f32, gpu.config.height as f32);
                let scale = gpu.scale();
                let panes: Vec<_> = self
                    .pane_views(tab, gpu.metrics(), screen, scale)
                    .into_iter()
                    .map(|(_, pane)| pane)
                    .collect();
                // File panes have no cells: they are painted with egui over
                // the rectangle the grid pass reserved for them.
                let (width, height) = (gpu.config.width as f32, gpu.config.height as f32);
                let status_height = crate::ui::STATUS_HEIGHT * gpu.scale();
                let area = crate::layout::Rect::new(
                    0.0,
                    0.0,
                    width,
                    (height - status_height).max(0.0),
                );
                let file_panes: Vec<FilePaneView<'_>> = tab
                    .layout
                    .rects(area, crate::ui::SPLIT_GAP * gpu.scale())
                    .into_iter()
                    .filter_map(|(id, rect)| match tab.leaves.get(&id) {
                        Some(Pane::Files(explorer)) => Some(FilePaneView {
                            id,
                            rect,
                            explorer,
                            focused: id == tab.focus,
                        }),
                        _ => None,
                    })
                    .collect();
                let terminal_pane = tab.focused_terminal().map(|session| &session.pane);
                let look = self.look.clone();
                let rendered = gpu.render(
                    &panes,
                    self.frames_drawn,
                    &file_panes,
                    &status,
                    &mut palette,
                    &mut action,
                    terminal_pane,
                    palette_open,
                    &mut copied_text,
                    &look,
                );
                self.palette = palette;
                self.frames_drawn = self.frames_drawn.wrapping_add(1);
                if let Err(err) = rendered {
                    tracing::error!(%err, "render");
                }
                if let Some(action) = action {
                    self.apply(action);
                }
                if self.quit {
                    event_loop.exit();
                }
                }
                self.gpu = gpu;
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
    grid: crate::render::GridRenderer,
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
        let grid = crate::render::GridRenderer::new(&device, config.format, look);
        tracing::info!(
            format = ?config.format,
            width = config.width,
            height = config.height,
            present_mode = ?config.present_mode,
            adapter = %adapter.get_info().name,
            "window surface"
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
            grid,
        })
    }

    /// Cell size in physical pixels, for hit-testing.
    fn metrics(&self) -> crate::render::Metrics {
        self.grid.metrics()
    }

    /// Backing-store pixels per logical point, for chrome measurements.
    fn scale(&self) -> f32 {
        self.config.width as f32 / self.window.inner_size().width.max(1) as f32
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
        panes: &[crate::render::PaneView<'_>],
        frames_drawn: u64,
        files: &[FilePaneView<'_>],
        status: &Status,
        palette: &mut Palette,
        action: &mut Option<Action>,
        pane: Option<&TermPane>,
        palette_open: bool,
        copied_text: &mut Option<String>,
        look: &Look,
    ) -> anyhow::Result<()> {
        // The first few frames at debug, then every 120th: enough to see a
        // wedged surface without drowning the log at 60 Hz.
        let loud = frames_drawn < 3 || frames_drawn % 120 == 0;
        let output = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture)
            | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => texture,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                if loud {
                    tracing::warn!("surface outdated; reconfiguring");
                }
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
                        .fill(crate::ui::Look::colour(look.surface))
                        .inner_margin(egui::Margin::symmetric(8, 4)),
                )
                .show(ui, |ui| crate::ui::status_bar(ui, status, look));
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(egui::Color32::TRANSPARENT))
                .show(ui, |_ui| {});
                // File panes sit over the grid rectangle their leaf owns. They
                // are areas rather than windows: fixed position, no dragging.
                for file in files {
                    crate::explorer::pane_ui(
                        ui,
                        egui::Id::unique(("dtr-files", file.id)),
                        file.rect,
                        file.explorer,
                        look,
                        file.focused,
                    );
                }
                // The palette is an overlay above the panes: a file listing
                // must not cover the command line it sits beside.
                if palette_open {
                    crate::ui::overlay(ui, "dtr-palette", |ui| {
                        if let Some(chosen) = palette.ui(ui, look) {
                            *action = Some(chosen);
                        }
                    });
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
                        if let Some(pane) = pane {
                            pane.send(crate::pane::Command::Paste(text.into_bytes()));
                        }
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

        self.grid.prepare(
            &self.device,
            &self.queue,
            panes,
            look,
            (self.config.width as f32, self.config.height as f32),
        );
        if loud {
            let with_content = panes.iter().filter(|pane| pane.frame.is_some()).count();
            tracing::debug!(
                frames_drawn,
                panes = panes.len(),
                with_content,
                instances = self.grid.instances(),
                "frame"
            );
        }

        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("grid"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: f64::from(look.background[0]) / 255.0,
                                g: f64::from(look.background[1]) / 255.0,
                                b: f64::from(look.background[2]) / 255.0,
                                a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            self.grid.draw(&mut pass);
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
                            // Keep the grid underneath.
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

#[cfg(test)]
mod tests {
    use super::{closed, cycled};

    #[test]
    fn cycling_wraps_both_ways() {
        assert_eq!(cycled(0, 3, true), 1);
        assert_eq!(cycled(2, 3, true), 0);
        assert_eq!(cycled(0, 3, false), 2);
        assert_eq!(cycled(2, 3, false), 1);
    }

    #[test]
    fn cycling_a_single_tab_stays_put() {
        assert_eq!(cycled(0, 1, true), 0);
        assert_eq!(cycled(0, 1, false), 0);
    }

    #[test]
    fn closing_the_last_tab_closes_the_window() {
        assert_eq!(closed(0, 1), None);
    }

    #[test]
    fn closing_a_middle_tab_selects_its_right_neighbour() {
        assert_eq!(closed(1, 3), Some(1));
    }

    #[test]
    fn closing_the_last_of_several_moves_left() {
        assert_eq!(closed(2, 3), Some(1));
        assert_eq!(closed(1, 2), Some(0));
    }
}
