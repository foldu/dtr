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
    /// Write a PNG of the live window once, `capture_after` milliseconds in.
    /// The only way to see what the window draws, as opposed to what the
    /// offscreen capture path draws.
    pub capture: Option<PathBuf>,
    pub capture_after: u64,
    /// A script of keys to type once the window opens, one per frame.
    pub keys: Option<String>,
    /// Close the window after this many milliseconds, to exercise shutdown.
    pub exit_after: Option<u64>,
    /// Adapter name to prefer, e.g. "nvidia".
    pub gpu: Option<String>,
    /// A cell range to show selected on the first frame (screenshots).
    pub select: Option<(usize, usize)>,
    /// Lines to scroll into the scrollback before capturing (screenshots).
    pub scroll: i64,
    /// Open the settings menu on start (also how it gets into screenshots).
    pub settings: bool,
    /// The config the menu edits, and where it came from.
    pub config: dtr_config::Config,
    pub config_path: PathBuf,
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
    let counters = !matches!(startup.transport, Transport::Shell(_));
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

    // Size the grid to the captured window before looking at it: a capture at
    // 80x24 in a 900 px window is a picture of a different program than the one
    // the window would show.
    {
        let metrics = crate::screenshot::cell_metrics(width, height)?;
        let cols = (f64::from(width) / f64::from(metrics.cell_width)).floor().max(1.0) as u16;
        let rows = ((f64::from(height) - f64::from(crate::ui::STATUS_HEIGHT)).max(1.0)
            / f64::from(metrics.cell_height))
        .floor()
        .max(1.0) as u16;
        pane.send(crate::pane::Command::Resize {
            cols,
            rows,
            width_px: width,
            height_px: height,
        });
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

    let settings = startup.settings.then(|| {
        let mut settings = crate::settings::Settings::new();
        settings.show(&startup.config);
        settings
    });
    crate::screenshot::capture(
        frame.as_ref(),
        path,
        width,
        height,
        &label,
        pane.stats(),
        startup.palette,
        serial,
        counters,
        &startup.look,
        startup.select,
        startup.split,
        files.as_ref(),
        None,
        settings.as_ref(),
    )
}

