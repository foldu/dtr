//! The application: the terminal's panes and tabs, the file browser, the
//! palette, and the draw pass that lays them out in egui.
//!
//! The *window* side — winit, the wgpu surface, the egui context, the render
//! passes and the event loop — is `dtr_frame` (`crates/dtr-frame`, documented in
//! `docs/frame.md`). This file keeps the application state and implements
//! `dtr_frame::App`: the frame hands over a `&egui::Context`, the screens here
//! draw into it, and every trap the two used to share (DPI, surface loss,
//! viewport commands, repaint carrying, drop order) lives in that crate instead.
//!
//! Deliberately not `eframe` (PLAN D6): we need winit events for the terminal
//! as well as for egui, and we want to redraw on bytes rather than on egui's
//! animation clock.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::Window;

use libghostty_vt::mouse::{Action as MouseAction, Button as MouseButton};

use crate::palette::{Action, Palette};
use crate::pane::{Frame, Pane as TermPane, Transport};
use crate::ui::Look;
use crate::ui::Status;
use dtr_files::{Browser, Layout as FileLayout, PaneView as FilePaneView, Sort as FileSort};

pub enum UserEvent {
    Frame,
    /// A background scan finished: the destinations to offer now.
    Destinations(Vec<crate::sources::Destination>),
    /// A host answered what its tmux has. The tab opens on the answer, so the
    /// question is asked before there is a session to put it in.
    Tmux {
        alias: String,
        listing: crate::tmux::Listing,
    },
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

/// How far either side of a divider still counts as a press on it, in points.
///
/// The gap is three points wide because it is a gap, not a handle; the pointer
/// has to be able to find it without pixel-hunting, and the cursor changing to
/// a resize arrow is what says it did.
const DIVIDER_SLOP: f32 = 2.0;

/// The smallest window a program may talk dtr down to, in physical pixels.
///
/// `CSI 8 ; 1 ; 1 t` is legal, and a window that obeys it literally is a window
/// with no room for its own caption — so the request is honoured down to here
/// and no further.
const MIN_WINDOW: (f32, f32) = (240.0, 160.0);

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
    /// Open the preferences card on start (also how it gets into screenshots).
    pub preferences: bool,
    /// Open the keyboard map on start, the same way.
    pub keymap: bool,
    /// Draw the debug overlay on start (`--stats`): fps, frame time, the present
    /// wait, glyphs, grid, scale and adapter. Off by default, because a counter
    /// over the terminal is a measurement and not decoration.
    pub stats: bool,
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
        // A capture has no clipboard of its own, and nothing to allow.
        std::sync::Arc::new(std::sync::Mutex::new(None)),
        false,
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
        .map(|path| file_browser(&startup.config, Some(path.clone())));

