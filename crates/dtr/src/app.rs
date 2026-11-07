//! Window, GPU and egui plumbing.
//!
//! Deliberately not `eframe` (PLAN D6): we need winit events for the terminal
//! as well as for egui, and we want to redraw on bytes rather than on egui's
//! animation clock.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

use libghostty_vt::mouse::{Action as MouseAction, Button as MouseButton};

use crate::explorer::{Explorer, PaneView as FilePaneView};
use crate::palette::{Action, Palette};
use crate::pane::{Frame, Pane as TermPane, Transport};
use crate::ui::Look;
use crate::ui::Status;

pub enum UserEvent {
    Frame,
    /// A watched file changed: `~/.ssh/config` was edited.
    Files,
    /// A background scan finished: the destinations to offer now.
    Destinations(Vec<crate::sources::Destination>),
    /// Something the SFTP worker finished.
    Sftp(SftpEvent),
    /// A link pointed at a directory, so the UI should show it — locally, or in a
    /// pane over `alias` when the session that printed the link was remote. The
    /// worker that resolved the link knows what the path *is*; only the UI knows
    /// where a pane may go.
    Reveal {
        path: PathBuf,
        alias: Option<String>,
    },
    /// A preview of an image the pointer hovered: decoded, or the reason it was
    /// not. Either way it is an answer about `key`, so a late one cannot replace
    /// the picture of a file the pointer has already left.
    Thumbnail {
        key: crate::thumb::Key,
        result: anyhow::Result<crate::thumb::Thumb>,
    },
    /// A message for the user that no worker can put on screen itself: a link
    /// that could not be opened, a path that is not what it claimed.
    Notice(String),
    /// The desktop's save dialog answered: where to put the copy, or `None` if it
    /// was dismissed — and, since `rfd` reports a missing portal the same way, if
    /// it could not be shown at all. `-v` says which, because the dialog's own
    /// complaint goes through `log`.
    SavePath {
        location: dtr_vfs::Location,
        path: Option<PathBuf>,
    },
}

/// What the SFTP worker is asked to do, and what it reports back.
///
/// The connection lives on its own thread, never on the pane's: a 2 GB transfer
/// must not be able to stall emulation (docs/sftp.md §5.2).
#[derive(Debug)]
pub enum SftpMsg {
    List {
        alias: String,
        path: PathBuf,
    },
    Download {
        alias: String,
        remote: PathBuf,
        local: PathBuf,
    },
}

#[derive(Debug)]
pub enum SftpEvent {
    Listed {
        alias: String,
        path: PathBuf,
        entries: Vec<dtr_sftp::Entry>,
    },
    Downloaded {
        local: PathBuf,
        bytes: u64,
    },
    Failed {
        alias: String,
        message: String,
    },
}

/// A link under the pointer: the cell it is on, and where it points.
#[derive(Clone, Debug)]
struct LinkHover {
    x: u16,
    y: u16,
    uri: String,
}

/// What the open link menu is about: the link as printed, and where it lives.
///
/// Both are kept because the two answers differ — "Open" wants the URI a click
/// would have used, "Save as" wants the location the byte comes from.
struct LinkMenuTarget {
    uri: String,
    location: dtr_vfs::Location,
}

/// How long the pointer has to rest on a remote picture before it is fetched.
///
/// A sweep across a row of names passes over every one of them, and each is a
/// different link: without the wait that is an `ssh` transfer per cell. A local
/// file has no wait, because there is nothing to spend but a read.
const PREVIEW_DELAY: std::time::Duration = std::time::Duration::from_millis(350);

/// A preview that is waiting for the pointer to settle.
struct PendingThumb {
    due: std::time::Instant,
    key: crate::thumb::Key,
    location: dtr_vfs::Location,
}

/// What a `file:` link names, once the session that printed it is taken into
/// account: the same URI is a local path in one tab and a path on another
/// machine in the next.
#[derive(Debug, PartialEq, Eq)]
enum LinkTarget {
    /// A path this session can reach.
    Path(PathBuf),
    /// Another machine, which this session has no way to read.
    Elsewhere(String),
    /// Not a path at all.
    Unsupported,
}

/// Resolve a link the way `session_remote` would.
///
/// A local session's `file:` link is on this disk only when the URI is about
/// this machine: no host, `localhost`, or this machine's own name — which is the
/// form `ls --hyperlink=always` prints, and the reason clicking its output used
/// to fail. Anything else is another machine, and resolving its path here is the
/// one mistake in this feature that cannot be undone by pointing at the right
/// file afterwards.
///
/// A remote session resolves *every* `file:` path on its own host, because that
/// is the machine the pane's backend reads: `file://localhost/x` printed on
/// `jupyter` means jupyter's `/x`, not this machine's, and the VFS never lets an
/// ssh backend fall back to a local read.
fn link_target(uri: &str, session_remote: bool) -> LinkTarget {
    let Some((host, path)) = crate::uri::split(uri) else {
        return LinkTarget::Unsupported;
    };
    if session_remote || crate::uri::is_local_host(host) {
        LinkTarget::Path(PathBuf::from(path))
    } else {
        LinkTarget::Elsewhere(host.to_owned())
    }
}

#[cfg(test)]
mod link_tests {
    use super::{LinkTarget, link_target};
    use crate::uri;
    use std::path::PathBuf;

    fn locally(uri: &str) -> LinkTarget {
        link_target(uri, false)
    }

    fn path(uri: &str, remote: bool) -> Option<PathBuf> {
        match link_target(uri, remote) {
            LinkTarget::Path(path) => Some(path),
            _ => None,
        }
    }

    /// `file:` with no host is this machine, and percent escapes are part of the
    /// path rather than decoration.
    #[test]
    fn a_file_uri_names_a_path() {
        assert_eq!(
            path("file:///tmp/notes.txt", false),
            Some(PathBuf::from("/tmp/notes.txt"))
        );
        assert_eq!(
            path("file://localhost/tmp/a%20b.png", false),
            Some(PathBuf::from("/tmp/a b.png"))
        );
        // A stray % is a literal: dropping it would silently change the path.
        assert_eq!(
            path("file:///tmp/100%", false),
            Some(PathBuf::from("/tmp/100%"))
        );
    }

    /// `ls --hyperlink=always` writes this machine's name into the URI, not
    /// `localhost` and not an empty host. Refusing that form refused every link
    /// `ls` ever printed.
    #[test]
    fn this_machine_by_name_is_this_machine() {
        let Some(name) = uri::local_hostname() else {
            return;
        };
        assert_eq!(
            path(&format!("file://{name}/tmp/notes.txt"), false),
            Some(PathBuf::from("/tmp/notes.txt"))
        );
    }

    /// Anything that is not this machine's file is named rather than guessed,
    /// so the toast can say which machine it was.
    #[test]
    fn another_machine_is_not_this_one() {
        assert_eq!(
            locally("file://some-other-host.invalid/etc/hosts"),
            LinkTarget::Elsewhere("some-other-host.invalid".to_owned())
        );
        assert_eq!(locally("https://example.com/x"), LinkTarget::Unsupported);
        assert_eq!(locally("/tmp/relative"), LinkTarget::Unsupported);
        assert_eq!(locally("file://"), LinkTarget::Unsupported);
    }

    /// On a remote session every `file:` path is that host's, including
    /// `localhost` — which names the *remote* machine there, not this one.
    #[test]
    fn a_remote_session_resolves_on_its_own_host() {
        assert_eq!(
            path("file://localhost/etc/hosts", true),
            Some(PathBuf::from("/etc/hosts"))
        );
        assert_eq!(
            path("file:///etc/hosts", true),
            Some(PathBuf::from("/etc/hosts"))
        );
        assert_eq!(
            path("file://jupyter.example/etc/hosts", true),
            Some(PathBuf::from("/etc/hosts"))
        );
    }
}