/// The adapter wgpu picks when nobody asked for one.
fn request_default_adapter(
    instance: &wgpu::Instance,
    surface: &wgpu::Surface<'static>,
) -> anyhow::Result<wgpu::Adapter> {
    Ok(pollster::block_on(instance.request_adapter(
        &wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
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
    let counters = !matches!(startup.transport, Transport::Shell(_));
    let port = match &startup.transport {
        Transport::Serial(cfg) => Some(cfg.port.clone()),
        _ => None,
    };

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
                port,
                frame: None,
                label,
                serial,
                counters,
                echo: false,
                selection: startup.select,
                meter: Meter::new(),
                grid: (0, 0),
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
        exit_after: startup.exit_after,
        started: std::time::Instant::now(),
        gpu_hint: startup.gpu.clone(),
        settings: {
            let mut settings = crate::settings::Settings::new();
            if startup.settings {
                settings.show(&startup.config);
            }
            settings
        },
        fonts_dirty: false,
        keymap: crate::keys::Keymap::new(&startup.config.keys),
        pending: Vec::new(),
        pending_since: None,
        script: startup
            .keys
            .as_deref()
            .map(crate::keys::parse_script)
            .unwrap_or_default()
            .into(),
        capture: startup
            .capture
            .clone()
            .map(|path| {
                (
                    path,
                    std::time::Instant::now()
                        + std::time::Duration::from_millis(startup.capture_after),
                )
            }),
        held: None,
        config: startup.config.clone(),
        config_path: startup.config_path.clone(),
        toast: None,
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
fn explorer_key(key: &winit::keyboard::Key) -> Option<crate::explorer::Key> {
    use winit::keyboard::{Key, NamedKey};

    match key {
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
    /// The serial port this session opened, when it is one. Per-port settings
    /// are remembered against it.
    port: Option<String>,
    /// The grid size we last asked for, so a resize is sent when it changes
    /// rather than on every frame.
    grid: (u16, u16),
    frame: Option<Frame>,
    label: String,
    /// Whether this session has modem lines, for the status strip.
    serial: bool,
    /// Whether byte counters belong on the status strip.
    counters: bool,
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
    /// What the tab strip calls this tab: the focused pane's session label,
    /// kept up to date when the tab is drawn for the *active* tab, and frozen
    /// otherwise so a background session cannot rename itself under the eye.
    title: String,
}

impl Tab {
    fn new(id: crate::layout::LeafId, pane: Pane) -> Self {
        let mut leaves = std::collections::BTreeMap::new();
        leaves.insert(id, pane);
        Self {
            title: String::new(),
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
    exit_after: Option<u64>,
    started: std::time::Instant,
    /// Adapter name to prefer, from `--gpu` or `DTR_GPU`.
    gpu_hint: Option<String>,
    /// Set when the grid must be rebuilt against a new face; the redraw has the
    /// device, and the settings menu does not.
    fonts_dirty: bool,
    /// What the config file says each chord means.
    keymap: crate::keys::Keymap,
    /// Chords typed towards a binding that takes more than one.
    pending: Vec<dtr_config::keys::Chord>,
    /// When the first chord of a pending binding was typed: Zed waits a second
    /// for the rest, then fires the longest binding that is already complete.
    pending_since: Option<std::time::Instant>,
    /// The keys still to type, from `--keys`.
    script: std::collections::VecDeque<crate::keys::KeyStroke>,
    /// Where to write a PNG of the live window, and when. Set by `--capture`.
    capture: Option<(std::path::PathBuf, std::time::Instant)>,
    /// The key the menu thinks is down. A second press for the same key without
    /// a release in between is a duplicate the compositor sent, not a second
    /// keystroke, and acting on it moves the selection two rows.
    held: Option<winit::keyboard::Key>,
    /// The settings menu, and the config it edits.
    settings: crate::settings::Settings,
    config: dtr_config::Config,
    config_path: PathBuf,
    /// A copy note and when it was made. Shown briefly, because "did that
    /// work?" is asked at the moment the answer still matters.
    toast: Option<(String, std::time::Instant)>,
}

impl App {
    /// A self-inflicted close: the window manager's close button cannot be
    /// pressed from a test, and shutdown bugs hide behind that.
    fn check_exit_deadline(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(after) = self.exit_after {
            if self.started.elapsed() >= std::time::Duration::from_millis(after) {
                tracing::info!("closing after the test interval");
                event_loop.exit();
            }
        }
    }

    /// Handle one key event. Both the window and `--keys` come through here, so
    /// a scripted run exercises the same decisions a person's hands would.
    fn on_key(&mut self, stroke: &crate::keys::KeyStroke) {
                // Every key event, before any decision: if a character arrives
                // twice this is where you see it.
                tracing::debug!(
                    key = ?stroke.key,
                    state = ?stroke.state,
                    text = ?stroke.text,
                    repeat = stroke.repeat,
                    "key"
                );
                // Alt+digit selects a tab by number: the only binding that
                // cannot be written as one chord, because it is "any digit".
                let alt_digit = stroke.mods.alt_key()
                    && stroke.state == winit::event::ElementState::Pressed
                    && match &stroke.key {
                        winit::keyboard::Key::Character(text) if stroke.mods.alt_key() => {
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

                // Everything else comes from the keymap, which the config file
                // can rewrite. `pending` holds the chords of a binding that is
                // not finished yet (`ctrl-k ctrl-s`).
                let pressed = stroke.state == winit::event::ElementState::Pressed;

                // The menu owns the keyboard while it is open. Releases only
                // clear the held key; duplicates are compositor noise.
                if self.settings.open {
                    if !pressed {
                        self.held = None;
                        return;
                    }
                    if !stroke.repeat && self.held.as_ref() == Some(&stroke.key) {
                        tracing::debug!(key = ?stroke.key, "ignoring a duplicate press");
                        return;
                    }
                    self.held = Some(stroke.key.clone());
                    let closes = crate::keys::chord_from_event(&stroke.key, stroke.mods)
                        .is_some_and(|chord| {
                            matches!(
                                self.keymap.resolve(&[], chord),
                                crate::keys::Matched::Action(crate::keys::Action::Settings)
                            )
                        });
                    if closes {
                        self.settings.open = false;
                        self.repaint("settings closed");
                        return;
                    }
                    match self.settings.handle(&stroke.key) {
                        Some(crate::settings::Effect::Apply) => self.apply_settings(),
                        Some(crate::settings::Effect::Cancel) => self.repaint("settings closed"),
                        Some(crate::settings::Effect::Changed) => self.repaint("settings changed"),
                        None => {}
                    }
                    return;
                }

                let mut consumed = alt_digit;
                let action = if pressed && !consumed {
                    crate::keys::chord_from_event(&stroke.key, stroke.mods).and_then(
                        |chord| match self.keymap.resolve(&self.pending, chord) {
                            crate::keys::Matched::Action(action) => {
                                self.pending.clear();
                                self.pending_since = None;
                                Some(action)
                            }
                            crate::keys::Matched::Prefix => {
                                self.pending.push(chord);
                                self.pending_since = Some(std::time::Instant::now());
                                self.repaint("prefix");
                                consumed = true;
                                None
                            }
                            crate::keys::Matched::Nothing => {
                                self.pending.clear();
                                self.pending_since = None;
                                None
                            }
                        },
                    )
                } else {
                    None
                };

                if let Some(action) = action {
                    consumed = self.dispatch(action);
                }

                if !consumed {
                    if self.session().is_none() {
                        // A file pane has focus: it gets navigation keys, and
                        // everything terminal-only is ignored.
                        if !self.palette.open
                            && !stroke.mods.control_key()
                            && !stroke.mods.alt_key()
                        {
                            if let Some(key) = explorer_key(&stroke.key) {
                                let chosen = self
                                    .focused_explorer_mut()
                                    .and_then(|explorer| explorer.handle(key));
                                if let Some(path) = chosen {
                                    self.insert_path_into_terminal(path);
                                }
                            }
                        }
                        self.repaint("file navigation");
                    } else if !self.palette.open && !self.settings.open {
                        // The palette and the menu own the keyboard while they
                        // are open; otherwise the key belongs to the shell.
                        if let Some(bytes) = crate::input::encode_key(&stroke.key, stroke.text.as_deref(), self.modifiers) {
                            if let Some(session) = self.session() {
                                session.pane.write(bytes);
                            }
                        }
                    }
                }
                }

    /// Do what a binding asks for. Returns whether it acted, so a terminal-only
    /// action with a file pane focused falls through to the pane's own keys.
    fn dispatch(&mut self, action: crate::keys::Action) -> bool {
        use crate::keys::Action;
        match action {
            Action::Palette => {
                self.palette.toggle();
                self.repaint("palette");
                true
            }
            Action::Settings => {
                if self.settings.open {
                    self.settings.open = false;
                } else {
                    let config = self.config.clone();
                    // The focused session's port, and what the store remembers
                    // for it: baud and CR translation are facts about that
                    // device, not about the terminal.
                    let port = self.session().and_then(|session| session.port.clone());
                    let record = port.as_ref().and_then(|port| {
                        self.store
                            .as_ref()
                            .and_then(|store| store.serial(port).ok().flatten())
                    });
                    self.settings.show_port(&config, port, record);
                }
                self.repaint("settings");
                true
            }
            Action::NewTab => {
                self.open(Transport::Shell(crate::pty::Config::default()));
                self.repaint("new tab");
                true
            }
            Action::ClosePane => {
                self.close_active();
                true
            }
            Action::NextTab => {
                self.cycle(true);
                self.repaint("next tab");
                true
            }
            Action::PreviousTab => {
                self.cycle(false);
                self.repaint("previous tab");
                true
            }
            Action::SplitHorizontal => {
                self.split(
                    crate::layout::Axis::Horizontal,
                    Transport::Shell(crate::pty::Config::default()),
                );
                true
            }
            Action::SplitVertical => {
                self.split(
                    crate::layout::Axis::Vertical,
                    Transport::Shell(crate::pty::Config::default()),
                );
                true
            }
            Action::Copy => {
                // The pane rebuilds the text from the terminal when asked; it
                // cannot reuse a frame, because drawing the frame is what
                // empties the pane's copy of it.
                let Some(session) = self.session() else {
                    return false;
                };
                match session.selection {
                    Some((start, end)) => session
                        .pane
                        .send(crate::pane::Command::CopyRange(start, end)),
                    None => session.pane.send(crate::pane::Command::Copy),
                }
                true
            }
            // The clipboard arrives as an egui paste event, which the render
            // pass collects; there is nothing to do on the key itself.
            Action::Paste => self.session().is_some(),
            Action::ScrollPageUp | Action::ScrollPageDown => {
                let page = self
                    .session()
                    .and_then(|session| session.frame.as_ref())
                    .map_or(10, |frame| frame.rows.saturating_sub(1)) as isize;
                let Some(session) = self.session() else {
                    return false;
                };
                let delta = if matches!(action, Action::ScrollPageUp) {
                    -page
                } else {
                    page
                };
                session.pane.send(crate::pane::Command::Scroll(delta));
                true
            }
            Action::ScrollTop | Action::ScrollBottom => {
                let Some(session) = self.session() else {
                    return false;
                };
                session.pane.send(crate::pane::Command::ScrollEdge {
                    top: matches!(action, Action::ScrollTop),
                });
                true
            }
            Action::ModemDtr | Action::ModemRts | Action::ModemBreak => {
                let Some(session) = self.session() else {
                    return false;
                };
                match action {
                    Action::ModemDtr => {
                        let level = !session.pane.stats().dtr();
                        session.pane.send(crate::pane::Command::Dtr(level));
                    }
                    Action::ModemRts => {
                        let level = !session.pane.stats().rts();
                        session.pane.send(crate::pane::Command::Rts(level));
                    }
                    _ => session.pane.send(crate::pane::Command::Break),
                }
                true
            }
            Action::Echo => {
                let Some(session) = self.session_mut() else {
                    return false;
                };
                session.echo = !session.echo;
                session.pane.send(crate::pane::Command::Echo(session.echo));
                true
            }
        }
    }

    /// Write the menu's draft to the config file and make it live.
    ///
    /// Serial settings reach the pane as a command because a baud rate cannot
    /// be applied to an open port in place: the pane reconnects, keeps its
    /// scrollback, and comes back talking at the new rate.
    fn apply_settings(&mut self) {
        let draft = self.settings.draft().clone();
        for (key, value) in self.settings.writes() {
            if let Err(err) = dtr_config::set_value(&self.config_path, &key, &value) {
                tracing::warn!(%key, %err, "could not save a setting");
            }
        }

        let font_changed = draft.font.size != self.config.font.size
            || draft.font.family != self.config.font.family;
        let theme_changed = draft.theme != self.config.theme;
        let serial_changed = draft.serial != self.config.serial;

        self.config = draft;
        self.look.size = self.config.font.size;
        self.look.family = self.config.font.family.clone();

        if theme_changed {
            let theme = self
                .config
                .theme
                .as_deref()
                .and_then(|name| crate::theme::load(name).ok());
            self.look = crate::ui::Look::themed(
                self.look.size,
                self.look.family.clone(),
                self.look.path.clone(),
                theme.as_ref(),
            );
            self.theme = theme.clone();
            for tab in &mut self.tabs {
                for pane in tab.leaves.values_mut() {
                    if let Pane::Terminal(session) = pane {
                        session
                            .pane
                            .send(crate::pane::Command::SetTheme(theme.clone()));
                    }
                }
            }
        }

        // The port's rate and CR translation are remembered per device, so they
        // go to the store and straight to the open session; `auto_reconnect` is
        // the one serial setting that is a policy of the terminal, so it goes to
        // every session.
        if let Some((port, record)) = self.settings.serial_write() {
            if let Some(store) = self.store.as_ref() {
                if let Err(err) = store.remember_serial(&port, &record) {
                    tracing::warn!(%port, %err, "could not remember the port settings");
                }
            }
            for tab in &mut self.tabs {
                for pane in tab.leaves.values_mut() {
                    if let Pane::Terminal(session) = pane {
                        if session.port.as_deref() == Some(port.as_str()) {
                            session.pane.send(crate::pane::Command::SetSerial {
                                baud: record.baud,
                                cr_to_crlf: record.cr_to_crlf,
                                auto_reconnect: self.config.serial.auto_reconnect,
                            });
                        }
                    }
                }
            }
        } else if serial_changed {
            for tab in &mut self.tabs {
                for pane in tab.leaves.values_mut() {
                    if let Pane::Terminal(session) = pane {
                        if let Some(port) = session.port.clone() {
                            let record = self
                                .store
                                .as_ref()
                                .and_then(|store| store.serial(&port).ok().flatten())
                                .unwrap_or_default();
                            session.pane.send(crate::pane::Command::SetSerial {
                                baud: record.baud,
                                cr_to_crlf: record.cr_to_crlf,
                                auto_reconnect: self.config.serial.auto_reconnect,
                            });
                        }
                    }
                }
            }
        }

        self.settings.open = false;
        if font_changed {
            // The grid must be rebuilt against the new face, which needs the
            // device; the next redraw has it.
            self.fonts_dirty = true;
        }
        self.repaint("settings applied");
    }

    /// Ask for a frame, and say why: a terminal that redraws continuously while
    /// nothing moves is a battery bug, and the reason is the only way to find it.
    fn repaint(&self, why: &'static str) {
        tracing::debug!(why, "request redraw");
        if let Some(gpu) = &self.gpu {
            gpu.window.request_redraw();
        }
    }

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
                    port: match &transport {
                        Transport::Serial(cfg) => Some(cfg.port.clone()),
                        _ => None,
                    },
                    frame: None,
                    label: transport.describe(),
                    serial: matches!(transport, Transport::Serial(_)),
                    counters: !matches!(transport, Transport::Shell(_)),
                    echo: false,
                    selection: None,
                    meter: Meter::new(),
                    grid: (0, 0),
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
                port: match &transport {
                    Transport::Serial(cfg) => Some(cfg.port.clone()),
                    _ => None,
                },
                frame: None,
                label: transport.describe(),
                serial: matches!(transport, Transport::Serial(_)),
                counters: !matches!(transport, Transport::Shell(_)),
                echo: false,
                selection: None,
                meter: Meter::new(),
                grid: (0, 0),
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
        let status_height = (crate::ui::STATUS_HEIGHT + crate::ui::GRID_GAP) * scale;
        // The tab strip takes the top, and only when there is more than one tab
        // to show: a lone tab is a title nobody needs.
        let tabs_height = if self.tabs.len() > 1 {
            (crate::ui::TAB_HEIGHT + crate::ui::GRID_GAP) * scale
        } else {
            0.0
        };
        let area = crate::layout::Rect::new(
            0.0,
            tabs_height,
            screen.0,
            (screen.1 - status_height - tabs_height).max(0.0),
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

    /// Ask every terminal pane for the grid that fits its rectangle.
    ///
    /// Called after the first frame and whenever the window or the layout
    /// changes: a terminal that keeps its 80×24 grid in a 1900 px window wastes
    /// two thirds of the screen, and one that keeps it in a 400 px window draws
    /// past its own edge.
    fn fit_panes(&mut self, metrics: crate::render::Metrics, screen: (f32, f32), scale: f32) {
        if metrics.cell_width <= 0.0 || metrics.cell_height <= 0.0 {
            return;
        }
        let tab = &mut self.tabs[self.active];
        let rects = tab
            .layout
            .rects(
                crate::layout::Rect::new(
                    0.0,
                    0.0,
                    screen.0,
                    (screen.1 - (crate::ui::STATUS_HEIGHT + crate::ui::GRID_GAP) * scale)
                        .max(0.0),
                ),
                crate::ui::SPLIT_GAP * scale,
            );
        for (id, rect) in rects {
            let Some(session) = tab.leaves.get_mut(&id).and_then(Pane::terminal_mut) else {
                continue;
            };
            let cols = (rect.width / metrics.cell_width).floor().max(1.0) as u16;
            let rows = (rect.height / metrics.cell_height).floor().max(1.0) as u16;
            if session.grid == (cols, rows) {
                continue;
            }
            session.grid = (cols, rows);
            session.pane.send(crate::pane::Command::Resize {
                cols,
                rows,
                width_px: rect.width as u32,
                height_px: rect.height as u32,
            });
        }
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
                        self.look.family.clone(),
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

        match Gpu::new(window, &self.look, self.gpu_hint.as_deref()) {
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

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.check_exit_deadline(event_loop);
        // `--keys`: one stroke per wake-up, so the frame that follows each one
        // exists before the next arrives. This is the harness the user asked
        // for: a scripted run goes through the same handler a keyboard does.
        if let Some(stroke) = self.script.pop_front() {
            self.on_key(&stroke);
            self.repaint("scripted key");
            event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                std::time::Instant::now() + std::time::Duration::from_millis(16),
            ));
        }

        // A pending capture needs a frame at its deadline. Without this the
        // window goes idle after the last redraw and the capture never
        // happens: the deadline passes with nothing drawing.
        if let Some((_, when)) = self.capture.as_ref().map(|(_, when)| ((), *when)) {
            self.repaint("capture pending");
            event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(when));
        }

        // A half-typed binding must not wait forever. One second, like Zed,
        // then whatever is already complete fires.
        const PENDING: std::time::Duration = std::time::Duration::from_secs(1);
        if let Some(since) = self.pending_since {
            let deadline = since + PENDING;
            if std::time::Instant::now() >= deadline {
                let pending = std::mem::take(&mut self.pending);
                self.pending_since = None;
                match self.keymap.flush(&pending) {
                    Some(action) => {
                        self.dispatch(action);
                    }
                    None => tracing::debug!(?pending, "no binding completed"),
                }
                self.repaint("pending flushed");
            } else {
                event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(deadline));
            }
        }
        // Wake up when the deadline is due, rather than hoping some other
        // event brings the loop back: an idle window that never wakes would
        // never take its own exit.
        if let Some(after) = self.exit_after {
            let deadline = self.started + std::time::Duration::from_millis(after);
            event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(deadline));
        }
    }

    fn new_events(
        &mut self,
        event_loop: &ActiveEventLoop,
        _cause: winit::event::StartCause,
    ) {
        self.check_exit_deadline(event_loop);
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
        self.repaint("pane frame");
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };

        // egui sees every event except the one asking for a frame: fed
        // `RedrawRequested`, egui asks for another frame, and the loop runs at
        // the refresh rate forever — which is exactly the CPU burn this cost.
        if !matches!(event, WindowEvent::RedrawRequested) {
            let response = gpu.egui_state.on_window_event(&gpu.window, &event);
            if response.repaint {
                tracing::debug!(why = "egui", "request redraw");
                gpu.window.request_redraw();
            }
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::KeyboardInput { event, .. } => {
                let stroke = crate::keys::KeyStroke {
                    key: event.logical_key.clone(),
                    text: event.text.as_ref().map(|text| text.to_string()),
                    state: event.state,
                    repeat: event.repeat,
                    mods: self.modifiers,
                };
                self.on_key(&stroke);
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
                let mut counters = false;
                if let Some(session) = self.session() {
                    let stats = session.pane.stats().clone();
                    label = session.label.clone();
                    (rx, tx) = (stats.rx(), stats.tx());
                    connected = stats.connected();
                    scrollback = stats.scrollback();
                    modem = session.serial.then(|| (stats.dtr(), stats.rts()));
                    counters = session.counters;
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
                    counters,
                };
                let mut action = None;
                let mut copied = None;
                let mut copied_text = self
                    .session_mut()
                    .and_then(|session| session.pane.take_copied());
                // A toast lives for 1.8 s and fades over its last 400 ms.
                const TOAST: std::time::Duration = std::time::Duration::from_millis(2200);
                // Owned, because the draw mutably borrows `self` further down.
                let toast_text: Option<(String, f32)> =
                    self.toast.as_ref().and_then(|(text, at)| {
                        let age = at.elapsed();
                        if age > TOAST {
                            return None;
                        }
                        let fade = std::time::Duration::from_millis(450);
                        let alpha = if age + fade > TOAST {
                            (TOAST - age).as_secs_f32() / fade.as_secs_f32()
                        } else {
                            1.0
                        };
                        Some((text.clone(), alpha))
                    });
                let palette_open = self.palette.open;
                // Split the borrows explicitly: the palette is `&mut` while the
                // frame, pane and look are `&`, and taking them out one by one
                // is what makes that legal.
                // `take` rather than building a placeholder: `Palette::new`
                // reads the machine, and this runs every frame.
                let mut palette = std::mem::take(&mut self.palette);
                let screen = (gpu.config.width as f32, gpu.config.height as f32);
                let scale = gpu.scale();
                if std::mem::take(&mut self.fonts_dirty) {
                    gpu.rebuild_grid(&self.look);
                }
                self.fit_panes(gpu.metrics(), screen, scale);
                let settings = if self.settings.open { Some(&self.settings) } else { None };
                // A window capture is due once, then never again.
                // The active tab follows its focused session; the others keep
                // the label they had, so a background session cannot rename
                // itself under the eye.
                if let Some(session) = self.tabs[self.active].focused_terminal() {
                    let label = session.label.clone();
                    self.tabs[self.active].title = label;
                }
                let tabs: Vec<String> = self
                    .tabs
                    .iter()
                    .enumerate()
                    .map(|(index, tab)| {
                        if tab.title.is_empty() {
                            format!("tab {}", index + 1)
                        } else {
                            tab.title.clone()
                        }
                    })
                    .collect();
                let mut tab_click = None;
                let capture_path = match self.capture.take() {
                    Some((path, when)) if std::time::Instant::now() >= when => Some(path),
                    other => {
                        self.capture = other;
                        None
                    }
                };
                let tab = &self.tabs[self.active];
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
                    self.started,
                    &file_panes,
                    &status,
                    &mut palette,
                    &mut action,
                    terminal_pane,
                    palette_open,
                    &mut copied_text,
                    &mut copied,
                    toast_text
                        .as_ref()
                        .map(|(text, alpha)| (text.as_str(), *alpha)),
                    settings,
                    &tabs,
                    self.active,
                    &mut tab_click,
                    capture_path.as_deref(),
                    &look,
                );
                self.palette = palette;
                if let Some(index) = tab_click {
                    // A click on a tab is a switch, exactly like the chord.
                    if index < self.tabs.len() {
                        self.active = index;
                        self.repaint("tab clicked");
                    }
                }
                drop(toast_text);
                if let Some(text) = copied {
                    let chars = text.chars().count();
                    let bytes = text.len();
                    // An empty copy is not a success: "copied 0 chars" claims
                    // work that did not happen, and the usual cause is a click
                    // that selected one blank cell.
                    let note = if chars == 0 {
                        "nothing to copy".to_owned()
                    } else if chars < 40 {
                        format!("copied {chars} chars")
                    } else {
                        format!("copied {bytes} bytes")
                    };
                    self.toast = Some((note, std::time::Instant::now()));
                }
                if self.toast.as_ref().is_some_and(|(_, at)| at.elapsed() > TOAST) {
                    self.toast = None;
                }
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
    // Fields drop in declaration order, and that order is load-bearing here.
    // Anything holding a texture, buffer or pipeline must go before the device
    // that owns them, the surface must go before the window it is a view of, and
    // the window must go last. Getting it wrong segfaults on close rather than
    // failing loudly, which is why it is written down.
    egui_renderer: egui_wgpu::Renderer,
    grid: crate::render::GridRenderer,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    window: Arc<Window>,
}

impl Gpu {
    /// Copy the frame that was just drawn into a PNG.
    ///
    /// The surface is configured `COPY_SRC` for this: an offscreen capture of
    /// the same state is not the same picture, and twice now the difference
    /// mattered (a menu that only drew in the window, a font that only broke
    /// there).
    fn write_capture(&self, output: &wgpu::SurfaceTexture, path: &std::path::Path) -> anyhow::Result<()> {
        let (width, height) = (self.config.width, self.config.height);
        // Copy rows must be a multiple of 256 bytes.
        let unpadded = width * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = unpadded.div_ceil(align) * align;

        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dtr-capture"),
            size: u64::from(padded) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("dtr-capture"),
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
        let _ = self
            .device
            .poll(wgpu::PollType::Wait {
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
        crate::screenshot::write_png(path, width, height, &pixels)?;
        tracing::info!(path = %path.display(), width, height, "captured the window");
        Ok(())
    }

    /// Rebuild the grid for a new face or size. Cheap enough to do on a settings
    /// change: it is a fresh atlas, not a reload of the whole renderer.
    fn rebuild_grid(&mut self, look: &Look) {
        self.grid = crate::render::GridRenderer::new(&self.device, self.config.format, look);
        tracing::info!(family = ?look.family, size = look.size, "grid rebuilt");
    }

    fn new(window: Arc<Window>, look: &Look, hint: Option<&str>) -> anyhow::Result<Self> {
        let size = window.inner_size();
        let instance = instance();
        let surface = instance.create_surface(Arc::clone(&window))?;

        // HighPerformance, not LowPower: this is a terminal someone stares at,
        // and asking for the low-power adapter hands it the integrated GPU of a
        // machine that has a real one sitting there.
        let adapter = match hint {
            Some(pattern) => match pick_adapter(&instance, &surface, pattern) {
                Some(adapter) => adapter,
                None => {
                    tracing::warn!(pattern, "no adapter matched; falling back");
                    request_default_adapter(&instance, &surface)?
                }
            },
            None => request_default_adapter(&instance, &surface)?,
        };
        tracing::info!(
            adapter = %adapter.get_info().name,
            backend = ?adapter.get_info().backend,
            "GPU adapter"
        );

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
            egui_renderer,
            grid,
            surface,
            device,
            queue,
            config,
            egui_ctx,
            egui_state,
            window,
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
        started: std::time::Instant,
        files: &[FilePaneView<'_>],
        status: &Status,
        palette: &mut Palette,
        action: &mut Option<Action>,
        pane: Option<&TermPane>,
        palette_open: bool,
        copied_text: &mut Option<String>,
        copied: &mut Option<String>,
        toast: Option<(&str, f32)>,
        settings: Option<&crate::settings::Settings>,
        tabs: &[String],
        active_tab: usize,
        tab_click: &mut Option<usize>,
        capture: Option<&std::path::Path>,
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
            if !tabs.is_empty() {
                egui::Panel::top("dtr-tabs")
                    .exact_size(crate::ui::TAB_HEIGHT * self.scale())
                    .frame(
                        egui::Frame::new()
                            .fill(crate::ui::Look::colour(look.surface))
                            .inner_margin(egui::Margin::symmetric(6, 4)),
                    )
                    .show(ui, |ui| {
                        if let Some(index) =
                            crate::ui::tab_bar(ui, tabs, active_tab, &look)
                        {
                            *tab_click = Some(index);
                        }
                    });
            }
            egui::Panel::bottom("dtr-status")
                // Exactly the height the grid reserves. Left to itself the panel
                // sizes to its content and its margins, comes out a couple of
                // pixels taller than STATUS_HEIGHT, and covers the bottom row —
                // which is where a shell puts its prompt.
                .exact_size(crate::ui::STATUS_HEIGHT * self.scale())
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
                if let Some(settings) = settings {
                    settings.ui(ui, look);
                }
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
                    ui.ctx().copy_text(text.clone());
                    *copied = Some(text);
                }
                if let Some((text, alpha)) = toast {
                    crate::ui::toast(ui, look, text, alpha);
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
        if frames_drawn == 0 {
            // Startup is the one cost a user feels directly: the window appears
            // when this line is logged, and everything before it is what they
            // stared at.
            tracing::info!(
                elapsed_ms = started.elapsed().as_millis(),
                "first frame"
            );
        }
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
        if let Some(path) = capture {
            if let Err(err) = self.write_capture(&output, path) {
                tracing::warn!(%err, path = %path.display(), "could not capture the window");
            }
        }
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