    let settings = startup.preferences.then(|| {
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

pub fn run(startup: Startup) -> anyhow::Result<()> {
    let frames = dtr_frame::FrameLoop::<UserEvent>::new()?;
    let proxy: EventLoopProxy<UserEvent> = frames.proxy();
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
    // Created before the first pane, because every pane shares it and the first
    // one is spawned here rather than through `App::open`.
    let clipboard_cache: std::sync::Arc<std::sync::Mutex<Option<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let pane = TermPane::spawn(
        startup.transport,
        startup.log.clone(),
        startup.theme.clone(),
        startup.scrollback,
        std::sync::Arc::clone(&frame_budget),
        std::sync::Arc::clone(&clipboard_cache),
        startup.config.terminal.clipboard_read,
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
        config_watch: crate::watch::ConfigWatch::new(&startup.config_path),
        config_settle: crate::watch::Settle::default(),
        config_deferred: false,
        // Looked at on `ConfigWatch::POLL`, not watched.
        config_due: std::time::Instant::now(),
        destinations: Vec::new(),
        // The background thread is started on first use (`App::bg`), so a
        // session that never scans or lists starts none.
        bg: None,
        scanning: false,
        scan_due: std::time::Instant::now(),
        listing_due: std::time::Instant::now() + LISTING_POLL,
        tabs: vec![Tab::new(
            1,
            Pane::Terminal(Session {
                pane,
                port,
                frame: None,
                label,
                program_title: String::new(),
                activity: crate::pane::Activity::default(),
                serial,
                remote,
                alias: startup_alias,
                counters,
                echo: false,
                meter: Meter::new(),
                bell_at: None,
                grid: (0, 0),
            }),
        )],
        active: 0,
        next_leaf: 2,
        gpu: None,
        window: None,
        egui_ctx: None,
        size: (0, 0),
        modifiers: ModifiersState::empty(),
        fatal: None,
        quit: false,
        wake: std::cell::Cell::new(false),
        strip_logged: None,
        folded_notices: std::collections::BTreeSet::new(),
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
        divider: None,
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
            if startup.preferences {
                settings.show(&startup.config);
            }
            settings
        },
        about_open: false,
        stats_overlay: startup.stats,
        frame_samples: crate::stats::FrameSamples::default(),
        stats_due: std::time::Instant::now(),
        key_delay: false,
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
        hover_cell: None,
        hover_link: None,
        thumb: crate::thumb::Cache::default(),
        thumb_pending: None,
        link_menu: crate::linkmenu::LinkMenu::default(),
        link_menu_target: None,
        tmux_pending: None,
        tmux_chooser: crate::tmux::Chooser::default(),
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
        notifier: crate::notify::system(startup.config.notifications.os),
        clipboard_cache,
        pending_resize: None,
    };
    // `--files` for screenshots and for starting next to a listing; the pane
    // gets focus, so the arrow keys work immediately.
    if let Some(path) = startup.files.clone() {
        app.split_files(Some(path));
    }
    // The desktop's chrome is live: the watcher re-reads it when the platform says
    // it changed — `WM_SETTINGCHANGE` on Windows, `SettingChanged` from the portal
    // on Linux — and asks for a repaint through the same door as every other
    // background result. It is the background runtime's first user, so the
    // runtime exists from the first frame even before anything has scanned.
    {
        let proxy = app.proxy.clone();
        app.bg_spawn(move |runtime| {
            crate::system::watch(runtime, move || {
                let _ = proxy.send_event(UserEvent::Frame);
            });
            async {}
        });
    }
    frames.run(app)
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
/// Everything blocking lives here, on a thread of its own with a runtime of its
/// own — the same shape as a pane, for the opposite reason: a transfer must
/// never share a thread with the emulator. compio's handles are thread-local,
/// so this is also the thread that must own the session.
fn sftp_worker(
    rx: std::sync::mpsc::Receiver<SftpMsg>,
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,
) {
    let runtime = match compio::runtime::Runtime::new() {
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
/// The file browser's download and the link menu's "Save as" share it, so a
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
            compio::fs::create_dir_all(parent).await?;
        }
    }
    // No completion-based copy: `std::fs::copy` is what the standard library
    // does best here, and a file copy blocks wherever it runs.
    let from = local.clone();
    let to = dest.to_owned();
    compio::runtime::spawn_blocking(move || std::fs::copy(&from, &to))
        .await
        .map_err(|err| anyhow::anyhow!("could not copy: {err}"))??;
    Ok(compio::fs::metadata(dest).await?.len())
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

/// How long a bell's flash lasts, and how strong it starts.
///
/// A bell is a flash in the window and nothing else: dtr never writes `\a` to
/// the pty, never plays a sound and never rings a system beep, because the only
/// terminal bell that is not turned off within a week is a visible one. Short
/// enough not to be in the way of reading, strong enough to notice at the edge
/// of vision.
const BELL_FLASH: std::time::Duration = std::time::Duration::from_millis(140);
const BELL_ALPHA: f32 = 0.12;

/// The bell's wash as it fades: `None` when nothing is ringing.
///
/// A fade rather than a blink, because a blink at a fixed alpha leaves the last
/// frame tinted if the window goes idle at the wrong moment.
fn bell_alpha(at: Option<std::time::Instant>) -> Option<f32> {
    let elapsed = at?.elapsed();
    if elapsed >= BELL_FLASH {
        return None;
    }
    let left = 1.0 - elapsed.as_secs_f32() / BELL_FLASH.as_secs_f32();
    Some(BELL_ALPHA * left.clamp(0.0, 1.0))
}

fn closed(active: usize, count: usize) -> Option<usize> {
    if count <= 1 {
        return None;
    }
    Some(active.min(count - 2))
}

/// A pane rectangle in egui's unit.
///
/// The layout, the grid and the pty are physical pixels; egui draws in points and
/// applies the scale itself, exactly once. This is the one conversion, asked by
/// every place that hands a pane rectangle to an egui surface — the IME's caret
/// box and the painted file panes.
pub(crate) fn to_points(rect: crate::layout::Rect, pixels_per_point: f32) -> egui::Rect {
    egui::Rect::from_min_size(
        egui::pos2(rect.x / pixels_per_point, rect.y / pixels_per_point),
        egui::vec2(
            rect.width / pixels_per_point,
            rect.height / pixels_per_point,
        ),
    )
}

/// Whether a wheel notch belongs to the terminal.
///
/// Three bits, and the notch has to pass all of them: no overlay has taken the
/// window, the pointer is over the focused pane, and no egui surface is on top
/// of it. The last two are separate questions — the `+` menu is a popup drawn
/// *over* the grid, and the grid's root layer is under both.
///
/// The wheel is not a key: egui reads it from the same event stream the pane
/// does, so a surface that scrolls itself moved *and* the scrollback behind it
/// moved too. The handler used to know two surfaces by name — the pager and the
/// serial sheet — and every other one scrolled two things at once.
fn wheel_reaches_the_terminal(overlay: bool, over_grid: bool, over_egui: bool) -> bool {
    !overlay && over_grid && !over_egui
}

/// The cell of the grid under a physical point, or `None` when it is not the
/// grid's.
///
/// A point outside the pane is no cell at all. Clamping it to the nearest one —
/// which is what the mapping did, on the argument that a drag leaving the grid
/// should still report — made the tab strip's row the grid's first row: a
/// pointer over the tabs asked the pane about a link there and raised the
/// pointing hand and an image preview of a file behind the tabs, and a press in
/// the strip anchored a selection in row 0. Reports to a *program* still clamp,
/// and that is `App::pane_pixels`; this is the one that answers "what is under
/// the pointer", which has a true answer only inside the grid.
///
/// `grid` is the frame's rows and columns, which can be smaller than the pane
/// while a resize settles, so the cell has to be checked against both.
fn cell_of(
    pane: crate::layout::Rect,
    cell: (f32, f32),
    grid: (u32, u32),
    at: (f64, f64),
) -> Option<(u32, u32)> {
    if cell.0 <= 0.0 || cell.1 <= 0.0 || !pane.contains(at.0 as f32, at.1 as f32) {
        return None;
    }
    let col = ((at.0 as f32 - pane.x) / cell.0).floor().max(0.0) as u32;
    let row = ((at.1 as f32 - pane.y) / cell.1).floor().max(0.0) as u32;
    if col >= grid.0 || row >= grid.1 {
        return None;
    }
    Some((col, row))
}

/// Translate the keys a file pane understands. Named arrows are
/// keyboard-layout independent; `h`, `r`, `g` and `G` are the vim-ish ones the
/// brief asks for. Modifiers are filtered by the caller.
/// The mode `[files] view` asks for.
///
/// The config crate parses text and the browser paints, so neither knows the
/// other's value and this is the one line that joins them — the same shape
/// `[ui] widgets` has with `dtr_widgets::Preset`.
fn file_layout(view: dtr_config::FilesView) -> FileLayout {
    match view {
        dtr_config::FilesView::Grid => FileLayout::Grid,
        dtr_config::FilesView::List => FileLayout::List,
    }
}

/// The config's spelling of a layout, for writing it back.
fn files_view_of(layout: FileLayout) -> dtr_config::FilesView {
    match layout {
        FileLayout::Grid => dtr_config::FilesView::Grid,
        FileLayout::List => dtr_config::FilesView::List,
    }
}

/// The view `[files]` asks for: the layout, the order, and what it hides.
fn file_view(config: &dtr_config::Config) -> dtr_files::View {
    dtr_files::View {
        layout: file_layout(config.files.view),
        show_hidden: config.files.hidden,
        sort: file_sort(config.files.sort),
        reverse: config.files.reverse,
        folders_first: config.files.folders_first,
    }
}

/// The config's spelling of a sort key, and back.
fn file_sort(sort: dtr_config::FilesSort) -> FileSort {
    match sort {
        dtr_config::FilesSort::Name => FileSort::Name,
        dtr_config::FilesSort::Modified => FileSort::Modified,
        dtr_config::FilesSort::Size => FileSort::Size,
        dtr_config::FilesSort::Kind => FileSort::Kind,
    }
}

fn files_sort(sort: FileSort) -> dtr_config::FilesSort {
    match sort {
        FileSort::Name => dtr_config::FilesSort::Name,
        FileSort::Modified => dtr_config::FilesSort::Modified,
        FileSort::Size => dtr_config::FilesSort::Size,
        FileSort::Kind => dtr_config::FilesSort::Kind,
    }
}

/// A browser carrying the configured view.
///
/// Every pane is built through one of these two functions, so a pane opened five
/// minutes after startup looks like the one opened at startup; a mode applied only
/// where the pane was first created would be a preference that depended on the
/// order of events.
fn file_browser(config: &dtr_config::Config, start: Option<PathBuf>) -> Browser {
    let mut browser = Browser::new(start);
    browser.set_view(file_view(config));
    browser
}

/// The same, for a pane over another machine.
fn remote_file_browser(config: &dtr_config::Config, alias: &str, path: PathBuf) -> Browser {
    let mut browser = Browser::remote_at(alias, path);
    browser.set_view(file_view(config));
    browser
}

fn browser_key(
    key: &winit::keyboard::Key,
    mods: winit::keyboard::ModifiersState,
) -> Option<dtr_files::Key> {
    use winit::keyboard::{Key, NamedKey};

    // One chord with a modifier, and it is the one every file manager has: the
    // parent directory is `Alt+Up`, because in a grid `Left` is a movement and
    // "up a level" needed somewhere to live.
    if mods.alt_key() || mods.control_key() || mods.super_key() {
        return match key {
            Key::Named(NamedKey::ArrowUp) if mods.alt_key() && !mods.control_key() => {
                Some(dtr_files::Key::Parent)
            }
            _ => None,
        };
    }

    match key {
        Key::Named(NamedKey::ArrowUp) => Some(dtr_files::Key::Up),
        Key::Named(NamedKey::ArrowDown) => Some(dtr_files::Key::Down),
        Key::Named(NamedKey::ArrowLeft) => Some(dtr_files::Key::Left),
        Key::Named(NamedKey::ArrowRight) => Some(dtr_files::Key::Right),
        Key::Named(NamedKey::PageUp) => Some(dtr_files::Key::PageUp),
        Key::Named(NamedKey::PageDown) => Some(dtr_files::Key::PageDown),
        Key::Named(NamedKey::Home) => Some(dtr_files::Key::Top),
        Key::Named(NamedKey::End) => Some(dtr_files::Key::Bottom),
        Key::Named(NamedKey::Enter) => Some(dtr_files::Key::Enter),
        // `Backspace` is the parent in both views; `h` is vim's left, which is
        // the parent in a list and a movement in a grid.
        Key::Named(NamedKey::Backspace) => Some(dtr_files::Key::Parent),
        Key::Character(text) => match text.as_str() {
            "h" => Some(dtr_files::Key::Left),
            "j" => Some(dtr_files::Key::Down),
            "k" => Some(dtr_files::Key::Up),
            "l" => Some(dtr_files::Key::Right),
            "r" => Some(dtr_files::Key::Reload),
            "g" => Some(dtr_files::Key::Top),
            "G" => Some(dtr_files::Key::Bottom),
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
    /// What the program says it is doing, for the tab chip's stripe. Polled
    /// rather than pushed: it is a state, not an event, and the last answer is
    /// the only one worth drawing.
    activity: crate::pane::Activity,
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
    /// When this session last rang its bell. The flash is a fade, so what is
    /// kept is the instant it started rather than a yes/no.
    bell_at: Option<std::time::Instant>,
}

/// What a leaf contains. The layout only knows leaf ids; this is the map entry
/// that says what the id *is*, which is why a widget pane is a new variant here
/// rather than a change to the split tree.
enum Pane {
    Terminal(Session),
    Files(Browser),
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

    fn browser(&self) -> Option<&Browser> {
        match self {
            Pane::Files(browser) => Some(browser),
            Pane::Terminal(_) => None,
        }
    }
}

/// One tab: a layout of panes, the panes themselves, and which one has focus.
///
/// The layout knows nothing about what a pane *is*; `leaves` is the map that
/// decides, which is how a file-browser pane becomes a new entry in this map
/// rather than a change to the geometry.
struct Tab {
    layout: crate::layout::Layout,
    leaves: std::collections::BTreeMap<crate::layout::LeafId, Pane>,
    focus: crate::layout::LeafId,
    /// The pane taking the whole tab, if one is. The layout is untouched — this
    /// is a view of it, not an edit — so unzooming puts every pane back exactly
    /// where it was, dividers and all.
    zoom: Option<crate::layout::LeafId>,
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
            zoom: None,
        }
    }

    fn focused_terminal(&self) -> Option<&Session> {
        self.leaves.get(&self.focus).and_then(Pane::terminal)
    }

    fn focused_terminal_mut(&mut self) -> Option<&mut Session> {
        let focus = self.focus;
        self.leaves.get_mut(&focus).and_then(Pane::terminal_mut)
    }

    fn focused_browser(&self) -> Option<&Browser> {
        self.leaves.get(&self.focus).and_then(Pane::browser)
    }

    fn focused_browser_mut(&mut self) -> Option<&mut Browser> {
        let focus = self.focus;
        match self.leaves.get_mut(&focus) {
            Some(Pane::Files(browser)) => Some(browser),
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
    /// The config file, stat-ed on a deadline rather than watched: a file an
    /// editor saves is one `stat`'s worth of work, and `notify` was a thread and
    /// two kernel watches for it (`docs/compio.md` §5).
    config_watch: crate::watch::ConfigWatch,
    /// The file is written in bursts, so a change only starts a clock.
    config_settle: crate::watch::Settle,
    /// A reload that arrived while the preferences card was open, waiting for it
    /// to close: the sheet owns the window's look until then, and re-reading
    /// the file under a half-finished draft would take the preview away.
    config_deferred: bool,
    /// When the config file is looked at next.
    config_due: std::time::Instant,
    /// The thread that does slow work for the window: enumerating serial ports
    /// reads udev or SetupAPI, a directory walk and a thumbnail decode are not
    /// for the drawing thread, and a closing tab's teardown waits on a child.
    /// Started on first use by `App::bg`; one thread with one runtime rather
    /// than three, so the idle count is one thread, not four
    /// (`docs/compio.md` §4).
    bg: Option<crate::worker::Worker>,
    /// A scan is in flight; asking again would only queue duplicate work.
    scanning: bool,
    /// When the next poll is due. Serial devices have no portable notification,
    /// so they are polled — off the UI thread.
    scan_due: std::time::Instant,
    /// When the open listings are next asked whether their directory changed.
    ///
    /// Polled, not watched: a directory's mtime is one `stat` of a path the pane
    /// already holds, twice a second, and `notify` in this tree would be two
    /// resources and a thread per pane for the same number (`docs/compio.md` §5).
    listing_due: std::time::Instant,
    active: usize,
    next_leaf: crate::layout::LeafId,
    /// dtr's half of the frame: the grid renderer and the chrome choices a
    /// frame is drawn with. The window, surface and egui context are the
    /// event loop's (`dtr_frame::Frame`).
    gpu: Option<Gpu>,
    /// The window this app draws into, cloned out of its `dtr_frame::Frame`.
    ///
    /// `repaint` is called from everywhere and the event loop owns the frame, so
    /// the app keeps the one handle that can ask for a redraw between frames
    /// without borrowing it.
    window: Option<Arc<Window>>,
    /// The window's egui context, for the few questions asked between frames:
    /// `pixels_per_point`, `layer_id_at`, and a texture upload for a preview.
    egui_ctx: Option<egui::Context>,
    /// The last configured surface size, in physical pixels: what the last
    /// drawn frame laid out against, and what a hit test between frames answers.
    /// At the top of a redraw the frame reconfigures a pending resize, and the
    /// app copies the new size here before it measures anything.
    size: (u32, u32),
    modifiers: ModifiersState,
    /// Set when the pane fails; `run` returns it once the loop has stopped.
    fatal: Option<String>,
    quit: bool,
    /// Set when a frame is wanted; `about_to_wait` turns it into a wake-up.
    wake: std::cell::Cell<bool>,
    /// The scale whose strip geometry was last logged.
    strip_logged: Option<f32>,
    /// Panes whose waiting notice has been folded away to its bar. Per pane, and
    /// only while that pane is waiting: the next drop starts loud again.
    folded_notices: std::collections::BTreeSet<crate::layout::LeafId>,
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
    /// The divider the pointer pressed and has not let go of. A drag is
    /// addressed by the split's path rather than by a borrow of the tree, so the
    /// layout can be rewritten between two frames without the drag losing it.
    divider: Option<crate::layout::Divider>,
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
    /// device, and the preferences menu does not.
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
    /// The host a tmux listing was asked of and has not answered yet. One at a
    /// time: a second click on the same host is the same question.
    tmux_pending: Option<String>,
    /// Which tmux session to open, once a host has said what it has.
    tmux_chooser: crate::tmux::Chooser,
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
    /// The preferences card, and the config it edits.
    settings: crate::settings::Settings,
    /// The about card: a page with no state, so a flag.
    about_open: bool,
    /// Whether the debug overlay is on (`--stats`, or the palette's `overlay`).
    /// The samples themselves are pushed after each frame from the
    /// `dtr_frame::FrameTiming` the frame returns, which is where the present
    /// wait is measured.
    stats_overlay: bool,
    /// The debug overlay's rolling frame and present durations. Empty and never
    /// written while the overlay is off — the `Instant` around `present` is not
    /// even read then.
    frame_samples: crate::stats::FrameSamples,
    /// When the overlay's readout is next due. While it is on, this is the only
    /// thing that asks for a frame — ten a second, not one per refresh: an
    /// instrument that forced every frame would measure itself. See
    /// [`crate::stats::REFRESH_INTERVAL`].
    stats_due: std::time::Instant,
    /// Whether serial keystrokes are spaced by `serial.key_delay_ms`.
    ///
    /// Off at startup on purpose: it is a property of the board on the other end
    /// of the cable, and a mode left on from yesterday makes every later
    /// keystroke feel sticky. Window-wide because the board is: every serial pane
    /// gets the same answer, and a shell pane ignores it.
    key_delay: bool,
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
    /// The clipboard as this window last saw it, shared with every pane.
    ///
    /// A program that asks (`OSC 52 ; c ; ?`) is answered on the pane thread
    /// inside a synchronous callback, and that thread cannot touch the OS
    /// clipboard — so the UI keeps this warm on the two occasions the clipboard
    /// can have changed under us: a copy of ours, and a paste of theirs.
    clipboard_cache: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    /// Where a program's notification goes: the desktop's channel when the
    /// machine has one, and the in-app toast when it does not.
    notifier: std::sync::Arc<dyn crate::notify::Notifier>,
    /// A `CSI 8 t` request the window has not been able to honour yet, because
    /// the first frame had not measured a cell when it arrived.
    pending_resize: Option<(Option<u16>, Option<u16>)>,
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

/// How often an open listing is asked whether its directory changed.
///
/// Twice a second is the resolution a person notices: a file created by a build
/// or a `git checkout` appears in half a second, and the cost is a `stat` per
/// open pane — the same bargain the config file's 250 ms poll already makes, at
/// half the rate because a directory changes less often than a file being
/// edited.
const LISTING_POLL: std::time::Duration = std::time::Duration::from_millis(500);

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
            .window
            .as_ref()
            .and_then(|window| window.current_monitor())
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

    /// Whether an overlay is up, and so owns the window.
    ///
    /// One list, because a surface that has taken the keys has taken the pointer
    /// with them: a link hovered behind a card lit a pointing hand and raised a
    /// preview *above* the card — the tooltip layer is above the foreground one —
    /// of a cell the person could not see. A new modal belongs here.
    fn overlay_open(&self) -> bool {
        self.palette.open
            || self.settings.open
            || self.keymap_card.open
            || self.about_open
            || self.pager.is_open()
            || self.link_menu.is_open()
            || self.file_menu_open()
            || self.tmux_chooser.is_open()
            || self.serial_dialog.is_some()
    }

    /// Whether keys belong to the terminal rather than to an overlay.
    ///
    /// The same surfaces `on_key` hands the keyboard to: a composition that
    /// committed into the shell behind an open palette would type somewhere the
    /// person is not looking.
    fn terminal_owns_keys(&self) -> bool {
        !self.overlay_open()
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

        // The about card is a page: Escape is its only key, and everything else
        // is swallowed so a keystroke cannot reach the shell behind it.
        if self.about_open {
            if !pressed {
                self.held = None;
                return;
            }
            if matches!(
                &stroke.key,
                winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
            ) {
                self.about_open = false;
                self.repaint("about closed");
            }
            self.held = Some(stroke.key.clone());
            return;
        }

        // The menu owns the keyboard while it is open. Releases only
        // clear the held key; duplicates are compositor noise.
        // The preferences card is mouse only: it has no keys of its own, and a
        // keystroke must not reach the shell behind it either. Save, cancel and
        // the cross are the way out, and they are all buttons.
        if self.settings.open {
            self.held = None;
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

        // The pane's right-press menu owns the keyboard for as long and in the
        // same way: any key closes it and then goes where it was going, except
        // Escape, which was aimed at the menu.
        if self.file_menu_open() && pressed {
            let escape = matches!(
                &stroke.key,
                winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
            );
            self.close_file_menu();
            self.repaint("file menu closed");
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
                if !self.palette.open {
                    // A picker's keys are its footer's: `Enter` accepts, `Escape`
                    // cancels, and the rest belong to the text field egui is
                    // already feeding — a listing's movement keys must not move a
                    // grid behind a question.
                    if self
                        .focused_browser()
                        .is_some_and(|browser| browser.is_picking())
                    {
                        if pressed {
                            self.pick_key(&stroke.key);
                        }
                        return;
                    }
                    if let Some(key) = browser_key(&stroke.key, stroke.mods) {
                        let chosen = self
                            .focused_browser_mut()
                            .and_then(|browser| browser.handle(key));
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
        let screen = self.screen();
        let rect = self
            .pane_views(tab, gpu.metrics(), screen, self.scale())
            .into_iter()
            .find(|(id, _)| *id == tab.focus)
            .map(|(_, pane)| pane.rect)?;
        Some((
            (x as f32 - rect.x).clamp(0.0, (rect.width - 1.0).max(0.0)),
            (y as f32 - rect.y).clamp(0.0, (rect.height - 1.0).max(0.0)),
        ))
    }

    /// Fold the notice over the focused pane away, if it is up.
    ///
    /// A wheel notch does this as well as a press: a person scrolling at a
    /// terminal that has stopped is asking for the scrollback, and a notice that
    /// answered "nothing" would be the reason they cannot read the command they
    /// were running when it went away.
    fn fold_notice(&mut self) -> bool {
        let id = self.tabs[self.active].focus;
        let waiting = self.tabs[self.active]
            .leaves
            .get(&id)
            .and_then(Pane::terminal)
            .is_some_and(|session| session.pane.stats().retry().is_some());
        if !waiting || self.folded_notices.contains(&id) {
            return false;
        }
        self.folded_notices.insert(id);
        self.repaint("a waiting notice folded away");
        true
    }

    /// Whether the pointer is over an egui surface — a file pane, a card, a menu,
    /// a scroll bar — rather than the grid.
    ///
    /// `layer_id_at` answers for the areas egui drew above the root layer, which
    /// is what a card, a popup and a file pane are. The tab strip and the status
    /// bar are painted into the root layer, not areas of their own, so they are
    /// the cell mapping's business: `cell_at` refuses a point outside the pane.
    fn over_egui(&self, at: (f64, f64)) -> bool {
        let Some(ctx) = self.egui_ctx.as_ref() else {
            return false;
        };
        let scale = self.scale();
        let points = egui::pos2(at.0 as f32 / scale, at.1 as f32 / scale);
        ctx.layer_id_at(points)
            .is_some_and(dtr_widgets::above_the_root)
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
        // An overlay has the window for the same reason, and an egui surface the
        // pointer is over is not the grid: no cell there is being pointed at.
        if self.mouse_captured() || self.overlay_open() || self.over_egui(at) {
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

        let vfs = std::sync::Arc::clone(&self.vfs);
        let proxy = self.proxy.clone();
        // A remote file cannot be handed to a viewer as a path, so it is
        // fetched first; a local one is its own path and is never copied.
        let remote = !matches!(location.backend, dtr_vfs::BackendId::Local);
        // A directory is shown in a file pane, and a remote one in a pane over
        // the machine it came from — that pane is the only thing that can list it.
        let reveal_alias = match &location.backend {
            dtr_vfs::BackendId::Ssh(alias) => Some(alias.clone()),
            dtr_vfs::BackendId::Local => None,
        };
        self.bg_spawn(move |_runtime| async move {
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
                compio::runtime::spawn_blocking(move || dtr_vfs::open_externally(&path))
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

    /// Whether a file pane's right-press menu is up.
    ///
    /// A menu is an overlay like the link menu's: it owns the keyboard while it
    /// is open, keeps the preview off the pane behind it, and is dismissed by a
    /// press that is not inside it.
    fn file_menu_open(&self) -> bool {
        self.focused_browser()
            .is_some_and(|browser| browser.menu_open())
    }

    fn close_file_menu(&mut self) {
        if let Some(browser) = self.focused_browser_mut() {
            browser.close_menu();
        }
    }

    /// The DPI factor, which the window holds and the GPU knows how to ask for.
    ///
    /// egui works in points and winit's pointer arrives in physical pixels, so
    /// every hand-off between them needs this — once, not twice.
    fn scale(&self) -> f32 {
        self.window
            .as_ref()
            .map(|window| window.scale_factor() as f32)
            .unwrap_or(1.0)
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
        // A right-press on a listing is the pane's own menu: it selects the row
        // under it and opens the two verbs the pane has. A press on nothing in a
        // file pane is still consumed — a menu that did not open is not a reason
        // to hand the press to whatever is behind.
        if let Some(point) = self.file_pane_point(at) {
            let opened = self
                .focused_browser_mut()
                .is_some_and(|browser| browser.menu_at(point));
            if opened {
                self.repaint("file menu");
            }
            return;
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
        // The other menu on a pane, dismissed the same way: a press outside it
        // closes it and goes nowhere else. A press *inside* it is the menu's own
        // and is left to egui, which is one frame behind this event.
        if self.file_menu_open() {
            let inside = self
                .focused_browser()
                .is_some_and(|browser| browser.menu_contains(points));
            if !inside {
                self.close_file_menu();
                self.repaint("file menu dismissed");
            }
            return;
        }
        // What egui drew, egui gets. A card's button, a file pane's row and the
        // notice over a pane that is waiting for its connection are all surfaces
        // in a layer above the root; a press that fell through them would start a
        // selection in the grid behind — for the waiting pane, a highlight drawn
        // over the very screen the notice dimmed.
        if self.over_egui(at) {
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
        let default = save_target(&location.path);
        let parent = self.proxy.clone();
        let window = self.window.clone();
        self.picking_save = true;
        self.bg_blocking(move || {
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
        let vfs = std::sync::Arc::clone(&self.vfs);
        let proxy = self.proxy.clone();
        // Said before rather than after: a file over a slow link is a silence,
        // and a silence nobody explained reads as a button that did nothing.
        self.toast = Some((format!("saving {name}..."), std::time::Instant::now()));
        self.bg_spawn(move |_runtime| async move {
            let outcome = copy_to(&vfs, &location, &dest).await;
            let note = match outcome {
                Ok(bytes) => format!(
                    "saved {} to {}",
                    dtr_files::human_size(bytes),
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
                    Some(Pane::Files(browser)) if browser.alias() == alias.as_deref()
                )
            })
        };
        let browser = match &alias {
            Some(alias) => remote_file_browser(&self.config, alias, path),
            None => file_browser(&self.config, Some(path)),
        };
        if let Some(id) = existing {
            self.tabs[self.active]
                .leaves
                .insert(id, Pane::Files(browser));
            return;
        }
        self.unzoom();
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
        tab.leaves.insert(id, Pane::Files(browser));
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
        let vfs = std::sync::Arc::clone(&self.vfs);
        let proxy = self.proxy.clone();
        // Claimed only once there is something to claim it with: a request that
        // died between the claim and the spawn would leave the key in flight for
        // the life of the process.
        if !self.thumb.begin(&key) {
            return;
        }
        let described = location.describe();
        self.bg_spawn(move |_runtime| async move {
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
                compio::runtime::spawn_blocking(move || crate::thumb::load(&local))
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
    fn hover_thumbnail(&mut self, scale: f32) -> Option<(egui::TextureHandle, egui::Vec2)> {
        let ctx = self.egui_ctx.clone();
        let ctx = ctx.as_ref()?;
        // An overlay owns the window while it is open; a tooltip over it would be
        // a picture of a file the user is not pointing at. The link menu is a
        // sharper case: it opens *at* the pointer, exactly where the preview would
        // be, and the tooltip layer sits above a foreground menu. The pointer over
        // an egui surface — a menu, a file pane, a scroll bar — is the same answer
        // for the same reason: the preview would follow it there. The window's own
        // layer is under the pointer everywhere, so only a layer above it counts.
        if self.overlay_open() {
            return None;
        }
        if let Some(pointer) = ctx.pointer_latest_pos()
            && ctx
                .layer_id_at(pointer)
                .is_some_and(dtr_widgets::above_the_root)
        {
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

    /// Put text on the system clipboard, by hand.
    ///
    /// egui's own path goes through `egui-winit`, which falls back to an
    /// in-app buffer when it cannot reach the display — the toast then says
    /// "copied" while the clipboard is untouched. Writing it here as well
    /// makes the copy real, and a failure is logged rather than assumed.
    fn set_clipboard(&mut self, text: &str) {
        self.remember_clipboard(text);
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

    /// Keep the panes' copy of the clipboard in step with the system's.
    ///
    /// It is written here, on the two occasions dtr itself touches the
    /// clipboard, rather than read on demand: a program's `OSC 52` read is
    /// answered synchronously on a pane thread, which cannot reach the OS
    /// clipboard at all.
    fn remember_clipboard(&self, text: &str) {
        if let Ok(mut slot) = self.clipboard_cache.lock() {
            *slot = Some(text.to_owned());
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
        let text = match self.clipboard.as_mut()?.get_text() {
            Ok(text) => {
                tracing::debug!(chars = text.chars().count(), "clipboard read");
                Some(text)
            }
            Err(err) => {
                tracing::warn!(%err, "could not read the clipboard");
                None
            }
        };
        if let Some(text) = text.as_deref() {
            self.remember_clipboard(text);
        }
        text
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

    /// The pointer went down at `at`. A press focuses the pane under it and
    /// starts a selection anchor; the drag turns the anchor into a range.
    ///
    /// A press on a divider is neither: it grabs the border, and the drag moves
    /// it. The program never hears about it — the gap is the window's, not a
    /// cell — which is also why it cannot be a click on the pane beside it.
    fn pointer_down(&mut self, at: (f64, f64)) {
        // A press always ends whatever drag was running, even one whose release
        // never arrived — the pointer left the window, or a compositor ate the
        // event — because a divider that follows the pointer afterwards is a
        // layout nobody can stop editing.
        self.divider = None;
        if let Some(divider) = self.divider_at_point(at) {
            self.dragging = false;
            self.drag_moved = false;
            // The hand and the preview belong to a cell, and the gap is not one.
            self.hover_cell = None;
            self.hover_link = None;
            self.thumb_pending = None;
            // A second press in the same place puts the divider back in the
            // middle: a layout that was dragged into a corner is otherwise only
            // recoverable by pixel-hunting it back.
            if self.count_clicks(at) > 1 {
                self.tabs[self.active].layout.set_ratio(&divider.path, 0.5);
            }
            self.divider = Some(divider);
            self.repaint("divider pressed");
            return;
        }
        if let Some(id) = self.pane_at(at.0, at.1) {
            self.tabs[self.active].focus = id;
        }
        // A press in a file pane is the listing's: it selects what is under it,
        // and the second press of the same entry opens it. The pane is painted
        // rather than made of widgets, so this is the app's hit test — the same
        // place the divider's lives, and for the same reason.
        if self.press_in_file_pane(at) {
            return;
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

    /// Where a press lands on a file pane, in window points.
    ///
    /// `None` for a terminal, for the gap between panes, and for the window's
    /// chrome: a press that is not on a listing's body belongs to somebody else.
    fn file_pane_point(&self, at: (f64, f64)) -> Option<egui::Pos2> {
        let gpu = self.gpu.as_ref()?;
        let tab = &self.tabs[self.active];
        let screen = self.screen();
        let scale = self.scale();
        let (id, _) = self
            .pane_views(tab, gpu.metrics(), screen, scale)
            .into_iter()
            .find(|(_, view)| view.rect.contains(at.0 as f32, at.1 as f32))?;
        if !matches!(tab.leaves.get(&id), Some(Pane::Files(_))) {
            return None;
        }
        // The window's point, not the pane's: a pane is handed an `egui::Rect` in
        // absolute points and records its layout in that same space, so a local
        // point would be compared against rectangles measured from a different
        // origin — the bug that made the first click select nothing.
        let points = self.pixels_per_point();
        Some(egui::pos2(at.0 as f32 / points, at.1 as f32 / points))
    }

    /// Hand a press to the listing under it, if there is one.
    ///
    /// Returns whether it was a file pane's, because a press that returns here
    /// must not also start a selection in the terminal behind it.
    fn press_in_file_pane(&mut self, at: (f64, f64)) -> bool {
        let Some(point) = self.file_pane_point(at) else {
            return false;
        };
        // The breadcrumb first: the header sits above the listing, and a press on
        // a crumb is a navigation rather than a selection.
        if let Some(path) = self
            .focused_browser()
            .and_then(|browser| browser.crumb_at(point))
        {
            if let Some(browser) = self.focused_browser_mut() {
                browser.navigate(path);
            }
            self.dragging = false;
            self.drag_moved = false;
            self.repaint("file pane breadcrumb");
            return true;
        }
        let clicks = self.count_clicks(at);
        // Read the modifiers first: the browser is borrowed mutably for the call,
        // and `self.modifiers` is not part of it.
        let additive = self.modifiers.control_key();
        let extend = self.modifiers.shift_key();
        let chosen = self
            .focused_browser_mut()
            .and_then(|browser| browser.clicked(point, clicks, additive, extend));
        if let Some(path) = chosen {
            self.insert_path_into_terminal(path);
        }
        self.dragging = false;
        self.drag_moved = false;
        self.repaint("file pane press");
        true
    }

    /// Scroll the listing under the pointer, if that is what the pointer is on.
    ///
    /// The pointer's pane, not the focused one: a wheel is aimed by pointing, and
    /// scrolling a listing nobody is pointing at is a surprise.
    fn scroll_file_pane(&mut self, lines: f64) -> bool {
        let Some((x, y)) = self.cursor_position else {
            return false;
        };
        let Some(id) = self.pane_at(x, y) else {
            return false;
        };
        let Some(Pane::Files(browser)) = self.tabs[self.active].leaves.get_mut(&id) else {
            return false;
        };
        browser.scroll(lines);
        self.repaint("file pane scrolled");
        true
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
        // A divider drag comes before everything: the press that started it was
        // not in a pane, so no program is following it and no selection began.
        if let Some(divider) = self.divider.take() {
            self.drag_divider(&divider, at);
            self.divider = Some(divider);
            return;
        }
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
        // Letting go of a divider ends the drag and copies nothing: there is no
        // selection, because the press never landed on a cell.
        if self.divider.take().is_some() {
            self.dragging = false;
            self.drag_moved = false;
            self.drag_multi = false;
            return;
        }
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

    /// Run `future` on the background thread and let it finish on its own.
    ///
    /// `make` builds the future *there*, which is where compio lets one be
    /// built; a bare `spawn` would not do, because compio cancels a task when
    /// its `JoinHandle` is dropped.
    fn bg_spawn<F, Fut>(&mut self, make: F)
    where
        F: FnOnce(&compio::runtime::Runtime) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + 'static,
    {
        if let Some(bg) = self.bg() {
            bg.spawn_with(make);
        }
    }

    /// The same for a blocking task; the answer arrives as a `UserEvent`.
    fn bg_blocking(&mut self, work: impl FnOnce() + Send + 'static) {
        if let Some(bg) = self.bg() {
            bg.blocking(work);
        }
    }

    /// The background thread, started the first time something asks for one.
    ///
    /// One thread, not one per kind of work: scans, file listings, thumbnails
    /// and a closing tab's teardown all answer on the UI thread with an event,
    /// and a long call is a blocking task of the runtime it runs on. The census
    /// in `docs/compio.md` §4 counted four threads for two such runtimes; this
    /// is one, and it is built where it is first needed rather than at startup
    /// for the sake of it.
    fn bg(&mut self) -> Option<&crate::worker::Worker> {
        if self.bg.is_none() {
            self.bg = match crate::worker::Worker::spawn("dtr-bg") {
                Ok(worker) => Some(worker),
                Err(err) => {
                    tracing::warn!(%err, "no background thread");
                    None
                }
            };
        }
        self.bg.as_ref()
    }

    /// Ask the background runtime for a fresh destination list.
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
        self.scanning = true;
        let proxy = self.proxy.clone();
        // The store is read on the worker, not here: a redb read is quick but it
        // is still I/O, and the UI thread only ever asks for the list.
        let store = self.store.clone();
        self.bg_blocking(move || {
            let uses = store
                .as_deref()
                .and_then(|store| match store.ssh_uses() {
                    Ok(uses) => Some(uses),
                    Err(err) => {
                        tracing::warn!(%err, "could not read the ssh hosts that were used");
                        None
                    }
                })
                .unwrap_or_default();
            let mut destinations = crate::sources::shell_destinations();
            destinations.extend(crate::sources::serial_destinations());
            destinations.extend(crate::sources::ssh_destinations(&uses));
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

    /// Select every row in the focused listing.
    fn select_all_files(&mut self) -> bool {
        let Some(browser) = self.focused_browser_mut() else {
            self.toast = Some((
                "no file pane is focused".to_owned(),
                std::time::Instant::now(),
            ));
            self.repaint("files select all");
            return true;
        };
        browser.select_all();
        self.repaint("files select all");
        true
    }

    /// Put the focused listing's selected path on the clipboard.
    ///
    /// The one thing a context menu would offer that nothing else does: `open` is
    /// `Enter`, `reveal` is opening a pane at the path, and the path itself is
    /// what a terminal wants on its command line.
    fn copy_file_path(&mut self) -> bool {
        let Some(path) = self
            .focused_browser()
            .and_then(|browser| browser.selected_path())
        else {
            self.toast = Some(("nothing is selected".to_owned(), std::time::Instant::now()));
            self.repaint("files copy path");
            return true;
        };
        let text = path.display().to_string();
        self.set_clipboard(&text);
        self.toast = Some((format!("copied {text}"), std::time::Instant::now()));
        self.repaint("files copy path");
        true
    }

    /// Apply what a file pane's right-press menu chose.
    ///
    /// The pane's id and not "the focused one": the menu belongs to the pane that
    /// was right-pressed, and although opening it moves the focus there, saying
    /// which pane it was is what keeps the two from having to agree.
    ///
    /// Both rows are the verbs the pane already has — `Enter` and `ctrl-alt-c` —
    /// so this is a second way to the same call and not a second implementation
    /// of it.
    fn file_menu_choice(&mut self, id: crate::layout::LeafId, choice: dtr_files::MenuChoice) {
        match choice {
            dtr_files::MenuChoice::Open => {
                let chosen = match self.tabs[self.active].leaves.get_mut(&id) {
                    Some(Pane::Files(browser)) => browser.handle(dtr_files::Key::Enter),
                    _ => None,
                };
                if let Some(path) = chosen {
                    self.insert_path_into_terminal(path);
                }
                self.repaint("file menu open");
            }
            // The clipboard and the toast are the chord's own path, so a copy
            // from the menu and a copy from `ctrl-alt-c` cannot drift apart.
            dtr_files::MenuChoice::CopyPath => {
                self.copy_file_path();
            }
        }
    }

    /// Apply what a pane's own sort control asked for, and remember it.
    ///
    /// The pane that was clicked, not the focused one: a sort control is aimed
    /// by pointing, and a press on it does not move the keyboard focus (the
    /// pane's body does, and this is the header).
    fn apply_sort_change(&mut self, id: crate::layout::LeafId, change: dtr_files::SortChange) {
        let view = {
            let Some(Pane::Files(browser)) = self.tabs[self.active].leaves.get_mut(&id) else {
                return;
            };
            let mut view = browser.view();
            match change {
                dtr_files::SortChange::Key(sort) => view.sort = sort,
                dtr_files::SortChange::Reverse => view.reverse = !view.reverse,
                dtr_files::SortChange::FoldersFirst => view.folders_first = !view.folders_first,
            }
            browser.set_view(view);
            view
        };
        self.persist_view(view);
    }

    /// Remember an order in the config file, which is the other writer of it.
    ///
    /// Three keys, written only when they moved: a sort change that rewrote the
    /// file every time would reset the order of every other pane through the
    /// watcher's reload.
    fn persist_view(&mut self, view: dtr_files::View) {
        // Collected first, then written: holding the config while asking `self`
        // for the path it is written to is a borrow the compiler is right to
        // refuse.
        let mut writes: Vec<(&'static str, &'static str)> = Vec::new();
        {
            let files = &mut self.config.files;
            if files.sort != files_sort(view.sort) {
                files.sort = files_sort(view.sort);
                writes.push(("files.sort", view.sort.label()));
            }
            if files.reverse != view.reverse {
                files.reverse = view.reverse;
                writes.push(("files.reverse", if view.reverse { "true" } else { "false" }));
            }
            if files.folders_first != view.folders_first {
                files.folders_first = view.folders_first;
                writes.push((
                    "files.folders-first",
                    if view.folders_first { "true" } else { "false" },
                ));
            }
        }
        for (key, value) in writes {
            self.write_config(key, value);
        }
    }

    /// Change what the focused listing is ordered by, and remember it.
    ///
    /// The three config keys behind one call, because they are one decision: a
    /// sort change that forgot its direction, or a direction that applied to a
    /// different key, is how a `[files]` table ends up describing a listing
    /// nobody asked for.
    fn change_file_view(&mut self, change: impl FnOnce(&mut dtr_files::View)) -> bool {
        let Some(before) = self.focused_browser().map(|browser| browser.view()) else {
            self.toast = Some((
                "no file pane is focused".to_owned(),
                std::time::Instant::now(),
            ));
            self.repaint("files sort");
            return true;
        };
        let mut view = before;
        change(&mut view);
        if let Some(browser) = self.focused_browser_mut() {
            browser.set_view(view);
        }
        self.persist_view(view);
        let _ = before;
        self.repaint("files sorted");
        true
    }

    /// One key of the config file, written the way every other writer here does.
    fn write_config(&self, key: &str, value: &str) {
        if let Err(err) = dtr_config::set_value(&self.config_path, key, value) {
            tracing::warn!(%err, key, "could not save a file-pane setting");
        }
    }

    /// Show or hide dot-files in every file pane, and remember the answer.
    ///
    /// Every pane, not the focused one: a desktop has one "show hidden files"
    /// setting, and two panes that disagreed about it would look broken. The
    /// listing already carries the entries — a hidden file is filtered by the
    /// sort, not left out of the walk — so this is a redraw and not a reload.
    fn toggle_hidden_files(&mut self) -> bool {
        let hidden = !self.config.files.hidden;
        self.config.files.hidden = hidden;
        for tab in &mut self.tabs {
            for pane in tab.leaves.values_mut() {
                if let Pane::Files(browser) = pane {
                    browser.set_hidden(hidden);
                }
            }
        }
        let written = if hidden { "true" } else { "false" };
        if let Err(err) = dtr_config::set_value(&self.config_path, "files.hidden", written) {
            tracing::warn!(%err, "could not save files.hidden");
        }
        self.repaint("files hidden");
        true
    }

    /// Switch the focused file pane's view, and remember it.
    ///
    /// Remembered by writing the config file, because that is what "the choice
    /// is remembered" means in `docs/manager.md` — a value that lived only in
    /// the window would be forgotten at exit, and the file that called it a
    /// preference would not have one in it. The watcher sees the write and
    /// reloads, which is how the other panes learn the same thing.
    fn set_file_layout(&mut self, layout: FileLayout) -> bool {
        if self.focused_browser().is_none() {
            self.toast = Some((
                "no file pane is focused".to_owned(),
                std::time::Instant::now(),
            ));
            self.repaint("files view");
            return true;
        }
        if let Some(browser) = self.focused_browser_mut() {
            browser.set_layout(layout);
        }
        let view = files_view_of(layout);
        if let Err(err) = dtr_config::set_value(&self.config_path, "files.view", view.as_str()) {
            tracing::warn!(%err, "could not save files.view");
        } else {
            self.config.files.view = view;
        }
        self.repaint("files view");
        true
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
            Action::Preferences => {
                // Opening only: the chord is a way in, not a way out. The card
                // takes no keys once it is up, so this cannot run while it is
                // open — save, cancel and the cross are the ways out.
                let config = self.config.clone();
                // The focused session's port, and what the store remembers for
                // it: baud and CR translation are facts about that device, not
                // about the terminal.
                let port = self.session().and_then(|session| session.port.clone());
                let record = port.as_ref().and_then(|port| {
                    self.store
                        .as_ref()
                        .and_then(|store| store.serial(port).ok().flatten())
                });
                self.settings.show_port(&config, port, record);
                self.repaint("preferences");
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
            Action::FocusLeft => {
                self.focus_direction(crate::layout::Direction::Left);
                true
            }
            Action::FocusRight => {
                self.focus_direction(crate::layout::Direction::Right);
                true
            }
            Action::FocusUp => {
                self.focus_direction(crate::layout::Direction::Up);
                true
            }
            Action::FocusDown => {
                self.focus_direction(crate::layout::Direction::Down);
                true
            }
            Action::ResizeLeft => {
                self.resize_pane(crate::layout::Direction::Left);
                true
            }
            Action::ResizeRight => {
                self.resize_pane(crate::layout::Direction::Right);
                true
            }
            Action::ResizeUp => {
                self.resize_pane(crate::layout::Direction::Up);
                true
            }
            Action::ResizeDown => {
                self.resize_pane(crate::layout::Direction::Down);
                true
            }
            Action::FilesGrid => self.set_file_layout(FileLayout::Grid),
            Action::FilesList => self.set_file_layout(FileLayout::List),
            Action::FilesHidden => self.toggle_hidden_files(),
            Action::FilesCopyPath => self.copy_file_path(),
            Action::FilesSelectAll => self.select_all_files(),
            Action::FilesSortName => self.change_file_view(|view| view.sort = FileSort::Name),
            Action::FilesSortModified => {
                self.change_file_view(|view| view.sort = FileSort::Modified)
            }
            Action::FilesSortSize => self.change_file_view(|view| view.sort = FileSort::Size),
            Action::FilesSortKind => self.change_file_view(|view| view.sort = FileSort::Kind),
            Action::FilesReverse => self.change_file_view(|view| view.reverse = !view.reverse),
            Action::FilesFoldersFirst => {
                self.change_file_view(|view| view.folders_first = !view.folders_first)
            }
            Action::FilesPick => {
                self.start_pick();
                true
            }
            Action::ZoomPane => {
                self.toggle_zoom();
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
            // A window-wide switch, not a per-pane one: it is the board on the
            // cable that needs it, and a person who turned it on for the board
            // in front of them means every serial tab. It is also *only* a
            // serial-line mode: a shell, a file pane or the demo screen has no
            // cable to pace and no strip to show the red chip on, so the switch
            // refuses there instead of flipping a mode the window cannot show.
            // The refusal is a toast rather than silence, because a chord that
            // does nothing is indistinguishable from a broken one.
            Action::KeyDelay => {
                if !self.session().is_some_and(|session| session.serial) {
                    tracing::debug!("key delay refused: the focused pane is not serial");
                    self.toast = Some((
                        "delayed input is for serial sessions".to_owned(),
                        std::time::Instant::now(),
                    ));
                    self.repaint("key delay refused");
                    return true;
                }
                self.key_delay = !self.key_delay;
                let delay = self.key_delay_period();
                tracing::debug!(
                    on = self.key_delay,
                    ms = self.config.serial.key_delay_ms,
                    "key delay"
                );
                for tab in &self.tabs {
                    for pane in tab.leaves.values() {
                        if let Pane::Terminal(session) = pane
                            && session.serial
                        {
                            session.pane.send(crate::pane::Command::KeyDelay(delay));
                        }
                    }
                }
                self.repaint("key delay");
                true
            }
        }
    }

    /// Write the menu's draft to the config file and make it live.
    ///
    /// The draft is already on the window — every edit previews — so this writes
    /// it, keeps it, and lets the serial rows through, which is the half that
    /// cannot be a preview: a baud rate cannot be applied to an open port in
    /// place, so the pane reconnects, keeps its scrollback, and comes back
    /// talking at the new rate.
    fn apply_settings(&mut self) {
        let draft = self.settings.draft().clone();
        for (key, value) in self.settings.writes() {
            if let Err(err) = dtr_config::set_value(&self.config_path, &key, &value) {
                tracing::warn!(%key, %err, "could not save a setting");
            }
        }

        let serial_changed = draft.serial != self.config.serial;

        // The theme was resolved when the row moved, not now: a palette command
        // switches the theme without touching the file, and a save must not
        // quietly undo it.
        self.adopt_config(draft);
        let config = self.config.clone();
        self.show_look(&config, false);

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
        // A save is the later word: a file change that arrived while the sheet
        // was open was overwritten by this one, so there is nothing left to
        // reload.
        self.config_deferred = false;
        self.repaint("preferences applied");
    }

    /// Take `config` as the truth: it is the file's now, and the keys follow it.
    ///
    /// The look is the caller's, because the two callers differ on one point
    /// that matters — a save must not resolve `theme` again (a palette command
    /// switched it without touching the file), and a reload must, because the
    /// file just said which theme it wants.
    fn adopt_config(&mut self, config: dtr_config::Config) {
        // The file is live, and this is a key it can change: rebuilding is a
        // thread and a bus connection, so it happens only when the answer
        // actually differs.
        if self.config.notifications.os != config.notifications.os {
            self.notifier = crate::notify::system(config.notifications.os);
        }
        self.config = config;
        // The view is a thing a person can see, so a reload has to reach the
        // panes that are already on screen: a `files.view` that only applied to
        // the next pane would look like a setting that did nothing.
        let view = file_view(&self.config);
        for tab in &mut self.tabs {
            for pane in tab.leaves.values_mut() {
                if let Pane::Files(browser) = pane {
                    browser.set_layout(view.layout);
                    browser.set_hidden(view.show_hidden);
                }
            }
        }
        // `[keys]` is a document, not a menu: a rebinding arrives here or not at
        // all, and a keymap that remembered the old chords would leave the file
        // describing a keyboard nobody has.
        self.keymap = crate::keys::Keymap::new(&self.config.keys);
        // `serial.key_delay_ms` is live too, and it is a period: a pane already
        // pacing at the old number would keep it until the mode was toggled.
        if self.key_delay {
            let delay = self.key_delay_period();
            for tab in &self.tabs {
                for pane in tab.leaves.values() {
                    if let Pane::Terminal(session) = pane
                        && session.serial
                    {
                        session.pane.send(crate::pane::Command::KeyDelay(delay));
                    }
                }
            }
        }
    }

    /// Read the config file again, because it changed on disk.
    ///
    /// `dtr --config-set`, an editor and a `git checkout` all land here, and what
    /// they change is everything the window is: the look, the font, the theme,
    /// the keys. A broken file is not a window that closes — the running config
    /// stays and the error is said out loud, the same bargain as at startup.
    fn reload_config(&mut self) {
        let parsed = dtr_config::Config::load(&self.config_path);
        let config = match parsed {
            Ok(config) => config,
            Err(err) => {
                tracing::warn!(%err, path = %self.config_path.display(), "config reload");
                // A toast is an overlay 240 points wide, not a stack trace: the
                // parser's line and column are in the log, and what a glance
                // needs is that the file did not take.
                self.toast = Some((
                    "config.toml has an error".to_owned(),
                    std::time::Instant::now(),
                ));
                self.repaint("config reload failed");
                return;
            }
        };
        if config == self.config {
            // A save of our own, or a touch that changed nothing: adopting it
            // would rebuild the grid and re-send the theme for no reason.
            tracing::debug!("config reload: unchanged");
            return;
        }
        // The terminals' colours are the file's too, and a theme it named is a
        // change like any other.
        self.adopt_config(config);
        tracing::debug!(
            widgets = %self.config.ui.widgets,
            scheme = %self.config.ui.scheme,
            theme = self.config.theme.as_deref().unwrap_or("built-in"),
            "config reloaded"
        );
        let config = self.config.clone();
        self.show_look(&config, true);
        self.repaint("config reloaded");
    }

    /// Put the sheet's draft on the window, now.
    ///
    /// The menu's rows are the window's controls, not a form to be submitted: a
    /// draft that took effect only on save would make every row a promise about
    /// what the window will look like afterwards, and the thing being chosen is
    /// usually the window behind the card. `re_theme` is the theme row's: only it
    /// re-resolves `theme`, because a palette command changes the terminals'
    /// colours without touching the file and every other row must leave that
    /// alone.
    fn preview_settings(&mut self, re_theme: bool) {
        let draft = self.settings.draft().clone();
        self.show_look(&draft, re_theme);
        self.repaint("preferences previewed");
    }

    /// Make `config` the window's look.
    ///
    /// The three readers — startup, a preview, a save — all land here and in
    /// `Look::from_config`, so a saved window cannot look different from the one
    /// that was dragged there.
    fn show_look(&mut self, config: &dtr_config::Config, re_theme: bool) {
        if re_theme {
            self.switch_theme(config.theme.as_deref());
        }
        // The grid is rebuilt against a new face on the next frame, which is
        // where `Gpu` is; `fit_panes` then re-fits the pty from the new metrics.
        let font_changed = config.font.size != self.look.size
            || config.font.family != self.look.family
            || config.font.fallback != self.look.fallback;
        self.look = crate::ui::Look::from_config(config, self.theme.as_ref());
        if font_changed {
            self.fonts_dirty = true;
        }
    }

    /// Give every terminal, and the chrome, the theme `name` asks for.
    ///
    /// The chrome and the terminals have to move together, or the window shows
    /// one theme's text on another theme's surface — so this is the only place
    /// that sends `SetTheme`, and it sends it to every pane in every tab. A name
    /// that does not load keeps the colours the window has, because a terminal
    /// that went blank over a typo is worse than one that stayed put.
    fn switch_theme(&mut self, name: Option<&str>) {
        let theme = match name {
            None => None,
            Some(name) => match crate::theme::load(name) {
                Ok(theme) => Some(theme),
                Err(err) => {
                    tracing::warn!(%err, name, "theme");
                    return;
                }
            },
        };
        let same = theme.as_ref().map(|theme| theme.name.as_str())
            == self.theme.as_ref().map(|theme| theme.name.as_str());
        if same {
            return;
        }
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

    /// Ask for a frame, and say why: a terminal that redraws continuously while
    /// nothing moves is a battery bug, and the reason is the only way to find it.
    fn repaint(&self, why: &'static str) {
        // Remembered so a spinning loop names its cause: the reason strings are
        // distinct per caller, and a flood of one of them is the answer.
        self.wake_why.set(why);
        tracing::debug!(why, "request redraw");
        self.request_redraw();
        // `request_redraw` alone is not enough on Wayland: a request made from
        // inside a redraw waits for the compositor's next frame callback, so the
        // change only appears when something else wakes the loop — a closed tab
        // that vanishes when the mouse moves. `about_to_wait` turns this flag
        // into a deadline, which wakes the loop at once.
        self.wake.set(true);
    }

    /// Ask the window for a frame, when there is one. Between frames this is
    /// the only handle on the window the app keeps (`repaint` is called from
    /// everywhere).
    fn request_redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    /// The configured surface size in physical pixels, as a float pair for the
    /// layout code: what the last frame drew at.
    fn screen(&self) -> (f32, f32) {
        (self.size.0 as f32, self.size.1 as f32)
    }

    /// egui's pixels per point: the window's context, as the last frame left it.
    fn pixels_per_point(&self) -> f32 {
        self.egui_ctx
            .as_ref()
            .map(|ctx| ctx.pixels_per_point())
            .unwrap_or_else(|| self.scale())
    }

    /// The focused terminal, when the focused pane is one. A file pane makes
    /// this `None`; callers that are terminal-only must handle that.
    fn session(&self) -> Option<&Session> {
        self.tabs[self.active].focused_terminal()
    }

    fn session_mut(&mut self) -> Option<&mut Session> {
        self.tabs[self.active].focused_terminal_mut()
    }

    fn focused_browser(&self) -> Option<&Browser> {
        self.tabs[self.active].focused_browser()
    }

    fn focused_browser_mut(&mut self) -> Option<&mut Browser> {
        self.tabs[self.active].focused_browser_mut()
    }

    /// Open a new tab on a local shell, in the focused tab's directory when that
    /// tab is local.
    ///
    /// `program` is one of the shells the new-tab menu discovered, or `None` for
    /// "whatever the spawner finds" — the row the menu draws when nothing could
    /// be discovered. A remote tab's directory is a path on *another* machine,
    /// so a shell that inherited it would try to `cd` into a directory that is
    /// not here.
    fn open_shell(&mut self, program: Option<String>) {
        let cwd = self
            .session()
            .filter(|session| !session.remote)
            .and_then(|session| session.pane.cwd());
        self.open(Transport::Shell(crate::pty::Config {
            program,
            cwd,
            ..Default::default()
        }));
        self.repaint("new tab");
    }

    /// Open a tab to an ssh host, asking what its tmux has first when the
    /// session is meant to outlive the connection.
    ///
    /// The question is a second connection and it costs a round trip, so it is
    /// asked off this thread and the tab opens on the answer. A host with no
    /// tmux, or one that would not answer, gets a plain login shell: a tab has to
    /// work on a machine nobody has set up, and silence about the difference
    /// would read as the feature having worked.
    fn open_ssh(&mut self, alias: String) {
        // Counted here rather than where the connection succeeds: the menu is a
        // list of what a person reaches for, and a host that is down this minute
        // is still the one they picked.
        if let Some(store) = &self.store {
            if let Err(err) = store.note_ssh_use(&alias) {
                tracing::warn!(%err, %alias, "could not remember the ssh host");
            }
        }
        if !self.config.tmux.enabled {
            self.open(Transport::Shell(crate::pty::Config::ssh(&alias)));
            return;
        }
        if self.tmux_pending.as_deref() == Some(alias.as_str()) {
            return;
        }
        self.tmux_pending = Some(alias.clone());
        self.toast = Some((
            format!("looking for tmux sessions on {alias}..."),
            std::time::Instant::now(),
        ));
        crate::tmux::spawn_list(&alias, self.proxy.clone());
        self.repaint("listing tmux sessions");
    }

    /// A host said what its tmux has: open the tab, or ask which session.
    fn tmux_answered(&mut self, alias: String, listing: crate::tmux::Listing) {
        if self.tmux_pending.as_deref() == Some(alias.as_str()) {
            self.tmux_pending = None;
        }
        match listing {
            // Nothing to choose between: the session a person would pick is the
            // one that does not exist yet.
            crate::tmux::Listing::Sessions(names) if names.is_empty() => {
                let session = crate::tmux::free_name(&names);
                self.toast = Some((
                    format!("{alias}: tmux session {session}"),
                    std::time::Instant::now(),
                ));
                self.open(Transport::Shell(crate::pty::Config::ssh_tmux(
                    &alias, &session,
                )));
            }
            crate::tmux::Listing::Sessions(names) => self.tmux_chooser.open(&alias, names),
            crate::tmux::Listing::Absent => {
                self.toast = Some((
                    format!("no tmux on {alias}: this tab will not survive a drop"),
                    std::time::Instant::now(),
                ));
                self.open(Transport::Shell(crate::pty::Config::ssh(&alias)));
            }
            crate::tmux::Listing::Failed(why) => {
                // The reason is in the log because it is usually a first
                // connection whose host key is not known yet: the tab it opens
                // is the one that will ask, and the next tab to this host gets
                // its tmux.
                tracing::warn!(%alias, %why, "could not list tmux sessions");
                self.toast = Some((
                    format!("could not list sessions on {alias}: opening a shell"),
                    std::time::Instant::now(),
                ));
                self.open(Transport::Shell(crate::pty::Config::ssh(&alias)));
            }
        }
        self.repaint("tmux answer");
    }

    /// The session chosen out of the chooser, or the chooser dismissed.
    fn tmux_pick(&mut self, pick: crate::tmux::Pick) {
        match pick {
            crate::tmux::Pick::Create(session) | crate::tmux::Pick::Attach(session) => {
                let alias = self.tmux_chooser.alias().to_owned();
                self.tmux_chooser.close();
                tracing::info!(%alias, %session, "opening a tmux session");
                self.open(Transport::Shell(crate::pty::Config::ssh_tmux(
                    &alias, &session,
                )));
            }
            crate::tmux::Pick::Close => {
                self.tmux_chooser.close();
                tracing::info!("tmux session list dismissed");
            }
        }
        self.repaint("tmux session chosen");
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
            std::sync::Arc::clone(&self.clipboard_cache),
            self.config.terminal.clipboard_read,
            move || {
                let _ = proxy.send_event(UserEvent::Frame);
            },
        );
        match spawned {
            Ok(pane) => {
                // A mode that is already on belongs to a pane that did not exist
                // when it was switched on, or a serial tab opened now would type
                // at full speed in a window whose chip says otherwise.
                if matches!(transport, Transport::Serial(_))
                    && let Some(delay) = self.key_delay_period()
                {
                    pane.send(crate::pane::Command::KeyDelay(Some(delay)));
                }
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
                    activity: crate::pane::Activity::default(),
                    serial: matches!(transport, Transport::Serial(_)),
                    // A remote shell reports the *remote* directory; it must not
                    // be adopted as a local path by a new tab.
                    remote: alias.is_some(),
                    alias,
                    counters: !matches!(transport, Transport::Shell(_)),
                    echo: false,
                    meter: Meter::new(),
                    bell_at: None,
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
        // happen between two frames: the tab goes to the background runtime and
        // the UI thread moves on.
        let at = std::time::Instant::now();
        let mut dead = Some(self.tabs.remove(index));
        if let Some(worker) = self.bg() {
            if let Some(tab) = dead.take() {
                worker.blocking(move || {
                    let started = std::time::Instant::now();
                    drop(tab);
                    tracing::debug!(ms = started.elapsed().as_millis(), "tab torn down");
                });
            }
        }
        if let Some(tab) = dead {
            drop(tab);
            tracing::debug!(ms = at.elapsed().as_millis(), "tab torn down inline");
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

    /// Close the focused pane; when it is the last pane in its tab, close the
    /// tab; when it is the last tab, close the window.
    fn close_active(&mut self) {
        let focused = self.tabs[self.active].focus;
        self.close_pane(focused);
    }

    /// Close one pane of the active tab.
    ///
    /// The last pane in the tab closes the tab, and the last tab the window —
    /// the rule `close_active` has always had. The focus only moves when the
    /// pane that went *was* the focused one: a file pane's close button is a
    /// press in another pane's corner, and a terminal somebody is typing in must
    /// not lose the keyboard because a listing beside it was dismissed.
    fn close_pane(&mut self, id: crate::layout::LeafId) {
        if !self.tabs[self.active].leaves.contains_key(&id) {
            return;
        }
        self.unzoom();
        let tab = &mut self.tabs[self.active];
        if tab.leaves.len() > 1 && tab.layout.remove(id) {
            tab.leaves.remove(&id);
            // Focus the first remaining pane when the one that went held the
            // keyboard: predictable, and never the one whose geometry just
            // changed under the pointer.
            if tab.focus == id {
                if let Some(first) = tab.layout.leaves().first().copied() {
                    tab.focus = first;
                }
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
            std::sync::Arc::clone(&self.clipboard_cache),
            self.config.terminal.clipboard_read,
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

        self.unzoom();
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
                activity: crate::pane::Activity::default(),
                serial: matches!(transport, Transport::Serial(_)),
                remote: alias.is_some(),
                alias,
                counters: !matches!(transport, Transport::Shell(_)),
                echo: false,
                meter: Meter::new(),
                bell_at: None,
                grid: (0, 0),
            }),
        );
        tab.focus = id;
    }

    /// Move the keyboard to the pane in that direction.
    ///
    /// A zoomed tab has one visible pane, so the arrows do nothing there: focus
    /// on a pane nobody can see would take the keyboard out of the one that is
    /// on screen, and the person typing would see nothing happen.
    fn focus_direction(&mut self, direction: crate::layout::Direction) {
        if self.window.is_none() {
            return;
        }
        let (screen, scale) = (self.screen(), self.scale());
        let area = self.pane_area(screen, scale);
        let tab = &mut self.tabs[self.active];
        if tab.zoom.is_some() {
            return;
        }
        let Some(next) =
            tab.layout
                .neighbour(area, crate::ui::SPLIT_GAP * scale, tab.focus, direction)
        else {
            return;
        };
        if next == tab.focus {
            return;
        }
        tab.focus = next;
        tracing::debug!(pane = next, ?direction, "pane focus");
        self.repaint("pane focus");
    }

    /// Move a divider by one cell, for a keyboard that has no pointer.
    ///
    /// The divider is the one bordering the focused pane on the side pressed —
    /// or the far one when that edge is the window's, because a key that does
    /// nothing is indistinguishable from a key that is broken. A cell rather
    /// than a fraction: the step is the same size at every window size and font,
    /// which is also what `resize-pane` in a mux means.
    fn resize_pane(&mut self, direction: crate::layout::Direction) {
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        if self.window.is_none() {
            return;
        }
        let (screen, scale, metrics) = (self.screen(), self.scale(), gpu.metrics());
        if self.tabs[self.active].zoom.is_some() {
            return;
        }
        let area = self.pane_area(screen, scale);
        let gap = crate::ui::SPLIT_GAP * scale;
        let tab = &mut self.tabs[self.active];
        let focus = tab.focus;
        let Some(divider) = tab
            .layout
            .divider_toward(area, gap, focus, direction)
            .or_else(|| {
                tab.layout
                    .divider_toward(area, gap, focus, direction.opposite())
            })
        else {
            return;
        };
        let step = match divider.axis {
            crate::layout::Axis::Horizontal => {
                metrics.cell_width / (divider.area.width - gap).max(1.0)
            }
            crate::layout::Axis::Vertical => {
                metrics.cell_height / (divider.area.height - gap).max(1.0)
            }
        };
        let Some(current) = tab.layout.ratio(&divider.path) else {
            return;
        };
        // A bigger ratio is a divider further right or down, wherever the pane
        // sits: the ratio is the first child's share, not the pane's.
        let delta = match direction {
            crate::layout::Direction::Right | crate::layout::Direction::Down => step,
            crate::layout::Direction::Left | crate::layout::Direction::Up => -step,
        };
        if tab.layout.set_ratio(&divider.path, current + delta) {
            self.repaint("pane resized");
        }
    }

    /// Give the focused pane the whole tab, or put the layout back.
    ///
    /// Nothing is rewritten: the tree keeps its dividers, so zooming out is
    /// exact rather than a reconstruction. A tab with one pane has nothing to
    /// zoom out of and is left alone.
    fn toggle_zoom(&mut self) {
        let tab = &mut self.tabs[self.active];
        if tab.leaves.len() < 2 {
            return;
        }
        tab.zoom = match tab.zoom {
            Some(id) if id == tab.focus => None,
            _ => Some(tab.focus),
        };
        tracing::debug!(pane = tab.focus, zoomed = tab.zoom.is_some(), "pane zoom");
        self.repaint("pane zoom");
    }

    /// Put the layout back before adding or removing a pane.
    ///
    /// A zoom hides every pane but one, so a split or a close made while zoomed
    /// would happen off screen — the new pane would be invisible and the closed
    /// one would still be the only thing shown.
    fn unzoom(&mut self) {
        self.tabs[self.active].zoom = None;
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

    /// Take whatever every file pane's listing has delivered, then ask for
    /// whatever a remote one is waiting on.
    ///
    /// Here rather than in the draw path because a browser is drawn through
    /// `&self`: merging a batch is a change to the model, and the model is only
    /// `&mut` before the frame starts. Every leaf in every tab, not the focused
    /// one: a pane nobody is typing in still has to fill in.
    fn pump_files(&mut self) {
        for tab in &mut self.tabs {
            for pane in tab.leaves.values_mut() {
                if let Pane::Files(browser) = pane {
                    browser.poll();
                }
            }
        }
        self.pump_sftp();
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
                let Pane::Files(browser) = pane else {
                    continue;
                };
                if let Some(request) = browser.take_request() {
                    requests.push(request);
                }
                if let Some(remote) = browser.take_download() {
                    if let Some(alias) = browser.alias() {
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
    fn browser_waiting(&mut self, alias: &str, path: &Path) -> Option<&mut Browser> {
        for tab in &mut self.tabs {
            for pane in tab.leaves.values_mut() {
                let Pane::Files(browser) = pane else {
                    continue;
                };
                if browser.alias() == Some(alias) && browser.is_waiting_for(path) {
                    return Some(browser);
                }
            }
        }
        None
    }

    /// Any pane browsing this host, for a failure that names no directory.
    fn browser_on(&mut self, alias: &str) -> Option<&mut Browser> {
        for tab in &mut self.tabs {
            for pane in tab.leaves.values_mut() {
                let Pane::Files(browser) = pane else {
                    continue;
                };
                if browser.alias() == Some(alias) {
                    return Some(browser);
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
                let entries: Vec<dtr_files::Entry> = entries
                    .into_iter()
                    .map(|entry| {
                        // The SFTP client is a Unix-host protocol in every deployment
                        // we have, and its `READDIR` carries no attributes: the dot
                        // rule is the far side's own.
                        let hidden = entry.name.starts_with('.');
                        dtr_files::Entry {
                            name: entry.name,
                            is_dir: entry.dir,
                            hidden,
                            size: (!entry.dir).then_some(entry.size),
                            // SFTP's `READDIR` carries the time in the same packet as
                            // the size, so a remote listing needs no metadata pass.
                            modified: entry.modified.map(|secs| {
                                std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)
                            }),
                        }
                    })
                    .collect();
                if let Some(browser) = self.browser_waiting(&alias, &path) {
                    browser.apply_listing(path, entries);
                }
            }
            SftpEvent::Downloaded { local, bytes } => {
                self.toast = Some((
                    format!("copied {} bytes to {}", bytes, local.display()),
                    std::time::Instant::now(),
                ));
            }
            SftpEvent::Failed { alias, message } => {
                if let Some(browser) = self.browser_on(&alias) {
                    browser.apply_failure(message);
                }
            }
        }
    }

    /// Open a file pane over another machine, beside the terminal that is on it.
    fn split_files_remote(&mut self, alias: &str) {
        self.unzoom();
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
        tab.leaves.insert(
            id,
            Pane::Files(remote_file_browser(&self.config, alias, PathBuf::from("."))),
        );
        tab.focus = id;
    }

    fn split_files(&mut self, path: Option<PathBuf>) {
        self.unzoom();
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
            .insert(id, Pane::Files(file_browser(&self.config, path)));
        tab.focus = id;
    }

    /// Put a file path on the first terminal in this tab's command line. With
    /// no terminal in the tab there is nowhere to put it, so the pane is inert
    /// rather than destructive.
    fn insert_path_into_terminal(&mut self, path: PathBuf) {
        self.insert_text_into_terminal(path.to_string_lossy().into_owned());
    }

    /// The bytes of the same rule, for a picker that answered with several paths
    /// at once: one write, so the shell sees one line rather than two half-typed
    /// ones.
    fn insert_text_into_terminal(&mut self, text: String) {
        let tab = &self.tabs[self.active];
        let Some(id) = tab.first_terminal() else {
            return;
        };
        if let Some(Pane::Terminal(session)) = self.tabs[self.active].leaves.get(&id) {
            session.pane.write(text.into_bytes());
        }
    }

    /// Open a picker in the focused pane's directory, for the terminal beside it.
    ///
    /// The first caller the picker has, and the honest one until F3's daemon
    /// exists: a terminal's own "choose a file and put its path on the command
    /// line". The modes the widget carries and this chord does not use — a
    /// folder, several files, a save name — are what the portal will ask for, and
    /// each is drawn and tested in the pane already.
    fn start_pick(&mut self) {
        let folder = self
            .focused_browser()
            .map(|browser| browser.directory().to_path_buf())
            .or_else(|| self.session().and_then(|session| session.pane.cwd()))
            .or_else(|| std::env::current_dir().ok());
        let mut request = dtr_files::Request::open().accept_label("insert");
        if let Some(folder) = folder {
            request = request.at(folder);
        }
        self.split_pick(request);
        self.repaint("pick opened");
    }

    /// A picker as a pane of its own, beside whatever is focused.
    ///
    /// Its own function rather than a flag on `split_files`, because the two say
    /// different things: a file pane is a place to look, and a picker is a
    /// question that will close itself.
    fn split_pick(&mut self, request: dtr_files::Request) {
        self.unzoom();
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
        let mut browser = file_browser(&self.config, None);
        browser.set_pick(request);
        tab.leaves.insert(id, Pane::Files(browser));
        tab.focus = id;
    }

    /// Act on what a picker answered.
    ///
    /// `Chosen` is the only one that ends it: a `Replace` question and a
    /// `Refused` sentence are both still on screen, and the footer is where they
    /// are said — a picker whose button did nothing is the outcome the memo
    /// forbids.
    fn apply_pick(&mut self, id: crate::layout::LeafId, outcome: dtr_files::Outcome) {
        match outcome {
            dtr_files::Outcome::Chosen(paths) => {
                let text = paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" ");
                self.insert_text_into_terminal(text);
                if let Some(Pane::Files(browser)) = self.tabs[self.active].leaves.get_mut(&id) {
                    browser.finish_pick();
                }
                self.repaint("picked a path");
            }
            dtr_files::Outcome::Replace(_) | dtr_files::Outcome::Refused(_) => {
                self.repaint("the picker asked again");
            }
            dtr_files::Outcome::Navigate(folder) => {
                // A folder pressed in a picker that wants a file: the browser's
                // own gesture, one level down, with the question still open.
                if let Some(Pane::Files(browser)) = self.tabs[self.active].leaves.get_mut(&id) {
                    browser.navigate(folder);
                }
                self.repaint("pick browsed");
            }
        }
    }

    /// The picker's own two keys, while it has focus.
    ///
    /// Enter is the accept button and Escape the cancel one; everything else is
    /// the text field's, so a listing's movement keys must not also run while
    /// somebody is typing a name.
    fn pick_key(&mut self, key: &winit::keyboard::Key) {
        use winit::keyboard::{Key, NamedKey};
        match key {
            Key::Named(NamedKey::Escape) => {
                if let Some(browser) = self.focused_browser_mut() {
                    browser.cancel_pick();
                }
                self.repaint("pick cancelled");
            }
            Key::Named(NamedKey::Enter) => {
                let answer = self
                    .focused_browser()
                    .and_then(|browser| browser.accept_pick());
                if let Some(outcome) = answer {
                    let id = self.tabs[self.active].focus;
                    self.apply_pick(id, outcome);
                }
            }
            _ => {}
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
        let area = self.pane_area(screen, scale);
        tab.layout
            .visible(area, crate::ui::SPLIT_GAP * scale, tab.zoom)
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
                // The bell belongs to the pane that rang it, so a split flashes
                // the half the program is in and not the whole window.
                let flash = match tab.leaves.get(&id) {
                    Some(Pane::Terminal(session)) => bell_alpha(session.bell_at),
                    _ => None,
                };
                (
                    id,
                    crate::render::PaneView {
                        rect,
                        frame,
                        selection,
                        cursor,
                        flash,
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
        // both in physical pixels. `strip_points` is the platform's answer — 46
        // for a GNOME header bar, Windows' own caption band — and `chip_points`
        // follows it, so a bar that is too thin for its chip shows up here as
        // numbers rather than as a judgement about a picture.
        if self.strip_logged == Some(scale) {
            return;
        }
        self.strip_logged = Some(scale);
        let strip = self.title_height(scale);
        tracing::debug!(
            scale,
            reserved_px = crate::chrome::reserved_pixels(strip, crate::ui::GRID_GAP, scale),
            strip_px = crate::chrome::strip_pixels(strip, scale),
            strip_points = strip,
            chip_points = crate::ui::tab_chip_height(strip),
            "strip geometry"
        );
    }

    /// The strip's height in points.
    ///
    /// With client chrome the strip *is* the title bar, so it is as tall as the
    /// platform's own (`chrome::title_height`); with the window manager drawing
    /// the frame it is a tab row and the height it always had.
    fn title_height(&self, scale: f32) -> f32 {
        if self.config.chrome == dtr_config::Chrome::Client {
            crate::chrome::title_height(scale)
        } else {
            crate::ui::TAB_HEIGHT
        }
    }

    fn top_inset(&self, scale: f32) -> f32 {
        // Always, not only when there are several tabs: the strip is drawn even
        // for one tab (it is where the session is named, now that a shell has no
        // status bar), and reserving space only sometimes is how the prompt ended
        // up underneath it.
        crate::chrome::reserved_pixels(self.title_height(scale), crate::ui::GRID_GAP, scale)
    }

    /// The rectangle panes live in: below the strip, above the status bar.
    ///
    /// One function because three things draw or size against it — the grid
    /// pass, the pty's resize and the divider the pointer grabs — and two of
    /// them disagreeing is a pane whose text is not where its rectangle is.
    fn pane_area(&self, screen: (f32, f32), scale: f32) -> crate::layout::Rect {
        let inset = self.top_inset(scale);
        let status = if self.status_visible() {
            (crate::ui::STATUS_HEIGHT + crate::ui::GRID_GAP) * scale
        } else {
            0.0
        };
        crate::layout::Rect::new(0.0, inset, screen.0, (screen.1 - status - inset).max(0.0))
    }

    /// Resize the window so the grid holds `rows` × `cols` cells.
    ///
    /// A *delta* against the size that is there rather than a computed absolute:
    /// the chrome above and below the grid is not something a program asked
    /// about, and the only thing it asked for is more or fewer cells. `None` on a
    /// side means it wanted that side left alone. The clamp is there because a
    /// program asking for one cell must not shrink the window to nothing.
    ///
    /// Returns whether it applied. It cannot before the first frame has measured
    /// a cell, and the caller keeps the request rather than losing it.
    fn resize_window_to(&mut self, rows: Option<u16>, cols: Option<u16>) -> bool {
        let (Some(gpu), Some(session), Some(window)) =
            (&self.gpu, self.session(), self.window.as_ref())
        else {
            return false;
        };
        let (current_cols, current_rows) = session.grid;
        // Before the first fit there is no grid to be a delta from.
        if current_cols == 0 || current_rows == 0 {
            return false;
        }
        let cell = gpu.metrics();
        let mut size = window.inner_size();
        if let Some(cols) = cols {
            let delta = (f32::from(cols) - f32::from(current_cols)) * cell.cell_width;
            size.width = (size.width as f32 + delta).max(MIN_WINDOW.0) as u32;
        }
        if let Some(rows) = rows {
            let delta = (f32::from(rows) - f32::from(current_rows)) * cell.cell_height;
            size.height = (size.height as f32 + delta).max(MIN_WINDOW.1) as u32;
        }
        tracing::info!(
            ?cols,
            ?rows,
            width = size.width,
            height = size.height,
            "a program resized the window"
        );
        // `request_` because a compositor may refuse a size, and a terminal that
        // insisted would be fighting the window manager.
        let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(size.width, size.height));
        true
    }

    fn fit_panes(&mut self, metrics: crate::render::Metrics, screen: (f32, f32), scale: f32) {
        if metrics.cell_width <= 0.0 || metrics.cell_height <= 0.0 {
            return;
        }
        let area = self.pane_area(screen, scale);
        let tab = &mut self.tabs[self.active];
        let rects = tab
            .layout
            .visible(area, crate::ui::SPLIT_GAP * scale, tab.zoom);
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
        let screen = self.screen();
        self.pane_views(tab, gpu.metrics(), screen, self.scale())
            .into_iter()
            .find(|(_, pane)| pane.rect.contains(x as f32, y as f32))
            .map(|(id, _)| id)
    }

    /// The divider under a point, if the point is on one.
    ///
    /// A zoomed tab has no dividers to grab: the layout is still there, but the
    /// only pane on screen owns the whole window, so a press in the middle of it
    /// must reach the program rather than move a border nobody can see.
    fn divider_at_point(&self, at: (f64, f64)) -> Option<crate::layout::Divider> {
        self.gpu.as_ref()?;
        let tab = &self.tabs[self.active];
        if tab.zoom.is_some() {
            return None;
        }
        let scale = self.scale();
        let screen = self.screen();
        tab.layout.divider_at(
            self.pane_area(screen, scale),
            crate::ui::SPLIT_GAP * scale,
            at.0 as f32,
            at.1 as f32,
            DIVIDER_SLOP * scale,
        )
    }

    /// Put the divider where the pointer is.
    ///
    /// The position is turned into the split's ratio rather than into an offset,
    /// so a drag that leaves the window, comes back, or runs past the edge keeps
    /// meaning the same thing — the clamp is the tree's, and the pointer's
    /// distance from the start does not accumulate error.
    fn drag_divider(&mut self, divider: &crate::layout::Divider, at: (f64, f64)) {
        let gap = crate::ui::SPLIT_GAP * self.scale();
        let (along, start, span) = match divider.axis {
            crate::layout::Axis::Horizontal => (
                at.0 as f32,
                divider.area.x,
                (divider.area.width - gap).max(1.0),
            ),
            crate::layout::Axis::Vertical => (
                at.1 as f32,
                divider.area.y,
                (divider.area.height - gap).max(1.0),
            ),
        };
        let ratio = (along - start) / span;
        if self.tabs[self.active]
            .layout
            .set_ratio(&divider.path, ratio)
        {
            self.repaint("divider dragged");
        }
    }

    /// The focused pane's rectangle, in physical pixels.
    ///
    /// A split moves the grid's origin, so "is the pointer over the terminal"
    /// is a question only the pane views answer. The cell under the pointer and
    /// the wheel both ask it, and two answers that disagreed would put a notch
    /// and a click in different panes.
    fn focused_pane_rect(&self) -> Option<crate::layout::Rect> {
        let gpu = self.gpu.as_ref()?;
        let metrics = gpu.metrics();
        let tab = &self.tabs[self.active];
        let screen = self.screen();
        self.pane_views(tab, metrics, screen, self.scale())
            .into_iter()
            .find(|(id, _)| *id == tab.focus)
            .map(|(_, pane)| pane.rect)
    }

    /// Which cell is under a physical pixel position, if any.
    ///
    /// The grid starts at the window origin: the status bar is a separate panel
    /// at the bottom, and the grid is not offset.
    fn cell_at(&self, x: f64, y: f64) -> Option<usize> {
        let gpu = self.gpu.as_ref()?;
        let metrics = gpu.metrics();
        let session = self.session()?;
        let frame = session.frame.as_ref()?;
        let pane = self.focused_pane_rect()?;
        let (col, row) = cell_of(
            pane,
            (metrics.cell_width, metrics.cell_height),
            (u32::from(frame.cols), u32::from(frame.rows)),
            (x, y),
        )?;
        Some(row as usize * usize::from(frame.cols) + col as usize)
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
            Action::Theme(name) => {
                // The palette's view switch: the terminals' colours and the
                // chrome's together, through the one function that sends
                // `SetTheme`, and without writing `theme` to the file.
                self.switch_theme(Some(&name));
                let config = self.config.clone();
                self.show_look(&config, false);
            }
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
                // reopens it when it drops instead of closing the tab — and the
                // tab is put inside a tmux session where the host has one, so
                // the work outlives the connection too.
                self.open_ssh(host);
            }
            Action::Keys => self.keymap_card.open = true,
            // The card's own way in, so the palette is a second way into the
            // same surface and never a second implementation of it.
            Action::Preferences => {
                self.dispatch(crate::keys::Action::Preferences);
            }
            Action::About => {
                self.about_open = true;
                self.repaint("about");
            }
            // The debug overlay: the same switch `--stats` flips at startup.
            Action::Stats => {
                self.stats_overlay = !self.stats_overlay;
                // A window switched on now starts its window now: a p99 from
                // ten minutes ago is not the p99 of what is being measured.
                if self.stats_overlay {
                    self.stats_due = std::time::Instant::now();
                    self.frame_samples.clear();
                }
                tracing::debug!(on = self.stats_overlay, "debug overlay");
                self.repaint("debug overlay toggled");
            }
            // The keys action and the palette command are one implementation:
            // the palette arm delegates, so the two cannot disagree about what
            // the switch does.
            Action::KeyDelay => {
                self.dispatch(crate::keys::Action::KeyDelay);
            }
            Action::Quit => self.quit = true,
        }
    }

    /// How long a byte waits behind the one before it while delayed input is on.
    ///
    /// `None` when the mode is off, which is the pane's "write it straight
    /// through" answer. The period is the config's, so a reload reaches panes
    /// through `adopt_config` rather than through a second copy here.
    fn key_delay_period(&self) -> Option<std::time::Duration> {
        self.key_delay
            .then(|| std::time::Duration::from_millis(self.config.serial.key_delay_ms.into()))
    }
}

/// What the event loop did in the last second.
struct LoopReport {
    since: std::time::Instant,
    iterations: u64,
    frames: u64,
    wakes: u64,
    /// The last completed second's rates, computed from the same counts the
    /// `-v` line logs. The overlay reads them instead of keeping a second set of
    /// counters that could disagree with the log.
    rates: crate::stats::LoopRates,
}

impl Default for LoopReport {
    fn default() -> Self {
        Self {
            since: std::time::Instant::now(),
            iterations: 0,
            frames: 0,
            wakes: 0,
            rates: crate::stats::LoopRates::default(),
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
    // The overlay shows the same second as rates rather than counts: a count
    // and a rate differ by the window's length, and the window is this
    // function's business, not the box's.
    let seconds = elapsed.as_secs_f32().max(f32::EPSILON);
    let rates = crate::stats::LoopRates {
        iterations_per_s: report.iterations as f32 / seconds,
        frames_per_s: report.frames as f32 / seconds,
        wakes_per_s: report.wakes as f32 / seconds,
    };
    *report = LoopReport {
        since: std::time::Instant::now(),
        rates,
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
/// and for Browser.
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

impl dtr_frame::App for App {
    type Event = UserEvent;

    fn resumed(&mut self, event_loop: &ActiveEventLoop, windows: &mut dtr_frame::Windows) {
        if !windows.is_empty() {
            return;
        }

        // Who paints the frame: the tab strip (`client`) or the window manager
        // (`native`). Read once, here: on Wayland `xdg_decoration` is a request a
        // compositor may refuse, and on X11 the motif hint is a hint, so
        // "switch it later" is not something that can be promised.
        let client_frame = self.config.chrome == dtr_config::Chrome::Client;
        // The caption's maximise button must answer Windows' hit test as
        // `HTMAXBUTTON`, or Windows 11 never offers the snap-layout flyout.
        // winit has no hook for a hit test, so the window procedure is
        // subclassed — once, when the window exists and before its first frame.
        #[cfg(target_os = "windows")]
        let on_created: Option<Box<dyn FnOnce(&Arc<Window>)>> = client_frame.then(|| {
            Box::new(|window: &Arc<Window>| {
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
            })
        });
        #[cfg(not(target_os = "windows"))]
        let on_created = None;

        let options = dtr_frame::WindowOptions {
            title: String::from("dtr"),
            inner_size: (900.0, 620.0),
            decorations: !client_frame,
            // The same artwork the desktop entry names. Without it Windows draws
            // a generic blank next to the title, and a window list has nothing
            // to show but the name.
            icon: window_icon(),
            // The desktop entry says `StartupWMClass=dtr` and `Icon=dtr`, so the
            // window has to say it is called that. Without a name, X11 derives
            // `WM_CLASS` from argv[0] — which under the nix wrapper is
            // `.dtr-unwrapped` — and Wayland gets no `app_id` at all, so a
            // running window lands in the dock as a second, iconless application.
            app_id: Some((String::from("dtr"), String::from("dtr"))),
            // A terminal is where CJK is typed: composition, the candidate
            // window and the caret report all hang off this.
            ime_allowed: true,
            // A frameless window with no shadow reads as a rectangle pasted onto
            // the desktop.
            undecorated_shadow: client_frame,
            adapter_hint: self.gpu_hint.clone(),
            on_created,
        };
        let frame = match windows.open(event_loop, options) {
            Ok(frame) => frame,
            Err(err) => {
                tracing::error!(%err, "create window");
                event_loop.exit();
                return;
            }
        };

        // dtr's half of the frame: the caption's glyph source, the egui style
        // and the grid renderer. The frame's context exists by now, and loading
        // the image loader and the fonts here is still before the first frame —
        // the only deadline either has.
        let ctx = frame.context().clone();
        crate::ui::install_image_loaders(&ctx);
        // Whether the platform's caption-glyph font is there: the buttons draw
        // with it when it is, and paint their shapes when it is not. Asked here
        // because a missing glyph is a panic, not a fallback.
        let caption_font = self.look.install(&ctx);
        // The window's style is the skin from the first frame — installed here
        // so nothing reads egui's own defaults.
        let skin = self.look.skin();
        skin.apply(&ctx);
        let gpu = Gpu {
            grid: crate::render::GridRenderer::new(
                frame.device(),
                frame.format(),
                &self.look,
                frame.scale(),
            ),
            format: frame.format(),
            client_frame,
            caption_glyphs: if caption_font {
                crate::chrome::Glyphs::Font
            } else {
                crate::chrome::Glyphs::Painted
            },
            installed_skin: Some(skin),
        };
        self.gpu = Some(gpu);
        self.window = Some(frame.window().clone());
        self.egui_ctx = Some(ctx);
        self.size = frame.size();
        frame.request_redraw();
        self.refresh_frame_budget();
    }

    fn about_to_wait(
        &mut self,
        event_loop: &ActiveEventLoop,
        _windows: &mut dtr_frame::Windows,
        deadlines: &mut dtr_frame::Deadlines,
    ) {
        report_loop(self);
        self.check_exit_deadline(event_loop);

        // A reload that arrived while the sheet was open runs once it is closed,
        // before anything else this iteration: the window the reload describes
        // should be the one the next frame draws.
        if self.config_deferred && !self.settings.open {
            self.config_deferred = false;
            self.reload_config();
        }

        // The control flow is **sticky**: winit keeps the last value until
        // something sets it again. Every deadline collected below is in the past
        // by the time the next iteration runs, so leaving one in place makes
        // every `poll` time out instantly — 55,000 loop iterations a second with
        // 20 frames drawn. `deadlines` keeps the earliest of them, and the
        // event loop sets exactly one value after this returns, defaulting to
        // `Wait` (`dtr_frame::Deadlines`).

        // A repaint asked for while the loop was idle: wake now rather than at
        // the next input event.
        if self.wake.replace(false) {
            self.loop_report.borrow_mut().wakes += 1;
            deadlines.soon(std::time::Instant::now());
            self.request_redraw();
        }

        // The bell is a fade, so it needs frames while it runs: a tint painted
        // once would sit at its first alpha until something else redrew, and the
        // window would look stained rather than rung.
        let ringing = self.tabs.iter().any(|tab| {
            tab.leaves.values().any(|pane| match pane {
                Pane::Terminal(session) => bell_alpha(session.bell_at).is_some(),
                Pane::Files(_) => false,
            })
        });
        if ringing {
            self.repaint("bell fading");
            deadlines.soon(std::time::Instant::now() + std::time::Duration::from_millis(16));
            self.request_redraw();
        }

        // A listing on its way in is the other state with no event to announce
        // it: the walk is on its own thread and its batches arrive whenever they
        // do. Sixteen milliseconds is the gate that turns a burst into one frame —
        // a 10 000-entry directory is 157 batches, and 157 repaints is 157
        // pictures nobody sees (`docs/manager.md` §3).
        let listing = self.tabs.iter().any(|tab| {
            tab.leaves
                .values()
                .any(|pane| matches!(pane, Pane::Files(browser) if browser.reading()))
        });
        if listing {
            self.repaint("a file listing is arriving");
            deadlines.soon(std::time::Instant::now() + std::time::Duration::from_millis(16));
            self.request_redraw();
        }

        // A pane waiting for its connection is a state that changes with the
        // clock: the notice counts the wait down, and a countdown that only
        // moved when the pointer did would be a still picture claiming to be a
        // timer. Two frames a second is the resolution it is written in — whole
        // seconds — and the state clears itself when the pty comes back.
        let waiting = self.tabs[self.active].leaves.values().any(|pane| {
            matches!(pane, Pane::Terminal(session) if session.pane.stats().retry().is_some())
        });
        if waiting {
            self.repaint("a pane is waiting for its connection");
            deadlines.soon(std::time::Instant::now() + std::time::Duration::from_millis(500));
            self.request_redraw();
        }

        // A directory that gained or lost an entry is a state with no event: no
        // thread, no port, nothing to wait on. So it is polled, and the answer is
        // one `stat` of a path the pane already holds.
        if self.listing_due <= std::time::Instant::now() {
            self.listing_due = std::time::Instant::now() + LISTING_POLL;
            let mut changed = false;
            for tab in &mut self.tabs {
                for pane in tab.leaves.values_mut() {
                    if let Pane::Files(browser) = pane {
                        changed |= browser.refresh_if_changed();
                    }
                }
            }
            if changed {
                self.repaint("a directory changed");
            }
        }
        // Set every iteration, because the control flow is sticky: a deadline
        // left in the past makes every `poll` time out instantly.
        deadlines.soon(self.listing_due);

        // A scan is cheap to *ask* for and expensive to *do*, so asking never
        // blocks the frame: the work goes to the worker runtime and comes back
        // as an event.
        if self.scan_due <= std::time::Instant::now() {
            self.request_scan();
        }
        // Scripted pointer gestures run after the scripted keys, because a
        // gesture is usually about what the keys just built: a drag on the
        // divider of a split that has not happened yet grabs nothing, and a
        // click lands on whatever was there before.
        if self.script.is_empty() {
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
        }

        // `--keys`: one stroke per wake-up, so the frame that follows each one
        // exists before the next arrives. This is the harness the user asked
        // for: a scripted run goes through the same handler a keyboard does.
        if let Some(stroke) = self.script.pop_front() {
            self.on_key(&stroke);
            self.repaint("scripted key");
            deadlines.soon(std::time::Instant::now() + std::time::Duration::from_millis(16));
        }

        // A pending capture needs a frame at its deadline. Without this the
        // window goes idle after the last redraw and the capture never
        // happens: the deadline passes with nothing drawing.
        if let Some((_, when)) = self.capture.as_ref().map(|(_, when)| ((), *when)) {
            self.repaint("capture pending");
            deadlines.soon(when);
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
                deadlines.soon(pending_deadline);
            }
        }
        // Wake up when the deadline is due, rather than hoping some other
        // event brings the loop back: an idle window that never wakes would
        // never take its own exit.
        if let Some(after) = self.exit_after {
            deadlines.soon(self.started + std::time::Duration::from_millis(after));
        }

        // A preview of a remote picture waits for the pointer to settle, and the
        // wait needs a wake of its own: the pointer resting produces no events, and
        // a request nobody wakes for would never be made.
        match self.thumb_pending.as_ref().map(|pending| pending.due) {
            Some(due) if std::time::Instant::now() >= due => self.due_thumbnail(),
            Some(due) => deadlines.soon(due),
            None => {}
        }

        // The config file is looked at on a deadline rather than watched: one
        // `stat` every 250 ms is the whole mechanism (`docs/compio.md` §5), and
        // the comparison is against the previous look, so a save that lands
        // between two of them is seen by the next one. A change only starts the
        // settle clock below — an editor writes in bursts, and the file has to be
        // quiet before it is read.
        let now = std::time::Instant::now();
        if self.config_due <= now {
            self.config_due = now + crate::watch::ConfigWatch::POLL;
            if self.config_watch.changed() {
                self.config_settle.note(now);
                self.repaint("config file changed");
            }
        }
        deadlines.soon(self.config_due);

        // The config file, once it has been quiet for a moment. A change that
        // arrives while the preferences card is open waits for it: the sheet owns
        // the window's look, and re-reading the file under a half-finished draft
        // would take the preview away from under the pointer. A save wins over
        // the deferred change — the draft was written over it.
        match self.config_settle.due() {
            Some(due) if std::time::Instant::now() >= due => {
                if self.config_settle.take(std::time::Instant::now()) {
                    if self.settings.open {
                        self.config_deferred = true;
                    } else {
                        self.reload_config();
                    }
                }
            }
            Some(due) => deadlines.soon(due),
            None => {}
        }

        // The debug overlay refreshes its readout on a fixed low cadence, not
        // once per frame. A counter that asked for a frame per refresh would
        // measure itself: idle fps would be the display's rate, the fan would
        // run for a number, and neither "is dtr idle?" nor "what does a frame
        // cost?" would have an answer. Its own refresh counts as a frame, so a
        // quiet window reads about ten a second, not zero; typing and animation
        // push it to whatever dtr really draws. The redraw is requested directly
        // rather than through `repaint`, because `repaint` also sets `wake`,
        // which would make the loop spin between frames — and because it would
        // log a line per frame under `-v`.
        if self.stats_overlay {
            if self.stats_due <= now {
                self.stats_due = now + crate::stats::REFRESH_INTERVAL;
                self.request_redraw();
            }
            deadlines.soon(self.stats_due);
        }

        // `dtr_frame` sets the one value, from the earliest of the deadlines
        // above, as soon as this returns.
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, _cause: winit::event::StartCause) {
        self.check_exit_deadline(event_loop);
    }

    fn user_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _windows: &mut dtr_frame::Windows,
        event: UserEvent,
    ) {
        match event {
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
            UserEvent::Tmux { alias, listing } => self.tmux_answered(alias, listing),
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
        let mut bell_rang = false;
        let mut bell_behind = false;
        let mut resize_to = None;
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            for pane in tab.leaves.values_mut() {
                if let Pane::Terminal(session) = pane {
                    if let Some((rows, cols)) = session.pane.take_resize_request() {
                        if self.config.terminal.window_resize {
                            resize_to = Some((rows, cols));
                        } else {
                            tracing::info!(
                                rows = ?rows,
                                cols = ?cols,
                                "a program asked to resize the window; terminal.window_resize is off"
                            );
                        }
                    }
                    // A program asked the person to see something: the window
                    // says it, because dtr has no notification centre of its own
                    // and a system one would arrive without the tab it came from.
                    for notice in session.pane.take_notices() {
                        let text = match (notice.title.is_empty(), notice.body.is_empty()) {
                            (false, false) => format!("{}: {}", notice.title, notice.body),
                            (false, true) => notice.title.clone(),
                            (true, false) => notice.body.clone(),
                            (true, true) => continue,
                        };
                        tracing::debug!(%text, "a program sent a notification");
                        // The desktop is asked first; the toast is what happens
                        // when the answer is no — not a second copy of the same
                        // notice on top of the desktop's own.
                        if self.notifier.notify(&notice) == crate::notify::Delivered::No {
                            self.toast = Some((text, std::time::Instant::now()));
                        }
                        bell_behind |= index != self.active;
                    }
                    let rung = session.pane.take_bells();
                    if rung > 0 {
                        session.bell_at = Some(std::time::Instant::now());
                        bell_rang = true;
                        tracing::trace!(bells = rung, "bell");
                        // A flash in a tab nobody is looking at is not a bell:
                        // it is invisible, which is the same as silence.
                        bell_behind |= index != self.active;
                    }
                    session.activity = session.pane.activity();
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
        if bell_rang {
            self.repaint("bell");
        }
        // `CSI 8 t`, when the config lets a program move the window: the pane
        // parked the request because it does not own the surface. It is *kept*
        // until it can be applied — the request can arrive before the first
        // frame has measured a cell, and a request dropped then is a request
        // silently ignored, which is the bug this whole document is about.
        if resize_to.is_some() {
            self.pending_resize = resize_to;
        }
        if let Some((rows, cols)) = self.pending_resize {
            if self.resize_window_to(rows, cols) {
                self.pending_resize = None;
            } else {
                self.repaint("a resize is waiting for a measured cell");
            }
        }
        if bell_behind {
            // The window, not the speaker: dtr never plays a sound, and a hint
            // to the taskbar is the only thing a hidden tab can do.
            if let Some(window) = &self.window {
                window
                    .request_user_attention(Some(winit::window::UserAttentionType::Informational));
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

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        frame: &mut dtr_frame::Frame,
        event: WindowEvent,
    ) {
        if matches!(event, WindowEvent::RedrawRequested) {
            self.egui_redraw_pending = false;
        }
        if self.gpu.is_none() {
            return;
        }

        // egui sees every event except the one asking for a frame: fed
        // `RedrawRequested`, egui asks for another frame, and the loop runs at
        // the refresh rate forever — which is exactly the CPU burn this cost.
        // `Frame::feed_event` drops that event itself, so this cannot regress.
        if !matches!(event, WindowEvent::RedrawRequested) {
            let response_repaint = frame.feed_event(&event);
            // **egui asks for a frame for a click, not for a move**, and every
            // control that changes on hover — a tab chip, the menu button, a
            // file pane's sort control — is a picture of a control until
            // something else draws. So a pointer event is worth a frame by
            // itself: the pointer moved, and what is under it is the answer.
            let pointer = matches!(
                event,
                WindowEvent::CursorMoved { .. }
                    | WindowEvent::MouseInput { .. }
                    | WindowEvent::MouseWheel { .. }
            );
            if response_repaint || pointer {
                tracing::debug!(why = "egui", "request redraw");
                frame.request_redraw();
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
                frame.request_redraw();
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            // `?1004`: a program can ask to be told when the window is focused,
            // and a shell uses it to change the cursor. Every pane in the tab on
            // screen hears it, because the focus is the window's; a pane in a
            // tab nobody is looking at is not "focused" in any sense the program
            // could act on.
            WindowEvent::Focused(active) => {
                if let Some(tab) = self.tabs.get(self.active) {
                    for pane in tab.leaves.values() {
                        if let Pane::Terminal(session) = pane {
                            session.pane.send(crate::pane::Command::Focused(active));
                        }
                    }
                }
            }
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
                // Remembered, not applied: Windows delivers one `WM_SIZE` per
                // mouse move during a drag, and `dtr_frame::Frame::configure_pending_resize`
                // turns however many arrive between two frames into one
                // `surface.configure` — each of which waits for the present
                // queue to go idle on DX12 (docs/windows.md §5.3). The pty is
                // re-fitted by the frame that draws the new size, not here.
                frame.resize(size);
                self.repaint("resize");
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_position = Some((position.x, position.y));
                // While an overlay is up the pointer belongs to it: hovering the
                // grid underneath would light up a link it covers, and a drag
                // would select text nobody can see. `hover` asks the same
                // question about egui's own surfaces.
                if !self.overlay_open() {
                    self.pointer_move((position.x, position.y));
                    // A divider drag is not a hover: the pointer is over a gap,
                    // and asking the pane about a cell that is not under it
                    // lights up a link nobody is pointing at.
                    if self.divider.is_none() {
                        self.hover((position.x, position.y));
                    }
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
                // The wheel is not a key, and egui reads the same event: a
                // surface that scrolls itself — the `+` menu's host list, a
                // card's body, the pager — would move *and* take the scrollback
                // behind it with it. Which of the two was asked is the pointer's
                // question, and it has three parts: no overlay has the window,
                // the pointer is over the focused pane, and no egui surface is
                // drawn above it.
                let (over_grid, over_egui) = match self.cursor_position {
                    Some((x, y)) => (
                        self.focused_pane_rect()
                            .is_some_and(|pane| pane.contains(x as f32, y as f32)),
                        self.over_egui((x, y)),
                    ),
                    // No pointer yet: the focused terminal is the only thing a
                    // notch can be for.
                    None => (true, false),
                };
                // A notch over a notice that covers the pane is "show me the
                // screen": it folds the notice and scrolls the scrollback in the
                // same motion, so the gesture that was refused hands back the text
                // it was asking for.
                let folded = lines != 0.0 && over_grid && self.fold_notice();
                if lines != 0.0 && self.scroll_file_pane(lines) {
                    // The listing took the notch, and the scrollback behind it
                    // has nothing to do with the pointer.
                } else if lines != 0.0
                    && wheel_reaches_the_terminal(
                        self.overlay_open(),
                        over_grid,
                        over_egui && !folded,
                    )
                {
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
                let Some(mut gpu) = self.gpu.take() else {
                    return;
                };
                // One swapchain reconfigure for however many `Resized`
                // events arrived since the last frame, before anything in
                // this frame reads the size, and the size the frame will
                // draw at is the one every hit test in this pass answers.
                frame.configure_pending_resize();
                self.size = frame.size();
                // The status strip follows the focused pane. A file pane has
                // no byte counters, and saying so is more honest than showing
                // the terminal beside it as if it were focused.
                let mut label = String::from("files");
                let (mut rx, mut tx) = (0u64, 0u64);
                let (mut rx_rate, mut tx_rate) = (0.0f64, 0.0f64);
                let mut scrollback = 0u64;
                let mut modem = None;
                let mut counters = false;
                let mut serial = false;
                if let Some(session) = self.session() {
                    let stats = session.pane.stats().clone();
                    label = session.label.clone();
                    (rx, tx) = (stats.rx(), stats.tx());
                    scrollback = stats.scrollback();
                    modem = session.serial.then(|| (stats.dtr(), stats.rts()));
                    counters = session.counters;
                    serial = session.serial;
                    if let Some(session) = self.session_mut() {
                        session.meter.sample(rx, tx);
                    }
                    if let Some(session) = self.session() {
                        rx_rate = session.meter.rx_rate;
                        tx_rate = session.meter.tx_rate;
                    }
                } else if let Some(browser) = self.focused_browser() {
                    label = browser.title();
                }
                let tabs = (self.active + 1, self.tabs.len());
                let status = Status {
                    label: &label,
                    tabs,
                    rx,
                    tx,
                    rx_rate,
                    tx_rate,
                    scrollback,
                    modem,
                    counters,
                    // Only a serial pane has bytes to space, and only its
                    // line needs the chip: a shell telling the strip it is
                    // delayed would be a mode that is doing nothing.
                    key_delay: if self.key_delay && serial {
                        self.key_delay_period()
                    } else {
                        None
                    },
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
                // A pointing hand is the whole affordance for a link, and a
                // resize arrow the whole affordance for a divider: neither
                // has an underline or a handle to draw. While a divider is
                // being dragged the arrow stays put, because the pointer
                // running ahead of the split does not mean the drag ended.
                // An overlay takes both affordances with the window: a hand
                // over a card is pointing at a link the card covers.
                let resizing = if self.overlay_open() {
                    None
                } else {
                    let held = self.divider.as_ref().map(|divider| divider.axis);
                    held.or_else(|| {
                        self.cursor_position
                            .and_then(|at| self.divider_at_point(at))
                            .map(|divider| divider.axis)
                    })
                };
                let wanted = match resizing {
                    Some(crate::layout::Axis::Horizontal) => winit::window::CursorIcon::ColResize,
                    Some(crate::layout::Axis::Vertical) => winit::window::CursorIcon::RowResize,
                    None if self.hover_link.is_some() && !self.overlay_open() => {
                        winit::window::CursorIcon::Pointer
                    }
                    None => winit::window::CursorIcon::Default,
                };
                if self.cursor_icon != Some(wanted) {
                    self.cursor_icon = Some(wanted);
                    frame.window().set_cursor(wanted);
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
                let screen = (frame.size().0 as f32, frame.size().1 as f32);
                let scale = frame.scale();
                if std::mem::take(&mut self.fonts_dirty) {
                    gpu.rebuild_grid(frame.device(), &self.look, scale);
                }
                self.pump_files();
                self.log_strip_geometry(scale);
                self.fit_panes(gpu.metrics(), screen, scale);
                // Before the borrows below: uploading a texture needs `&mut self`
                // (the cache turns pixels into a texture once) and `settings` is
                // about to lend `self` out for the whole frame.
                let thumbnail = self.hover_thumbnail(scale);
                // A menu opened before the startup font probe finished picks
                // the list up here, on the first frame it exists. Before the
                // borrows, because filling it in needs `&mut self`.
                if self.settings.open {
                    self.settings.refresh_families();
                }
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
                let mut tmux_pick = None;
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
                    frame.set_title(&format!("{label} — dtr"));
                }
                // One stripe per chip: what the tab's program says it is
                // doing. The activity lives on the session, because the
                // strip is drawn from labels and a label cannot carry it.
                let activity: Vec<crate::pane::Activity> = self
                    .tabs
                    .iter()
                    .map(|tab| {
                        tab.focused_terminal()
                            .map(|session| session.activity)
                            .unwrap_or_default()
                    })
                    .collect();
                let mut tab_click = None;
                let mut settings_reply = None;
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
                let area = self.pane_area(screen, scale);
                let points = frame.context().pixels_per_point();
                let file_panes: Vec<FilePaneView<'_>> = tab
                    .layout
                    .visible(area, crate::ui::SPLIT_GAP * scale, tab.zoom)
                    .into_iter()
                    .filter_map(|(id, rect)| match tab.leaves.get(&id) {
                        Some(Pane::Files(browser)) => Some(FilePaneView {
                            id,
                            rect: to_points(rect, points),
                            browser,
                            focused: id == tab.focus,
                        }),
                        _ => None,
                    })
                    .collect();
                // A pane whose connection is not there has no cells worth
                // reading and a state worth showing: it is drawn over the
                // rectangle the grid pass reserved, once per pane, so a
                // split says which half is waiting.
                // A fold is remembered per pane, and only while that pane is
                // waiting: the next drop is a new notice and starts covered.
                let still_waiting: std::collections::BTreeSet<_> = tab
                    .leaves
                    .iter()
                    .filter_map(|(id, pane)| {
                        let session = pane.terminal()?;
                        session.pane.stats().retry()?;
                        Some(*id)
                    })
                    .collect();
                self.folded_notices.retain(|id| still_waiting.contains(id));
                let waiting: Vec<crate::ui::Waiting> = tab
                    .layout
                    .visible(area, crate::ui::SPLIT_GAP * scale, tab.zoom)
                    .into_iter()
                    .filter_map(|(id, rect)| {
                        let session = tab.leaves.get(&id)?.terminal()?;
                        Some(crate::ui::Waiting {
                            id,
                            rect,
                            retry: session.pane.stats().retry()?,
                            folded: self.folded_notices.contains(&id),
                        })
                    })
                    .collect();
                let mut reconnect_ask = None;
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
                    if let (Some(rect), Some(session)) = (focused_rect, tab.focused_terminal()) {
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
                                frame.window().set_ime_cursor_area(
                                    winit::dpi::PhysicalPosition::new(cell.x as i32, cell.y as i32),
                                    winit::dpi::PhysicalSize::new(
                                        cell.width as u32,
                                        cell.height as u32,
                                    ),
                                );
                            }
                            // egui draws in points; the grid is laid out in
                            // physical pixels. One conversion, here.
                            let points = frame.context().pixels_per_point();
                            ime_anchor = Some(crate::ime::Anchor {
                                caret: to_points(cell, points),
                                pane: to_points(rect, points),
                            });
                        }
                    }
                }
                let ime = ime_anchor.map(|anchor| (anchor, &self.preedit));
                let look = &self.look;
                // Taken out for the frame for the same reason the palette is: the
                // pager mutates itself while it draws, and `self` is borrowed.
                let mut pager = std::mem::take(&mut self.pager);
                let mut link_menu = std::mem::take(&mut self.link_menu);
                let mut menu_choice = None;
                let mut close_window = false;
                // The file pane whose close button was pressed, if any: the
                // drawing code says *which* pane, and only the app can take
                // a leaf out of the layout.
                let mut close_file = None;
                let mut sort_change = None;
                let mut place_choice = None;
                let mut file_menu = None;
                let mut pick_answer = None;
                let mut pick_cancel = None;
                let mut keymap_event = None;
                let mut about_close = false;
                // The overlay's zero-cost state: a bool, the last completed
                // second's rates (from `LoopReport`, the same numbers `-v`
                // logs) and the focused grid's size.
                let stats = crate::stats::Input {
                    enabled: self.stats_overlay,
                    rates: self.loop_report.borrow().rates,
                    grid: self.session().map_or((0, 0), |session| session.grid),
                };
                // The debug frame line, before the scene borrows `panes` for
                // the draw: nothing after the render may use them.
                if self.frames_drawn < 3 || self.frames_drawn % 120 == 0 {
                    let with_content = panes.iter().filter(|pane| pane.frame.is_some()).count();
                    tracing::debug!(
                        frames_drawn = self.frames_drawn,
                        panes = panes.len(),
                        with_content,
                        instances = gpu.grid.instances(),
                        "frame"
                    );
                }
                // The resolved skin is the window's style, and installing
                // it before the egui pass is what makes every dialog a
                // guest: a dialog that installs its own style on the
                // context used to leave the strip under the card's row
                // height. It is a no-op while the skin is unchanged.
                gpu.install_skin(frame.context(), &self.look);
                // The draw pass needs a few things from the renderer and
                // the frame copied out first: the scene below holds
                // `&mut gpu.grid`, so the closure cannot borrow `gpu` too.
                let metrics = gpu.metrics();
                let glyphs = gpu.glyphs();
                let client_frame = gpu.client_frame;
                let caption_glyphs = gpu.caption_glyphs;
                let adapter_name = frame.adapter_name().to_owned();
                let window = self.window.clone();
                let status_visible = self.status_visible();
                // The grid is dtr's underlay: the scene draws the terminal
                // and the frame lays egui over it.
                let mut scene = GridScene {
                    grid: &mut gpu.grid,
                    panes: &panes,
                    look: &look,
                };
                let rendered = frame.render(
                    &mut scene,
                    dtr_frame::RenderOptions {
                        frames_drawn: self.frames_drawn,
                        started: self.started,
                        capture: capture_path.as_deref(),
                        measure_present: self.stats_overlay,
                    },
                    |ui| {
                        // The strip is the title bar when this window paints its own frame, so
                        // its height is the platform's own caption — 46 points for a GNOME
                        // header bar, Windows' queried band — and the chip follows it. With
                        // the window manager drawing a title bar the strip is only a tab row.
                        let title_height = if client_frame {
                            crate::chrome::title_height(scale)
                        } else {
                            crate::ui::TAB_HEIGHT
                        };
                        let chip_height = crate::ui::tab_chip_height(title_height);
                        // A client-side frame must exist even with no tabs, or the window
                        // has no way to be moved or closed while the last tab is going away.
                        if !tabs.is_empty() || client_frame {
                            // The panel is the strip's full height and the chips are
                            // shorter than it, so nothing egui rounds or grows can paint
                            // over the first terminal row. Same lesson as the status bar.
                            egui::Panel::top("dtr-tabs")
                                // Points, not physical pixels: egui is in points and `scale`
                                // is applied by egui itself. Multiplying here made the strip
                                // grow with DPI twice, so at scale 2 it was 60 points tall
                                // over a 60 *pixel* reservation and the first rows slid
                                // underneath it — invisible at scale 1.
                                .exact_size(title_height)
                                // No inner margin: with a client-side frame the buttons run
                                // the strip's full height and the drag region is the whole
                                // row, so the inset belongs to the chips alone (below).
                                .frame(egui::Frame::new().fill(look.skin().palette.card))
                                .show(ui, |ui| {
                                    let strip = ui.max_rect();
                                    // The chips keep the inset the panel used to give them,
                                    // except on Windows, where the caption icon's square
                                    // comes first.
                                    let left = if client_frame {
                                        crate::chrome::chips_left()
                                    } else {
                                        6.0
                                    };
                                    let inset = egui::vec2(
                                        left,
                                        ((title_height - chip_height) / 2.0).floor(),
                                    );
                                    let mut chips = ui.new_child(
                                        egui::UiBuilder::new()
                                            .max_rect(strip.shrink2(inset))
                                            .layout(egui::Layout::left_to_right(
                                                egui::Align::Center,
                                            )),
                                    );
                                    let bar = crate::ui::tab_bar(
                                        &mut chips,
                                        &tabs,
                                        self.active,
                                        &self.destinations,
                                        &look,
                                        chip_height,
                                        &activity,
                                    );
                                    if let Some(hit) = bar.hit {
                                        tab_click = Some(hit);
                                    }
                                    // The window's own menu, at the right end of the strip
                                    // beside the caption buttons — or at the strip's own
                                    // right edge in `native` mode, where there are none. The
                                    // same place on both platforms: the left is where tabs
                                    // start, and a button there would move them.
                                    let menu_edge = if client_frame {
                                        crate::chrome::buttons_rect(strip, crate::chrome::kind())
                                            .left()
                                    } else {
                                        strip.right()
                                    };
                                    let menu_rect = egui::Rect::from_min_max(
                                        egui::pos2(
                                            (menu_edge - 4.0 - 32.0).max(strip.left()),
                                            strip.top(),
                                        ),
                                        egui::pos2(menu_edge - 4.0, strip.bottom()),
                                    );
                                    let skin = look.skin();
                                    if let Some(chosen) =
                                        crate::ui::app_menu(ui, menu_rect, &skin, &self.keymap)
                                    {
                                        tab_click = Some(crate::ui::TabHit::Menu(chosen));
                                    }
                                    // The rest of the row is the title bar. In `native` mode
                                    // the window manager already gave us one, and a strip
                                    // that also moved the window would be two title bars in
                                    // one place.
                                    if client_frame {
                                        let maximized =
                                            ui.input(|i| i.viewport().maximized.unwrap_or(false));
                                        // The app icon, where Windows puts it, and doing what
                                        // Windows does with it: a click is the system menu.
                                        if let Some(at) =
                                            crate::chrome::caption_icon(ui, strip, &look)
                                        {
                                            if let Some(window) = &window {
                                                dtr_frame::show_system_menu(window, at);
                                            }
                                        }
                                        // The drag region stops at the menu's left edge: the
                                        // menu and the buttons are both inside the caption
                                        // area, and a press on either that also started a
                                        // drag would move the window instead of pressing it.
                                        let caption = egui::Rect::from_min_max(
                                            strip.min,
                                            egui::pos2(menu_rect.left(), strip.bottom()),
                                        );
                                        match crate::chrome::drag_area(ui, caption, bar.used) {
                                            Some(crate::chrome::Drag::Move) => {
                                                ui.ctx().send_viewport_cmd(
                                                    egui::ViewportCommand::StartDrag,
                                                );
                                            }
                                            Some(crate::chrome::Drag::ToggleMaximize) => {
                                                ui.ctx().send_viewport_cmd(
                                                    egui::ViewportCommand::Maximized(!maximized),
                                                );
                                            }
                                            None => {}
                                        }
                                        match crate::chrome::buttons(
                                            ui,
                                            strip,
                                            maximized,
                                            &look,
                                            caption_glyphs,
                                        ) {
                                            Some(crate::chrome::Hit::Minimize) => {
                                                ui.ctx().send_viewport_cmd(
                                                    egui::ViewportCommand::Minimized(true),
                                                );
                                            }
                                            Some(crate::chrome::Hit::Maximize) => {
                                                ui.ctx().send_viewport_cmd(
                                                    egui::ViewportCommand::Maximized(!maximized),
                                                );
                                            }
                                            Some(crate::chrome::Hit::Close) => close_window = true,
                                            None => {}
                                        }
                                        // Where the maximise button is this frame, for the
                                        // Win32 hit test that offers the snap layouts.
                                        #[cfg(target_os = "windows")]
                                        crate::chrome::windows::set_maximize_rect(
                                            crate::chrome::maximize_rect(
                                                strip,
                                                crate::chrome::kind(),
                                            ),
                                            scale,
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
                                .show(ui, |ui| crate::ui::status_bar(ui, &status, look));
                        }
                        egui::CentralPanel::default()
                            .frame(egui::Frame::new().fill(egui::Color32::TRANSPARENT))
                            .show(ui, |ui| {
                                // The notice over a pane that is waiting for its connection,
                                // one per pane, drawn here rather than over the whole window:
                                // the pane's rectangle is the one thing that says *which*
                                // session is not there.
                                for pane in &waiting {
                                    if let Some(ask) = crate::ui::reconnect(ui, pane, look) {
                                        reconnect_ask = Some((pane.id, ask));
                                    }
                                }
                            });
                        // A frameless window's borders. The window manager's grip went with
                        // the frame, so the strips are ours — added after the panels so the
                        // few pixels at the edge belong to the resize and not to whatever
                        // happens to be drawn underneath them.
                        if client_frame {
                            if let Some(direction) = crate::chrome::resize_edges(ui, ui.max_rect())
                            {
                                ui.ctx()
                                    .send_viewport_cmd(egui::ViewportCommand::BeginResize(
                                        direction,
                                    ));
                            }
                        }
                        // File panes sit over the grid rectangle their leaf owns. They
                        // are areas rather than windows: fixed position, no dragging.
                        for file in &file_panes {
                            let outcome = dtr_files::pane_ui(
                                ui,
                                egui::Id::unique(("dtr-files", file.id)),
                                file.rect,
                                file.browser,
                                &look.skin(),
                                file.focused,
                            );
                            if outcome.close {
                                close_file = Some(file.id);
                            }
                            if let Some(change) = outcome.sort {
                                sort_change = Some((file.id, change));
                            }
                            if let Some(path) = outcome.navigate {
                                place_choice = Some((file.id, path));
                            }
                            if let Some(choice) = outcome.menu {
                                file_menu = Some((file.id, choice));
                            }
                            if let Some(outcome) = outcome.pick {
                                pick_answer = Some((file.id, outcome));
                            }
                            if outcome.pick_cancel {
                                pick_cancel = Some(file.id);
                            }
                        }
                        // The debug overlay (`--stats`, or the palette's `overlay`): one
                        // label of chrome text in the bottom-right of the pane area, above
                        // the status row. `crate::stats::overlay` puts it in `Order::Middle`
                        // — over the panes, under every card — and makes it
                        // `interactable(false)`, so a click over it still reaches the cell
                        // underneath. The numbers are the ring as it stood when this frame
                        // began: a frame's own cost is only known once it is presented.
                        if stats.enabled {
                            let live = crate::stats::Live {
                                input: stats,
                                glyphs,
                                cell: (metrics.cell_width, metrics.cell_height),
                                scale,
                                adapter: &adapter_name,
                                status_visible,
                            };
                            crate::stats::overlay(ui, look, &self.frame_samples, &live);
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
                            if let Some(asked) = settings.ui(ui, look) {
                                settings_reply = Some(asked);
                            }
                        }
                        // The keyboard map sits above the palette and the pager: it is the
                        // thing a person opens precisely because they do not know what is
                        // open behind it.
                        if self.about_open {
                            crate::ui::overlay(ui, "dtr-about", |ui| {
                                if crate::about::ui(ui, look) == Some(crate::about::Event::Close) {
                                    about_close = true;
                                }
                            });
                        }
                        if let Some(keymap) = self.keymap_card.open.then_some(&self.keymap_card) {
                            crate::ui::overlay(ui, "dtr-keymap", |ui| {
                                if let Some(asked) =
                                    crate::keymap::ui(ui, look, &self.keymap, keymap)
                                {
                                    keymap_event = Some(asked);
                                }
                            });
                        }
                        if palette_open {
                            crate::ui::overlay(ui, "dtr-palette", |ui| {
                                if let Some(chosen) = palette.ui(ui, look) {
                                    action = Some(chosen);
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
                        if let Some((texture, size)) = &thumbnail {
                            crate::thumb::draw(ui, texture, *size, look);
                        }
                        // A right-click's menu. "Save as" on it opens the desktop's own
                        // dialog, which is not drawn here at all.
                        if link_menu.is_open() {
                            menu_choice = link_menu.ui(ui, look);
                        }
                        // The serial sheet is the topmost surface while it is up: it is a
                        // question about the session, not a decoration on it.
                        if let Some(serial_dialog) = &serial_dialog {
                            if let Some(asked) = serial_dialog.ui(ui, look) {
                                serial_event = Some(asked);
                            }
                        }
                        // Which tmux session to open, when the host had more than one to
                        // offer. Above the sheet and the palette: it is the only surface
                        // that will *open* a tab, and a tab opened behind a card would be a
                        // card hiding its own consequence.
                        if let Some(chooser) =
                            self.tmux_chooser.is_open().then_some(&self.tmux_chooser)
                        {
                            if let Some(picked) = chooser.ui(ui, look) {
                                tmux_pick = Some(picked);
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
                                if let Some(pane) = terminal_pane {
                                    pane.send(crate::pane::Command::Paste(text.into_bytes()));
                                }
                            }
                        }
                        if let Some(text) = copied_text.take() {
                            ui.ctx().copy_text(text.clone());
                            copied = Some(text);
                        }
                        if let Some((text, alpha)) = toast_text
                            .as_ref()
                            .map(|(text, alpha)| (text.as_str(), *alpha))
                        {
                            crate::ui::toast(ui, look, text, alpha);
                        }
                    },
                );
                // The overlay's samples: the frame measured the present
                // wait where it presented, and only because the overlay
                // asked for it. A frame that was skipped measured nothing.
                let timing = rendered.as_ref().ok().and_then(|timing| *timing);
                if let Some(timing) = timing {
                    if let Some(present_ms) = timing.present_ms {
                        self.frame_samples.push(timing.cpu_ms, present_ms);
                    }
                }
                // The keyboard map's answers. Its Escape and its captured
                // key are handled where the keys are; a click has to come back
                // from the frame, because only the frame knows the card was
                // drawn.
                if let Some(asked) = keymap_event {
                    self.keymap_event(asked);
                }
                if about_close {
                    self.about_open = false;
                    self.repaint("about closed");
                }
                if let Some(picked) = tmux_pick {
                    self.tmux_pick(picked);
                }
                // What the notice over a waiting pane was asked for. Both
                // answers are the app's: the pane can hurry its own wait, and
                // only the app can close a tab.
                if let Some((id, ask)) = reconnect_ask {
                    match ask {
                        crate::ui::ReconnectAsk::Now => {
                            let command = crate::pane::Command::Retry;
                            if let Some(session) = self.tabs[self.active]
                                .leaves
                                .get(&id)
                                .and_then(Pane::terminal)
                            {
                                session.pane.send(command);
                            }
                        }
                        crate::ui::ReconnectAsk::Close => self.close_tab(self.active),
                        crate::ui::ReconnectAsk::Fold => {
                            self.folded_notices.insert(id);
                        }
                        crate::ui::ReconnectAsk::Unfold => {
                            self.folded_notices.remove(&id);
                        }
                    }
                    self.repaint("a waiting pane was answered");
                }
                // A file pane's own close button. The pane cannot close
                // itself — a leaf is the layout's — so the answer comes back
                // here, the same way a clicked tab's does.
                if let Some(id) = close_file {
                    self.close_pane(id);
                    self.repaint("file pane closed");
                }
                if let Some((id, change)) = sort_change {
                    self.apply_sort_change(id, change);
                    self.repaint("files sorted");
                }
                if let Some((id, path)) = place_choice {
                    // The places popup chose a directory: the pane that owns
                    // the button goes there, which may be another machine.
                    if let Some(Pane::Files(browser)) = self.tabs[self.active].leaves.get_mut(&id) {
                        browser.navigate(path);
                    }
                    self.repaint("files place");
                }
                if let Some((id, choice)) = file_menu {
                    self.file_menu_choice(id, choice);
                }
                if let Some((id, outcome)) = pick_answer {
                    self.apply_pick(id, outcome);
                }
                if let Some(id) = pick_cancel {
                    // A cancel inside a `replace` question closes the
                    // question, not the picker, and that is the browser's
                    // rule: one button, one word, two depths.
                    if let Some(Pane::Files(browser)) = self.tabs[self.active].leaves.get_mut(&id) {
                        browser.cancel_pick();
                    }
                    self.repaint("pick cancelled");
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
                if let Some(reply) = settings_reply {
                    // A mouse edit updates the draft *and* the window: every
                    // row previews, and the file is written when the menu is
                    // saved — same as a keyboard edit. The card's own buttons
                    // are the keyboard's Enter and Esc, so the mouse and the
                    // keyboard leave through one door.
                    match reply {
                        crate::settings::Reply::Change(change) => {
                            // The theme is the one row that has to re-resolve
                            // the terminals' colours; everything else leaves
                            // them alone.
                            let themed = matches!(change, crate::settings::Change::Theme(_));
                            self.settings.apply(change);
                            self.preview_settings(themed);
                        }
                        crate::settings::Reply::Apply => self.apply_settings(),
                        crate::settings::Reply::Cancel => {
                            self.settings.cancel();
                            // The draft is the file's again — `cancel`
                            // restores it — and so is the window: a preview
                            // is not a change until Save.
                            self.preview_settings(true);
                            self.repaint("preferences closed");
                        }
                    }
                }
                match tab_click {
                    // A click on a tab is a switch, exactly like the chord.
                    Some(crate::ui::TabHit::Select(index)) if index < self.tabs.len() => {
                        self.set_active(index, "chip clicked");
                        self.repaint("tab clicked");
                    }
                    Some(crate::ui::TabHit::Close(index)) => self.close_tab(index),
                    // The strip's menu: the three commands that belong to the
                    // window rather than to a tab. Each opens the same surface
                    // its chord does, so the menu is a second way in, never a
                    // second implementation.
                    Some(crate::ui::TabHit::Menu(menu)) => match menu {
                        crate::ui::Menu::Preferences => {
                            self.dispatch(crate::keys::Action::Preferences);
                        }
                        crate::ui::Menu::Keymap => {
                            self.dispatch(crate::keys::Action::Keymap);
                        }
                        crate::ui::Menu::About => {
                            self.about_open = true;
                            self.repaint("about");
                        }
                    },
                    Some(crate::ui::TabHit::New(destination)) => match destination {
                        // A port goes through the palette's own connect, because
                        // somebody may have set a baud rate or CR translation for
                        // this device and it is remembered per port. Opening a
                        // bare `Transport::Serial` here would ignore both.
                        Some(crate::sources::Destination::Serial { path, .. }) => {
                            self.apply(Action::Connect(path))
                        }
                        Some(crate::sources::Destination::Ssh { alias, .. }) => {
                            self.open_ssh(alias)
                        }
                        // A shell the menu named: the program is opened
                        // explicitly, because the row said which one it was.
                        Some(crate::sources::Destination::Shell { program, .. }) => {
                            self.open_shell(Some(program))
                        }
                        None => self.open_shell(None),
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
                if frame.context().has_requested_repaint() && !self.egui_redraw_pending {
                    self.egui_redraw_pending = true;
                    frame.request_redraw();
                }
                self.loop_report.borrow_mut().frames += 1;
                self.frames_drawn = self.frames_drawn.wrapping_add(1);
                // A frame that took long enough to feel is worth a line: the
                // glyph count says whether it was font work or drawing.
                let frame_ms = timing.map_or(0.0, |timing| timing.cpu_ms);
                if frame_ms > 8.0 {
                    tracing::debug!(
                        ms = format!("{frame_ms:.1}"),
                        glyphs = gpu.grid.glyphs(),
                        "slow frame"
                    );
                }
                // Nothing here asks for another frame for the overlay. Its
                // readout runs on its own low cadence in `about_to_wait`: a
                // counter that forced a frame per refresh would measure
                // itself, and idle fps would be the display's rate rather
                // than dtr's (`crate::stats::REFRESH_INTERVAL`).
                if let Err(err) = rendered {
                    tracing::error!(%err, "render");
                }
                if let Some(action) = action {
                    self.apply(action);
                }
                if self.quit {
                    event_loop.exit();
                }
                self.gpu = Some(gpu);
            }
            _ => {}
        }
    }

    /// The process's exit status: a pane's failure is the loop's error, and it
    /// is reported here, before the app and its GPU resources are dropped.
    fn finished(&self) -> anyhow::Result<()> {
        match &self.fatal {
            Some(err) => anyhow::bail!("{err}"),
            None => Ok(()),
        }
    }
}

/// dtr's half of a frame: the grid renderer and the chrome choices a frame is
/// drawn with.
///
/// The window, the surface, the egui context and the render passes belong to
/// `dtr_frame::Frame`. This is what dtr adds on top, and it is dropped before
/// the frame's device because `dtr_frame` drops the application before its
/// windows — the other half of the wgpu drop-order rule.
struct Gpu {
    /// The grid holds wgpu buffers and textures: it must drop before the device
    /// that owns them, which the drop order above guarantees.
    grid: crate::render::GridRenderer,
    /// The surface format the pipeline was built for: a rebuild after a face or
    /// size change must match the swapchain the frame draws into.
    format: wgpu::TextureFormat,
    /// Whether dtr paints the window frame, from `[window] chrome`. Read once,
    /// at creation: it decides whether the strip is a drag region and whether
    /// the buttons are drawn.
    client_frame: bool,
    /// Where the caption buttons get their symbols: the platform's icon font
    /// when it resolved at startup, drawn shapes when it did not.
    caption_glyphs: crate::chrome::Glyphs,
    /// The skin whose `egui::Style` is installed on the context. Kept so a
    /// frame that resolves the same skin again does not rebuild the style.
    installed_skin: Option<dtr_widgets::Skin>,
}

impl Gpu {
    /// Cell size in physical pixels, for hit-testing.
    fn metrics(&self) -> crate::render::Metrics {
        self.grid.metrics()
    }

    /// Glyphs the last frame rasterised: a large number means the frame is
    /// paying for font work, not for drawing.
    fn glyphs(&self) -> usize {
        self.grid.glyphs()
    }

    /// Rebuild the grid for a new face or size. Cheap enough to do on a
    /// preferences change: it is a fresh atlas, not a reload of the whole
    /// renderer.
    fn rebuild_grid(&mut self, device: &wgpu::Device, look: &Look, scale: f32) {
        self.grid = crate::render::GridRenderer::new(device, self.format, look, scale);
        tracing::info!(
            family = ?look.family,
            size = look.size,
            scale,
            "grid rebuilt"
        );
    }

    /// Install the resolved skin's `egui::Style` when it changed.
    ///
    /// This is where the window becomes the authority on the style, and every
    /// dialog a guest: a dialog that installed its own style on the context
    /// used to leave the tab strip under the card's row height after it closed.
    fn install_skin(&mut self, ctx: &egui::Context, look: &Look) {
        let skin = look.skin();
        if self.installed_skin.as_ref() != Some(&skin) {
            skin.apply(ctx);
            self.installed_skin = Some(skin);
        }
    }
}

/// dtr's underlay for [`dtr_frame::Frame::render`]: the terminal grid.
///
/// The frame owns the passes and the surface; this hands it the grid's
/// per-frame vertex data and its two draw calls. A frontend with no underlay
/// would answer the same trait with a background colour and nothing else.
struct GridScene<'a> {
    grid: &'a mut crate::render::GridRenderer,
    panes: &'a [crate::render::PaneView<'a>],
    look: &'a Look,
}

impl dtr_frame::Scene for GridScene<'_> {
    fn clear_color(&self) -> wgpu::Color {
        wgpu::Color {
            r: f64::from(self.look.background[0]) / 255.0,
            g: f64::from(self.look.background[1]) / 255.0,
            b: f64::from(self.look.background[2]) / 255.0,
            a: 1.0,
        }
    }

    fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, size: (u32, u32)) {
        self.grid.prepare(
            device,
            queue,
            self.panes,
            self.look,
            (size.0 as f32, size.1 as f32),
        );
    }

    fn draw(&mut self, pass: &mut wgpu::RenderPass<'_>) {
        self.grid.draw(pass);
    }

    fn draw_above(&mut self, pass: &mut wgpu::RenderPass<'_>) {
        self.grid.draw_above(pass);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BELL_FLASH, bell_alpha, browser_key, cell_of, closed, copy_to, cycled,
        mouse_goes_to_program, save_name, tab_name, wheel_reaches_the_terminal,
    };

    /// A file pane's keys are its own, and the one with a modifier is the one
    /// every file manager has. The traps are the chords around it: `ctrl-alt-up`
    /// moves the *focus* between panes, so stealing it for the parent directory
    /// would make a window command stop working inside a listing.
    #[test]
    fn a_file_pane_gets_two_dimensional_keys_and_alt_up() {
        use dtr_files::Key as PaneKey;
        use winit::keyboard::{Key, ModifiersState, NamedKey};

        let none = ModifiersState::empty();
        for (named, wanted) in [
            (NamedKey::ArrowRight, PaneKey::Right),
            (NamedKey::ArrowLeft, PaneKey::Left),
            (NamedKey::ArrowUp, PaneKey::Up),
            (NamedKey::ArrowDown, PaneKey::Down),
            (NamedKey::PageUp, PaneKey::PageUp),
            (NamedKey::PageDown, PaneKey::PageDown),
            (NamedKey::Home, PaneKey::Top),
            (NamedKey::End, PaneKey::Bottom),
            (NamedKey::Backspace, PaneKey::Parent),
        ] {
            assert_eq!(
                browser_key(&Key::Named(named.clone()), none),
                Some(wanted),
                "{named:?} is not the key it should be"
            );
        }
        for (text, wanted) in [
            ("h", PaneKey::Left),
            ("j", PaneKey::Down),
            ("k", PaneKey::Up),
            ("l", PaneKey::Right),
        ] {
            assert_eq!(
                browser_key(&Key::Character(text.into()), none),
                Some(wanted),
                "{text} is not the key it should be"
            );
        }

        // Alt+Up is the parent; ctrl+alt+Up belongs to the window.
        let alt = ModifiersState::ALT;
        assert_eq!(
            browser_key(&Key::Named(NamedKey::ArrowUp), alt),
            Some(PaneKey::Parent)
        );
        let ctrl_alt = ModifiersState::CONTROL | ModifiersState::ALT;
        assert_eq!(browser_key(&Key::Named(NamedKey::ArrowUp), ctrl_alt), None);
        // And no other chord with a modifier is the pane's: ctrl+c is a copy, not
        // a directory listing's business.
        assert_eq!(
            browser_key(&Key::Character("c".into()), ModifiersState::CONTROL),
            None
        );
    }

    /// A bell is a flash that fades and then stops: `Some` while it rings, `None`
    /// once it is over, and never a fixed alpha that the window could leave on
    /// screen if it went idle at the wrong moment.
    #[test]
    fn a_bell_fades_and_then_stops() {
        assert_eq!(bell_alpha(None), None, "nothing rang");
        let just_now = Some(std::time::Instant::now());
        let first = bell_alpha(just_now).expect("a fresh bell is visible");
        assert!(first > 0.0 && first <= 0.12, "alpha {first}");
        let over = Some(std::time::Instant::now() - BELL_FLASH);
        assert_eq!(bell_alpha(over), None, "a finished bell must not tint");
    }

    /// A point outside the pane is no cell at all.
    ///
    /// The tab strip is above the grid and the status bar below it, and both are
    /// the window's rather than a cell's. Clamping either to the nearest row is
    /// what put a pointing hand and an image preview behind the tabs.
    #[test]
    fn a_point_outside_the_pane_is_no_cell() {
        // 800 × 600 at (100, 30), in 10 × 20 cells: 80 columns, 30 rows.
        let pane = crate::layout::Rect::new(100.0, 30.0, 800.0, 600.0);
        let cell = (10.0, 20.0);
        let grid = (80, 30);
        assert_eq!(cell_of(pane, cell, grid, (100.0, 30.0)), Some((0, 0)));
        assert_eq!(cell_of(pane, cell, grid, (895.0, 625.0)), Some((79, 29)));
        assert_eq!(
            cell_of(pane, cell, grid, (500.0, 10.0)),
            None,
            "the tab strip"
        );
        assert_eq!(cell_of(pane, cell, grid, (50.0, 300.0)), None, "left of it");
        assert_eq!(
            cell_of(pane, cell, grid, (900.0, 300.0)),
            None,
            "right of it"
        );
        assert_eq!(
            cell_of(pane, cell, grid, (500.0, 630.0)),
            None,
            "the status bar"
        );
        // A pane larger than the frame's grid — a resize that has not settled —
        // is still bounded by the grid.
        assert_eq!(cell_of(pane, cell, (10, 2), (500.0, 300.0)), None);
        // No cell size is no cell: the grid is not laid out yet.
        assert_eq!(cell_of(pane, (0.0, 0.0), grid, (500.0, 300.0)), None);
    }

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
    #[compio::test]
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

    /// A wheel notch is the pointer's, not the focused pane's.
    ///
    /// egui reads the same wheel event, so a notch over a surface that scrolls
    /// itself used to move that surface *and* the scrollback behind it. The
    /// pager was the only such surface the handler knew about by name; the `+`
    /// menu and a card's body scrolled two things at once.
    #[test]
    fn a_wheel_over_an_egui_surface_is_not_the_terminals() {
        assert!(
            wheel_reaches_the_terminal(false, true, false),
            "over the grid, nothing in front of it"
        );
        assert!(
            !wheel_reaches_the_terminal(true, true, false),
            "an overlay has taken the window, even with the pointer outside it"
        );
        assert!(
            !wheel_reaches_the_terminal(false, false, false),
            "the tab strip and the status bar are not the grid"
        );
        assert!(
            !wheel_reaches_the_terminal(false, true, true),
            "a popup drawn over the grid — the + menu — scrolls itself"
        );
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