/// Everything a fresh window needs that is decided on the command line.
pub struct Startup {
    pub transport: Transport,
    pub send: Vec<u8>,
    /// Bytes to put through the *paste* path, as if they came from the
    /// clipboard: `--paste` exists so the bracketed and line-ending handling is
    /// checkable against a real program.
    pub paste: Vec<u8>,
    pub log: Option<(PathBuf, crate::serial::LogFormat)>,
    pub store: Option<Arc<dtr_store::Store>>,
    /// Open the palette on start (also how it gets into screenshots).
    pub palette: bool,
    pub look: Look,
    pub theme: Option<crate::theme::Theme>,
    /// Bytes of scrollback per tab; `dtr_config::SCROLLBACK_LIMIT` is the
    /// ceiling and the default. Never unlimited — see `pane::new_terminal`.
    pub scrollback: usize,
    /// Write a PNG of the live window once, `capture_after` milliseconds in.
    /// The only way to see what the window draws, as opposed to what the
    /// offscreen capture path draws.
    pub capture: Option<PathBuf>,
    pub capture_after: u64,
    /// A script of keys to type once the window opens, one per frame.
    pub keys: Option<String>,
    /// A drag to perform once the window opens: `x1,y1,x2,y2` in pixels.
    pub drag: Option<String>,
    /// A click to perform once the window opens: `x,y` in pixels. Two frames,
    /// like a hand: move (which asks the pane what is under the pointer), then
    /// press and release.
    pub click: Option<String>,
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
    /// Open the keyboard map on start, the same way.
    pub keymap: bool,
    /// The serial connection sheet to show over the session, for reviewing the
    /// chrome before the connect path uses it (`--serial-dialog`).
    pub serial_dialog: Option<crate::serial_dialog::SerialDialog>,
    /// The config the menu edits, and where it came from.
    pub config: dtr_config::Config,
    pub config_path: PathBuf,
    /// Draw the capture as a split, to review the layout (screenshots).
    pub split: Option<crate::layout::Axis>,
    /// Open a file pane beside the initial session, at this directory.
    pub files: Option<PathBuf>,
    /// A composition to show before the platform has sent one, for the capture
    /// path: the box's geometry is the half of IME support that a real input
    /// method cannot be asked about in a headless run.
    pub preedit: Option<String>,
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
        startup.scrollback,
        // No window, so nothing to ask about a refresh rate: the 60 Hz default.
        std::sync::Arc::new(std::sync::atomic::AtomicU64::new(
            crate::pane::frame_budget(None).as_nanos() as u64,
        )),
        || {},
    )?;
    if !startup.send.is_empty() {
        pane.write(startup.send.clone());
    }
    if !startup.paste.is_empty() {
        // Through the paste path, not the typing one: a program that asked for
        // bracketed paste has to see the markers here too, or `--paste` would
        // test something other than what a clipboard does.
        pane.send(crate::pane::Command::Paste(startup.paste.clone()));
    }
    if startup.scroll != 0 {
        // After the output has arrived: scrolling an empty screen clamps to
        // zero, and the result looks like a scroll that did nothing.
        std::thread::sleep(std::time::Duration::from_millis(u64::from(
            delay.as_millis() as u64 / 2,
        )));
        pane.send(crate::pane::Command::Scroll(-startup.scroll as isize));
    }

    // Size the grid to the captured window before looking at it: a capture at
    // 80x24 in a 900 px window is a picture of a different program than the one
    // the window would show.
    {
        let metrics = crate::screenshot::cell_metrics(width, height)?;
        let cols = (f64::from(width) / f64::from(metrics.cell_width))
            .floor()
            .max(1.0) as u16;
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
    let serial_dialog = startup.serial_dialog;
    let keymap = startup
        .keymap
        .then(|| crate::keys::Keymap::new(&startup.config.keys));
    // The composition `--preedit` asked for, in the shape the window gives the
    // box: the same type, so both paths cannot disagree about the layout.
    let mut preedit = crate::ime::Preedit::default();
    if let Some(text) = startup.preedit.clone() {
        preedit.set(text, None);
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
        counters,
        &startup.look,
        startup.select,
        startup.split,
        files.as_ref(),
        None,
        settings.as_ref(),
        serial_dialog.as_ref(),
        keymap.as_ref(),
        Some(&preedit),
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
    let startup_alias = ssh_alias(&startup.transport);
    let remote = startup_alias.is_some();
    let counters = !matches!(startup.transport, Transport::Shell(_));
    let port = match &startup.transport {
        Transport::Serial(cfg) => Some(cfg.port.clone()),
        _ => None,
    };

    // The window does not exist yet, so this starts at the 60 Hz default and
    // `resumed` replaces it with the panel's own rate.
    let frame_budget: crate::pane::FrameBudget = std::sync::Arc::new(
        std::sync::atomic::AtomicU64::new(crate::pane::frame_budget(None).as_nanos() as u64),
    );
    let pane = TermPane::spawn(
        startup.transport,
        startup.log.clone(),
        startup.theme.clone(),
        startup.scrollback,
        std::sync::Arc::clone(&frame_budget),
        {
            let proxy = proxy.clone();
            move || {
                let _ = proxy.send_event(UserEvent::Frame);
            }
        },
    )?;
    if !startup.send.is_empty() {
        pane.write(startup.send.clone());
    }
    if !startup.paste.is_empty() {
        pane.send(crate::pane::Command::Paste(startup.paste.clone()));
    }

    let mut app = App {
        frame_budget,
        scripted_selection: startup.select,
        // `--preedit` is how the composition box gets into a screenshot: the
        // platform cannot be asked for a preedit without a hand on a keyboard
        // with an input method, and the box is the half that is easy to get
        // wrong.
        preedit: {
            let mut preedit = crate::ime::Preedit::default();
            if let Some(text) = startup.preedit.clone() {
                preedit.set(text, None);
            }
            preedit
        },
        ime_caret: None,
        clipboard: None,
        watcher: crate::watch::Watcher::spawn({
            let proxy = proxy.clone();
            move || {
                let _ = proxy.send_event(UserEvent::Files);
            }
        }),
        destinations: Vec::new(),
        // A background runtime for the UI's own slow work, separate from the
        // panes: those are single-threaded by design, one per session.
        worker: tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .thread_name("dtr-scan")
            .build()
            .map_err(|err| tracing::warn!(%err, "no worker runtime"))
            .ok(),
        scanning: false,
        scan_due: std::time::Instant::now(),
        tabs: vec![Tab::new(
            1,
            Pane::Terminal(Session {
                pane,
                port,
                frame: None,
                label,
                program_title: String::new(),
                serial,
                remote,
                alias: startup_alias,
                counters,
                echo: false,
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
        wake: std::cell::Cell::new(false),
        strip_logged: None,
        sftp: None,
        wake_why: std::cell::Cell::new("startup"),
        egui_redraw_pending: false,
        loop_report: std::cell::RefCell::new(LoopReport::default()),
        palette: Palette::new(startup.palette),
        pager: crate::pager::Pager::default(),
        look: startup.look.clone(),
        theme: startup.theme.clone(),
        proxy,
        store: startup.store,
        log: startup.log,
        dragging: false,
        drag_moved: false,
        drag_origin: None,
        drag_multi: false,
        mouse_button: None,
        last_press: None,
        clicks: 0,
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
        keymap_card: crate::keymap::Overlay {
            open: startup.keymap,
            edit: None,
        },
        serial_dialog: startup.serial_dialog,
        fonts_dirty: false,
        keymap: crate::keys::Keymap::new(&startup.config.keys),
        pending: Vec::new(),
        pending_since: None,
        drag_script: startup.drag.as_deref().and_then(|text| {
            let parts: Vec<f64> = text
                .split(',')
                .filter_map(|part| part.trim().parse().ok())
                .collect();
            match parts.as_slice() {
                [x1, y1, x2, y2] => Some((*x1, *y1, *x2, *y2)),
                _ => {
                    tracing::warn!(text, "drag expects x1,y1,x2,y2");
                    None
                }
            }
        }),
        click_script: startup.click.as_deref().and_then(|text| {
            let parts: Vec<f64> = text
                .split(',')
                .filter_map(|part| part.trim().parse().ok())
                .collect();
            match parts.as_slice() {
                [x, y] => Some(((*x, *y), false)),
                _ => {
                    tracing::warn!(text, "click expects x,y");
                    None
                }
            }
        }),
        vfs: std::sync::Arc::new(dtr_vfs::Vfs::new()),
        files_rt: tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("dtr-files")
            .build()
            .map_err(|err| tracing::warn!(%err, "no files runtime"))
            .ok(),
        hover_cell: None,
        hover_link: None,
        thumb: crate::thumb::Cache::default(),
        thumb_pending: None,
        link_menu: crate::linkmenu::LinkMenu::default(),
        link_menu_target: None,
        picking_save: false,
        cursor_icon: None,
        script: startup
            .keys
            .as_deref()
            .map(crate::keys::parse_script)
            .unwrap_or_default()
            .into(),
        capture: startup.capture.clone().map(|path| {
            (
                path,
                std::time::Instant::now() + std::time::Duration::from_millis(startup.capture_after),
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
/// The worker: one connection per alias, reused until the alias changes.
///
/// Everything blocking lives here, on a thread of its own with a
/// current-thread runtime — the same shape as a pane, for the opposite reason:
/// a transfer must never share a thread with the emulator.
fn sftp_worker(
    rx: std::sync::mpsc::Receiver<SftpMsg>,
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::error!(%err, "sftp runtime");
            return;
        }
    };
    let mut connected: Option<(String, dtr_sftp::Session)> = None;

    while let Ok(message) = rx.recv() {
        let alias = match &message {
            SftpMsg::List { alias, .. } | SftpMsg::Download { alias, .. } => alias.clone(),
        };

        let outcome: Result<SftpEvent, anyhow::Error> = runtime.block_on(async {
            if connected.as_ref().map(|(name, _)| name.as_str()) != Some(alias.as_str()) {
                connected = None;
                let session = dtr_sftp::Session::connect(&alias).await?;
                connected = Some((alias.clone(), session));
            }
            let (_, session) = connected
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("no session"))?;
            match message {
                SftpMsg::List { alias, path } => {
                    // "." is the pane's way of asking for the home directory:
                    // only the server knows where that is.
                    let path = if path == std::path::Path::new(".") {
                        session.home().await?
                    } else {
                        path
                    };
                    let entries = session.list(&path.to_string_lossy()).await?;
                    Ok(SftpEvent::Listed {
                        alias,
                        path,
                        entries,
                    })
                }
                SftpMsg::Download { remote, local, .. } => {
                    let bytes = session.download(&remote.to_string_lossy(), &local).await?;
                    tracing::info!(?remote, ?local, bytes, "sftp download");
                    Ok(SftpEvent::Downloaded { local, bytes })
                }
            }
        });

        let event = match outcome {
            Ok(event) => event,
            Err(err) => SftpEvent::Failed {
                alias,
                message: format!("{err:#}"),
            },
        };
        let _ = proxy.send_event(UserEvent::Sftp(event));
    }
}

/// Where a copy lands by default: `~/Downloads`, created if it is missing.
///
/// The file explorer's download and the link menu's "Save as" share it, so a
/// remote file taken two different ways does not end up in two different places.
/// For "Save as" it is only where the dialog *starts*: the user picks the real
/// destination, and picking somewhere else is the whole point of asking.
fn save_target(source: &std::path::Path) -> PathBuf {
    save_dir().join(save_name(source))
}

/// The name a copy keeps: the source's own last component, or `download`.
fn save_name(source: &std::path::Path) -> std::ffi::OsString {
    source
        .file_name()
        .map(|name| name.to_owned())
        .unwrap_or_else(|| "download".into())
}

/// `~/Downloads`, created if it is missing, because both save paths write there.
fn save_dir() -> PathBuf {
    let dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Downloads");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// The desktop's save dialog, asked for one path.
///
/// Blocking, and it must not be called on the drawing thread — the window would
/// stop answering while the dialog is up. `rfd` gets the platform's own dialog:
/// the XDG portal on Linux (falling back to `zenity` when no portal answers) and
/// the Common Item Dialog on Windows, neither of which is a GTK dependency and
/// neither of which is a window dtr draws.
///
/// `set_parent` is what makes it *this* window's dialog rather than a stray one:
/// on Windows a transient dialog is centred on and owned by its parent, and on
/// Wayland the portal needs the surface to attach the dialog to.
fn pick_save_path(window: Option<Arc<Window>>, default: &Path) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_title("Save a copy");
    if let Some(dir) = default.parent() {
        dialog = dialog.set_directory(dir);
    }
    if let Some(name) = default.file_name() {
        dialog = dialog.set_file_name(name.to_string_lossy());
    }
    if let Some(window) = window {
        dialog = dialog.set_parent(&*window);
    }
    dialog.save_file()
}

/// Copy `location` to `dest`, making the directory it names.
///
/// `materialise` first, because a remote file has to exist on this disk before
/// anything can copy it, and a local one must never be copied — it is its own
/// path. The picked path's parent is created rather than assumed: a native save
/// dialog lets a name be typed into a folder that the user typed as well, and
/// "saved" for a file that is not there would be the worst answer of the three.
async fn copy_to(
    vfs: &dtr_vfs::Vfs,
    location: &dtr_vfs::Location,
    dest: &Path,
) -> anyhow::Result<u64> {
    let local = vfs.materialise(location).await?;
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }
    tokio::fs::copy(&local, dest).await?;
    Ok(tokio::fs::metadata(dest).await?.len())
}

/// The alias an `ssh` pane is connected to, if it is remote at all.
fn ssh_alias(transport: &Transport) -> Option<String> {
    match transport {
        Transport::Shell(config) => config.remote.clone(),
        _ => None,
    }
}

/// Whether a mouse gesture belongs to the program or to this terminal.
///
/// Two bits decide it, and only one of them is about the program: shift is the
/// exception every terminal makes, so a program that grabbed the mouse still
/// lets a selection be dragged. Without it, a click in vim is unrecoverable —
/// the program has the mouse until it gives it back.
fn mouse_goes_to_program(captured: bool, shift: bool) -> bool {
    captured && !shift
}

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

/// What the strip calls a session: the title a program set, else who it is.
///
/// The program wins because it knows more — a shell reports its directory, nvim
/// the file, tmux the window — and `1 shell` is only what is left when nobody
/// said anything. An empty title is not a name, so it falls back.
fn tab_name<'a>(program: &'a str, fallback: &'a str) -> &'a str {
    if program.is_empty() {
        fallback
    } else {
        program
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
    /// The title the program set with OSC 0 or OSC 2, empty until it does. Kept
    /// apart from `label` rather than overwriting it: a program that sets a
    /// title and later clears it should fall back to naming the session, not to
    /// being called nothing.
    program_title: String,
    /// Whether this session has modem lines, for the status strip.
    serial: bool,
    /// Whether the shell is an `ssh` client, so its reported directory belongs
    /// to another machine and must not be used as a local path.
    remote: bool,
    /// The ssh alias this session is connected to, when it is remote.
    alias: Option<String>,
    /// Whether byte counters belong on the status strip.
    counters: bool,
    echo: bool,
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
    /// When each pane may publish, shared with every pane as nanoseconds: the
    /// gate follows the panel, and the panel changes when the window moves.
    frame_budget: crate::pane::FrameBudget,
    /// The system clipboard, held for as long as the app runs: on X11 the owner
    /// of the selection has to stay alive or the text goes with it.
    clipboard: Option<arboard::Clipboard>,
    /// Destinations the new-tab menu offers: the serial ports that exist now,
    /// then the hosts in `~/.ssh/config`. Typed, so the menu can group them and
    /// the click can open the right kind of session.
    destinations: Vec<crate::sources::Destination>,
    /// The watcher for destination files. Held so the watch stays alive.
    #[allow(dead_code)]
    watcher: Option<crate::watch::Watcher>,
    /// The runtime that does the slow work: enumerating serial ports reads
    /// udev or SetupAPI, which is milliseconds-to-tens-of-milliseconds — an
    /// eternity on a thread that is also drawing the frame. The UI thread asks
    /// for a scan and gets the answer back as an event.
    worker: Option<tokio::runtime::Runtime>,
    /// A scan is in flight; asking again would only queue duplicate work.
    scanning: bool,
    /// When the next poll is due. Serial devices have no portable notification,
    /// so they are polled — off the UI thread.
    scan_due: std::time::Instant,
    active: usize,
    next_leaf: crate::layout::LeafId,
    gpu: Option<Gpu>,
    modifiers: ModifiersState,
    /// Set when the pane fails; `run` returns it once the loop has stopped.
    fatal: Option<String>,
    quit: bool,
    /// Set when a frame is wanted; `about_to_wait` turns it into a wake-up.
    wake: std::cell::Cell<bool>,
    /// The scale whose strip geometry was last logged.
    strip_logged: Option<f32>,
    /// The SFTP worker, and the alias it is connected to.
    sftp: Option<std::sync::mpsc::Sender<SftpMsg>>,
    /// Why the last wake was asked for, so a spin can name its cause.
    wake_why: std::cell::Cell<&'static str>,
    /// A redraw asked for on egui's behalf has not been drawn yet. Asking again
    /// while it is pending makes winit's Wayland pump busy-poll.
    egui_redraw_pending: bool,
    /// Loop iterations and frames in the last reporting second, so "spinning"
    /// can be told apart from "drawing a lot" and from "blocked in poll".
    loop_report: std::cell::RefCell<LoopReport>,
    palette: Palette,
    /// The pager for the last command block. It owns the keyboard while it is
    /// open, because a keystroke that leaked to the shell would run a command
    /// nobody typed.
    pager: crate::pager::Pager,
    proxy: EventLoopProxy<UserEvent>,
    store: Option<Arc<dtr_store::Store>>,
    log: Option<(PathBuf, crate::serial::LogFormat)>,
    look: Look,
    dragging: bool,
    /// Whether the pointer moved to another cell while down: a press that never
    /// moved is a click, and a click copies nothing — unless it was a
    /// double-click, which is a word on purpose.
    drag_moved: bool,
    /// The cell the press landed on, so a pixel of jitter does not turn a
    /// double-click's word into a one-character range.
    drag_origin: Option<(u16, u16)>,
    /// The press that started this drag was the second or third in a row.
    drag_multi: bool,
    /// The button a program is holding, if it is holding one: a motion report
    /// has to name the button, and a release has to know which one ended.
    mouse_button: Option<libghostty_vt::mouse::Button>,
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
    /// A drag still to perform, from `--drag`.
    drag_script: Option<(f64, f64, f64, f64)>,
    /// A click still to perform, from `--click`: the position, and whether the
    /// hover pass has run yet.
    click_script: Option<((f64, f64), bool)>,
    /// The machines this app can read files from.
    vfs: std::sync::Arc<dtr_vfs::Vfs>,
    /// The runtime files work runs on, separate from `worker`: the user asked
    /// for scans to move elsewhere, and a directory walk must not queue behind
    /// a serial-port enumeration.
    files_rt: Option<tokio::runtime::Runtime>,
    /// The cell the pointer is over, and the link it holds if the pane said so.
    /// Kept together so a reply for an older cell can be dropped.
    hover_cell: Option<(u16, u16)>,
    hover_link: Option<LinkHover>,
    /// Preview textures for image files the pointer has hovered, asked about
    /// once each. Decoded on the files runtime, never on this thread.
    thumb: crate::thumb::Cache,
    /// A remote preview waiting for the pointer to rest. Any new hover drops it.
    thumb_pending: Option<PendingThumb>,
    /// The right-click menu for a link, and what it is about.
    link_menu: crate::linkmenu::LinkMenu,
    link_menu_target: Option<LinkMenuTarget>,
    /// True while the desktop's save dialog is up. The dialog is the platform's
    /// and it keeps our window alive, so a second "Save as..." would open a
    /// second dialog on top of the first.
    picking_save: bool,
    /// The cursor this window was last told to show, so the icon is set when it
    /// changes rather than on every frame.
    cursor_icon: Option<winit::window::CursorIcon>,
    /// The keys still to type, from `--keys`.
    script: std::collections::VecDeque<crate::keys::KeyStroke>,
    /// Where to write a PNG of the live window, and when. Set by `--capture`.
    capture: Option<(std::path::PathBuf, std::time::Instant)>,
    /// The key the menu thinks is down. A second press for the same key without
    /// a release in between is a duplicate the compositor sent, not a second
    /// keystroke, and acting on it moves the selection two rows.
    /// The last press, and how many presses in a row it was: winit reports no
    /// click count, so a double-click is a second press in the same place
    /// within `DOUBLE_CLICK`.
    last_press: Option<(std::time::Instant, (f64, f64))>,
    clicks: u8,
    held: Option<winit::keyboard::Key>,
    /// The settings menu, and the config it edits.
    settings: crate::settings::Settings,
    /// The keyboard map: the card's own state (whether it is up, and which row is
    /// waiting for a key), plus the bindings it draws, which are rebuilt whenever
    /// one is written.
    keymap_card: crate::keymap::Overlay,
    /// The serial connection sheet. `Some` while it is open; the connect path
    /// will put it here, and `--serial-dialog` is how the chrome is previewed.
    serial_dialog: Option<crate::serial_dialog::SerialDialog>,
    config: dtr_config::Config,
    config_path: PathBuf,
    /// A copy note and when it was made. Shown briefly, because "did that
    /// work?" is asked at the moment the answer still matters.
    toast: Option<(String, std::time::Instant)>,
    /// `--select START:END`, still waiting for a frame to say how wide the grid
    /// is: the pane wants viewport cells, the flag names linear ones.
    scripted_selection: Option<(usize, usize)>,
    /// What the platform's input method is composing. While there is a preedit
    /// the keystrokes belong to the composition — the commit carries the text,
    /// so the keys must not — and the box is drawn above the caret.
    preedit: crate::ime::Preedit,
    /// The caret's physical rect as the input method was last told it, so a
    /// frame that did not move the caret does not repeat itself.
    ime_caret: Option<[f32; 4]>,
}

impl App {
    /// Follow the panel the window is on.
    ///
    /// The publish gate is half a refresh interval, so it is wrong the moment
    /// the window moves to a display with a different rate — and on Wayland
    /// nothing tells the client it did. It is therefore recomputed from the
    /// once-a-second loop report as well as from the events that do arrive, and
    /// stored where every pane already reads it.
    fn refresh_frame_budget(&self) {
        let refresh = self
            .gpu
            .as_ref()
            .and_then(|gpu| gpu.window.current_monitor())
            .and_then(|monitor| monitor.refresh_rate_millihertz());
        let budget = crate::pane::frame_budget(refresh);
        let nanos = budget.as_nanos() as u64;
        let before = self
            .frame_budget
            .swap(nanos, std::sync::atomic::Ordering::Relaxed);
        if before != nanos {
            tracing::debug!(
                refresh_mhz = refresh.unwrap_or(0),
                gate_us = budget.as_micros() as u64,
                "publish gate"
            );
        }
    }

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

    /// Close a session that has ended: `Ctrl-D`, `exit`, or a pty that would
    /// not read again.
    ///
    /// The pane cannot close itself — the UI owns the tab — so it sets a flag
    /// and wakes the loop, and this reaps it. The last tab closing still takes
    /// the window with it, which is what a shell exiting does in any terminal:
    /// there is nothing left to show.
    fn reap_ended(&mut self) {
        let ended: Vec<usize> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, tab)| {
                tab.leaves.values().any(|pane| match pane {
                    Pane::Terminal(session) => session.pane.ended(),
                    _ => false,
                })
            })
            .map(|(index, _)| index)
            .collect();
        // Highest first: closing shifts the indices after it.
        for index in ended.into_iter().rev() {
            tracing::debug!(tab = index, "session ended; closing its tab");
            self.close_tab(index);
        }
    }

    /// Change the active tab, and say who asked.
    ///
    /// A tab that switches itself back is impossible to chase from the outside:
    /// five call sites can do it, and only one of them is the mouse. Every one
    /// goes through here so `-v` names the culprit.
    fn set_active(&mut self, index: usize, why: &'static str) {
        if index >= self.tabs.len() || index == self.active {
            return;
        }
        tracing::debug!(from = self.active, to = index, why, "active tab");
        self.active = index;
    }

    /// Whether keys belong to the terminal rather than to an overlay.
    ///
    /// The same surfaces `on_key` hands the keyboard to: a composition that
    /// committed into the shell behind an open palette would type somewhere the
    /// person is not looking. A new modal belongs in both places.
    fn terminal_owns_keys(&self) -> bool {
        !self.palette.open
            && !self.settings.open
            && !self.keymap_card.open
            && !self.pager.is_open()
            && self.serial_dialog.is_none()
    }

    /// Handle one event from the platform's input method.
    ///
    /// The preedit is only a picture — nothing reaches the pty until the commit,
    /// which is what makes a composition cancelable. The keys typed during one
    /// carry `composing` and are suppressed by the encoder for the same reason.
    fn on_ime(&mut self, ime: winit::event::Ime) {
        match ime {
            winit::event::Ime::Preedit(text, caret) => {
                if self.preedit.set(text, caret) {
                    self.repaint("ime preedit");
                }
            }
            winit::event::Ime::Commit(text) => {
                self.preedit.clear();
                self.repaint("ime commit");
                if text.is_empty() || !self.terminal_owns_keys() {
                    return;
                }
                // A commit is typed text, not a key: it goes through the same
                // path `--send` and a paste's contents do, so the pane's
                // scroll-to-bottom and held-before-connect rules apply.
                if let Some(session) = self.session() {
                    tracing::debug!(bytes = text.len(), "ime commit");
                    session.pane.write(text.into_bytes());
                }
            }
            winit::event::Ime::Enabled | winit::event::Ime::Disabled => {
                if self.preedit.clear() {
                    self.repaint("ime closed");
                }
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
                            self.set_active(self.tabs.len() - 1, "alt+0");
                            true
                        }
                        Some(digit) if digit as usize <= self.tabs.len() => {
                            self.set_active(digit as usize - 1, "alt+digit");
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

        // The sheet owns the keyboard while it is open. It is a form, so
        // egui has already seen the text a field wants; what must not leak to
        // the shell is everything else. Enter is its answer, Escape its no.
        if self.serial_dialog.is_some() {
            if !pressed {
                self.held = None;
                return;
            }
            let enter = matches!(
                &stroke.key,
                winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter)
            );
            let escape = matches!(
                &stroke.key,
                winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
            );
            if escape {
                self.serial_dialog = None;
                self.repaint("serial dialog cancelled");
            } else if enter {
                if let Some(dialog) = self.serial_dialog.take() {
                    // Enter is the primary action, but an unparseable rate is
                    // not a rate: the sheet stays open rather than connecting
                    // with the last one that happened to parse.
                    match dialog.baud() {
                        Some(baud) => {
                            let mut settings = dialog.settings.clone();
                            settings.baud = baud;
                            self.connect_serial_sheet(dialog, settings);
                        }
                        None => self.serial_dialog = Some(dialog),
                    }
                }
            }
            self.held = Some(stroke.key.clone());
            return;
        }

        // The keyboard map owns the keyboard, because every key it does not
        // need is a key it must not pass on: it is the answer to "what does
        // this key do", and answering by doing it would be a puzzle.
        if self.keymap_card.open {
            if !pressed {
                self.held = None;
                return;
            }
            if !stroke.repeat && self.held.as_ref() == Some(&stroke.key) {
                tracing::debug!(key = ?stroke.key, "ignoring a duplicate press");
                return;
            }
            self.held = Some(stroke.key.clone());
            let escape = matches!(
                &stroke.key,
                winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
            );
            // A row that is waiting for a key takes this one, whatever it is: a
            // replacement is captured by the same code that dispatches, so what
            // the card promises and what the keyboard does cannot differ.
            if let Some((action, _)) = self.keymap_card.edit {
                if escape {
                    self.keymap_card.edit = None;
                    self.repaint("rebind cancelled");
                } else {
                    self.capture_binding(action, &stroke);
                }
                return;
            }
            let toggles =
                crate::keys::chord_from_event(&stroke.key, stroke.mods).is_some_and(|chord| {
                    matches!(
                        self.keymap.resolve(&[], chord),
                        crate::keys::Matched::Action(crate::keys::Action::Keymap)
                    )
                });
            if escape || toggles {
                self.keymap_card.open = false;
                self.keymap_card.edit = None;
                self.repaint("keymap closed");
            }
            return;
        }

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
            let closes =
                crate::keys::chord_from_event(&stroke.key, stroke.mods).is_some_and(|chord| {
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

        // The pager owns the keyboard while it is open. Strictly: a
        // keystroke that reached the shell would run a command nobody
        // meant to type, and `j`/`k` are exactly the keys a shell keeps.
        if self.pager.is_open() {
            if !pressed {
                self.held = None;
                return;
            }
            match self.pager.handle(&stroke.key, &stroke.mods) {
                Some(crate::pager::Action::Close) => {
                    self.pager.close();
                    self.repaint("pager closed");
                }
                Some(crate::pager::Action::Copy(text)) => {
                    self.set_clipboard(&text);
                    let note = format!("copied {} lines from the block", text.lines().count());
                    self.toast = Some((note, std::time::Instant::now()));
                    self.repaint("block copied");
                }
                None => self.repaint("pager scrolled"),
            }
            self.held = Some(stroke.key.clone());
            return;
        }

        // The menu owns the keyboard in the same way, but only briefly:
        // any key dismisses it and then goes where it was going. Escape is
        // the one that must not reach the shell, because it was aimed at
        // the menu.
        if self.link_menu.is_open() && pressed {
            let escape = matches!(
                &stroke.key,
                winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
            );
            self.link_menu.close();
            self.repaint("link menu closed");
            if escape {
                return;
            }
        }

        let mut consumed = alt_digit;
        let action = if pressed && !consumed {
            crate::keys::chord_from_event(&stroke.key, stroke.mods).and_then(|chord| {
                match self.keymap.resolve(&self.pending, chord) {
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
                }
            })
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
                if !self.palette.open && !stroke.mods.control_key() && !stroke.mods.alt_key() {
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
                // are open; otherwise the key belongs to the session.
                //
                // Releases go too, and that is not a doubled keystroke:
                // the encoder emits nothing for a release unless the
                // program asked for the kitty protocol's press and
                // release reporting — and when it did, the release is
                // exactly what it asked for.
                if let Some(session) = self.session() {
                    session
                        .pane
                        .send(crate::pane::Command::Key(Box::new(stroke.clone())));
                }
            }
        }
    }

    /// Whether the program in the focused pane asked for the mouse.
    ///
    /// Read from the pane rather than kept here: the modes belong to the
    /// terminal, which never leaves the pane's thread, so the pane publishes the
    /// one bit that decides where a click goes.
    fn mouse_captured(&self) -> bool {
        self.session()
            .is_some_and(|session| session.pane.stats().mouse())
    }

    /// Hand a mouse event to the program, remembering which button is held.
    ///
    /// A press is the program's mouse when it asked for it; a motion reports the
    /// button that is down, if any; a release ends it. Nothing here reaches the
    /// terminal's own selection, because a program that tracks the mouse has its
    /// own idea of what a drag does.
    fn forward_mouse(
        &mut self,
        action: MouseAction,
        button: Option<MouseButton>,
        at: (f64, f64),
    ) -> bool {
        let held = self.mouse_button;
        // A release is only ever sent for a press the program heard: a button
        // coming up that it never saw go down is worse than no event at all, and
        // a right-click menu never sends a press to begin with.
        if action == MouseAction::Release {
            self.mouse_button = None;
            if held.is_none() {
                return false;
            }
        }
        // A gesture that has already started belongs to the program until it
        // ends. Shift pressed half way through a drag must not start a selection
        // in the middle of a report the program is following.
        let ongoing = action != MouseAction::Press && held.is_some();
        if !ongoing && !mouse_goes_to_program(self.mouse_captured(), self.modifiers.shift_key()) {
            return false;
        }
        // A press that is not in the pane (the status bar, the tab strip) is not
        // a click for the program — it would land on a clamped cell at the edge.
        // A drag that leaves the pane *is*: the program is following it, and a
        // report of nothing is how a selection in vim freezes at the edge.
        if action == MouseAction::Press
            && self.pane_at(at.0, at.1) != Some(self.tabs[self.active].focus)
        {
            return false;
        }
        if action == MouseAction::Press {
            self.mouse_button = button;
        }
        let down = self.mouse_button.is_some();
        let Some((x, y)) = self.pane_pixels(at.0, at.1) else {
            return false;
        };
        self.send_mouse(action, button, (x, y), down)
    }

    /// The bytes side of a mouse event: nothing remembered, nothing decided.
    ///
    /// Split from `forward_mouse` because a wheel notch is a press and a release
    /// that hold nothing between them, and it must not leave a button down.
    fn send_mouse(
        &mut self,
        action: MouseAction,
        button: Option<MouseButton>,
        at: (f32, f32),
        down: bool,
    ) -> bool {
        let Some(session) = self.session() else {
            return false;
        };
        session.pane.send(crate::pane::Command::Mouse {
            action,
            button,
            mods: crate::input::mods_of(self.modifiers),
            x: at.0,
            y: at.1,
            down,
        });
        true
    }

    /// One wheel notch to the program: buttons 4 and 5, as a press and a release.
    ///
    /// That pair is what X10 reporting has — there is no "wheel" event in it —
    /// and it is how vim, less and every pager over ssh scroll.
    fn scroll_to_program(&mut self, up: bool, notches: usize) {
        let Some(at) = self
            .cursor_position
            .and_then(|(x, y)| self.pane_pixels(x, y))
        else {
            return;
        };
        let button = if up {
            MouseButton::Four
        } else {
            MouseButton::Five
        };
        for _ in 0..notches {
            // `down` is false for both: a notch is not a button being held.
            self.send_mouse(MouseAction::Press, Some(button), at, false);
            self.send_mouse(MouseAction::Release, Some(button), at, false);
        }
    }

    /// The pointer in pixels from the focused pane's top-left corner.
    ///
    /// Clamped into the pane rather than dropped: a drag that leaves the grid is
    /// still a drag for the program, and a report of nothing is how a selection
    /// in vim freezes at the edge.
    fn pane_pixels(&self, x: f64, y: f64) -> Option<(f32, f32)> {
        let gpu = self.gpu.as_ref()?;
        let tab = &self.tabs[self.active];
        let screen = (gpu.config.width as f32, gpu.config.height as f32);
        let rect = self
            .pane_views(tab, gpu.metrics(), screen, gpu.scale())
            .into_iter()
            .find(|(id, _)| *id == tab.focus)
            .map(|(_, pane)| pane.rect)?;
        Some((
            (x as f32 - rect.x).clamp(0.0, (rect.width - 1.0).max(0.0)),
            (y as f32 - rect.y).clamp(0.0, (rect.height - 1.0).max(0.0)),
        ))
    }

    /// The pointer moved: if it entered a different cell, ask the pane what
    /// link is on it.
    ///
    /// Asked per cell rather than carried in the frame — `hyperlink_uri` is an
    /// FFI call per cell, and on a dense screen that is another 30 µs on a
    /// 41 µs publish for a link nobody is pointing at.
    fn hover(&mut self, at: (f64, f64)) {
        // A program that owns the mouse owns its links too: a pointing hand and a
        // thumbnail are an invitation to click, and that click is the program's.
        if self.mouse_captured() {
            self.hover_cell = None;
            self.hover_link = None;
            self.thumb_pending = None;
            return;
        }
        let cell = self.cell_xy(at.0, at.1);
        if cell == self.hover_cell {
            return;
        }
        self.hover_cell = cell;
        self.hover_link = None;
        // A preview that was waiting for the pointer to settle is not wanted any
        // more: the pointer has moved, which is the whole question.
        self.thumb_pending = None;
        if let (Some((x, y)), Some(session)) = (cell, self.session()) {
            session
                .pane
                .send(crate::pane::Command::HyperlinkAt { x, y });
        }
    }

    /// Open a hyperlink, resolving it the way this session would.
    ///
    /// A file goes to the desktop's opener; a directory opens a file pane beside
    /// the terminal, because clicking a directory is asking to look inside it and
    /// a second window from the desktop's file manager is one to close again.
    fn open_link(&mut self, uri: &str) {
        let Some(remote) = self.session().map(|session| session.remote) else {
            return;
        };
        let path = match link_target(uri, remote) {
            LinkTarget::Path(path) => path,
            // Said out loud, and named: "cannot open" leaves the reader with
            // nothing to do about it, and the host is the interesting part.
            LinkTarget::Elsewhere(host) => {
                self.toast = Some((
                    format!("{uri} is on {host}, not this machine"),
                    std::time::Instant::now(),
                ));
                return;
            }
            LinkTarget::Unsupported => {
                self.toast = Some((format!("cannot open {uri}"), std::time::Instant::now()));
                return;
            }
        };
        let Some(location) = self.location(path.clone()) else {
            self.toast = Some((
                "this session has no host to ask".to_owned(),
                std::time::Instant::now(),
            ));
            return;
        };
        let described = location.describe();
        tracing::info!(target = %described, "opening a link");

        let (Some(runtime), vfs, proxy) = (
            self.files_rt.as_ref(),
            std::sync::Arc::clone(&self.vfs),
            self.proxy.clone(),
        ) else {
            return;
        };
        // A remote file cannot be handed to a viewer as a path, so it is
        // fetched first; a local one is its own path and is never copied.
        let remote = !matches!(location.backend, dtr_vfs::BackendId::Local);
        // A directory is shown in a file pane, and a remote one in a pane over
        // the machine it came from — that pane is the only thing that can list it.
        let reveal_alias = match &location.backend {
            dtr_vfs::BackendId::Ssh(alias) => Some(alias.clone()),
            dtr_vfs::BackendId::Local => None,
        };
        runtime.spawn(async move {
            let outcome = async {
                let meta = vfs.stat(&location).await?;
                if meta.kind.is_dir() {
                    let _ = proxy.send_event(UserEvent::Reveal {
                        path,
                        alias: reveal_alias,
                    });
                    return Ok(());
                }
                let path = vfs.materialise(&location).await?;
                tokio::task::spawn_blocking(move || dtr_vfs::open_externally(&path))
                    .await
                    .map_err(|err| anyhow::anyhow!("{err}"))??;
                Ok::<(), anyhow::Error>(())
            }
            .await;
            // Silence is not success, and a failure belongs on screen: the
            // worker that failed has no UI to say it with. A remote copy is
            // said out loud too, because it is a copy and not the file.
            let note = match outcome {
                Ok(()) if remote => format!("opened a copy of {described}"),
                Ok(()) => return,
                Err(err) => format!("{err:#}"),
            };
            let _ = proxy.send_event(UserEvent::Notice(note));
        });
    }

    /// The DPI factor, which the window holds and the GPU knows how to ask for.
    ///
    /// egui works in points and winit's pointer arrives in physical pixels, so
    /// every hand-off between them needs this — once, not twice.
    fn scale(&self) -> f32 {
        self.gpu.as_ref().map(|gpu| gpu.scale()).unwrap_or(1.0)
    }

    /// The right button on a link: a menu, because a link that is a file can be
    /// taken away as well as opened.
    ///
    /// Nothing else gets a menu — a right-click on ordinary output has no target,
    /// and a menu of nothing is worse than no menu.
    fn right_click(&mut self, at: (f64, f64)) {
        // Focus first: the cell under the pointer is computed against the focused
        // pane, and a right-click in a split is a right-click on *that* pane.
        if let Some(id) = self.pane_at(at.0, at.1) {
            self.tabs[self.active].focus = id;
        }
        // A program that asked for the mouse gets the right button too. A menu
        // about a link is this terminal's idea, and it is not the program's.
        if self.forward_mouse(MouseAction::Press, Some(MouseButton::Right), at) {
            return;
        }
        let Some(link) = self.hover_link.clone() else {
            return;
        };
        // The hover can be a frame older than the pointer; the cell is what
        // decides, exactly as it does for the press that opens a link.
        if self.cell_xy(at.0, at.1) != Some((link.x, link.y)) {
            return;
        }
        let Some(remote) = self.session().map(|session| session.remote) else {
            return;
        };
        let LinkTarget::Path(path) = link_target(&link.uri, remote) else {
            return;
        };
        let Some(location) = self.location(path) else {
            return;
        };
        let title = location
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| location.describe());
        // egui is in points and winit's pointer is in physical pixels; the DPI
        // factor is applied exactly once, here.
        let scale = self.scale();
        self.link_menu.open_at(
            egui::pos2(at.0 as f32 / scale, at.1 as f32 / scale),
            title,
            location.describe(),
        );
        self.link_menu_target = Some(LinkMenuTarget {
            uri: link.uri,
            location,
        });
        self.repaint("link menu");
    }

    /// A middle press: nothing of this terminal's, but a program that grabbed
    /// the mouse wants it — in a terminal it usually means "paste" at the far end.
    fn middle_click(&mut self, at: (f64, f64)) {
        if let Some(id) = self.pane_at(at.0, at.1) {
            self.tabs[self.active].focus = id;
        }
        self.forward_mouse(MouseAction::Press, Some(MouseButton::Middle), at);
    }

    /// A left press, with the overlays given first refusal.
    ///
    /// A press outside an overlay dismisses it and goes nowhere else — that is
    /// what every menu on the desktop does, and a click that both dismisses a
    /// menu and starts a selection is how text gets selected by accident.
    fn left_press(&mut self, at: (f64, f64)) {
        let scale = self.scale();
        let points = egui::pos2(at.0 as f32 / scale, at.1 as f32 / scale);
        if self.link_menu.is_open() {
            if !self.link_menu.contains(points) {
                self.link_menu.close();
                self.repaint("link menu dismissed");
            }
            return;
        }
        self.pointer_down(at);
    }

    /// Act on what the link menu was asked for.
    fn link_menu_choice(&mut self, choice: crate::linkmenu::Choice) {
        let Some(target) = self.link_menu_target.take() else {
            return;
        };
        self.link_menu.close();
        match choice {
            // The URI, not the location: "Open" must be exactly what a left
            // click does, remote copy and all.
            crate::linkmenu::Choice::Open => self.open_link(&target.uri),
            crate::linkmenu::Choice::SaveAs => self.ask_where_to_save(target.location),
            crate::linkmenu::Choice::CopyPath => {
                let text = target.location.path.display().to_string();
                self.set_clipboard(&text);
                self.toast = Some((format!("copied {text}"), std::time::Instant::now()));
                self.repaint("path copied");
            }
        }
    }

    /// Ask the desktop where to put a copy of `location`.
    ///
    /// The dialog is the platform's, not egui's and not dtr's: `rfd` reaches the
    /// XDG portal on Linux (with a zenity fallback) and the Common Item Dialog on
    /// Windows, so a Windows user gets the same browse-and-overwrite behaviour as
    /// everywhere else rather than a text field in a terminal. It cannot run on
    /// this thread — `pick_file` blocks until the user answers, and the window
    /// would be frozen while it does — so it runs on the files runtime and comes
    /// back as an event, the same way an SFTP listing does.
    fn ask_where_to_save(&mut self, location: dtr_vfs::Location) {
        if self.picking_save {
            tracing::debug!("a save dialog is already up");
            return;
        }
        let Some(runtime) = self.files_rt.as_ref() else {
            return;
        };
        let default = save_target(&location.path);
        let parent = self.proxy.clone();
        let window = self.gpu.as_ref().map(|gpu| Arc::clone(gpu.window()));
        self.picking_save = true;
        runtime.spawn_blocking(move || {
            let chosen = pick_save_path(window, &default);
            tracing::debug!(?chosen, "save dialog answered");
            let _ = parent.send_event(UserEvent::SavePath {
                location,
                path: chosen,
            });
        });
    }

    /// Copy a file on this machine or another to `dest`.
    ///
    /// The fetch is `materialise`, the same call a click makes: local files are
    /// their own path, remote ones are fetched into the cache first, and the copy
    /// is only the last step. The size the user gets told is the copy's.
    fn begin_save(&mut self, location: dtr_vfs::Location, dest: PathBuf) {
        let described = location.describe();
        let name = dest
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| dest.display().to_string());
        let (Some(runtime), vfs, proxy) = (
            self.files_rt.as_ref(),
            std::sync::Arc::clone(&self.vfs),
            self.proxy.clone(),
        ) else {
            return;
        };
        // Said before rather than after: a file over a slow link is a silence,
        // and a silence nobody explained reads as a button that did nothing.
        self.toast = Some((format!("saving {name}..."), std::time::Instant::now()));
        runtime.spawn(async move {
            let outcome = copy_to(&vfs, &location, &dest).await;
            let note = match outcome {
                Ok(bytes) => format!(
                    "saved {} to {}",
                    crate::explorer::human_size(bytes),
                    dest.display()
                ),
                Err(err) => format!("could not save {described}: {err:#}"),
            };
            let _ = proxy.send_event(UserEvent::Notice(note));
        });
    }

    /// Show a directory in a file pane beside the terminal that asked for it.
    ///
    /// Focus stays on the terminal: the click happened in the shell and the next
    /// keystroke belongs there, not in a listing nobody meant to type into. A pane
    /// already browsing the *same machine* is retargeted rather than a second one
    /// growing — another click means "look there", not "one more pane". Another
    /// machine is another pane, because a pane reading this disk cannot become one
    /// reading somebody else's.
    fn reveal_directory(&mut self, path: PathBuf, alias: Option<String>) {
        let existing = {
            let tab = &self.tabs[self.active];
            tab.layout.leaves().into_iter().find(|id| {
                matches!(
                    tab.leaves.get(id),
                    Some(Pane::Files(explorer)) if explorer.alias() == alias.as_deref()
                )
            })
        };
        let explorer = match &alias {
            Some(alias) => Explorer::remote_at(alias, path),
            None => Explorer::new(Some(path)),
        };
        if let Some(id) = existing {
            self.tabs[self.active]
                .leaves
                .insert(id, Pane::Files(explorer));
            return;
        }
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
        tab.leaves.insert(id, Pane::Files(explorer));
    }

    /// Where a path on this session lives: this disk, or the host the session is
    /// on. `None` when a remote session has no alias to ask, which is the one case
    /// with no backend at all — never a fallback to reading it locally.
    fn location(&self, path: PathBuf) -> Option<dtr_vfs::Location> {
        let session = self.session()?;
        let backend = match (session.remote, session.alias.clone()) {
            (false, _) => dtr_vfs::BackendId::Local,
            (true, Some(alias)) => dtr_vfs::BackendId::Ssh(alias),
            (true, None) => return None,
        };
        Some(dtr_vfs::Location { backend, path })
    }

    /// What a link would be previewed as, if it is a picture this session can
    /// reach: the key the cache uses, and where the bytes are.
    ///
    /// The machine is in the key because the same path on two machines is two
    /// files; `None` for the scheme means nothing here.
    fn thumbnail_target(&self, uri: &str) -> Option<(crate::thumb::Key, dtr_vfs::Location)> {
        let remote = self.session()?.remote;
        let LinkTarget::Path(path) = link_target(uri, remote) else {
            return None;
        };
        if !crate::thumb::looks_like_image(&path) {
            return None;
        }
        let location = self.location(path.clone())?;
        let machine = match &location.backend {
            dtr_vfs::BackendId::Ssh(alias) => Some(alias.clone()),
            dtr_vfs::BackendId::Local => None,
        };
        Some((crate::thumb::Key::new(machine, path), location))
    }

    /// Ask for a preview of the picture `uri` names.
    ///
    /// A remote one is *not* fetched the instant the pointer crosses the name: a
    /// sweep across a row of files would start an `ssh` transfer per cell. It waits
    /// [`PREVIEW_DELAY`] on one link first, and any new hover cancels the wait. A
    /// local file is read immediately — there is nothing to spend but a read.
    fn request_thumbnail(&mut self, uri: &str) {
        // Every hover reply is a new target, so whatever was waiting is not wanted
        // any more.
        self.thumb_pending = None;
        let Some((key, location)) = self.thumbnail_target(uri) else {
            return;
        };
        let delay = match &location.backend {
            dtr_vfs::BackendId::Ssh(_) => PREVIEW_DELAY,
            dtr_vfs::BackendId::Local => std::time::Duration::ZERO,
        };
        if delay.is_zero() {
            self.load_thumbnail(key, location);
        } else {
            self.thumb_pending = Some(PendingThumb {
                due: std::time::Instant::now() + delay,
                key,
                location,
            });
        }
    }

    /// The wait is over: claim the key and start the fetch and decode.
    fn due_thumbnail(&mut self) {
        let Some(pending) = self.thumb_pending.take() else {
            return;
        };
        self.load_thumbnail(pending.key, pending.location);
    }

    /// Fetch (if remote) and decode, off this thread, then answer with the key so
    /// a reply for a link the pointer has left cannot replace a newer picture.
    fn load_thumbnail(&mut self, key: crate::thumb::Key, location: dtr_vfs::Location) {
        let (Some(runtime), vfs, proxy) = (
            self.files_rt.as_ref(),
            std::sync::Arc::clone(&self.vfs),
            self.proxy.clone(),
        ) else {
            return;
        };
        // Claimed only once there is something to claim it with: a request that
        // died between the claim and the spawn would leave the key in flight for
        // the life of the process.
        if !self.thumb.begin(&key) {
            return;
        }
        let described = location.describe();
        runtime.spawn(async move {
            let result = async {
                // The size limit has to come from a `stat`, before the bytes: a
                // remote 400 MB scan must never be downloaded to find out it is
                // too big to preview.
                let meta = vfs.stat(&location).await?;
                anyhow::ensure!(!meta.kind.is_dir(), "a directory has no picture");
                anyhow::ensure!(
                    meta.len <= crate::thumb::MAX_BYTES,
                    "{} bytes is more than a preview is worth",
                    meta.len
                );
                let local = vfs.materialise(&location).await?;
                tokio::task::spawn_blocking(move || crate::thumb::load(&local))
                    .await
                    .map_err(|err| anyhow::anyhow!("{err}"))?
            }
            .await;
            if result.is_err() {
                tracing::debug!(target = %described, "no preview");
            }
            let _ = proxy.send_event(UserEvent::Thumbnail { key, result });
        });
    }

    /// The preview to draw for the link under the pointer, if there is one ready.
    ///
    /// The texture is cloned rather than borrowed because the draw function is
    /// already holding `&mut` the fields around it — a handle is a reference
    /// count, not a copy of the pixels.
    fn hover_thumbnail(
        &mut self,
        scale: f32,
        ctx: &egui::Context,
    ) -> Option<(egui::TextureHandle, egui::Vec2)> {
        // The pager owns the window while it is open; a tooltip over it would be
        // a picture of a file the user is not pointing at. The link menu is a
        // sharper case: it opens *at* the pointer, exactly where the preview would
        // be, and the tooltip layer sits above a foreground menu.
        if self.pager.is_open() || self.link_menu.is_open() {
            return None;
        }
        let uri = self.hover_link.as_ref()?.uri.clone();
        let (key, _) = self.thumbnail_target(&uri)?;
        let texture = self.thumb.texture(&key, ctx)?;
        let [width, height] = texture.size();
        // Points, because the texture is decoded at 2× the drawn size so it stays
        // sharp on a HiDPI display.
        Some((
            texture.clone(),
            egui::vec2(width as f32 / scale, height as f32 / scale),
        ))
    }

    /// The pointer went down at `at`. A press focuses the pane under it and
    /// starts a selection anchor; the drag turns the anchor into a range.
    /// Put text on the system clipboard, by hand.
    ///
    /// egui's own path goes through `egui-winit`, which falls back to an
    /// in-app buffer when it cannot reach the display — the toast then says
    /// "copied" while the clipboard is untouched. Writing it here as well
    /// makes the copy real, and a failure is logged rather than assumed.
    fn set_clipboard(&mut self, text: &str) {
        let clipboard = match self.clipboard.as_mut() {
            Some(clipboard) => clipboard,
            None => match arboard::Clipboard::new() {
                Ok(clipboard) => {
                    self.clipboard = Some(clipboard);
                    // Just created, so it is there.
                    self.clipboard.as_mut().expect("just stored")
                }
                Err(err) => {
                    tracing::warn!(%err, "no system clipboard");
                    return;
                }
            },
        };
        match clipboard.set_text(text.to_owned()) {
            // Said out loud because "no output" and "copied fine" must not look
            // the same: `-v` shows this line, and the absence of it means the
            // copy never ran.
            Ok(()) => tracing::debug!(
                chars = text.chars().count(),
                bytes = text.len(),
                "clipboard written"
            ),
            Err(err) => tracing::warn!(%err, "could not write the clipboard"),
        }
    }

    /// Read the system clipboard, by hand.
    ///
    /// The same reason as `set_clipboard`: egui owns its own handle and its own
    /// shortcut, and a chord that only this app's keymap knows about never
    /// reaches egui's paste path at all.
    fn clipboard_text(&mut self) -> Option<String> {
        if self.clipboard.is_none() {
            match arboard::Clipboard::new() {
                Ok(clipboard) => self.clipboard = Some(clipboard),
                Err(err) => {
                    tracing::warn!(%err, "no system clipboard");
                    return None;
                }
            }
        }
        match self.clipboard.as_mut()?.get_text() {
            Ok(text) => {
                tracing::debug!(chars = text.chars().count(), "clipboard read");
                Some(text)
            }
            Err(err) => {
                tracing::warn!(%err, "could not read the clipboard");
                None
            }
        }
    }

    /// Hand a paste to the focused session.
    ///
    /// The bytes here are the clipboard's, untouched: turning line breaks into
    /// CR and wrapping the text for a program that asked for bracketed paste are
    /// the pane's decisions, because the terminal it holds is the only place
    /// that knows what the program requested.
    fn paste(&mut self, text: String) {
        let Some(session) = self.session() else {
            return;
        };
        session
            .pane
            .send(crate::pane::Command::Paste(text.into_bytes()));
    }

    fn pointer_down(&mut self, at: (f64, f64)) {
        if let Some(id) = self.pane_at(at.0, at.1) {
            self.tabs[self.active].focus = id;
        }
        // A program that asked for the mouse gets the click. Selection is what is
        // left when nobody did — or when shift says the user meant to select.
        if self.forward_mouse(MouseAction::Press, Some(MouseButton::Left), at) {
            self.dragging = false;
            return;
        }
        // A link is a click target, not a selection: pressing one opens it and
        // starts no drag. Without a link under the pointer this falls straight
        // through to the selection code below.
        if let Some(link) = self.hover_link.clone() {
            if self.cell_xy(at.0, at.1) == Some((link.x, link.y)) {
                self.open_link(&link.uri);
                self.dragging = false;
                return;
            }
        }
        let clicks = self.count_clicks(at);
        self.drag_moved = false;
        self.drag_multi = clicks > 1;
        let cell = self.cell_xy(at.0, at.1);
        self.drag_origin = cell;
        if let (Some((x, y)), Some(session)) = (cell, self.session()) {
            let command = match clicks {
                2 => crate::pane::Command::SelectWord { x, y },
                3 => crate::pane::Command::SelectLine { x, y },
                _ => crate::pane::Command::SelectStart { x, y },
            };
            session.pane.send(command);
        }
        self.dragging = self.session().is_some();
    }

    /// How many presses in a row this is: one starts a selection, two take the
    /// word under the pointer, three take the line.
    ///
    /// winit has no click count, so it is counted here — a press within
    /// `DOUBLE_CLICK` and a few pixels of the last one continues the run, and
    /// the fourth press starts over.
    fn count_clicks(&mut self, at: (f64, f64)) -> u8 {
        const DOUBLE_CLICK: std::time::Duration = std::time::Duration::from_millis(400);
        const SLOP: f64 = 4.0;
        let again = self.last_press.is_some_and(|(when, (x, y))| {
            when.elapsed() <= DOUBLE_CLICK && (x - at.0).abs() <= SLOP && (y - at.1).abs() <= SLOP
        });
        let clicks = if again { self.clicks % 3 + 1 } else { 1 };
        self.clicks = clicks;
        self.last_press = Some((std::time::Instant::now(), at));
        clicks
    }

    /// The pointer moved while down: move the selection's head with it.
    fn pointer_move(&mut self, at: (f64, f64)) {
        // The program first: `?1003h` wants motion with nothing held and
        // `?1002h` only while a button is, which the encoder decides from
        // whether one is down — and it drops motion inside the same cell, so a
        // drag across a row is a report per cell, not per pixel.
        if self.forward_mouse(MouseAction::Motion, self.mouse_button, at) {
            return;
        }
        if !self.dragging {
            return;
        }
        let Some((x, y)) = self.cell_xy(at.0, at.1) else {
            return;
        };
        // A word or a line is a click, not a drag: a pixel of jitter while the
        // button is down must not turn it into a one-character selection.
        if self.drag_origin == Some((x, y)) {
            return;
        }
        self.drag_moved = true;
        if let Some(session) = self.session() {
            session.pane.send(crate::pane::Command::SelectHead { x, y });
        }
    }

    /// The pointer came up: end the drag, and copy what it selected.
    ///
    /// Selecting text in a terminal is understood to put it on the clipboard —
    /// the drag ends, the text is there. A single-cell selection is a click,
    /// not a selection, and copying one blank cell is the "0 chars" bug again.
    fn pointer_up(&mut self) {
        // A release the program is waiting for ends nothing on this side: the
        // gesture was never a selection, so there is nothing to copy.
        let button = self.mouse_button;
        if let (Some(at), Some(button)) = (self.cursor_position, button) {
            if self.forward_mouse(MouseAction::Release, Some(button), at) {
                self.dragging = false;
                self.drag_moved = false;
                self.drag_multi = false;
                return;
            }
        }
        self.mouse_button = None;
        let dragged = std::mem::take(&mut self.dragging);
        let moved = std::mem::take(&mut self.drag_moved);
        let multi = std::mem::take(&mut self.drag_multi);
        if !dragged || !(moved || multi) {
            return;
        }
        if let Some(session) = self.session() {
            session.pane.send(crate::pane::Command::CopySelection);
        }
    }

    /// The viewport cell under the pointer. The pane wants cells rather than
    /// linear indices because a selection is anchored to the characters, and a
    /// linear index names whichever character happens to be there later.
    fn cell_xy(&self, x: f64, y: f64) -> Option<(u16, u16)> {
        let cell = self.cell_at(x, y)?;
        let cols = usize::from(self.session()?.frame.as_ref()?.cols).max(1);
        Some(((cell % cols) as u16, (cell / cols) as u16))
    }

    /// Ask the worker runtime for a fresh destination list.
    ///
    /// Port enumeration touches udev or SetupAPI and the ssh config is a file
    /// read; both are small but neither belongs on the thread that draws the
    /// frame, where a few milliseconds is a dropped frame.
    fn request_scan(&mut self) {
        const POLL: std::time::Duration = std::time::Duration::from_secs(2);
        self.scan_due = std::time::Instant::now() + POLL;
        if self.scanning {
            return;
        }
        let Some(worker) = self.worker.as_ref() else {
            return;
        };
        self.scanning = true;
        let proxy = self.proxy.clone();
        worker.spawn_blocking(move || {
            let mut destinations = crate::sources::serial_destinations();
            destinations.extend(crate::sources::ssh_destinations());
            let _ = proxy.send_event(UserEvent::Destinations(destinations));
        });
    }

    /// Do what a binding asks for. Returns whether it acted, so a terminal-only
    /// action with a file pane focused falls through to the pane's own keys.
    /// Apply what the keyboard map asked for.
    fn keymap_event(&mut self, asked: crate::keymap::Event) {
        use crate::keymap::Event;
        match asked {
            Event::Close => {
                self.keymap_card.open = false;
                self.keymap_card.edit = None;
            }
            Event::Edit(action) => self.keymap_card.edit = Some((action, None)),
            Event::Cancel => self.keymap_card.edit = None,
            Event::Reset(action) => self.reset_binding(action),
        }
        self.repaint("keymap");
    }

    /// Take the keystroke a waiting row receives as its new chord.
    ///
    /// Refusals are said in the popup rather than swallowed: a key that does
    /// nothing and a key that was rejected are the same silence, and this card
    /// exists to remove that silence.
    fn capture_binding(&mut self, action: crate::keys::Action, stroke: &crate::keys::KeyStroke) {
        let Some(chord) = crate::keys::chord_from_event(&stroke.key, stroke.mods) else {
            self.refuse_binding(action, "a character on its own belongs to the shell");
            return;
        };
        if let Some(why) = crate::keys::refuse_reason(&self.keymap, action, chord) {
            self.refuse_binding(action, &why);
            return;
        }
        let sequence = dtr_config::keys::Sequence::single(chord);
        self.write_binding(action, Some(&sequence.to_string()));
    }

    /// Say why the last keystroke was not taken, and keep waiting.
    fn refuse_binding(&mut self, action: crate::keys::Action, why: &str) {
        self.keymap_card.edit = Some((action, Some(why.to_owned())));
        self.repaint("rebind refused");
    }

    /// Write a binding to `[keys]`, or remove the override with `None`.
    ///
    /// The file is the source of truth: the config is written first and the live
    /// map is rebuilt from it, so the card cannot show a chord that a restart
    /// would not have.
    fn write_binding(&mut self, action: crate::keys::Action, text: Option<&str>) {
        let name = action.name();
        let result = match text {
            Some(text) => dtr_config::set_value(&self.config_path, &format!("keys.{name}"), text),
            None => dtr_config::remove_value(&self.config_path, &format!("keys.{name}")),
        };
        if let Err(err) = result {
            tracing::warn!(%name, %err, "could not save a key binding");
            self.refuse_binding(action, "could not write the config");
            return;
        }
        match text {
            Some(text) => match dtr_config::keys::parse_sequence(text) {
                Ok(sequence) => {
                    self.config.keys.insert(name.to_owned(), sequence);
                }
                Err(err) => {
                    tracing::warn!(%name, %err, "the chord it just wrote will not parse");
                }
            },
            None => {
                self.config.keys.remove(name);
            }
        }
        self.keymap = crate::keys::Keymap::new(&self.config.keys);
        // A half-typed sequence belongs to the old map: flushing it now would
        // fire an action nobody chose on the way out.
        self.pending.clear();
        self.pending_since = None;
        self.keymap_card.edit = None;
        let now = self.keymap.bound(action);
        let note = match (text.is_some(), now.first()) {
            (true, Some(chord)) => format!("{} is now {chord}", action.label()),
            (false, Some(chord)) => format!("{} is back to {chord}", action.label()),
            (false, None) => format!("{} has no default", action.label()),
            (true, None) => format!("{} is unbound", action.label()),
        };
        self.toast = Some((note, std::time::Instant::now()));
        self.repaint("rebind");
    }

    fn reset_binding(&mut self, action: crate::keys::Action) {
        if !self.config.keys.contains_key(action.name()) {
            self.keymap_card.edit = None;
            self.toast = Some((
                format!("{} was already the default", action.label()),
                std::time::Instant::now(),
            ));
            return;
        }
        self.write_binding(action, None);
    }

    fn dispatch(&mut self, action: crate::keys::Action) -> bool {
        use crate::keys::Action;
        match action {
            Action::Palette => {
                self.palette.toggle();
                self.repaint("palette");
                true
            }
            Action::Keymap => {
                self.keymap_card.open = !self.keymap_card.open;
                if !self.keymap_card.open {
                    self.keymap_card.edit = None;
                }
                self.repaint("keymap");
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
                // empties the pane's copy of it. A selection is copied as
                // anchored, and with nothing selected the whole screen is what
                // ctrl-shift-c has always meant.
                let Some(session) = self.session() else {
                    return false;
                };
                let selected = session
                    .frame
                    .as_ref()
                    .is_some_and(|frame| frame.selection.is_some());
                if selected {
                    session.pane.send(crate::pane::Command::CopySelection);
                } else {
                    session.pane.send(crate::pane::Command::Copy);
                }
                true
            }
            // egui turns *its* paste shortcut (ctrl+v) into an event and knows
            // nothing about `ctrl-shift-v`, which is the binding this app
            // documents — so the chord only this app can see has to read the
            // clipboard itself. Both paths end in `Command::Paste`, which is
            // where a program's bracketed paste is honoured.
            Action::Paste => match self.clipboard_text() {
                Some(text) if !text.is_empty() => {
                    self.paste(text);
                    true
                }
                // Consumed even when there is nothing to paste: the alternative
                // is the chord falling through to the shell as a literal ^V,
                // which is what "insert the next character literally" means to
                // vim and emacs — a key nobody pressed.
                Some(_) => {
                    self.toast = Some((
                        "the clipboard is empty".to_owned(),
                        std::time::Instant::now(),
                    ));
                    self.repaint("paste");
                    true
                }
                None => {
                    self.toast = Some((
                        "could not read the clipboard".to_owned(),
                        std::time::Instant::now(),
                    ));
                    self.repaint("paste");
                    true
                }
            },
            Action::ScrollPageUp | Action::ScrollPageDown => {
                let page = self
                    .session()
                    .and_then(|session| session.frame.as_ref())
                    .map_or(10, |frame| frame.rows.saturating_sub(1))
                    as isize;
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
            Action::PreviousPrompt | Action::NextPrompt => {
                let Some(session) = self.session() else {
                    return false;
                };
                session.pane.send(crate::pane::Command::ScrollPrompt(
                    if matches!(action, Action::PreviousPrompt) {
                        -1
                    } else {
                        1
                    },
                ));
                true
            }
            Action::CaptureBlock => {
                let Some(session) = self.session() else {
                    return false;
                };
                session.pane.send(crate::pane::Command::CaptureBlock);
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
            || draft.font.family != self.config.font.family
            || draft.font.fallback != self.config.font.fallback;
        let theme_changed = draft.theme != self.config.theme;
        let serial_changed = draft.serial != self.config.serial;

        self.config = draft;
        self.look.size = self.config.font.size;
        self.look.family = self.config.font.family.clone();
        self.look.fallback = self.config.font.fallback.clone();

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
                self.look.preset,
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
        if let Some((port, mut record)) = self.settings.serial_write() {
            // The menu's auto_reconnect row is the global policy; if it moved,
            // this port's record takes the new value too. A menu that only
            // changed the rate leaves the port's own policy alone.
            if serial_changed {
                record.auto_reconnect = self.config.serial.auto_reconnect;
            }
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
                                data_bits: record.data_bits,
                                parity: record.parity,
                                stop_bits: record.stop_bits,
                                flow_control: record.flow_control,
                                dtr: record.dtr,
                                rts: record.rts,
                                cr_to_crlf: record.cr_to_crlf,
                                auto_reconnect: record.auto_reconnect,
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
                            let mut record = self
                                .store
                                .as_ref()
                                .and_then(|store| store.serial(&port).ok().flatten())
                                .unwrap_or_default();
                            // The config file's reconnect policy is the
                            // terminal's, so a change there reaches every port
                            // and is remembered as the new per-port default.
                            record.auto_reconnect = self.config.serial.auto_reconnect;
                            if let Some(store) = self.store.as_ref() {
                                if let Err(err) = store.remember_serial(&port, &record) {
                                    tracing::warn!(%port, %err, "could not remember the reconnect policy");
                                }
                            }
                            session.pane.send(crate::pane::Command::SetSerial {
                                baud: record.baud,
                                data_bits: record.data_bits,
                                parity: record.parity,
                                stop_bits: record.stop_bits,
                                flow_control: record.flow_control,
                                dtr: record.dtr,
                                rts: record.rts,
                                cr_to_crlf: record.cr_to_crlf,
                                auto_reconnect: record.auto_reconnect,
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
        // Remembered so a spinning loop names its cause: the reason strings are
        // distinct per caller, and a flood of one of them is the answer.
        self.wake_why.set(why);
        tracing::debug!(why, "request redraw");
        if let Some(gpu) = &self.gpu {
            gpu.window.request_redraw();
        }
        // `request_redraw` alone is not enough on Wayland: a request made from
        // inside a redraw waits for the compositor's next frame callback, so the
        // change only appears when something else wakes the loop — a closed tab
        // that vanishes when the mouse moves. `about_to_wait` turns this flag
        // into a deadline, which wakes the loop at once.
        self.wake.set(true);
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
            self.config.terminal.scrollback_limit,
            std::sync::Arc::clone(&self.frame_budget),
            move || {
                let _ = proxy.send_event(UserEvent::Frame);
            },
        );
        match spawned {
            Ok(pane) => {
                let alias = ssh_alias(&transport);
                let session = Session {
                    pane,
                    port: match &transport {
                        Transport::Serial(cfg) => Some(cfg.port.clone()),
                        _ => None,
                    },
                    frame: None,
                    label: transport.describe(),
                    program_title: String::new(),
                    serial: matches!(transport, Transport::Serial(_)),
                    // A remote shell reports the *remote* directory; it must not
                    // be adopted as a local path by a new tab.
                    remote: alias.is_some(),
                    alias,
                    counters: !matches!(transport, Transport::Shell(_)),
                    echo: false,
                    meter: Meter::new(),
                    grid: (0, 0),
                };
                let id = self.next_leaf;
                self.next_leaf += 1;
                self.tabs.push(Tab::new(id, Pane::Terminal(session)));
                self.active = self.tabs.len() - 1;
                tracing::debug!(active = self.active, "tab opened");
            }
            Err(err) => {
                tracing::error!(%err, "could not start the session");
                self.fatal = Some(format!("{err}"));
            }
        }
    }

    /// Close one tab by index, whichever is active.
    ///
    /// The last tab closing takes the window with it: a terminal with no
    /// sessions has nothing to show, which is the same rule `close_active`
    /// follows.
    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        if self.tabs.len() == 1 {
            self.quit = true;
            return;
        }
        // Dropping a tab tears down a pty and the thread that owns it, which
        // can take as long as the child takes to notice. That is not allowed to
        // happen between two frames: the tab goes to the worker runtime and the
        // UI thread moves on.
        let dead = self.tabs.remove(index);
        let at = std::time::Instant::now();
        match self.worker.as_ref() {
            Some(worker) => {
                worker.spawn_blocking(move || {
                    let started = std::time::Instant::now();
                    drop(dead);
                    tracing::debug!(ms = started.elapsed().as_millis(), "tab torn down");
                });
            }
            None => {
                drop(dead);
                tracing::debug!(ms = at.elapsed().as_millis(), "tab torn down inline");
            }
        }
        // Focus the tab that took its place, or the one before it when the last
        // tab closed.
        if self.active > index {
            self.active -= 1;
        } else if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        self.repaint("tab closed");
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
            self.config.terminal.scrollback_limit,
            std::sync::Arc::clone(&self.frame_budget),
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
        let alias = ssh_alias(&transport);
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
                program_title: String::new(),
                serial: matches!(transport, Transport::Serial(_)),
                remote: alias.is_some(),
                alias,
                counters: !matches!(transport, Transport::Shell(_)),
                echo: false,
                meter: Meter::new(),
                grid: (0, 0),
            }),
        );
        tab.focus = id;
    }

    /// Split the focused pane and put a file manager in the new half. Side by
    /// side is the useful default: a listing next to the command line it feeds.
    /// Start the SFTP worker if it is not running yet.
    ///
    /// One worker per window, not per pane: the file browser is a window-level
    /// tool, and every transfer it makes shares one connection per host.
    fn sftp_worker(&mut self) -> &std::sync::mpsc::Sender<SftpMsg> {
        if self.sftp.is_none() {
            let (tx, rx) = std::sync::mpsc::channel::<SftpMsg>();
            let proxy = self.proxy.clone();
            std::thread::Builder::new()
                .name("dtr-sftp".into())
                .spawn(move || sftp_worker(rx, proxy))
                .ok();
            self.sftp = Some(tx);
        }
        // `is_none` was false either way by now.
        self.sftp.as_ref().expect("worker started")
    }

    /// Ask for whatever any remote pane is waiting on, and start any download it
    /// asked for.
    ///
    /// Every leaf in every tab, not the focused one: a pane nobody is typing in
    /// still has to list, and the panes a link opens are deliberately unfocused.
    /// Pumping only the focus left those showing "loading…" for ever.
    fn pump_sftp(&mut self) {
        let mut requests = Vec::new();
        let mut downloads = Vec::new();
        for tab in &mut self.tabs {
            for pane in tab.leaves.values_mut() {
                let Pane::Files(explorer) = pane else {
                    continue;
                };
                if let Some(request) = explorer.take_request() {
                    requests.push(request);
                }
                if let Some(remote) = explorer.take_download() {
                    if let Some(alias) = explorer.alias() {
                        downloads.push((alias.to_owned(), remote));
                    }
                }
            }
        }
        for (alias, path) in requests {
            let tx = self.sftp_worker();
            let _ = tx.send(SftpMsg::List { alias, path });
        }
        for (alias, remote) in downloads {
            let local = save_target(&remote);
            let tx = self.sftp_worker();
            let _ = tx.send(SftpMsg::Download {
                alias,
                remote,
                local,
            });
        }
    }

    /// The pane waiting for this listing, on this host.
    ///
    /// A reply names the directory it answers and two panes can browse one host,
    /// so the match is by host *and* by what the pane asked for.
    fn explorer_waiting(&mut self, alias: &str, path: &Path) -> Option<&mut Explorer> {
        for tab in &mut self.tabs {
            for pane in tab.leaves.values_mut() {
                let Pane::Files(explorer) = pane else {
                    continue;
                };
                if explorer.alias() == Some(alias) && explorer.is_waiting_for(path) {
                    return Some(explorer);
                }
            }
        }
        None
    }

    /// Any pane browsing this host, for a failure that names no directory.
    fn explorer_on(&mut self, alias: &str) -> Option<&mut Explorer> {
        for tab in &mut self.tabs {
            for pane in tab.leaves.values_mut() {
                let Pane::Files(explorer) = pane else {
                    continue;
                };
                if explorer.alias() == Some(alias) {
                    return Some(explorer);
                }
            }
        }
        None
    }

    fn sftp_event(&mut self, event: SftpEvent) {
        match event {
            SftpEvent::Listed {
                alias,
                path,
                entries,
            } => {
                let entries: Vec<crate::explorer::Entry> = entries
                    .into_iter()
                    .map(|entry| crate::explorer::Entry {
                        name: entry.name,
                        is_dir: entry.dir,
                        size: (!entry.dir).then_some(entry.size),
                    })
                    .collect();
                if let Some(explorer) = self.explorer_waiting(&alias, &path) {
                    explorer.apply_listing(path, entries);
                }
            }
            SftpEvent::Downloaded { local, bytes } => {
                self.toast = Some((
                    format!("copied {} bytes to {}", bytes, local.display()),
                    std::time::Instant::now(),
                ));
            }
            SftpEvent::Failed { alias, message } => {
                if let Some(explorer) = self.explorer_on(&alias) {
                    explorer.apply_failure(message);
                }
            }
        }
    }

    /// Open a file pane over another machine, beside the terminal that is on it.
    fn split_files_remote(&mut self, alias: &str) {
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
        tab.leaves.insert(id, Pane::Files(Explorer::remote(alias)));
        tab.focus = id;
    }

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
        tab.leaves.insert(id, Pane::Files(Explorer::new(path)));
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
        let next = cycled(self.active, self.tabs.len(), forward);
        self.set_active(next, "tab chord");
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
        let status_height = if self.status_visible() {
            (crate::ui::STATUS_HEIGHT + crate::ui::GRID_GAP) * scale
        } else {
            0.0
        };
        // The tab strip takes the top, and only when there is more than one tab
        // to show: a lone tab is a title nobody needs.
        let tabs_height = self.top_inset(scale);
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
                        session.frame.as_ref().and_then(|frame| frame.selection),
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
    /// How much of the top of the window the tab strip takes, in physical
    /// pixels. One place, because the two users of it — the layout that sizes
    /// the terminal and the layout that draws it — disagreeing is exactly how a
    /// prompt ends up underneath the strip.
    /// Whether the status bar is drawn at all.
    ///
    /// It exists to show a serial port's counters and modem lines. A shell has
    /// nothing to put there that the tab strip is not already saying, so the
    /// bar is not drawn and its height is not reserved — otherwise a shell
    /// carries an empty strip of dead pixels along the bottom.
    fn status_visible(&self) -> bool {
        self.session().is_some_and(|session| session.counters)
    }

    fn log_strip_geometry(&mut self, scale: f32) {
        // Once per scale change, so a HiDPI mismatch is visible without a
        // screenshot: the reservation and the strip must be the same number,
        // both in physical pixels.
        if self.strip_logged == Some(scale) {
            return;
        }
        self.strip_logged = Some(scale);
        tracing::debug!(
            scale,
            reserved_px = (crate::ui::TAB_HEIGHT + crate::ui::GRID_GAP) * scale,
            strip_px = crate::ui::TAB_HEIGHT * scale,
            tab_height_points = crate::ui::TAB_HEIGHT,
            "strip geometry"
        );
    }

    fn top_inset(&self, scale: f32) -> f32 {
        // Always, not only when there are several tabs: the strip is drawn even
        // for one tab (it is where the session is named, now that a shell has no
        // status bar), and reserving space only sometimes is how the prompt ended
        // up underneath it.
        (crate::ui::TAB_HEIGHT + crate::ui::GRID_GAP) * scale
    }

    fn fit_panes(&mut self, metrics: crate::render::Metrics, screen: (f32, f32), scale: f32) {
        if metrics.cell_width <= 0.0 || metrics.cell_height <= 0.0 {
            return;
        }
        let inset = self.top_inset(scale);
        let status = if self.status_visible() {
            (crate::ui::STATUS_HEIGHT + crate::ui::GRID_GAP) * scale
        } else {
            0.0
        };
        let tab = &mut self.tabs[self.active];
        let rects = tab.layout.rects(
            crate::layout::Rect::new(0.0, inset, screen.0, (screen.1 - status - inset).max(0.0)),
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
        let col = ((x as f32 - origin.0) / metrics.cell_width)
            .floor()
            .max(0.0) as u32;
        let row = ((y as f32 - origin.1) / metrics.cell_height)
            .floor()
            .max(0.0) as u32;
        if col >= cols || row >= rows {
            return None;
        }
        Some((row * cols + col) as usize)
    }

    /// Open the connection sheet for `port`, prefilled from the store.
    ///
    /// A serial connection is the one action that asks first: the settings are
    /// per-device and the port is not opened until the sheet is answered. The
    /// remembered record supplies the form; the remembered profile supplies the
    /// dropdown's selection.
    fn open_serial_sheet(&mut self, port: String) {
        // The detail line comes from the destination scan the new-tab menu
        // already did; a port the menu does not list (a typed path) gets none.
        let detail = self
            .destinations
            .iter()
            .find_map(|destination| match destination {
                crate::sources::Destination::Serial { path, detail } if path == &port => {
                    Some(detail.clone())
                }
                _ => None,
            })
            .unwrap_or_default();
        self.serial_dialog = Some(crate::serial_dialog::SerialDialog::from_store(
            port,
            detail,
            self.store.as_deref(),
        ));
        self.repaint("serial connection sheet");
    }

    /// The sheet was answered: remember the form against the port and open it.
    fn connect_serial_sheet(
        &mut self,
        dialog: crate::serial_dialog::SerialDialog,
        settings: dtr_store::SerialSettings,
    ) {
        let port = dialog.port().to_owned();
        if let Some(store) = self.store.as_ref() {
            if let Err(err) = store.remember_serial(&port, &settings) {
                tracing::warn!(%port, %err, "could not remember the port settings");
            }
            let _ = store.set_last_port(&port);
        }
        self.open(Transport::Serial(crate::serial::Config::from_settings(
            port, &settings,
        )));
        self.repaint("serial sheet connected");
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::Connect(port) => self.open_serial_sheet(port),
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
                        self.look.preset,
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
                if let Some(alias) = self.session().and_then(|session| session.alias.clone()) {
                    // Another machine: its own files, over SFTP. A local browser
                    // here would show this machine's files under a remote tab's
                    // name, which is worse than saying so.
                    self.split_files_remote(&alias);
                    self.repaint("remote files");
                } else {
                    // No argument means "the directory this session is in",
                    // which is what a file pane next to a terminal should show.
                    let path = path
                        .filter(|path| !path.is_empty())
                        .map(PathBuf::from)
                        .or_else(|| self.session().and_then(|session| session.pane.cwd()));
                    self.split_files(path);
                }
            }
            Action::Shell(shell) => {
                let program = (!shell.is_empty()).then_some(shell);
                self.open(Transport::Shell(crate::pty::Config {
                    program,
                    ..Default::default()
                }));
            }
            Action::Replay(path) => self.open(Transport::Replay(PathBuf::from(path))),
            Action::Demo => self.open(Transport::Demo),
            Action::Ssh(host) => {
                // Delegating to the system's ssh client: it already knows about
                // the config file, agent forwarding, jump hosts and known_hosts,
                // and re-implementing those is a project, not a feature. The
                // config also says that this is a connection, so the pane
                // reopens it when it drops instead of closing the tab.
                self.open(Transport::Shell(crate::pty::Config::ssh(&host)));
            }
            Action::Keys => self.keymap_card.open = true,
            Action::Quit => self.quit = true,
        }
    }
}

/// What the event loop did in the last second.
struct LoopReport {
    since: std::time::Instant,
    iterations: u64,
    frames: u64,
    wakes: u64,
}

impl Default for LoopReport {
    fn default() -> Self {
        Self {
            since: std::time::Instant::now(),
            iterations: 0,
            frames: 0,
            wakes: 0,
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

/// Report what the loop is actually doing, once a second.
///
/// A trait impl cannot add inherent methods, so this is a free function.
fn report_loop(app: &App) {
    let mut report = app.loop_report.borrow_mut();
    report.iterations += 1;
    let elapsed = report.since.elapsed();
    if elapsed < std::time::Duration::from_secs(1) {
        return;
    }
    tracing::debug!(
        iterations_per_s = report.iterations,
        frames_per_s = report.frames,
        wakes = report.wakes,
        "loop"
    );
    *report = LoopReport {
        since: std::time::Instant::now(),
        ..LoopReport::default()
    };
    // Free check once a second: a Wayland move emits no `Moved`, so this is
    // the only thing that notices the window is on a different panel.
    app.refresh_frame_budget();
}

/// The window's icon, decoded from the app icon the desktop entry names.
///
/// `include_bytes!` and not a path: the one-file Windows build has no `share/`,
/// which is the same reason the theme is compiled in. Windows also wants the
/// icon in the *executable* — that is `build.rs`, and it is what the taskbar
/// falls back to for a window that has not been shown yet, for a pinned entry,
/// and for Explorer.
fn window_icon() -> Option<winit::window::Icon> {
    let image = image::load_from_memory(include_bytes!("../../../assets/logo/dtr-icon-32.png"))
        .ok()?
        .to_rgba8();
    let (width, height) = image.dimensions();
    match winit::window::Icon::from_rgba(image.into_raw(), width, height) {
        Ok(icon) => Some(icon),
        Err(err) => {
            tracing::warn!(%err, "window icon refused");
            None
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }

        // Who paints the frame: the tab strip (`client`) or the window manager
        // (`native`). Read once, here: on Wayland `xdg_decoration` is a request a
        // compositor may refuse, and on X11 the motif hint is a hint, so
        // "switch it later" is not something that can be promised.
        let client_frame = self.config.chrome == dtr_config::Chrome::Client;
        let attrs = Window::default_attributes()
            .with_title("dtr")
            .with_decorations(!client_frame)
            // The same artwork the desktop entry names. Without it Windows draws
            // a generic blank next to the title, and a window list has nothing
            // to show but the name.
            .with_window_icon(window_icon())
            .with_inner_size(LogicalSize::new(900.0, 620.0));
        // A frameless window with no shadow reads as a rectangle pasted onto the
        // desktop; winit draws one only when asked, and only for a window whose
        // decorations are off.
        #[cfg(target_os = "windows")]
        let attrs = if client_frame {
            use winit::platform::windows::WindowAttributesExtWindows as _;
            attrs.with_undecorated_shadow(true)
        } else {
            attrs
        };
        // The desktop entry says `StartupWMClass=dtr` and `Icon=dtr`, so the
        // window has to say it is called that. Without a name, X11 derives
        // `WM_CLASS` from argv[0] — which under the nix wrapper is
        // `.dtr-unwrapped` — and Wayland gets no `app_id` at all, so a running
        // window lands in the dock as a second, iconless application. Both
        // platforms read the same field, which is why one call is enough.
        #[cfg(target_os = "linux")]
        let attrs = {
            use winit::platform::wayland::WindowAttributesExtWayland as _;
            attrs.with_name("dtr", "dtr")
        };
        let window = match event_loop.create_window(attrs) {
            Ok(window) => Arc::new(window),
            Err(err) => {
                tracing::error!(%err, "create window");
                event_loop.exit();
                return;
            }
        };
        // A terminal is where CJK is typed, and without this the platform never
        // offers its input method: no composition, no candidate window, and a
        // pinyin keyboard types latin letters at the shell. Enabling it is not
        // enough on its own — the caret has to be reported as it moves
        // (`set_ime_cursor_area` in the redraw) or the candidate window lands in
        // the corner of the screen.
        window.set_ime_allowed(true);

        // The caption's maximise button must answer Windows' hit test as
        // `HTMAXBUTTON`, or Windows 11 never offers the snap-layout flyout.
        // winit has no hook for a hit test, so the window procedure is
        // subclassed — once, here, for the lifetime of the window.
        #[cfg(target_os = "windows")]
        if client_frame {
            use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(handle) = window.window_handle() {
                if let RawWindowHandle::Win32(win32) = handle.as_raw() {
                    // `raw-window-handle` hands the handle over as a non-zero
                    // `isize`; Win32 wants the pointer it really is.
                    let hwnd = win32.hwnd.get() as *mut std::ffi::c_void;
                    // SAFETY: the window was just created on this thread and
                    // lives for as long as the process uses it.
                    unsafe { crate::chrome::windows::install(hwnd) };
                }
            }
        }

        match Gpu::new(
            window,
            &self.look,
            self.gpu_hint.as_deref(),
            self.config.chrome == dtr_config::Chrome::Client,
        ) {
            Ok(gpu) => {
                gpu.window.request_redraw();
                self.gpu = Some(gpu);
                self.refresh_frame_budget();
            }
            Err(err) => {
                tracing::error!(%err, "initialise GPU");
                event_loop.exit();
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        report_loop(self);
        self.check_exit_deadline(event_loop);

        // The control flow is **sticky**: winit keeps the last value until
        // something sets it again. Every deadline set below is in the past by
        // the time the next iteration runs, so leaving one in place makes every
        // `poll` time out instantly — 55,000 loop iterations a second with 20
        // frames drawn. So this collects the earliest deadline and sets exactly
        // one value at the end, defaulting to `Wait`.
        let mut deadline: Option<std::time::Instant> = None;
        let sooner = |at: std::time::Instant, deadline: &mut Option<std::time::Instant>| {
            *deadline = Some(match *deadline {
                Some(current) => current.min(at),
                None => at,
            });
        };

        // A repaint asked for while the loop was idle: wake now rather than at
        // the next input event.
        if self.wake.replace(false) {
            self.loop_report.borrow_mut().wakes += 1;
            sooner(std::time::Instant::now(), &mut deadline);
            if let Some(gpu) = &self.gpu {
                gpu.window.request_redraw();
            }
        }

        // A scan is cheap to *ask* for and expensive to *do*, so asking never
        // blocks the frame: the work goes to the worker runtime and comes back
        // as an event.
        if self.scan_due <= std::time::Instant::now() {
            self.request_scan();
        }
        // `--drag`: press, move, release — the three steps a person's hand
        // makes, so the copy-on-selection path runs without a mouse.
        if let Some((x1, y1, x2, y2)) = self.drag_script.take() {
            self.pointer_down((x1, y1));
            self.pointer_move((x2, y2));
            self.pointer_up();
            self.repaint("scripted drag");
        }
        // `--click`: hover first, then press and release on the next wake. A
        // hand hovers before it clicks, and the link under the pointer only
        // exists once the pane has answered, which is one frame later.
        if let Some((at, hovered)) = self.click_script.take() {
            self.cursor_position = Some(at);
            if hovered {
                self.pointer_down(at);
                self.pointer_up();
                self.repaint("scripted click");
            } else {
                self.hover(at);
                self.click_script = Some((at, true));
                self.repaint("scripted click: hover");
            }
        }

        // `--keys`: one stroke per wake-up, so the frame that follows each one
        // exists before the next arrives. This is the harness the user asked
        // for: a scripted run goes through the same handler a keyboard does.
        if let Some(stroke) = self.script.pop_front() {
            self.on_key(&stroke);
            self.repaint("scripted key");
            sooner(
                std::time::Instant::now() + std::time::Duration::from_millis(16),
                &mut deadline,
            );
        }

        // A pending capture needs a frame at its deadline. Without this the
        // window goes idle after the last redraw and the capture never
        // happens: the deadline passes with nothing drawing.
        if let Some((_, when)) = self.capture.as_ref().map(|(_, when)| ((), *when)) {
            self.repaint("capture pending");
            sooner(when, &mut deadline);
        }

        // A half-typed binding must not wait forever. One second, like Zed,
        // then whatever is already complete fires.
        const PENDING: std::time::Duration = std::time::Duration::from_secs(1);
        if let Some(since) = self.pending_since {
            let pending_deadline = since + PENDING;
            if std::time::Instant::now() >= pending_deadline {
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
                sooner(pending_deadline, &mut deadline);
            }
        }
        // Wake up when the deadline is due, rather than hoping some other
        // event brings the loop back: an idle window that never wakes would
        // never take its own exit.
        if let Some(after) = self.exit_after {
            sooner(
                self.started + std::time::Duration::from_millis(after),
                &mut deadline,
            );
        }

        // A preview of a remote picture waits for the pointer to settle, and the
        // wait needs a wake of its own: the pointer resting produces no events, and
        // a request nobody wakes for would never be made.
        match self.thumb_pending.as_ref().map(|pending| pending.due) {
            Some(due) if std::time::Instant::now() >= due => self.due_thumbnail(),
            Some(due) => sooner(due, &mut deadline),
            None => {}
        }

        // One value, every iteration: without this the last deadline stays set
        // and every poll times out immediately.
        event_loop.set_control_flow(match deadline {
            Some(at) => winit::event_loop::ControlFlow::WaitUntil(at),
            None => winit::event_loop::ControlFlow::Wait,
        });
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, _cause: winit::event::StartCause) {
        self.check_exit_deadline(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Files => {
                // A file changed: scan now rather than at the next poll.
                self.scan_due = std::time::Instant::now();
                self.repaint("watched file changed");
            }
            UserEvent::Sftp(event) => {
                self.sftp_event(event);
                self.repaint("sftp");
            }
            UserEvent::Reveal { path, alias } => {
                self.reveal_directory(path, alias);
                self.repaint("a link named a directory");
            }
            UserEvent::Thumbnail { key, result } => {
                self.thumb.finish(key, result);
                self.repaint("thumbnail");
            }
            UserEvent::Notice(text) => {
                tracing::info!(%text, "notice");
                self.toast = Some((text, std::time::Instant::now()));
                self.repaint("notice");
            }
            UserEvent::SavePath { location, path } => {
                self.picking_save = false;
                match path {
                    Some(path) => self.begin_save(location, path),
                    // Dismissed, or a dialog that never appeared. Nothing is
                    // written, and the log is where the difference is.
                    None => {
                        tracing::info!("save dialog closed with no path");
                        self.repaint("save dialog dismissed");
                    }
                }
            }
            UserEvent::Destinations(destinations) => {
                self.destinations = destinations;
                self.scanning = false;
                self.repaint("destinations");
            }
            UserEvent::Frame => {}
        }
        // Every terminal pane publishes frames, not only the focused one: a
        // `--select START:END` names linear cells from a script; the pane wants
        // viewport cells, and only a frame knows how wide the grid is.
        if let Some((start, end)) = self.scripted_selection {
            if let Some(cols) = self
                .session()
                .and_then(|session| session.frame.as_ref())
                .map(|frame| usize::from(frame.cols).max(1))
            {
                let cell = |at: usize| ((at % cols) as u16, (at / cols) as u16);
                let (sx, sy) = cell(start);
                let (hx, hy) = cell(end);
                if let Some(session) = self.session() {
                    session
                        .pane
                        .send(crate::pane::Command::SelectStart { x: sx, y: sy });
                    session
                        .pane
                        .send(crate::pane::Command::SelectHead { x: hx, y: hy });
                }
                self.scripted_selection = None;
            }
        }
        // file pane can have focus while the terminal beside it is still live.
        for tab in &mut self.tabs {
            for pane in tab.leaves.values_mut() {
                if let Pane::Terminal(session) = pane {
                    if let Some(frame) = session.pane.take_frame() {
                        if frame.title != session.program_title {
                            tracing::debug!(
                                from = %session.program_title,
                                to = %frame.title,
                                "session renamed itself"
                            );
                            session.program_title = frame.title.clone();
                        }
                        session.frame = Some(frame);
                    }
                }
            }
        }
        self.reap_ended();
        if let Some(err) = self.session().and_then(|session| session.pane.error()) {
            tracing::error!(%err, "pane failed");
            self.fatal = Some(err);
            event_loop.exit();
            return;
        }
        self.repaint("pane frame");
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if matches!(event, WindowEvent::RedrawRequested) {
            self.egui_redraw_pending = false;
        }
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
            // Dragged to another monitor: the refresh rate may not be the one
            // the gate was computed from.
            WindowEvent::Moved(_) => self.refresh_frame_budget(),
            WindowEvent::ScaleFactorChanged { .. } => {
                // The grid was rasterised at the old factor, so every cell is
                // now the wrong size for the display; the next frame rebuilds it
                // (`fonts_dirty` is that hook) and `fit_panes` then re-fits the
                // pty from the new metrics. Without the rebuild the terminal
                // keeps the cell of the monitor it came from.
                //
                // The refresh-rate budget is not refreshed here: it follows the
                // monitor (`Moved`), not the factor.
                self.fonts_dirty = true;
                gpu.window.request_redraw();
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::KeyboardInput { event, .. } => {
                let stroke = crate::keys::KeyStroke {
                    // The position is what a terminal reports; the character a
                    // layout produces is `text`, and the encoder uses both for
                    // different things.
                    physical: match event.physical_key {
                        winit::keyboard::PhysicalKey::Code(code) => Some(code),
                        winit::keyboard::PhysicalKey::Unidentified(_) => None,
                    },
                    key: event.logical_key.clone(),
                    text: event.text.as_ref().map(|text| text.to_string()),
                    state: event.state,
                    repeat: event.repeat,
                    // A key pressed while composing belongs to the composition:
                    // the commit will carry the text, so this one must not.
                    composing: self.preedit.is_composing(),
                    mods: self.modifiers,
                };
                self.on_key(&stroke);
            }
            WindowEvent::Ime(ime) => self.on_ime(ime),
            WindowEvent::Resized(size) => {
                gpu.resize(size);
                // The surface is not the only thing that depends on the window
                // size: the terminal's rows and columns do, and every pane
                // rectangle does. Without this the pty keeps the grid of the old
                // window, and with a strip across the top the first row slides
                // underneath it.
                let (metrics, scale) = match self.gpu.as_ref() {
                    Some(gpu) => (gpu.metrics(), gpu.scale()),
                    None => return,
                };
                self.fit_panes(metrics, (size.width as f32, size.height as f32), scale);
                self.repaint("resize");
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_position = Some((position.x, position.y));
                // While the pager is open the pointer belongs to egui: hovering
                // the grid underneath would light up a link the pager covers, and
                // a drag would select text nobody can see. A menu is the same: it
                // is about one file, not about the cell under the pointer.
                if !self.pager.is_open()
                    && !self.link_menu.is_open()
                    && self.serial_dialog.is_none()
                {
                    self.pointer_move((position.x, position.y));
                    self.hover((position.x, position.y));
                }
            }
            // Leaving the window is not a move to another cell, so without this the
            // last hover would stand: a pointing hand and a preview of a file the
            // pointer is no longer anywhere near.
            WindowEvent::CursorLeft { .. } => {
                self.hover_cell = None;
                self.hover_link = None;
                self.thumb_pending = None;
            }
            WindowEvent::MouseInput { state, button, .. } => {
                use winit::event::{ElementState, MouseButton as WinitButton};
                if self.pager.is_open() {
                    // The pager owns the pointer while it is open; the wheel and
                    // the drag belong to it.
                } else if self.serial_dialog.is_some() {
                    // The sheet is a form: egui has already seen the click, and
                    // it must not also start a selection in the terminal behind.
                } else if let Some(at) = self.cursor_position {
                    match (button, state) {
                        (WinitButton::Left, ElementState::Pressed) => self.left_press(at),
                        (WinitButton::Right, ElementState::Pressed) => self.right_click(at),
                        (WinitButton::Middle, ElementState::Pressed) => self.middle_click(at),
                        // The two side buttons have terminal numbers: X11 calls
                        // them 8 and 9 and the protocol is X11's. `Other` carries
                        // a platform button number with no terminal meaning, so
                        // it is not invented into one.
                        (WinitButton::Back, ElementState::Pressed) => {
                            self.forward_mouse(MouseAction::Press, Some(MouseButton::Eight), at);
                        }
                        (WinitButton::Forward, ElementState::Pressed) => {
                            self.forward_mouse(MouseAction::Press, Some(MouseButton::Nine), at);
                        }
                        (WinitButton::Other(_), _) => {}
                        // Every release, whichever button pressed: the program
                        // that heard the press is waiting for the one that ends
                        // it, and a drag it is following does not end without it.
                        (_, ElementState::Released) => {
                            // A press that dismissed a menu started no drag, so
                            // there is no drag to end.
                            if !self.link_menu.is_open() {
                                self.pointer_up();
                            }
                        }
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(y),
                    winit::event::MouseScrollDelta::PixelDelta(position) => position.y / 20.0,
                };
                // The pager scrolls itself: egui reads the wheel from the event
                // stream, and sending the same wheel to the shell's scrollback
                // would move a view the pager is covering.
                if lines != 0.0 && !self.pager.is_open() && self.serial_dialog.is_none() {
                    // A program that tracks the mouse gets the wheel as buttons 4
                    // and 5, which is how vim and every pager over ssh scroll.
                    // Shift is the selection key again, so it still scrolls the
                    // scrollback, as does a terminal nobody grabbed.
                    if mouse_goes_to_program(self.mouse_captured(), self.modifiers.shift_key()) {
                        let notches = lines.abs().round().max(1.0) as usize;
                        self.scroll_to_program(lines > 0.0, notches);
                    } else {
                        // Wheel up shows older lines, which the terminal calls a
                        // negative delta. Only a focused terminal scrolls.
                        if let Some(session) = self.session() {
                            session
                                .pane
                                .send(crate::pane::Command::Scroll(-lines.round() as isize));
                        }
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
                    let captured = self
                        .session_mut()
                        .and_then(|session| session.pane.take_captured());
                    // The pane answered the hover question. Only accept it if the
                    // pointer is still on the cell it was asked about: a reply that
                    // arrives a frame late must not open the wrong link.
                    if let Some(reply) = self
                        .session_mut()
                        .and_then(|session| session.pane.take_link())
                    {
                        if self.hover_cell == Some((reply.x, reply.y)) {
                            let uri = reply.uri;
                            self.hover_link = uri.as_deref().map(|uri| LinkHover {
                                x: reply.x,
                                y: reply.y,
                                uri: uri.to_owned(),
                            });
                            // Only when the link *changes*: the reply is edge-triggered
                            // by the pane, so this is once per hovered cell, not once
                            // per frame.
                            if let Some(uri) = uri.as_deref() {
                                self.request_thumbnail(uri);
                            }
                        }
                    }
                    // A pointing hand is the whole affordance: a link cell has no
                    // underline to draw, and the cursor says "this is clickable"
                    // everywhere else on the desktop too.
                    let wanted = if self.hover_link.is_some() {
                        winit::window::CursorIcon::Pointer
                    } else {
                        winit::window::CursorIcon::Default
                    };
                    if self.cursor_icon != Some(wanted) {
                        self.cursor_icon = Some(wanted);
                        gpu.window.set_cursor(wanted);
                    }
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
                    self.pump_sftp();
                    self.log_strip_geometry(scale);
                    self.fit_panes(gpu.metrics(), screen, scale);
                    // Before the borrows below: uploading a texture needs `&mut self`
                    // (the cache turns pixels into a texture once) and `settings` is
                    // about to lend `self` out for the whole frame.
                    let thumbnail = self.hover_thumbnail(scale, &gpu.egui_ctx);
                    let settings = if self.settings.open {
                        Some(&self.settings)
                    } else {
                        None
                    };
                    // Taken out for the frame for the same reason the pager is:
                    // the sheet borrows itself while it draws, and the answer is
                    // applied after the egui pass.
                    let serial_dialog = self.serial_dialog.take();
                    let mut serial_event = None;
                    // A window capture is due once, then never again.
                    // The active tab follows its focused session; the others keep
                    // the label they had, so a background session cannot rename
                    // itself under the eye.
                    if let Some(session) = self.tabs[self.active].focused_terminal() {
                        let label = tab_name(&session.program_title, &session.label).to_owned();
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
                    // The strip is the title bar now, so the active tab is the
                    // window's name too: the taskbar and Alt-Tab read this, and
                    // with several sessions "dtr" names none of them.
                    if let Some(label) = tabs.get(self.active) {
                        gpu.set_window_title(label);
                    }
                    let mut tab_click = None;
                    let mut settings_change = None;
                    let capture_path = match self.capture.take() {
                        Some((path, when)) if std::time::Instant::now() >= when => Some(path),
                        other => {
                            self.capture = other;
                            None
                        }
                    };
                    let tab = &self.tabs[self.active];
                    // The focused pane's rectangle, kept while the views are built:
                    // the IME box hangs off its caret.
                    let mut focused_rect = None;
                    let panes: Vec<_> = self
                        .pane_views(tab, gpu.metrics(), screen, scale)
                        .into_iter()
                        .map(|(id, pane)| {
                            if id == tab.focus {
                                focused_rect = Some(pane.rect);
                            }
                            pane
                        })
                        .collect();
                    // File panes have no cells: they are painted with egui over
                    // the rectangle the grid pass reserved for them.
                    let (width, height) = (gpu.config.width as f32, gpu.config.height as f32);
                    let status_height = if self.status_visible() {
                        (crate::ui::STATUS_HEIGHT + crate::ui::GRID_GAP) * gpu.scale()
                    } else {
                        0.0
                    };
                    let area = crate::layout::Rect::new(
                        0.0,
                        self.top_inset(gpu.scale()),
                        width,
                        (height - status_height - self.top_inset(gpu.scale())).max(0.0),
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
                    // The composition box hangs off the caret of the focused
                    // terminal, and the platform's candidate window is told the same
                    // place: one geometry, two consumers, so they cannot disagree
                    // about where the person is typing.
                    let mut ime_anchor = None;
                    // Not while an overlay owns the keyboard: a dialogue's text field
                    // shows the composition itself (egui-winit feeds it), and a box
                    // hanging off the terminal cursor would claim the composition
                    // belongs to a pane that is not receiving it.
                    if self.preedit.is_composing() && self.terminal_owns_keys() {
                        if let (Some(rect), Some(session)) = (focused_rect, tab.focused_terminal())
                        {
                            let cursor = session.frame.as_ref().and_then(|frame| frame.cursor);
                            if let Some(cursor) = cursor {
                                let (col, row) = (cursor.x, cursor.y);
                                let metrics = gpu.metrics();
                                let cell = crate::layout::Rect::new(
                                    rect.x + f32::from(col) * metrics.cell_width,
                                    rect.y + f32::from(row) * metrics.cell_height,
                                    metrics.cell_width,
                                    metrics.cell_height,
                                );
                                // Physical pixels: winit's IME area is in them, and
                                // repeating an unchanged one would send a request
                                // per frame on Wayland.
                                let key = [cell.x, cell.y, cell.width, cell.height];
                                if self.ime_caret != Some(key) {
                                    self.ime_caret = Some(key);
                                    gpu.window.set_ime_cursor_area(
                                        winit::dpi::PhysicalPosition::new(
                                            cell.x as i32,
                                            cell.y as i32,
                                        ),
                                        winit::dpi::PhysicalSize::new(
                                            cell.width as u32,
                                            cell.height as u32,
                                        ),
                                    );
                                }
                                // egui draws in points; the grid is laid out in
                                // physical pixels. One conversion, here.
                                let points = gpu.egui_ctx.pixels_per_point();
                                let to_points = |r: crate::layout::Rect| {
                                    egui::Rect::from_min_size(
                                        egui::pos2(r.x / points, r.y / points),
                                        egui::vec2(r.width / points, r.height / points),
                                    )
                                };
                                ime_anchor = Some(crate::ime::Anchor {
                                    caret: to_points(cell),
                                    pane: to_points(rect),
                                });
                            }
                        }
                    }
                    let ime = ime_anchor.map(|anchor| (anchor, &self.preedit));
                    let look = self.look.clone();
                    // Taken out for the frame for the same reason the palette is: the
                    // pager mutates itself while it draws, and `self` is borrowed.
                    let mut pager = std::mem::take(&mut self.pager);
                    let mut link_menu = std::mem::take(&mut self.link_menu);
                    let mut menu_choice = None;
                    let mut close_window = false;
                    let mut keymap_event = None;
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
                        &mut pager,
                        thumbnail.as_ref().map(|(texture, size)| (texture, *size)),
                        &mut link_menu,
                        &mut menu_choice,
                        &mut copied_text,
                        &mut copied,
                        toast_text
                            .as_ref()
                            .map(|(text, alpha)| (text.as_str(), *alpha)),
                        settings,
                        self.keymap_card.open.then_some(&self.keymap_card),
                        &self.keymap,
                        &mut keymap_event,
                        serial_dialog.as_ref(),
                        &mut serial_event,
                        &tabs,
                        self.active,
                        &self.destinations,
                        &mut tab_click,
                        self.status_visible(),
                        &mut settings_change,
                        capture_path.as_deref(),
                        &look,
                        ime,
                        &mut close_window,
                    );
                    // The keyboard map's answers. Its Escape and its captured
                    // key are handled where the keys are; a click has to come back
                    // from the frame, because only the frame knows the card was
                    // drawn.
                    if let Some(asked) = keymap_event {
                        self.keymap_event(asked);
                    }
                    // The caption's × is the window's close button. Same exit as
                    // the window manager's, so there is one shutdown path.
                    if close_window {
                        event_loop.exit();
                        return;
                    }
                    self.palette = palette;
                    // The pager comes back with the scroll the frame measured, which is
                    // the only place that knows how tall the text turned out.
                    self.pager = pager;
                    self.link_menu = link_menu;
                    self.serial_dialog = serial_dialog;
                    if let Some(asked) = serial_event {
                        match asked {
                            crate::serial_dialog::Event::Connect(settings) => {
                                if let Some(dialog) = self.serial_dialog.take() {
                                    self.connect_serial_sheet(dialog, settings);
                                }
                            }
                            crate::serial_dialog::Event::Cancel => {
                                self.serial_dialog = None;
                                tracing::info!("serial connection sheet cancelled");
                            }
                            crate::serial_dialog::Event::Save => {
                                // The sheet owns the list; the store owns the
                                // file. Only a named save is written.
                                if let Some(dialog) = self.serial_dialog.as_mut() {
                                    if let Some((name, settings)) = dialog.save() {
                                        if let Some(store) = self.store.as_ref() {
                                            if let Err(err) = store.save_profile(&name, &settings) {
                                                tracing::warn!(%name, %err, "could not save the profile");
                                            }
                                        }
                                    }
                                }
                            }
                            other => {
                                if let Some(dialog) = self.serial_dialog.as_mut() {
                                    dialog.apply(other);
                                }
                            }
                        }
                        self.repaint("serial dialog");
                    }
                    // The answers come back after the frame, like a clicked tab: the
                    // drawing code knows what was clicked, the app knows what to do
                    // about it.
                    if let Some(choice) = menu_choice {
                        self.link_menu_choice(choice);
                    }
                    if let Some(change) = settings_change {
                        // A mouse edit updates the draft, and the file is written
                        // when the menu is saved — same as a keyboard edit.
                        self.settings.apply(change);
                        self.repaint("settings changed");
                    }
                    match tab_click {
                        // A click on a tab is a switch, exactly like the chord.
                        Some(crate::ui::TabHit::Select(index)) if index < self.tabs.len() => {
                            self.set_active(index, "chip clicked");
                            self.repaint("tab clicked");
                        }
                        Some(crate::ui::TabHit::Close(index)) => self.close_tab(index),
                        Some(crate::ui::TabHit::New(destination)) => match destination {
                            // A port goes through the palette's own connect, because
                            // somebody may have set a baud rate or CR translation for
                            // this device and it is remembered per port. Opening a
                            // bare `Transport::Serial` here would ignore both.
                            Some(crate::sources::Destination::Serial { path, .. }) => {
                                self.apply(Action::Connect(path))
                            }
                            Some(crate::sources::Destination::Ssh { alias, .. }) => {
                                self.open(Transport::Shell(crate::pty::Config::ssh(&alias)))
                            }
                            None => {
                                let cwd = self
                                    .session()
                                    .filter(|session| !session.remote)
                                    .and_then(|session| session.pane.cwd());
                                self.open(Transport::Shell(crate::pty::Config {
                                    cwd,
                                    ..Default::default()
                                }));
                                self.repaint("new tab");
                            }
                        },
                        _ => {}
                    }
                    drop(toast_text);
                    if let Some(text) = captured {
                        // An empty block stays a toast: an empty pager would look like
                        // a bug rather than a shell without prompt marks.
                        if text.is_empty() {
                            self.toast = Some((
                                "nothing to capture — the shell has not marked a prompt".to_owned(),
                                std::time::Instant::now(),
                            ));
                        } else {
                            tracing::info!(lines = text.lines().count(), "paging a block");
                            self.pager.show(text, "the last command block");
                            // The pointer may be over a link; the pager is not a
                            // terminal, so the pointing hand has to go.
                            self.cursor_icon = Some(winit::window::CursorIcon::Default);
                            self.repaint("pager opened");
                        }
                    }
                    if let Some(text) = copied {
                        // Both paths: egui's, so its own paste and IME state agree,
                        // and ours, so the text reaches the desktop either way.
                        self.set_clipboard(&text);
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
                    if self
                        .toast
                        .as_ref()
                        .is_some_and(|(_, at)| at.elapsed() > TOAST)
                    {
                        self.toast = None;
                    }
                    // egui asks for another frame when something it drew needs one —
                    // opening a popup, an animation, a hover highlight. Nobody calls
                    // `repaint` for those, so the request has to reach the event
                    // loop. It does **not** set the `wake` flag: a wake means "this
                    // is due now" and a zero-delay wait, and egui asks again every
                    // frame while anything animates, which spins the loop at 100% CPU
                    // instead of idling. A plain redraw request is enough — the
                    // compositor delivers it.
                    // Once per drawn frame, not once per check: egui keeps asking
                    // while anything animates, and a `request_redraw` that is never
                    // answered (the Wayland backend waits for a frame callback)
                    // makes winit's pump loop busy-poll — 55k iterations a second
                    // with 20 frames.
                    // Hover feedback is worth the redraw: the strip lights up under
                    // the pointer, and a frame per refresh while the mouse moves is
                    // what that costs.
                    if gpu.egui_ctx.has_requested_repaint() && !self.egui_redraw_pending {
                        self.egui_redraw_pending = true;
                        gpu.window.request_redraw();
                    }
                    self.loop_report.borrow_mut().frames += 1;
                    self.frames_drawn = self.frames_drawn.wrapping_add(1);
                    // A frame that took long enough to feel is worth a line: the
                    // glyph count says whether it was font work or drawing.
                    let frame_ms = gpu.cpu_ms;
                    if frame_ms > 8.0 {
                        tracing::debug!(
                            ms = format!("{frame_ms:.1}"),
                            glyphs = gpu.grid.glyphs(),
                            "slow frame"
                        );
                    }
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
    /// Whether dtr paints the window frame, from `[window] chrome`. Read once,
    /// at creation: it decides whether the strip is a drag region and whether
    /// the buttons are drawn.
    client_frame: bool,
    /// Where the caption buttons get their symbols: the platform's icon font
    /// when it resolved at startup, drawn shapes when it did not.
    caption_glyphs: crate::chrome::Glyphs,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    /// When the current frame started, and how much of it was CPU work rather
    /// than waiting for the compositor.
    frame_began: std::time::Instant,
    cpu_ms: f32,
    /// The title the window was last told, so a frame that changes nothing does
    /// not talk to the window manager — which on Wayland is a round trip.
    title: String,
    window: Arc<Window>,
}

impl Gpu {
    /// Copy the frame that was just drawn into a PNG.
    ///
    /// The surface is configured `COPY_SRC` for this: an offscreen capture of
    /// the same state is not the same picture, and twice now the difference
    /// mattered (a menu that only drew in the window, a font that only broke
    /// there).
    fn write_capture(
        &self,
        output: &wgpu::SurfaceTexture,
        path: &std::path::Path,
    ) -> anyhow::Result<()> {
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
        crate::screenshot::write_png(path, width, height, &pixels)?;
        tracing::info!(path = %path.display(), width, height, "captured the window");
        Ok(())
    }

    /// Rebuild the grid for a new face or size. Cheap enough to do on a settings
    /// change: it is a fresh atlas, not a reload of the whole renderer.
    fn rebuild_grid(&mut self, look: &Look) {
        let scale = self.scale();
        self.grid = crate::render::GridRenderer::new(&self.device, self.config.format, look, scale);
        tracing::info!(
            family = ?look.family,
            size = look.size,
            scale,
            "grid rebuilt"
        );
    }

    fn new(
        window: Arc<Window>,
        look: &Look,
        hint: Option<&str>,
        client_frame: bool,
    ) -> anyhow::Result<Self> {
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

        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
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
        // Before any image is drawn: the SVG loader is what decodes the icons.
        crate::ui::install_image_loaders(&egui_ctx);
        // Whether the platform's caption-glyph font is there: the buttons draw
        // with it when it is, and paint their shapes when it is not. Asked here
        // because a missing glyph is a panic, not a fallback.
        let caption_font = look.install(&egui_ctx);
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
        let grid = crate::render::GridRenderer::new(
            &device,
            config.format,
            look,
            window.scale_factor() as f32,
        );
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
            client_frame,
            caption_glyphs: if caption_font {
                crate::chrome::Glyphs::Font
            } else {
                crate::chrome::Glyphs::Painted
            },
            egui_ctx,
            egui_state,
            frame_began: std::time::Instant::now(),
            cpu_ms: 0.0,
            title: String::new(),
            window,
        })
    }

    /// Keep the OS title in step with the active tab.    ///
    /// With a client-side frame the strip is the only place the session's name
    /// is written, so this is what the taskbar and Alt-Tab show. A window called
    /// `dtr` is anonymous when three sessions are open, and `set_title` is a
    /// window-manager round trip on Wayland, hence the comparison.
    fn set_window_title(&mut self, label: &str) {
        let title = format!("{label} — dtr");
        if self.title == title {
            return;
        }
        self.window.set_title(&title);
        self.title = title;
    }

    /// Ask Windows for the window's system menu, at a point in egui points.
    ///
    /// The menu is the one a native caption shows on a right-click or on its
    /// title-bar icon, and winit exposes exactly that as `show_window_menu`.
    /// Windows only, so everything else compiles to nothing.
    fn show_system_menu(&self, at: egui::Pos2) {
        #[cfg(target_os = "windows")]
        {
            let scale = self.scale();
            if let Ok(inner) = self.window.inner_position() {
                self.window
                    .show_window_menu(winit::dpi::PhysicalPosition::new(
                        inner.x + (at.x * scale) as i32,
                        inner.y + (at.y * scale) as i32,
                    ));
            }
        }
        #[cfg(not(target_os = "windows"))]
        let _ = at;
    }

    /// Cell size in physical pixels, for hit-testing.
    fn metrics(&self) -> crate::render::Metrics {
        self.grid.metrics()
    }

    /// The window a native dialog is parented to.
    ///
    /// A file picker is the one piece of the UI egui does not draw, and a dialog
    /// with no parent is a window that can open behind this one — or, on a
    /// Wayland session, in a corner with the compositor's idea of where a
    /// transient belongs rather than ours.
    fn window(&self) -> &Arc<Window> {
        &self.window
    }

    /// Backing-store pixels per logical point, for chrome measurements.
    fn scale(&self) -> f32 {
        // The DPI factor, not a ratio of two physical sizes: `inner_size` is
        // already physical in winit 0.30, so dividing the surface width by it is
        // always 1.0 and everything that multiplies by `scale` — the tab strip's
        // reservation, split gaps, the status bar — reserved half the space it
        // needed on a HiDPI display.
        self.window.scale_factor() as f32
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
        pager: &mut crate::pager::Pager,
        thumbnail: Option<(&egui::TextureHandle, egui::Vec2)>,
        link_menu: &mut crate::linkmenu::LinkMenu,
        menu_choice: &mut Option<crate::linkmenu::Choice>,
        copied_text: &mut Option<String>,
        copied: &mut Option<String>,
        toast: Option<(&str, f32)>,
        settings: Option<&crate::settings::Settings>,
        keymap: Option<&crate::keymap::Overlay>,
        keymap_bindings: &crate::keys::Keymap,
        keymap_event: &mut Option<crate::keymap::Event>,
        serial_dialog: Option<&crate::serial_dialog::SerialDialog>,
        serial_event: &mut Option<crate::serial_dialog::Event>,
        tabs: &[String],
        active_tab: usize,
        destinations: &[crate::sources::Destination],
        tab_click: &mut Option<crate::ui::TabHit>,
        status_visible: bool,
        settings_change: &mut Option<crate::settings::Change>,
        capture: Option<&std::path::Path>,
        look: &Look,
        // The composition being typed, and where its box belongs. Both are
        // absent unless an input method is in the middle of something.
        ime: Option<(crate::ime::Anchor, &crate::ime::Preedit)>,
        // The caption's close button: the drawing code knows it was pressed, but
        // only the event loop can close the window.
        close: &mut bool,
    ) -> anyhow::Result<()> {
        // The first few frames at debug, then every 120th: enough to see a
        // wedged surface without drowning the log at 60 Hz.
        let loud = frames_drawn < 3 || frames_drawn % 120 == 0;
        self.frame_began = std::time::Instant::now();
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
            // A client-side frame must exist even with no tabs, or the window
            // has no way to be moved or closed while the last tab is going away.
            if !tabs.is_empty() || self.client_frame {
                // The panel is the strip's full height and the chips are
                // shorter than it, so nothing egui rounds or grows can paint
                // over the first terminal row. Same lesson as the status bar.
                egui::Panel::top("dtr-tabs")
                    // Points, not physical pixels: egui is in points and `scale`
                    // is applied by egui itself. Multiplying here made the strip
                    // grow with DPI twice, so at scale 2 it was 60 points tall
                    // over a 60 *pixel* reservation and the first rows slid
                    // underneath it — invisible at scale 1.
                    .exact_size(crate::ui::TAB_HEIGHT)
                    // No inner margin: with a client-side frame the buttons run
                    // the strip's full height and the drag region is the whole
                    // row, so the inset belongs to the chips alone (below).
                    .frame(egui::Frame::new().fill(look.skin().palette.card))
                    .show(ui, |ui| {
                        let strip = ui.max_rect();
                        // The chips keep the inset the panel used to give them,
                        // except on Windows, where the caption icon's square
                        // comes first.
                        let left = if self.client_frame {
                            crate::chrome::chips_left()
                        } else {
                            6.0
                        };
                        let inset = egui::vec2(
                            left,
                            ((crate::ui::TAB_HEIGHT - crate::ui::TAB_CHIP_HEIGHT) / 2.0).floor(),
                        );
                        let mut chips = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(strip.shrink2(inset))
                                .layout(egui::Layout::left_to_right(egui::Align::Center)),
                        );
                        let bar =
                            crate::ui::tab_bar(&mut chips, tabs, active_tab, destinations, &look);
                        if let Some(hit) = bar.hit {
                            *tab_click = Some(hit);
                        }
                        // The rest of the row is the title bar. In `native` mode
                        // the window manager already gave us one, and a strip
                        // that also moved the window would be two title bars in
                        // one place.
                        if self.client_frame {
                            let maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
                            // The app icon, where Windows puts it, and doing what
                            // Windows does with it: a click is the system menu.
                            if let Some(at) = crate::chrome::caption_icon(ui, strip, &look) {
                                self.show_system_menu(at);
                            }
                            // The drag region stops where the buttons begin: a
                            // press that starts a drag must never be a press on
                            // the close button's neighbour.
                            let buttons = crate::chrome::buttons_rect(strip, crate::chrome::kind());
                            let caption = egui::Rect::from_min_max(
                                strip.min,
                                egui::pos2(buttons.left(), strip.bottom()),
                            );
                            match crate::chrome::drag_area(ui, caption, bar.used) {
                                Some(crate::chrome::Drag::Move) => {
                                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                                }
                                Some(crate::chrome::Drag::ToggleMaximize) => {
                                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(
                                        !maximized,
                                    ));
                                }
                                None => {}
                            }
                            match crate::chrome::buttons(
                                ui,
                                strip,
                                maximized,
                                &look,
                                self.caption_glyphs,
                            ) {
                                Some(crate::chrome::Hit::Minimize) => {
                                    ui.ctx()
                                        .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                                }
                                Some(crate::chrome::Hit::Maximize) => {
                                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(
                                        !maximized,
                                    ));
                                }
                                Some(crate::chrome::Hit::Close) => *close = true,
                                None => {}
                            }
                            // Where the maximise button is this frame, for the
                            // Win32 hit test that offers the snap layouts.
                            #[cfg(target_os = "windows")]
                            crate::chrome::windows::set_maximize_rect(
                                crate::chrome::maximize_rect(strip, crate::chrome::kind()),
                                self.scale(),
                            );
                        }
                    });
            }
            if status_visible {
                egui::Panel::bottom("dtr-status")
                    // Exactly the height the grid reserves. Left to itself the panel
                    // sizes to its content and its margins, comes out a couple of
                    // pixels taller than STATUS_HEIGHT, and covers the bottom row —
                    // which is where a shell puts its prompt.
                    //
                    // In *points*, because that is what egui sizes a panel in: it
                    // multiplies by `pixels_per_point` itself. Doing it here too
                    // made the bar `24 × scale²` pixels tall over a `24 × scale`
                    // reservation, so at 150 % it covered most of the bottom row
                    // (`docs/windows.md` §2.2).
                    .exact_size(crate::ui::STATUS_HEIGHT)
                    .frame(
                        egui::Frame::new()
                            .fill(look.skin().palette.card)
                            .inner_margin(egui::Margin::symmetric(8, 4)),
                    )
                    .show(ui, |ui| crate::ui::status_bar(ui, status, look));
            }
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(egui::Color32::TRANSPARENT))
                .show(ui, |_ui| {});
            // A frameless window's borders. The window manager's grip went with
            // the frame, so the strips are ours — added after the panels so the
            // few pixels at the edge belong to the resize and not to whatever
            // happens to be drawn underneath them.
            if self.client_frame {
                if let Some(direction) = crate::chrome::resize_edges(ui, ui.max_rect()) {
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::BeginResize(direction));
                }
            }
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
            // The composition box goes over the grid and under the
            // modals: it is part of typing, and a palette a composition
            // could cover would be a palette nobody could read.
            if let Some((anchor, preedit)) = ime {
                preedit.ui(ui, anchor, look);
            }
            // The palette is an overlay above the panes: a file listing
            // must not cover the command line it sits beside.
            if let Some(settings) = settings {
                if let Some(change) = settings.ui(ui, look) {
                    *settings_change = Some(change);
                }
            }
            // The keyboard map sits above the palette and the pager: it is the
            // thing a person opens precisely because they do not know what is
            // open behind it.
            if let Some(keymap) = keymap {
                crate::ui::overlay(ui, "dtr-keymap", |ui| {
                    if let Some(asked) = crate::keymap::ui(ui, look, keymap_bindings, keymap) {
                        *keymap_event = Some(asked);
                    }
                });
            }
            if palette_open {
                crate::ui::overlay(ui, "dtr-palette", |ui| {
                    if let Some(chosen) = palette.ui(ui, look) {
                        *action = Some(chosen);
                    }
                });
            }
            // Above the panes, in the same foreground layer as the palette: a
            // pager a file pane could cover would be a pager nobody could
            // read.
            if pager.is_open() {
                crate::ui::overlay(ui, "dtr-pager", |ui| pager.ui(ui, look));
            }
            // A preview of the image the pointer is on. Last, so it is over
            // the panes, and in the tooltip layer, so a file pane cannot hide
            // it.
            if let Some((texture, size)) = thumbnail {
                crate::thumb::draw(ui, texture, size, look);
            }
            // A right-click's menu. "Save as" on it opens the desktop's own
            // dialog, which is not drawn here at all.
            if link_menu.is_open() {
                *menu_choice = link_menu.ui(ui, look);
            }
            // The serial sheet is the topmost surface while it is up: it is a
            // question about the session, not a decoration on it.
            if let Some(serial_dialog) = serial_dialog {
                if let Some(asked) = serial_dialog.ui(ui, look) {
                    *serial_event = Some(asked);
                }
            }
            // Clipboard in and out. egui hands paste over as an event; the
            // pane hands copied text back through a slot it fills itself.
            let pasted: Vec<String> = ui.ctx().input(|input| {
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
                if !palette_open && serial_dialog.is_none() {
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
            tracing::info!(elapsed_ms = started.elapsed().as_millis(), "first frame");
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

        self.queue.submit(
            user_cmds
                .into_iter()
                .chain(std::iter::once(encoder.finish())),
        );
        if let Some(path) = capture {
            if let Err(err) = self.write_capture(&output, path) {
                tracing::warn!(%err, path = %path.display(), "could not capture the window");
            }
        }
        // Everything above is our work; `present` waits for the compositor and
        // would otherwise be logged as a slow frame at 120 Hz.
        self.cpu_ms = self.frame_began.elapsed().as_secs_f32() * 1000.0;
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
    use super::{closed, copy_to, cycled, mouse_goes_to_program, save_name, tab_name};

    /// A program's title names the tab, and silence does not.
    ///
    /// A shell that reports its directory, nvim that reports the file and tmux
    /// that reports the window all say something better than `3 shell`; a
    /// program that sets no title (or clears it) must not leave the tab blank.
    #[test]
    fn a_program_title_names_the_tab() {
        assert_eq!(tab_name("", "serial /dev/ttyUSB0"), "serial /dev/ttyUSB0");
        assert_eq!(tab_name("", "1 shell"), "1 shell");
        assert_eq!(tab_name("nvim src/lib.rs", "3 shell"), "nvim src/lib.rs");
    }

    /// A path the save dialog hands back is where the copy goes, and the folder
    /// it names does not have to exist yet: a native dialog lets a name be typed
    /// into a folder that was typed as well, and "saved" for a file that is not
    /// there is the worst of the three answers.
    #[tokio::test]
    async fn a_picked_path_is_where_the_copy_lands() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("holiday.png");
        std::fs::write(&source, b"pixels").expect("the file to copy");
        let vfs = dtr_vfs::Vfs::with_root(dir.path().join("cache"));
        let dest = dir.path().join("Pictures").join("2026").join("holiday.png");

        let bytes = copy_to(&vfs, &dtr_vfs::Location::local(&source), &dest)
            .await
            .expect("a copy");

        assert_eq!(bytes, 6);
        assert_eq!(std::fs::read(&dest).expect("the copy"), b"pixels");
        // A local file is its own path: the source is still there, and they are
        // two files rather than one.
        assert_eq!(std::fs::read(&source).expect("the source"), b"pixels");
    }

    /// A copy keeps the name it had, and a path with no name at all still gets
    /// one — the dialog is handed a file name, never an empty field.
    #[test]
    fn a_copy_keeps_its_name() {
        assert_eq!(
            save_name(std::path::Path::new("/srv/pictures/holiday.png")),
            "holiday.png"
        );
        assert_eq!(save_name(std::path::Path::new("/")), "download");
        assert_eq!(save_name(std::path::Path::new(".")), "download");
    }

    /// A program that grabbed the mouse still lets go of it for a selection.
    ///
    /// The alternative is a terminal where `set mouse=a` makes text impossible to
    /// copy, and there is no key that gets it back.
    #[test]
    fn shift_hands_the_mouse_back_to_the_terminal() {
        assert!(mouse_goes_to_program(true, false), "a captured mouse");
        assert!(
            !mouse_goes_to_program(true, true),
            "shift in a captured pane"
        );
        assert!(!mouse_goes_to_program(false, false), "nobody asked for it");
        assert!(!mouse_goes_to_program(false, true), "shift changes nothing");
    }

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
