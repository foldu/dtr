//! The emulation side of a session.
//!
//! A pane owns a `!Send` `libghostty_vt::Terminal` on its own thread, inside a
//! current-thread tokio runtime and a `LocalSet` (PLAN §2.5). What it publishes
//! is an owned [`Frame`] in a latest-value slot: the UI always draws the newest
//! one and never touches the terminal or blocks the emulation thread.

use std::cell::RefCell;
use std::io::Read as _;
use std::io::Write as _;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use libghostty_vt::Terminal;
use libghostty_vt::key::{Encoder as KeyEncoder, Event as KeyEvent};
use libghostty_vt::mouse;
use libghostty_vt::render::{
    CellIterator, CursorVisualStyle, Dirty, RenderState, RowIterator, Snapshot,
};
use libghostty_vt::screen::TrackedGridRef;
use libghostty_vt::selection::{FormatOptions, SelectLineOptions, SelectWordOptions, Selection};
use libghostty_vt::terminal::{Mode, Point, PointCoordinate, PointSpace};
use portable_pty::MasterPty;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};

use crate::keys::KeyStroke;
use crate::pty;
use crate::serial::{self, CrToCrlf};

pub const COLS: u16 = 80;
pub const ROWS: u16 = 24;

/// When each pane may publish, in nanoseconds, shared with the UI.
///
/// The budget follows the panel, and the panel changes: the window can be
/// dragged to another monitor, and on Wayland nothing tells the client it
/// moved. So the value lives in an atomic that the UI rewrites and every pane
/// reads, rather than being baked into a pane at spawn time.
pub type FrameBudget = std::sync::Arc<std::sync::atomic::AtomicU64>;

/// Half a refresh interval: how long a burst of output may be parsed before a
/// frame is published.
///
/// One publish rebuilds every cell of the grid — about 50 µs for a 34×112 grid
/// — and a flood arrives in chunks small enough that a publish per chunk spends
/// the whole core on frames nobody can see. Half an interval is the useful
/// point: the UI can show one frame per refresh, so publishing twice per
/// refresh means a publish almost always lands before the next draw, while the
/// work stays at two frames per drawn frame. A session with nothing queued
/// publishes immediately, so a shell prompt is never delayed by this.
///
/// It has to follow the display, because it is a latency floor as much as a
/// work cap: a fixed 8 ms would serve a 240 Hz panel at 125 Hz and leave every
/// other frame stale. The default is half of 60 Hz for a monitor that will not
/// say; the floor keeps a 1000 Hz panel from turning the gate into a busy loop.
pub fn frame_budget(refresh_millihertz: Option<u32>) -> std::time::Duration {
    let mhz = refresh_millihertz.filter(|mhz| *mhz > 0).unwrap_or(60_000);
    std::time::Duration::from_micros((500_000_000u64 / u64::from(mhz)).max(2_000))
}

#[derive(Clone, Debug, Default)]
pub struct Cell {
    pub text: String,
    /// A wide character occupies two cells; its glyph is drawn across both, or
    /// the grid shows a one-cell glyph and a one-cell hole.
    pub wide: bool,
    pub fg: [u8; 3],
    pub bg: [u8; 3],
    pub has_bg: bool,
    pub bold: bool,
    pub italic: bool,
}

/// What a cursor is drawn as.
///
/// The program asks with DECSCUSR (`CSI Ps SP q`), and the difference is not
/// decoration: nvim sends `ver25` in insert mode and a block in normal, so a
/// terminal that always draws a block is telling the user they are about to
/// replace text when they are about to insert it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorShape {
    #[default]
    Block,
    /// A vertical bar against the cell's left edge — ncurses' `ver25`.
    Bar,
    /// A horizontal bar along the cell's bottom — `hor20`.
    Underline,
}

/// Where the cursor is and what it looks like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub x: u16,
    pub y: u16,
    pub shape: CursorShape,
}

impl Cursor {
    /// The rectangle the cursor's mark fills inside one cell.
    ///
    /// A block is the cell; a bar is a quarter of its width, a fifth of its
    /// height for an underline — the fractions DECSCUSR's own names use, so a
    /// program that asked for `ver25` gets a quarter and not a hairline. Both
    /// have a one-pixel floor: a sub-pixel bar draws nothing, which is worse
    /// than drawing it a pixel too wide, because the user cannot tell an insert
    /// cursor from no cursor at all.
    pub fn mark(&self, cell: crate::layout::Rect) -> crate::layout::Rect {
        match self.shape {
            CursorShape::Block => cell,
            CursorShape::Bar => crate::layout::Rect::new(
                cell.x,
                cell.y,
                (cell.width * 0.25).ceil().clamp(1.0, cell.width.max(1.0)),
                cell.height,
            ),
            CursorShape::Underline => {
                let height = (cell.height * 0.2).ceil().clamp(1.0, cell.height.max(1.0));
                crate::layout::Rect::new(cell.x, cell.y + cell.height - height, cell.width, height)
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Frame {
    pub cols: u16,
    pub rows: u16,
    pub default_fg: [u8; 3],
    pub default_bg: [u8; 3],
    pub cursor: Option<Cursor>,
    /// The mouse selection as linear cell indices into `cells`, in either
    /// order. Recomputed on every publish, so it names the same characters
    /// after the view scrolls — and `None` once those characters have left the
    /// frame, because a clamped highlight would lie about what is selected.
    pub selection: Option<(usize, usize)>,
    /// Row-major, `cols * rows` entries.
    pub cells: Vec<Cell>,
}

/// Byte counters, shared with the UI. Totals only: the rate is the UI's
/// business, because it is the UI's clock that decides when to repaint.
#[derive(Debug)]
pub struct Stats {
    rx: AtomicU64,
    tx: AtomicU64,
    connected: std::sync::atomic::AtomicBool,
    /// Rows below the viewport: 0 means you are looking at the live screen.
    scrollback: AtomicU64,
    dtr: std::sync::atomic::AtomicBool,
    rts: std::sync::atomic::AtomicBool,
    /// The program asked for the mouse.
    ///
    /// Here rather than asked for on demand because the UI has to know *before*
    /// it decides what a press means: a click in a program that tracks the mouse
    /// is a click for the program, and starting a selection instead swallows it.
    /// The modes live in the terminal on the pane's thread, so this is the one
    /// part of them that crosses over.
    mouse: std::sync::atomic::AtomicBool,
}

impl Default for Stats {
    fn default() -> Self {
        Self {
            rx: AtomicU64::new(0),
            tx: AtomicU64::new(0),
            // A pane that exists has a live source until it says otherwise; a
            // shell or a replay never disconnects, and claiming otherwise put a
            // "reconnecting…" warning on a healthy session.
            connected: std::sync::atomic::AtomicBool::new(true),
            scrollback: AtomicU64::new(0),
            dtr: std::sync::atomic::AtomicBool::new(false),
            rts: std::sync::atomic::AtomicBool::new(false),
            mouse: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl Stats {
    pub fn rx(&self) -> u64 {
        self.rx.load(Ordering::Relaxed)
    }

    pub fn tx(&self) -> u64 {
        self.tx.load(Ordering::Relaxed)
    }

    fn add_rx(&self, bytes: usize) {
        self.rx.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    fn add_tx(&self, bytes: usize) {
        self.tx.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// False while the transport is gone and we are trying to get it back.
    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    fn set_connected(&self, connected: bool) {
        self.connected.store(connected, Ordering::Relaxed);
    }

    /// Rows of scrollback below the viewport.
    pub fn scrollback(&self) -> u64 {
        self.scrollback.load(Ordering::Relaxed)
    }

    fn set_scrollback(&self, rows: u64) {
        self.scrollback.store(rows, Ordering::Relaxed);
    }

    pub fn dtr(&self) -> bool {
        self.dtr.load(Ordering::Relaxed)
    }

    pub fn rts(&self) -> bool {
        self.rts.load(Ordering::Relaxed)
    }

    fn set_dtr(&self, level: bool) {
        self.dtr.store(level, Ordering::Relaxed);
    }

    fn set_rts(&self, level: bool) {
        self.rts.store(level, Ordering::Relaxed);
    }

    /// True while the program is tracking the mouse — X10, normal, button or
    /// any-event. Which of them it is stays in the terminal: the encoder reads
    /// it per event, and the UI only needs to know whether the mouse is the
    /// program's or this terminal's.
    pub fn mouse(&self) -> bool {
        self.mouse.load(Ordering::Relaxed)
    }

    fn set_mouse(&self, on: bool) {
        self.mouse.store(on, Ordering::Relaxed);
    }
}

/// Something the UI asks the pane to do. Keystrokes are only one of these:
/// the modem lines and the viewport live on the pane side too.
pub enum Command {
    /// Raw bytes for the session, as though they had been typed.
    Write(Vec<u8>),
    /// A keystroke, encoded here rather than by the UI.
    ///
    /// Not bytes, because the encoding depends on the terminal: `DECCKM`,
    /// `DECKPAM`, the alt prefix, `modifyOtherKeys` and the kitty keyboard
    /// protocol are all state the terminal holds, and a table in the UI can only
    /// ever be wrong about them.
    Key(Box<KeyStroke>),
    /// A mouse event for the program, encoded here for the same reason a key is:
    /// what a click means is the terminal's state. `?1000h` reports presses and
    /// nothing else, `?1002h` adds motion while a button is held, `?1003h` adds
    /// motion always, and `?1006h` turns the X10 byte form into SGR.
    ///
    /// `x` and `y` are pixels from the pane's top-left corner, because a cell is
    /// what the encoder converts them into and the pane's rectangle is where its
    /// grid starts. `down` is whether a button is held: it is how the encoder
    /// tells a drag from plain motion, which is the whole difference between
    /// `?1002h` reporting and staying quiet.
    Mouse {
        action: mouse::Action,
        button: Option<mouse::Button>,
        mods: libghostty_vt::key::Mods,
        x: f32,
        y: f32,
        down: bool,
    },
    Dtr(bool),
    Rts(bool),
    Break,
    Scroll(isize),
    /// Resize the grid to the window, in cells and in pixels.
    Resize {
        cols: u16,
        rows: u16,
        width_px: u32,
        height_px: u32,
    },
    /// Jump to the top or the bottom of the scrollback.
    ScrollEdge {
        top: bool,
    },
    Echo(bool),
    Copy,
    /// Copy a cell range: `(start, end)` as linear indices, in either order.
    Paste(Vec<u8>),
    /// Begin a mouse selection at a viewport cell: `x` and `y` are the cell the
    /// pointer went down on.
    SelectStart {
        x: u16,
        y: u16,
    },
    /// Move the selection's head to a viewport cell.
    SelectHead {
        x: u16,
        y: u16,
    },
    /// A double-click: the word under a viewport cell.
    SelectWord {
        x: u16,
        y: u16,
    },
    /// A triple-click: the line under a viewport cell.
    SelectLine {
        x: u16,
        y: u16,
    },
    /// Put the current selection on the clipboard.
    CopySelection,
    /// Report the hyperlink (OSC 8) at a viewport cell, if any. Answered into
    /// the pane's link slot, because the URI lives in the terminal and the
    /// terminal never leaves the pane's thread.
    HyperlinkAt {
        x: u16,
        y: u16,
    },
    /// Walk the viewport to the previous (negative) or next prompt mark, which
    /// only exists when the shell emits OSC 133.
    ScrollPrompt(isize),
    /// Capture the block the last command produced — from the prompt before this
    /// one through to the cursor — into the pane's captured slot.
    CaptureBlock,
    /// Apply a theme to the live terminal. Cells are re-read from the terminal,
    /// so existing text takes the new palette too.
    SetTheme(Option<crate::theme::Theme>),
    /// Apply serial settings. A baud or framing change reconnects; the pane
    /// keeps its scrollback and its place.
    SetSerial {
        baud: u32,
        cr_to_crlf: bool,
        auto_reconnect: bool,
    },
}

/// What a pane is attached to.
#[derive(Clone)]
pub enum Transport {
    Serial(serial::Config),
    Shell(pty::Config),
    /// Play a captured byte stream back into the terminal, with pacing.
    Replay(PathBuf),
    /// A canned screen, for screenshots and tests.
    Demo,
}

impl Transport {
    /// One line describing what a pane is attached to: the status bar and the
    /// logbook both use it, so they cannot drift apart.
    pub fn describe(&self) -> String {
        match self {
            Transport::Serial(config) => format!(
                "{} {} 8N1{}",
                config.port,
                config.baud,
                if config.cr_to_crlf { " cr=crlf" } else { "" }
            ),
            Transport::Shell(config) => match (&config.remote, &config.program) {
                // The alias and not the command line: a tab says where it is
                // connected, and `ssh -t jupyter` is three words of plumbing.
                (Some(alias), _) => format!("ssh {alias}"),
                (None, Some(program)) => format!("shell {program}"),
                (None, None) => "shell".to_owned(),
            },
            Transport::Replay(path) => format!("replay {}", path.display()),
            Transport::Demo => "demo".to_owned(),
        }
    }
}

/// Where bytes typed in the UI go.
enum Sink {
    Serial(WriteHalf<tokio_serial::SerialStream>),
    Pty(Box<dyn std::io::Write + Send>),
}

impl Sink {
    async fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        match self {
            Sink::Serial(write) => write.write_all(bytes).await,
            Sink::Pty(write) => write.write_all(bytes),
        }
    }
}

/// A hyperlink under a cell, and the cell it was under.
#[derive(Clone, Debug)]
pub struct LinkReply {
    pub x: u16,
    pub y: u16,
    pub uri: Option<String>,
}

pub struct Pane {
    latest: Arc<Mutex<Option<Frame>>>,
    tx: tokio::sync::mpsc::UnboundedSender<Command>,
    stats: Arc<Stats>,
    /// The last thing the pane copied, for the UI to put on the clipboard.
    copied: Arc<Mutex<Option<String>>>,
    /// The block the last `CaptureBlock` read out of the terminal, waiting for
    /// the UI to take it. The pager that will read it does not exist yet.
    captured: Arc<Mutex<Option<String>>>,
    /// The last `HyperlinkAt` answer. It carries the cell it was asked about,
    /// so a reply that arrives after the pointer moved can be dropped instead
    /// of opening the wrong link.
    link: Arc<Mutex<Option<LinkReply>>>,
    /// A fatal pane error. The pane thread owns the terminal, so a failure to
    /// open a port happens out of sight; this is how it reaches the UI.
    error: Arc<Mutex<Option<String>>>,
    /// The child is gone: `Ctrl-D`, `exit`, or a pty that would not read again.
    /// A shell that has ended has nothing left to show, so the UI closes it.
    ended: Arc<std::sync::atomic::AtomicBool>,
    /// The directory the session last reported with OSC 7.
    cwd: Arc<Mutex<Option<std::path::PathBuf>>>,
}

impl Pane {
    /// Spawn the pane thread. `wake` is fired whenever a new frame is ready.
    ///
    /// With no serial config the pane shows a demo screen; with one it opens
    /// the port and drives it.
    pub fn spawn(
        transport: Transport,
        log: Option<(PathBuf, serial::LogFormat)>,
        theme: Option<crate::theme::Theme>,
        budget: FrameBudget,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        let latest = Arc::new(Mutex::new(None::<Frame>));
        let published = Arc::clone(&latest);
        let stats = Arc::new(Stats::default());
        let counted = Arc::clone(&stats);
        let copied: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let copy_sink = Arc::clone(&copied);
        let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let captured_sink = Arc::clone(&captured);
        let link: Arc<Mutex<Option<LinkReply>>> = Arc::new(Mutex::new(None));
        let link_sink = Arc::clone(&link);
        let error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let failure = Arc::clone(&error);
        let ended = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ended_flag = Arc::clone(&ended);
        let cwd = Arc::new(Mutex::new(None));
        let cwd_seen = Arc::clone(&cwd);
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Command>();

        thread::Builder::new()
            .name("dtr-pane".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(err) => {
                        tracing::error!(%err, "pane runtime");
                        return;
                    }
                };
                let local = tokio::task::LocalSet::new();
                local.block_on(&rt, async move {
                    if let Err(err) = run(
                        rx,
                        published,
                        Arc::clone(&wake),
                        transport,
                        counted,
                        log,
                        copy_sink,
                        captured_sink,
                        link_sink,
                        Arc::clone(&ended_flag),
                        Arc::clone(&cwd_seen),
                        theme,
                        budget,
                    )
                    .await
                    {
                        tracing::error!(%err, "pane");
                        if let Ok(mut slot) = failure.lock() {
                            *slot = Some(format!("{err}"));
                        }
                        wake();
                    }
                });
            })?;

        Ok(Self {
            latest,
            tx,
            stats,
            error,
            copied,
            captured,
            link,
            ended,
            cwd,
        })
    }

    /// Take the newest frame, if the emulator produced one since the last call.
    pub fn take_frame(&self) -> Option<Frame> {
        self.latest.lock().ok().and_then(|mut slot| slot.take())
    }

    pub fn stats(&self) -> &Arc<Stats> {
        &self.stats
    }

    /// Text the pane copied, if it has produced any since the last call.
    pub fn take_copied(&self) -> Option<String> {
        self.copied.lock().ok().and_then(|mut slot| slot.take())
    }

    /// The last command block the pane captured, if one is waiting.
    pub fn take_captured(&self) -> Option<String> {
        self.captured.lock().ok().and_then(|mut slot| slot.take())
    }

    /// The last hyperlink answer, if one is waiting.
    pub fn take_link(&self) -> Option<LinkReply> {
        self.link.lock().ok().and_then(|mut slot| slot.take())
    }

    /// The pane's fatal error, if it hit one.
    /// Where the session says it is: the last OSC 7 it sent.
    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        self.cwd.lock().ok().and_then(|slot| slot.clone())
    }

    /// True once the process behind this pane has gone away.
    pub fn ended(&self) -> bool {
        self.ended.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|slot| slot.clone())
    }

    /// Queue bytes for the session. Non-blocking; safe from the UI thread.
    pub fn write(&self, bytes: Vec<u8>) {
        self.send(Command::Write(bytes));
    }

    /// Queue any command. Non-blocking; safe from the UI thread.
    pub fn send(&self, command: Command) {
        // A closed channel means the pane is gone; the app is already tearing
        // down or replacing it, so there is nothing useful to say.
        let _ = self.tx.send(command);
    }
}

/// How the child behind a shell pane ended.
struct ShellExit {
    /// The exit status, or `None` when a signal took it — which is never a user
    /// typing `exit`.
    code: Option<u32>,
    /// When it happened: a connection that stayed up for a while is not a flaky
    /// one, and the next drop is worth retrying straight away.
    at: std::time::Instant,
}

/// What the end of the program behind a shell pane means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ending {
    /// The user left: `exit`, `Ctrl-D`, or whatever status the shell chose.
    Chose,
    /// The connection failed under it.
    Dropped,
}

/// `ssh` exits 255 when ssh itself failed — the host is unreachable, the key was
/// rejected, the connection died — and anything else is the remote command's own
/// status, which means the session ended rather than the transport. A signal is
/// never a user.
fn ending(code: Option<u32>) -> Ending {
    match code {
        Some(255) | None => Ending::Dropped,
        Some(_) => Ending::Chose,
    }
}

/// How long a connection has to stay up before a drop is retried at once.
const BACKOFF_RESET: std::time::Duration = std::time::Duration::from_secs(10);
/// The first wait after a connection drops. Not zero: a connection that has just
/// died will not come back in the next millisecond, and a host that is down must
/// not be asked once a second for ever.
const RETRY_FIRST: std::time::Duration = std::time::Duration::from_secs(1);
const RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(30);

/// Waiting longer after every failed attempt, up to a cap.
#[derive(Default)]
struct Backoff {
    attempt: u32,
}

impl Backoff {
    fn reset(&mut self) {
        self.attempt = 0;
    }

    fn next(&mut self) -> std::time::Duration {
        let step = self.attempt.min(5);
        self.attempt = self.attempt.saturating_add(1);
        (RETRY_FIRST * 2u32.pow(step)).min(RETRY_MAX)
    }
}

/// The pieces of a running pty that the pane has to keep.
struct Open {
    /// Dropping it closes the pty, which is what ends the child.
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn std::io::Write + Send>,
}

/// Start a pty and the thread that reads it.
///
/// The child goes to that thread rather than staying in the pane: after EOF its
/// status is the only thing that says whether the session ended or its
/// connection dropped, and `wait` blocks, which the pane's own thread must not.
fn start_shell(
    config: &pty::Config,
    bytes: &tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    exited: &tokio::sync::mpsc::UnboundedSender<ShellExit>,
    log: &Option<Arc<LogWriter>>,
) -> anyhow::Result<Open> {
    let pty::Shell {
        master,
        child,
        reader,
        writer,
    } = config.open()?;
    let tx = bytes.clone();
    let done = exited.clone();
    let log = log.clone();
    let mut reader = reader;
    let mut child = child;
    std::thread::Builder::new()
        .name("dtr-pty".into())
        .spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    // EOF: the child closed the pty, so it has exited.
                    Ok(0) => break,
                    Ok(n) => {
                        log_chunk(&log, &buf[..n]);
                        if tx.send(buf[..n].to_vec()).is_err() {
                            // Nobody is listening any more: the pane is gone,
                            // and so is any reason to wait for this child.
                            return;
                        }
                    }
                    Err(err) => {
                        tracing::warn!(%err, "pty read");
                        break;
                    }
                }
            }
            let code = match child.wait() {
                Ok(status) => status.signal().is_none().then(|| status.exit_code()),
                Err(err) => {
                    tracing::warn!(%err, "pty wait");
                    None
                }
            };
            let _ = done.send(ShellExit {
                code,
                at: std::time::Instant::now(),
            });
        })?;
    Ok(Open { master, writer })
}

/// Put the terminal back into a state a fresh session can start in.
///
/// A connection can drop while a full-screen program is running, and the shell
/// that comes back would then start *inside* the alternate screen, with the dead
/// program's mouse modes still on. The text is deliberately left alone: the last
/// screen before the drop is the first thing one wants to read.
fn reset_modes(term: &mut Terminal<'_, '_>) {
    for mode in [
        Mode::ALT_SCREEN_SAVE,
        Mode::ALT_SCREEN,
        Mode::ALT_SCREEN_LEGACY,
        Mode::X10_MOUSE,
        Mode::NORMAL_MOUSE,
        Mode::BUTTON_MOUSE,
        Mode::ANY_MOUSE,
        Mode::UTF8_MOUSE,
        Mode::SGR_MOUSE,
        Mode::URXVT_MOUSE,
        Mode::BRACKETED_PASTE,
        Mode::KEYPAD_KEYS,
        // In application mode a shell would be typed `ESC O A` for an arrow
        // key, which readline happens to accept but is not what a shell asked
        // for — the mode belonged to the program that just died.
        Mode::DECCKM,
    ] {
        if let Err(err) = term.set_mode(mode, false) {
            tracing::debug!(%err, "could not clear a terminal mode");
        }
    }
    // The kitty keyboard protocol is not a mode: it is a stack of flag sets
    // pushed with `CSI > … u`. A program that died while holding one would have
    // the shell that replaces it typed `CSI 97;5u` for ctrl+a, which is bytes no
    // shell has ever read as a control character. Pop the stack, then zero what
    // is left. DECSCUSR goes with it: a bar cursor belongs to the insert mode of
    // the editor that dropped, not to the shell that comes back.
    term.vt_write(b"\x1b[<999u\x1b[=0u\x1b[0 q");
    // The cursor is the one mode that has to come *back*, and the loop above
    // cannot do it: a program that hid the cursor and then lost its connection
    // would leave the shell that replaces it with no cursor at all, which reads
    // as a frozen terminal.
    if let Err(err) = term.set_mode(Mode::CURSOR_VISIBLE, true) {
        tracing::debug!(%err, "could not show the cursor again");
    }
}

/// Notice when the program changed how keys are spelled, what the mouse means or
/// whether the cursor is showing, and say so once.
///
/// Only output can change a mode, so this runs after a burst and logs a change
/// rather than a burst: it is how "did nvim ask for the kitty protocol?" gets
/// answered from a log instead of a guess.
///
/// The key modes are only logged — the encoder reads them per keystroke. The
/// mouse flag is different: the UI decides what a press means before the pane
/// ever sees it, so that one has to cross threads. The cursor is different
/// again: it is the return value, because a cursor appearing or disappearing is
/// a change to the *picture* that the render state does not count as dirty.
#[derive(Default)]
struct KeyModes {
    cursor_keys: bool,
    kitty: u8,
    mouse: bool,
    /// Visible until something says otherwise: `?25` starts enabled, and the
    /// first read must not look like a change.
    cursor_visible: bool,
}

/// Notice the modes the UI or the encoders care about, and log a change once.
fn note_modes(term: &Terminal<'_, '_>, seen: &mut KeyModes, stats: &Stats) {
    let cursor_keys = term.mode(Mode::DECCKM).unwrap_or(false);
    let kitty = term
        .kitty_keyboard_flags()
        .map(|flags| flags.bits())
        .unwrap_or(0);
    if seen.cursor_keys != cursor_keys || seen.kitty != kitty {
        seen.cursor_keys = cursor_keys;
        seen.kitty = kitty;
        tracing::debug!(cursor_keys, kitty, "key modes");
    }
    let mouse = term.is_mouse_tracking().unwrap_or(false);
    if seen.mouse != mouse {
        seen.mouse = mouse;
        stats.set_mouse(mouse);
        // Which tracking mode, not just that there is one: the byte form of the
        // reports depends on it, and "nvim does not see my clicks" is a question
        // about 1000 versus 1002 versus no mode at all.
        tracing::debug!(mouse, "mouse tracking");
    }
    // Only a change is worth a line: btop toggles this once per frame it draws.
    let visible = term.mode(Mode::CURSOR_VISIBLE).unwrap_or(true);
    if seen.cursor_visible != visible {
        seen.cursor_visible = visible;
        tracing::debug!(visible, "cursor visibility");
    }
}

/// A line of dtr's own in the session's scrollback.
///
/// The shell cannot say this — it is gone — and a pane that simply stops looks
/// exactly like one waiting for input. It is written into the terminal rather
/// than the pty: nothing downstream is running that could read it.
#[allow(clippy::too_many_arguments)]
fn notice(
    term: &mut Terminal<'static, '_>,
    text: &str,
    state: &mut RenderState<'static>,
    rows: &mut RowIterator<'static>,
    cells: &mut CellIterator<'static>,
    out: &Mutex<Option<Frame>>,
    wake: &Arc<dyn Fn() + Send + Sync>,
    selection: &mut Option<SelectionAnchors>,
    shown: &mut Option<Cursor>,
) -> anyhow::Result<()> {
    let mut line = String::with_capacity(text.len() + 16);
    // CR LF, not LF: the session is raw, so nothing rewrites it for us.
    line.push_str("\r\n\x1b[33m");
    line.push_str(text);
    line.push_str("\x1b[0m\r\n");
    term.vt_write(line.as_bytes());
    publish(term, state, rows, cells, out, wake, true, selection, shown)
}

async fn run(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<Command>,
    out: Arc<Mutex<Option<Frame>>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    transport: Transport,
    stats: Arc<Stats>,
    log_path: Option<(PathBuf, serial::LogFormat)>,
    copied: Arc<Mutex<Option<String>>>,
    captured: Arc<Mutex<Option<String>>>,
    link: Arc<Mutex<Option<LinkReply>>>,
    ended: Arc<std::sync::atomic::AtomicBool>,
    cwd: Arc<Mutex<Option<std::path::PathBuf>>>,
    theme: Option<crate::theme::Theme>,
    budget: FrameBudget,
) -> anyhow::Result<()> {
    // Raw bytes as they arrive, before any mapping: this is what the wire said,
    // which is what you want when the point is to find out what the device did.
    let log: Option<Arc<LogWriter>> = match &log_path {
        Some((path, format)) => match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            Ok(file) => Some(Arc::new(LogWriter {
                file: Mutex::new(file),
                format: *format,
                started: std::time::Instant::now(),
            })),
            Err(err) => {
                tracing::warn!(path = %path.display(), %err, "cannot open the session log");
                None
            }
        },
        None => None,
    };
    // Terminal replies (device attributes, DECRQM, size reports) have to go
    // back out to the pty, or programs that probe capabilities — fish, tmux —
    // sit waiting on an answer that never comes.
    let replies: Rc<RefCell<Vec<u8>>> = Rc::new(RefCell::new(Vec::new()));
    let reply_sink = Rc::clone(&replies);

    let mut term = Terminal::new(COLS, ROWS)?;

    // OSC 52: a program (tmux, vim, an agent over ssh) asks the terminal to put
    // something on the user's clipboard. The write arrives here; the UI does the
    // actual clipboard work, because the pane has no window to ask.
    {
        let sink = Arc::clone(&copied);
        term.on_clipboard_write(move |_term, write| {
            // Every representation is the same value; text is what a terminal
            // can carry, and the first text/plain wins.
            let text = write
                .contents()
                .find(|content| content.mime.starts_with(b"text/"))
                .map(|content| String::from_utf8_lossy(content.data).into_owned());
            match text {
                Some(text) => {
                    tracing::debug!(bytes = text.len(), "clipboard write (osc 52)");
                    if let Ok(mut slot) = sink.lock() {
                        *slot = Some(text);
                    }
                    write.reply(Ok(()), false);
                }
                None => write.reply(
                    Err(libghostty_vt::terminal::ClipboardWriteError::Unsupported),
                    false,
                ),
            }
        })?;
    }
    if let Some(theme) = theme.as_ref() {
        if let Err(err) = theme.apply(&mut term) {
            tracing::warn!(theme = %theme.name, %err, "could not apply the theme");
        } else {
            tracing::info!(theme = %theme.name, "theme applied");
        }
    }
    term.on_pty_write(move |_term, data| {
        reply_sink.borrow_mut().extend_from_slice(data);
    })?;
    let mut state = RenderState::new()?;
    // Declared before the first publish: publishing is where a selection that
    // has scrolled out of the frame is dropped.
    let mut selection: Option<SelectionAnchors> = None;
    // The cursor the last frame carried, so `publish` can notice when the program
    // changed it without changing any cell — see the dirty check there.
    let mut shown: Option<Cursor> = None;
    let mut rows = RowIterator::new()?;
    let mut cells = CellIterator::new()?;

    let (bytes_tx, mut bytes_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    // Separate from `bytes_rx`: the pane keeps `bytes_tx` alive for reconnects,
    // so the byte channel never closes, and "the reader ended" needs its own
    // signal or a disconnect goes unnoticed.
    let (closed_tx, mut closed_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let mut sink: Option<Sink> = None;
    let mut source_alive = false;
    // What to reopen if the device disappears, and how to recognise it.
    let mut reconnect: Option<(serial::Config, Option<String>)> = None;
    // Set to make the current reader return, which the reconnect path treats as
    // a device that went away and brings back with the new settings.
    let mut serial_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut modem: Option<crate::modem::Modem> = None;
    let mut echo = false;
    // Pasting is paced only on a serial line: a device with a tiny input buffer
    // drops bytes when the whole clipboard arrives at once. A shell in a pty
    // has a proper buffer and wants the paste immediately — pacing there just
    // makes it feel broken.
    let paste_tick =
        matches!(transport, Transport::Serial(_)).then(|| std::time::Duration::from_millis(20));
    let mut paste: Vec<u8> = Vec::new();
    let mut paste_at: Option<tokio::time::Instant> = None;
    let (reconnect_tx, mut reconnect_rx) =
        tokio::sync::mpsc::unbounded_channel::<tokio_serial::SerialStream>();
    // Dropping the master closes the pty, which is what ends the child.
    let mut pty_keep: Option<Box<dyn MasterPty + Send>> = None;
    // A shell — local or ssh — is opened by one path, used at startup and after
    // every dropped connection. `open_at` is when to try next.
    let mut shell: Option<pty::Config> = None;
    let mut open_at: Option<tokio::time::Instant> = None;
    // Set when the next open is a reconnection, so the pane says so and puts the
    // session back where it was rather than starting it at home.
    let mut reopening = false;
    let mut opened_at = std::time::Instant::now();
    let mut backoff = Backoff::default();
    // The pty is made at the default size and told the real one as soon as the
    // UI resizes it. A connection that comes back needs the size the window has
    // *now*, so the loop remembers it.
    let mut pty_size = portable_pty::PtySize {
        rows: ROWS,
        cols: COLS,
        pixel_width: 0,
        pixel_height: 0,
    };
    // Separate from `closed_rx` because a shell's exit says something a serial
    // port's does not: how it exited, which is the difference between a session
    // the user ended and a connection that dropped under it.
    let (exit_tx, mut exit_rx) = tokio::sync::mpsc::unbounded_channel::<ShellExit>();
    // The last directory the session reported, whichever machine it is on. Not
    // `cwd`: a remote path is not a local directory, but it is where a reconnect
    // belongs.
    let mut last_dir: Option<String> = None;
    // Bytes typed before the first connection opened. `--send` is the command
    // line's "type this now", and it arrives before an ssh client has finished
    // starting; only the first connection holds them, because typing at a
    // session that is *reconnecting* is typing at a screen it never drew.
    let mut typed_early: Vec<u8> = Vec::new();
    let mut ever_opened = false;
    // A paste that arrived before the program could answer whether it wants
    // bracketed paste, and the deadline that stops it waiting for a program
    // that never speaks.
    let mut early_paste: Option<Vec<u8>> = None;
    let mut early_at: Option<tokio::time::Instant> = None;
    // The key encoder is the terminal's, not a table: what a key sends depends
    // on modes only the terminal knows (`input::fill` explains which). One per
    // pane, with one event reused, because both hold allocations and a keystroke
    // is not the place to make them.
    let (mut key_encoder, mut key_event) = match (KeyEncoder::new(), KeyEvent::new()) {
        (Ok(encoder), Ok(event)) => (Some(encoder), Some(event)),
        (encoder, event) => {
            tracing::error!(
                encoder = encoder.is_ok(),
                event = event.is_ok(),
                "no key encoder; keys will not be sent"
            );
            (None, None)
        }
    };
    // What the program last asked of the keyboard, so a change is logged once.
    // The cursor starts visible, because that is what a fresh terminal is.
    let mut keys_seen = KeyModes {
        cursor_visible: true,
        ..KeyModes::default()
    };
    // The mouse encoder, for the same reason: a click is a press, a release, or
    // motion, and which of those the program is told about depends on modes only
    // the terminal knows. The event holds the position, so it is reused too.
    let (mut mouse_encoder, mut mouse_event) = match (mouse::Encoder::new(), mouse::Event::new()) {
        (Ok(encoder), Ok(event)) => (Some(encoder), Some(event)),
        (encoder, event) => {
            tracing::error!(
                encoder = encoder.is_ok(),
                event = event.is_ok(),
                "no mouse encoder; clicks will not be reported"
            );
            (None, None)
        }
    };

    match transport {
        Transport::Serial(cfg) => {
            let stream = cfg.open()?;
            tracing::info!(port = %cfg.port, baud = cfg.baud, "serial open");
            // Before splitting: the duplicate has to come from the stream while
            // it is still one object.
            modem = crate::modem::Modem::new(&stream);
            if modem.is_some() {
                stats.set_dtr(true);
                stats.set_rts(true);
            }
            let (read_half, write_half) = tokio::io::split(stream);
            sink = Some(Sink::Serial(write_half));
            let mapper = cfg.cr_to_crlf.then(CrToCrlf::default);
            source_alive = true;
            stats.set_connected(true);
            let closed = closed_tx.clone();
            let tx = bytes_tx.clone();
            let reader_log = log.clone();
            let stop = Arc::clone(&serial_stop);
            tokio::task::spawn_local(async move {
                read_loop(read_half, mapper, tx, reader_log, stop).await;
                let _ = closed.send(());
            });
            if cfg.auto_reconnect {
                let identity = serial::identity(&cfg.port);
                if identity.is_some() {
                    tracing::info!(port = %cfg.port, serial = ?identity, "will follow this device across replugs");
                }
                reconnect = Some((cfg.clone(), identity));
            }
        }
        Transport::Shell(config) => {
            // Not opened here. An ssh session is a connection and a connection
            // drops, so the first connection goes through the same path as the
            // ones after it: a host that is not answering is a pane that waits,
            // not an app that exits with an error nobody can act on.
            shell = Some(config);
            open_at = Some(tokio::time::Instant::now());
        }
        Transport::Replay(path) => {
            let bytes = std::fs::read(&path)?;
            tracing::info!(path = %path.display(), bytes = bytes.len(), "replay");
            source_alive = true;
            let tx = bytes_tx.clone();
            let replay_log = log.clone();
            tokio::task::spawn_local(async move {
                for chunk in bytes.chunks(256) {
                    log_chunk(&replay_log, chunk);
                    if tx.send(chunk.to_vec()).is_err() {
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            });
        }
        Transport::Demo => term.vt_write(DEMO),
    }

    // The alias, when this pane is a connection rather than a shell of its own.
    let alias = shell.as_ref().and_then(|config| config.remote.clone());

    // Always publish once, even if the terminal is still clean: a pane that
    // has produced no output yet still has an (empty) grid to show.
    publish(
        &mut term,
        &mut state,
        &mut rows,
        &mut cells,
        &out,
        &wake,
        true,
        &mut selection,
        &mut shown,
    )?;

    let mut sniffer = CwdSniffer::default();
    let mut marks = PromptMarks::default();
    // Publishing is what a flood costs, not parsing: one frame rebuilds every
    // cell of the grid. So a pane publishes at most once per budget — and, when
    // it has to wait, a timer arm publishes the frame it owes rather than
    // leaving the picture stale until the next byte arrives.
    let mut next_publish = tokio::time::Instant::now();
    let mut publish_pending = false;
    loop {
        tokio::select! {
            chunk = bytes_rx.recv(), if source_alive => {
                match chunk {
                    Some(bytes) => {
                        // A flood arrives as thousands of small chunks — the pty
                        // hands over only what is queued when the reader wakes.
                        // Parse everything already queued, for at most one
                        // budget's worth of time, then publish once; see the
                        // `next_publish` gate below.
                        // Read per burst: the panel can change under the
                        // window, and the UI rewrites this when it does.
                        let gate =
                            std::time::Duration::from_nanos(budget.load(Ordering::Relaxed));
                        let deadline = tokio::time::Instant::now() + gate;
                        let mut bytes = bytes;
                        let (mut chunks, mut total) = (0u32, 0usize);
                        loop {
                            chunks += 1;
                            total += bytes.len();
                            stats.add_rx(bytes.len());
                            if let Some(report) = sniffer.feed(&bytes) {
                                // Where the session is, on whichever machine owns
                                // it: this is what a dropped connection comes
                                // back to.
                                last_dir =
                                    Some(report.path.to_string_lossy().into_owned());
                                // Only a path on *this* machine is a local
                                // directory. A remote session never adopts its
                                // own, even when the far host happens to share
                                // this machine's name.
                                if alias.is_none() && crate::uri::is_local_host(&report.host)
                                {
                                    if let Ok(mut slot) = cwd.lock() {
                                        *slot = Some(report.path);
                                    }
                                }
                            }
                            marks.write(&mut term, &bytes)?;
                            if tokio::time::Instant::now() >= deadline {
                                break;
                            }
                            match bytes_rx.try_recv() {
                                Ok(next) => bytes = next,
                                Err(_) => break,
                            }
                        }
                        flush_replies(&replies, &mut sink).await;
                        // The program's answer to "how should a key look",
                        // "what does the mouse mean" and "is the cursor showing"
                        // is in the terminal now — the encoders read it per event
                        // — and a *change* is worth one line of log.
                        note_modes(&term, &mut keys_seen, &stats);
                        // The program has spoken, so whatever it wanted to say
                        // about pasting is in the terminal now — which is the
                        // answer a held paste was waiting for.
                        if let Some(text) = early_paste.take() {
                            early_at = None;
                            write_paste(
                                &mut term,
                                &mut sink,
                                &stats,
                                paste_tick,
                                &mut paste,
                                &mut paste_at,
                                &mut typed_early,
                                ever_opened,
                                text,
                            )
                            .await;
                        }
                        tracing::debug!(chunks, bytes = total, "burst parsed");
                        if tokio::time::Instant::now() >= next_publish {
                            publish(&mut term, &mut state, &mut rows, &mut cells, &out, &wake, false, &mut selection, &mut shown)?;
                            next_publish = tokio::time::Instant::now() + gate;
                            publish_pending = false;
                        } else {
                            publish_pending = true;
                        }
                    }
                    // The reader is gone — the child closed the pty. This arm is
                    // ready for ever once the channel is empty, so stop
                    // selecting on it rather than spinning through it.
                    None => source_alive = false,
                }
            }
            _ = tokio::time::sleep_until(next_publish), if publish_pending => {
                publish(&mut term, &mut state, &mut rows, &mut cells, &out, &wake, false, &mut selection, &mut shown)?;
                next_publish = tokio::time::Instant::now()
                    + std::time::Duration::from_nanos(budget.load(Ordering::Relaxed));
                publish_pending = false;
            }
            _ = closed_rx.recv(), if source_alive => {
                source_alive = false;
                stats.set_connected(false);
                if let Some((cfg, identity)) = reconnect.clone() {
                    tracing::warn!(port = %cfg.port, "device gone; watching for it");
                    let tx = reconnect_tx.clone();
                    tokio::task::spawn_local(async move {
                        loop {
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                            if tx.is_closed() {
                                return;
                            }
                            let path = match &identity {
                                Some(serial) => serial::find_by_serial(serial),
                                None => Some(cfg.port.clone()),
                            };
                            let Some(path) = path else { continue };
                            match cfg.open_at(&path) {
                                Ok(stream) => {
                                    tracing::info!(port = %path, "device is back");
                                    let _ = tx.send(stream);
                                    return;
                                }
                                Err(err) => {
                                    tracing::debug!(port = %path, %err, "still gone");
                                }
                            }
                        }
                    });
                }
            }
            _ = tokio::time::sleep_until(
                    open_at.unwrap_or_else(tokio::time::Instant::now)
                ), if open_at.is_some() && shell.is_some() =>
            {
                open_at = None;
                let config = shell.as_ref().expect("guarded by `shell.is_some()`");
                let mut config = if reopening {
                    config.reopened(last_dir.as_deref())
                } else {
                    config.clone()
                };
                // The size the window has now, not the one it had when the
                // session first opened.
                config.size = Some(pty_size);
                if reopening {
                    // The shell that died cannot say this, and a pane that
                    // simply stops is indistinguishable from one waiting for a
                    // key. Written before the attempt, so whatever ssh says
                    // about it appears underneath.
                    let where_ = match &last_dir {
                        Some(dir) => format!(" in {dir}"),
                        None => String::new(),
                    };
                    notice(
                        &mut term,
                        &format!(
                            "-- reconnecting to {}{where_} --",
                            alias.as_deref().unwrap_or("the session")
                        ),
                        &mut state,
                        &mut rows,
                        &mut cells,
                        &out,
                        &wake,
                        &mut selection,
                        &mut shown,
                    )?;
                }
                match start_shell(&config, &bytes_tx, &exit_tx, &log) {
                    Ok(open) => {
                        pty_keep = Some(open.master);
                        sink = Some(Sink::Pty(open.writer));
                        source_alive = true;
                        opened_at = std::time::Instant::now();
                        ever_opened = true;
                        tracing::info!(reconnecting = reopening, "session opened");
                        if !typed_early.is_empty() {
                            let bytes = std::mem::take(&mut typed_early);
                            if let Some(sink) = sink.as_mut() {
                                match sink.write(&bytes).await {
                                    Ok(()) => stats.add_tx(bytes.len()),
                                    Err(err) => {
                                        tracing::warn!(%err, "write the early keystrokes")
                                    }
                                }
                            }
                        }
                    }
                    Err(err) => {
                        // A local shell that will not start is a bug in the
                        // installation, and saying so is better than a pane
                        // retrying something that cannot work. A connection is
                        // different: it is exactly what has to be retried.
                        if alias.is_none() {
                            return Err(err);
                        }
                        tracing::warn!(%err, alias = ?alias, "cannot reopen the session");
                        notice(
                            &mut term,
                            &format!("-- cannot start ssh: {err} --"),
                            &mut state,
                            &mut rows,
                            &mut cells,
                            &out,
                            &wake,
                            &mut selection,
                            &mut shown,
                        )?;
                        let wait = backoff.next();
                        tracing::info!(ms = wait.as_millis() as u64, "retrying");
                        open_at = Some(tokio::time::Instant::now() + wait);
                        reopening = true;
                    }
                }
            }
            exit = exit_rx.recv() => {
                // The loop holds a sender for as long as the pane exists, so the
                // channel cannot end while anything is listening.
                let Some(exit) = exit else { continue };
                // The child is gone. What that means is the whole question: an
                // ssh client that exits because the user typed `exit` is the end
                // of the session, and one that exits because the network went
                // away is a connection to reopen.
                if alias.is_some() && ending(exit.code) == Ending::Dropped {
                    if exit.at.duration_since(opened_at) >= BACKOFF_RESET {
                        // It was up for a while, so this is not a flaky host:
                        // the next drop is worth a prompt retry.
                        backoff.reset();
                    }
                    let wait = backoff.next();
                    tracing::warn!(
                        code = exit.code,
                        ms = wait.as_millis() as u64,
                        "connection lost; reopening"
                    );
                    sink = None;
                    pty_keep = None;
                    // Whatever was on screen may have been a full-screen
                    // program with its modes still set; the shell that comes
                    // back must not start inside them.
                    reset_modes(&mut term);
                    note_modes(&term, &mut keys_seen, &stats);
                    reopening = true;
                    open_at = Some(tokio::time::Instant::now() + wait);
                } else {
                    tracing::info!(code = exit.code, "session ended");
                    source_alive = false;
                    ended.store(true, std::sync::atomic::Ordering::Relaxed);
                    wake();
                }
            }
            _ = tokio::time::sleep_until(
                    early_at.unwrap_or_else(tokio::time::Instant::now)
                ), if early_at.is_some() =>
            {
                early_at = None;
                if let Some(text) = early_paste.take() {
                    write_paste(
                        &mut term,
                        &mut sink,
                        &stats,
                        paste_tick,
                        &mut paste,
                        &mut paste_at,
                        &mut typed_early,
                        ever_opened,
                        text,
                    )
                    .await;
                }
            }
            _ = tokio::time::sleep_until(paste_at.unwrap_or_else(tokio::time::Instant::now)),
                if paste_at.is_some() && !paste.is_empty() =>
            {
                const CHUNK: usize = 128;
                let tick = paste_tick.unwrap_or_default();
                let take = CHUNK.min(paste.len());
                let chunk: Vec<u8> = paste.drain(..take).collect();
                if let Some(sink) = sink.as_mut() {
                    if let Err(err) = sink.write(&chunk).await {
                        tracing::warn!(%err, "paste write");
                        paste.clear();
                    } else {
                        stats.add_tx(chunk.len());
                    }
                }
                paste_at = if paste.is_empty() {
                    None
                } else {
                    Some(tokio::time::Instant::now() + tick)
                };
            }
            stream = reconnect_rx.recv(), if !source_alive && reconnect.is_some() => {
                if let Some(stream) = stream {
                    let (read_half, write_half) = tokio::io::split(stream);
                    sink = Some(Sink::Serial(write_half));
                    let mapper = reconnect
                        .as_ref()
                        .map(|(cfg, _)| cfg.cr_to_crlf)
                        .unwrap_or(false)
                        .then(CrToCrlf::default);
                    source_alive = true;
                    stats.set_connected(true);
                    let closed = closed_tx.clone();
                    let tx = bytes_tx.clone();
                    let reader_log = log.clone();
                    // A fresh flag per connection: the old one is spent.
                    serial_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let stop = Arc::clone(&serial_stop);
                    tokio::task::spawn_local(async move {
                        read_loop(read_half, mapper, tx, reader_log, stop).await;
                        let _ = closed.send(());
                    });
                }
            }
            cmd = rx.recv() => {
                let Some(command) = cmd else { break };
                match command {
                    Command::Write(bytes) => {
                        // What actually reaches the session, and how often: the
                        // other half of a doubled keystroke.
                        tracing::debug!(
                            bytes = ?String::from_utf8_lossy(&bytes).escape_debug().to_string(),
                            "write"
                        );
                        deliver(
                            &bytes,
                            &mut term,
                            &stats,
                            &mut sink,
                            &mut typed_early,
                            ever_opened,
                        )
                        .await;
                        // Local echo is for devices that do not echo: the bytes
                        // are shown without being sent anywhere else.
                        if echo {
                            term.vt_write(&bytes);
                            publish(&mut term, &mut state, &mut rows, &mut cells, &out, &wake, false, &mut selection, &mut shown)?;
                        }
                    }
                    Command::Key(stroke) => {
                        let (Some(encoder), Some(event)) =
                            (key_encoder.as_mut(), key_event.as_mut())
                        else {
                            continue;
                        };
                        crate::input::fill(event, &stroke);
                        // Per key, not once: a program can change its modes
                        // between two keystrokes, and the encoder option that
                        // matters is `DECCKM` as much as the kitty flags.
                        encoder.set_options_from_terminal(&term);
                        let mut bytes = Vec::new();
                        if let Err(err) = encoder.encode_to_vec(event, &mut bytes) {
                            tracing::warn!(%err, "could not encode a key");
                            continue;
                        }
                        if bytes.is_empty() {
                            // A release under legacy encoding, a modifier on its
                            // own, a key the protocol has nothing to say about:
                            // the encoder is not the least surprised, and neither
                            // is this.
                            tracing::debug!(key = ?stroke.key, "a key with no bytes");
                            continue;
                        }
                        tracing::debug!(
                            key = ?stroke.key,
                            bytes = ?String::from_utf8_lossy(&bytes).escape_debug().to_string(),
                            "key"
                        );
                        deliver(
                            &bytes,
                            &mut term,
                            &stats,
                            &mut sink,
                            &mut typed_early,
                            ever_opened,
                        )
                        .await;
                    }
                    // Same argument as a key: what a click means is the
                    // terminal's state, and only the terminal has it. The encoder
                    // drops what the modes do not report — a release in X10 mode,
                    // motion nobody asked for — so an event with no bytes is the
                    // normal case and not an error.
                    Command::Mouse {
                        action,
                        button,
                        mods,
                        x,
                        y,
                        down,
                    } => {
                        let (Some(encoder), Some(event)) =
                            (mouse_encoder.as_mut(), mouse_event.as_mut())
                        else {
                            continue;
                        };
                        encoder
                            // Per event, not once: a program can turn tracking on
                            // between two clicks, and nvim does exactly that when
                            // it enters a window that maps the mouse.
                            .set_options_from_terminal(&term)
                            .set_size(mouse_size(&term))
                            .set_any_button_pressed(down)
                            // A cell, not a pixel: the grid is all the program
                            // can see, so one report per pixel of a drag is
                            // thousands of reports of the same cell.
                            .set_track_last_cell(true);
                        event
                            .set_action(action)
                            .set_button(button)
                            .set_mods(mods)
                            .set_position(mouse::Position { x, y });
                        let mut bytes = Vec::new();
                        if let Err(err) = encoder.encode_to_vec(event, &mut bytes) {
                            tracing::warn!(%err, "could not encode a mouse event");
                            continue;
                        }
                        if bytes.is_empty() {
                            tracing::debug!(?action, ?button, "a mouse event with no bytes");
                            continue;
                        }
                        tracing::debug!(
                            ?action,
                            ?button,
                            x,
                            y,
                            bytes = ?String::from_utf8_lossy(&bytes).escape_debug().to_string(),
                            "mouse"
                        );
                        deliver(
                            &bytes,
                            &mut term,
                            &stats,
                            &mut sink,
                            &mut typed_early,
                            ever_opened,
                        )
                        .await;
                    }
                    Command::Dtr(level) => {
                        if let Some(modem) = modem.as_mut() {
                            match modem.set_dtr(level) {
                                Ok(()) => stats.set_dtr(level),
                                Err(err) => tracing::warn!(%err, "set DTR"),
                            }
                        }
                    }
                    Command::Rts(level) => {
                        if let Some(modem) = modem.as_mut() {
                            match modem.set_rts(level) {
                                Ok(()) => stats.set_rts(level),
                                Err(err) => tracing::warn!(%err, "set RTS"),
                            }
                        }
                    }
                    Command::Break => {
                        if let Some(modem) = modem.as_mut() {
                            if let Err(err) = modem.pulse_break() {
                                tracing::warn!(%err, "break");
                            }
                        }
                    }
                    Command::Resize {
                        cols: new_cols,
                        rows: new_rows,
                        width_px,
                        height_px,
                    } => {
                        if new_cols == 0 || new_rows == 0 {
                            continue;
                        }
                        // The core's pixel arguments are the *cell* size, not the
                        // screen's: it multiplies by the grid for the size it
                        // reports (CSI 18 t, image protocols) and divides a
                        // pointer by it to reach a cell. Handing it the pane's own
                        // size made every report 80× too wide and put every click
                        // in the top-left cell — which is how this was found.
                        let cell_width = width_px / u32::from(new_cols).max(1);
                        let cell_height = height_px / u32::from(new_rows).max(1);
                        if let Err(err) = term.resize(new_cols, new_rows, cell_width, cell_height)
                        {
                            tracing::warn!(%err, "resize the grid");
                        }
                        // The shell needs the size too, or it wraps for the old
                        // width and the terminal shows a staircase. The size is
                        // remembered even with no pty behind it: a connection
                        // that comes back is opened at the size the window has
                        // *now*, not the one it had when the tab was opened.
                        pty_size = portable_pty::PtySize {
                            rows: new_rows,
                            cols: new_cols,
                            pixel_width: u16::try_from(width_px).unwrap_or(u16::MAX),
                            pixel_height: u16::try_from(height_px).unwrap_or(u16::MAX),
                        };
                        if let Some(pty) = pty_keep.as_mut() {
                            if let Err(err) = pty.resize(pty_size) {
                                tracing::warn!(%err, "resize the pty");
                            }
                        }
                        tracing::debug!(cols = new_cols, rows = new_rows, "resized");
                        publish(&mut term, &mut state, &mut rows, &mut cells, &out, &wake, true, &mut selection, &mut shown)?;
                    }
                    Command::Scroll(lines) => {
                        term.scroll_viewport(libghostty_vt::terminal::ScrollViewport::Delta(lines));
                        // Forced: a viewport move does not necessarily mark the
                        // render state dirty, and a scroll that does not repaint
                        // looks like a broken wheel.
                        publish(&mut term, &mut state, &mut rows, &mut cells, &out, &wake, true, &mut selection, &mut shown)?;
                        note_scrollback(&term, &stats);
                    }
                    Command::ScrollPrompt(delta) => {
                        term.scroll_viewport(
                            libghostty_vt::terminal::ScrollViewport::DeltaPrompt(delta),
                        );
                        // Forced, for the same reason as `Scroll`: a viewport
                        // move does not necessarily dirty the render state.
                        publish(&mut term, &mut state, &mut rows, &mut cells, &out, &wake, true, &mut selection, &mut shown)?;
                        note_scrollback(&term, &stats);
                    }
                    Command::CaptureBlock => {
                        match capture_block(&term, &marks) {
                            Ok(Some(text)) => {
                                tracing::info!(
                                    bytes = text.len(),
                                    lines = text.lines().count(),
                                    "captured the last command block"
                                );
                                if let Ok(mut slot) = captured.lock() {
                                    *slot = Some(text);
                                }
                                wake();
                            }
                            Ok(None) => tracing::info!(
                                "nothing to capture: no prompt marks, or no command yet"
                            ),
                            Err(err) => tracing::warn!(%err, "could not capture the block"),
                        }
                    }
                    Command::ScrollEdge { top } => {
                        term.scroll_viewport(if top {
                            libghostty_vt::terminal::ScrollViewport::Top
                        } else {
                            libghostty_vt::terminal::ScrollViewport::Bottom
                        });
                        publish(&mut term, &mut state, &mut rows, &mut cells, &out, &wake, true, &mut selection, &mut shown)?;
                        note_scrollback(&term, &stats);
                    }
                    Command::Echo(on) => echo = on,
                    Command::SetSerial {
                        baud,
                        cr_to_crlf,
                        auto_reconnect,
                    } => {
                        if let Some((cfg, _)) = reconnect.as_mut() {
                            let reconnect_needed =
                                cfg.baud != baud || cfg.cr_to_crlf != cr_to_crlf;
                            cfg.baud = baud;
                            cfg.cr_to_crlf = cr_to_crlf;
                            cfg.auto_reconnect = auto_reconnect;
                            if reconnect_needed && source_alive {
                                tracing::info!(baud, cr_to_crlf, "serial settings changed; reconnecting");
                                serial_stop.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                    }
                    Command::SetTheme(next) => {
                        if let Some(next) = next.as_ref() {
                            match next.apply(&mut term) {
                                Ok(()) => {
                                    tracing::info!(theme = %next.name, "theme switched");
                                    publish(&mut term, &mut state, &mut rows, &mut cells, &out, &wake, true, &mut selection, &mut shown)?;
                                }
                                Err(err) => tracing::warn!(%err, "could not switch theme"),
                            }
                        }
                    }
                    Command::SelectStart { x, y } => {
                        match SelectionAnchors::at(&term, x, y) {
                            Ok(anchors) => {
                                selection = Some(anchors);
                                publish(
                                    &mut term, &mut state, &mut rows, &mut cells, &out, &wake,
                                    true, &mut selection, &mut shown,
                                )?;
                            }
                            Err(err) => tracing::warn!(%err, "could not start a selection"),
                        }
                    }
                    Command::SelectHead { x, y } => {
                        let point = Point::Viewport(PointCoordinate {
                            x,
                            y: u32::from(y),
                        });
                        let moved = match selection.as_mut() {
                            Some(anchors) => anchors
                                .head
                                .set(&mut term, point)
                                .map(|_| anchors.deliberate = true)
                                .is_ok(),
                            None => false,
                        };
                        if moved {
                            publish(
                                &mut term, &mut state, &mut rows, &mut cells, &out, &wake,
                                true, &mut selection, &mut shown,
                            )?;
                        }
                    }
                    Command::SelectWord { x, y } => {
                        if let Some(anchors) = select_word(&term, x, y) {
                            selection = Some(anchors);
                            publish(
                                &mut term, &mut state, &mut rows, &mut cells, &out, &wake,
                                true, &mut selection, &mut shown,
                            )?;
                        }
                    }
                    Command::SelectLine { x, y } => {
                        if let Some(anchors) = select_line(&term, x, y) {
                            selection = Some(anchors);
                            publish(
                                &mut term, &mut state, &mut rows, &mut cells, &out, &wake,
                                true, &mut selection, &mut shown,
                            )?;
                        }
                    }
                    Command::HyperlinkAt { x, y } => {
                        let uri = hyperlink_at(&term, x, y);
                        tracing::debug!(x, y, ?uri, "hyperlink");
                        if let Ok(mut slot) = link.lock() {
                            *slot = Some(LinkReply { x, y, uri });
                        }
                        wake();
                    }
                    Command::CopySelection => {
                        let text = selection
                            .as_ref()
                            .and_then(|anchors| anchors.text(&term))
                            .unwrap_or_default();
                        if let Ok(mut slot) = copied.lock() {
                            *slot = Some(text);
                        }
                        wake();
                    }
                    Command::Copy => {
                        // Read the text out of the terminal now. The published
                        // frame cannot be used: the UI takes it out of the slot
                        // to draw it, so a copy would find nothing there — which
                        // is exactly the "copied 0 chars" that shipped.
                        let text = match copy_text(&mut term, &mut state, &mut rows, &mut cells) {
                            Ok(text) => text,
                            Err(err) => {
                                tracing::warn!(%err, "could not read the screen to copy");
                                String::new()
                            }
                        };
                        if let Ok(mut slot) = copied.lock() {
                            *slot = Some(text);
                        }
                        wake();
                    }
                    Command::Paste(bytes) => {
                        if sink.is_none() && !ever_opened {
                            // A paste at a session that has not opened yet
                            // (`--paste` is instant, and a tab can be pasted
                            // into before its shell starts): held as *text*,
                            // because the wrapping is the program's answer and
                            // no program is running yet to give one.
                            early_paste = Some(bytes);
                            early_at = Some(tokio::time::Instant::now() + EARLY_PASTE);
                            continue;
                        }
                        write_paste(
                            &mut term,
                            &mut sink,
                            &stats,
                            paste_tick,
                            &mut paste,
                            &mut paste_at,
                            &mut typed_early,
                            ever_opened,
                            bytes,
                        )
                        .await;
                    }
                }
            }
        }
    }

    Ok(())
}

/// The bytes a paste puts on the wire.
///
/// Two things happen here, and both are the terminal's job rather than the
/// clipboard's.
///
/// **Line breaks become CR.** A clipboard holds LF on Wayland and CRLF on
/// Windows, while a pty reads the *CR* that the Enter key sends; a program that
/// gets a bare LF sees a literal ctrl-J, which is not the key it has bound to
/// "new line". egui-winit has already turned CRLF into LF by the time the text
/// reaches us, so this is LF → CR, with CRLF collapsing to one break.
///
/// **A program that asked for bracketed paste gets markers around it.** `ESC [
/// ? 2004 h` is how nvim, vim, fish, zsh and readline say "wrap a paste, so I
/// can insert it literally instead of running every byte through my input
/// loop" — which is what keeps the lines of a block without re-running
/// autoindent, abbreviations or a one-key mapping on the way in. It is also what
/// makes a paste into a *shell* not execute each line as a command. The mode is
/// the program's, the terminal is the only thing that knows it, and so this
/// decision belongs to the pane.
/// How long a paste waits for a program that has not said anything yet.
///
/// A program can only ask for bracketed paste by writing to the terminal, so a
/// paste that arrives before the program has *spoken* has no answer to honour.
/// Any output at all flushes it sooner; this is the fallback for a program that
/// stays silent (a `cat`), so a scripted paste is never silently dropped.
const EARLY_PASTE: std::time::Duration = std::time::Duration::from_millis(250);

fn pasted(bracketed: bool, text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 16);
    if bracketed {
        out.extend_from_slice(b"\x1b[200~");
    }
    let mut bytes = text.iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        match byte {
            b'\r' => {
                out.push(b'\r');
                // CRLF is one line break, not two.
                if bytes.peek() == Some(&b'\n') {
                    bytes.next();
                }
            }
            b'\n' => out.push(b'\r'),
            _ => out.push(byte),
        }
    }
    if bracketed {
        out.extend_from_slice(b"\x1b[201~");
    }
    out
}

/// Put a paste on the wire, honouring what the program asked for.
///
/// One place, because a paste can be written the moment it arrives and a held
/// Put bytes on the session, wherever the session happens to be.
///
/// Shared by typing, a key the encoder produced and a paste, because the rule is
/// the same for all three: follow the cursor when the viewport is scrolled back
/// (typing at the live screen is what a person means), hold what arrives before
/// the first connection, and drop what is typed at a connection that is not
/// there.
async fn deliver(
    bytes: &[u8],
    term: &mut Terminal<'_, '_>,
    stats: &Stats,
    sink: &mut Option<Sink>,
    typed_early: &mut Vec<u8>,
    ever_opened: bool,
) {
    if stats.scrollback() > 0 {
        term.scroll_viewport(libghostty_vt::terminal::ScrollViewport::Bottom);
        note_scrollback(term, stats);
    }
    if let Some(sink) = sink.as_mut() {
        match sink.write(bytes).await {
            Ok(()) => stats.add_tx(bytes.len()),
            Err(err) => tracing::warn!(%err, "write to session"),
        }
    } else if !ever_opened {
        // Nothing is open *yet*: `--send` arrives before the first connection
        // does, and an ssh client takes a moment to start. Held, not dropped, or
        // the flag would work only when the host was fast.
        typed_early.extend_from_slice(bytes);
    } else {
        // Typed at a connection that is not there. Dropped rather than queued: a
        // shell that comes back has no idea what was typed at a screen it never
        // drew.
        tracing::debug!(bytes = bytes.len(), "no session; the keystroke is dropped");
    }
}

/// one is written later — and the answer to "bracketed?" has to be read from the
/// terminal at the moment of writing, not at the moment of arriving.
async fn write_paste(
    term: &mut Terminal<'static, '_>,
    sink: &mut Option<Sink>,
    stats: &Stats,
    paste_tick: Option<std::time::Duration>,
    paste: &mut Vec<u8>,
    paste_at: &mut Option<tokio::time::Instant>,
    typed_early: &mut Vec<u8>,
    ever_opened: bool,
    text: Vec<u8>,
) {
    // The program's own request, read from the terminal it was made on: only
    // the pane has the terminal, and the UI has no business knowing what a
    // program asked for.
    let bracketed = term.mode(Mode::BRACKETED_PASTE).unwrap_or(false);
    let bytes = pasted(bracketed, &text);
    tracing::debug!(bracketed, bytes = bytes.len(), "paste");
    match paste_tick {
        // No pacing: hand the whole thing over at once.
        None => deliver(&bytes, term, stats, sink, typed_early, ever_opened).await,
        Some(_) => {
            *paste = bytes;
            *paste_at = Some(tokio::time::Instant::now());
        }
    }
}

struct LogWriter {
    file: Mutex<std::fs::File>,
    format: serial::LogFormat,
    started: std::time::Instant,
}

/// The visible screen as text: rows trimmed of trailing blanks, and no empty
/// lines off the bottom. This is what "copy" means in a terminal — what you can
/// see, not the whole scrollback.
pub(crate) fn frame_text(frame: &Frame) -> String {
    let cols = frame.cols as usize;
    let mut lines: Vec<String> = frame
        .cells
        .chunks(cols.max(1))
        .map(|row| {
            let mut line: String = row.iter().map(|cell| cell.text.as_str()).collect();
            let trimmed = line.trim_end().len();
            line.truncate(trimmed);
            line
        })
        .collect();
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

/// The text a copy command wants: the whole screen, or a cell range.
fn copy_text(
    term: &mut Terminal<'static, '_>,
    state: &mut RenderState<'static>,
    rows: &mut RowIterator<'static>,
    cells: &mut CellIterator<'static>,
) -> anyhow::Result<String> {
    let snapshot = state.update(term)?;
    let mut frame = Frame {
        cols: snapshot.cols()?,
        rows: snapshot.rows()?,
        default_fg: [0; 3],
        default_bg: [0; 3],
        cursor: None,
        selection: None,
        cells: Vec::with_capacity(usize::from(snapshot.cols()?) * usize::from(snapshot.rows()?)),
    };
    fill_cells(&snapshot, rows, cells, &mut frame)?;
    Ok(frame_text(&frame))
}

/// The geometry the mouse encoder needs, read from the terminal itself.
///
/// `resize` is what set both the cell count and the pixel size, so the division
/// here is the same arithmetic the renderer did — one cell size, not a second
/// copy of it that can fall out of step with the first.
fn mouse_size(term: &Terminal<'_, '_>) -> mouse::EncoderSize {
    let cols = u32::from(term.cols().unwrap_or(COLS)).max(1);
    let rows = u32::from(term.rows().unwrap_or(ROWS)).max(1);
    let width = term.width_px().unwrap_or(0);
    let height = term.height_px().unwrap_or(0);
    mouse::EncoderSize {
        screen_width: width,
        screen_height: height,
        // Never zero: the encoder divides by these, and a pane that has not been
        // resized yet still has to answer a click instead of panicking.
        cell_width: (width / cols).max(1),
        cell_height: (height / rows).max(1),
        // The grid is its own rectangle: the tab strip and the status bar are
        // outside the pixels the pane was resized to, so there is nothing to
        // subtract.
        padding_top: 0,
        padding_bottom: 0,
        padding_right: 0,
        padding_left: 0,
    }
}

/// Record how far the viewport is from the live screen.
fn note_scrollback(term: &Terminal<'_, '_>, stats: &Stats) {
    let below = match term.scrollbar() {
        Ok(scrollbar) => scrollbar
            .total
            .saturating_sub(scrollbar.offset + scrollbar.len),
        Err(_) => 0,
    };
    stats.set_scrollback(below);
}

/// Append bytes to the session log, in whichever shape it was asked for.
fn log_chunk(log: &Option<Arc<LogWriter>>, bytes: &[u8]) {
    let Some(log) = log else { return };
    let mut line = Vec::with_capacity(bytes.len() + 24);
    match log.format {
        serial::LogFormat::Raw => line.extend_from_slice(bytes),
        serial::LogFormat::Timestamped => {
            let elapsed = log.started.elapsed().as_secs_f64();
            line.extend_from_slice(format!("[+{elapsed:.3}s] ").as_bytes());
            for byte in bytes {
                match byte {
                    b'\r' => line.extend_from_slice(b"\\r"),
                    b'\n' => line.extend_from_slice(b"\\n"),
                    b'\t' => line.extend_from_slice(b"\\t"),
                    0x20..=0x7e => line.push(*byte),
                    other => line.extend_from_slice(format!("\\x{other:02x}").as_bytes()),
                }
            }
            line.push(b'\n');
        }
    }
    match log.file.lock() {
        Ok(mut file) => {
            if let Err(err) = file.write_all(&line) {
                tracing::warn!(%err, "session log write");
            }
        }
        Err(err) => tracing::warn!("session log poisoned: {err}"),
    }
}

/// Write any bytes the terminal produced in response to a query back to the
/// session. Without this a shell that probes DA1/XTGETTCAP waits forever.
async fn flush_replies(replies: &RefCell<Vec<u8>>, sink: &mut Option<Sink>) {
    let out: Vec<u8> = replies.borrow_mut().drain(..).collect();
    if out.is_empty() {
        return;
    }
    if let Some(sink) = sink.as_mut() {
        if let Err(err) = sink.write(&out).await {
            tracing::warn!(%err, "write terminal reply");
        }
    }
}

/// Read the port until it fails or closes, mapping bytes on the way through.
async fn read_loop(
    mut reader: ReadHalf<tokio_serial::SerialStream>,
    mut mapper: Option<CrToCrlf>,
    tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    log: Option<Arc<LogWriter>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
) {
    let mut buf = [0u8; 8192];
    let mut out = Vec::with_capacity(8192);
    loop {
        // A live settings change (baud, framing) cannot reconfigure an open
        // port in place: it closes this reader, which the reconnect path sees
        // as a device that went away and immediately brings back at the new
        // settings.
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        // A timeout is how a pending CR gets flushed: nothing else can tell us
        // the device has stopped mid-line.
        let read = tokio::time::timeout(serial::IDLE_FLUSH, reader.read(&mut buf)).await;
        let n = match read {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => n,
            Ok(Err(err)) => {
                tracing::warn!(%err, "serial read");
                break;
            }
            Err(_idle) => 0,
        };

        if n > 0 {
            log_chunk(&log, &buf[..n]);
        }

        out.clear();
        match mapper.as_mut() {
            Some(m) if n > 0 => m.feed(&buf[..n], &mut out),
            Some(m) => m.flush(&mut out),
            None => out.extend_from_slice(&buf[..n]),
        }
        if !out.is_empty() && tx.send(out.clone()).is_err() {
            break;
        }
    }
}

fn publish(
    term: &mut Terminal<'static, '_>,
    state: &mut RenderState<'static>,
    rows: &mut RowIterator<'static>,
    cells: &mut CellIterator<'static>,
    out: &Mutex<Option<Frame>>,
    wake: &Arc<dyn Fn() + Send + Sync>,
    force: bool,
    selection: &mut Option<SelectionAnchors>,
    shown: &mut Option<Cursor>,
) -> anyhow::Result<()> {
    let started = std::time::Instant::now();
    let snapshot = state.update(term)?;

    // Hidden is not somewhere else. A program that draws the whole screen itself
    // — btop, vim in the alternate screen, anything using a full-screen UI —
    // sends `?25l` and leaves a position behind, and `cursor_viewport` answers
    // *where* the cursor is rather than whether it is showing: drawing that
    // stray block puts a cursor in the middle of a monitor that has none.
    let cursor = snapshot.cursor()?;
    let shape = match cursor.visual_style {
        CursorVisualStyle::Bar => CursorShape::Bar,
        CursorVisualStyle::Underline => CursorShape::Underline,
        // `BlockHollow` is a block whose middle shows the character through it.
        // At terminal sizes that is a pixel of border, and drawing the block is
        // closer to what the program meant than drawing nothing.
        _ => CursorShape::Block,
    };
    let cursor = cursor
        .visible
        .then_some(cursor.viewport)
        .flatten()
        .map(|at| Cursor {
            x: at.x,
            y: at.y,
            shape,
        });

    // A cursor is not a dirty cell. Hiding it, showing it, or turning its block
    // into the bar nvim wants in insert mode changes nothing in the grid, so the
    // render state stays clean — and a frame that is never published leaves the
    // old cursor on screen while the program's own idea of it has moved on.
    let cursor_changed = cursor != *shown;
    // A clean frame means the UI already has the current contents.
    if !force && !cursor_changed && matches!(snapshot.dirty()?, Dirty::Clean) {
        return Ok(());
    }

    let colors = snapshot.colors()?;
    let mut frame = Frame {
        cols: snapshot.cols()?,
        rows: snapshot.rows()?,
        default_fg: rgb(colors.foreground),
        default_bg: rgb(colors.background),
        cursor,
        selection: None,
        cells: Vec::with_capacity(usize::from(snapshot.cols()?) * usize::from(snapshot.rows()?)),
    };

    fill_cells(&snapshot, rows, cells, &mut frame)?;

    // The selection is resolved here rather than kept by the UI: a viewport
    // move republishes, so the highlight follows the characters the user
    // dragged over instead of staying at a row number.
    let resolved = selection.as_ref().and_then(|anchors| {
        Some((
            viewport_cell(&anchors.start, frame.rows)?,
            viewport_cell(&anchors.head, frame.rows)?,
            anchors.deliberate,
        ))
    });
    match resolved {
        Some((start, head, deliberate)) => {
            // One cell is a press, not a selection — unless the user chose it,
            // which is why a single click that never moves deselects and a
            // one-character word still highlights.
            frame.selection = selection_span(start, head, frame.cols, deliberate);
        }
        // Out of the frame: a highlight clamped to the edge would claim cells
        // the selection does not cover, so the selection goes instead of
        // shrinking to a lie.
        None => *selection = None,
    }

    // Dirty state is sticky by design; clearing it is the consumer's job.
    snapshot.set_dirty(Dirty::Clean)?;
    // Per-frame cost on the pane thread: filling a frame allocates a `String`
    // per cell, and this is the number that says whether that matters.
    tracing::debug!(
        cells = frame.cells.len(),
        elapsed_us = started.elapsed().as_micros() as u64,
        "frame published"
    );

    match out.lock() {
        Ok(mut slot) => *slot = Some(frame),
        Err(err) => tracing::error!("frame slot poisoned: {err}"),
    }
    // After the frame is in the slot, so a failed publish is retried by the next
    // one rather than remembered as drawn.
    *shown = cursor;
    wake();
    Ok(())
}

/// A mouse selection, held as two tracked references.
///
/// Cell indices would name whichever characters happen to be at those rows
/// after a scroll or a reflow; a tracked reference follows the content itself,
/// which is why dragging across a line and then scrolling still highlights that
/// line. Resolved to viewport cells on each publish for the renderer, and to a
/// `Selection` for the copy.
struct SelectionAnchors {
    start: TrackedGridRef,
    head: TrackedGridRef,
    /// Whether the user chose this. A press that has not moved is an anchor —
    /// no highlight, so a click deselects — but a word, a line or a drag is a
    /// choice, and one cell of it is a real answer: the word `x` has one.
    deliberate: bool,
}

impl SelectionAnchors {
    /// A selection of one cell, at a viewport cell the pointer was over.
    fn at(term: &Terminal<'_, '_>, x: u16, y: u16) -> anyhow::Result<Self> {
        let point = Point::Viewport(PointCoordinate { x, y: u32::from(y) });
        Ok(Self {
            start: term.track_grid_ref(point)?,
            head: term.track_grid_ref(point)?,
            deliberate: false,
        })
    }

    /// The whole selection as text, or `None` when it holds nothing — the
    /// first cell of a click, or a block that has been pruned away.
    fn text(&self, term: &Terminal<'_, '_>) -> Option<String> {
        let start = self.start.snapshot(term).ok()??;
        let end = self.head.snapshot(term).ok()??;
        let selection = Selection::new(start, end, false);
        let options = FormatOptions::new()
            .with_selection(&selection)
            .with_trim(true)
            .with_unwrap(true);
        let bytes = term.format_selection_alloc(None, options).ok()??;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// Hold a selection the core derived — a word, a line — as anchors.
///
/// Both ends have to be on screen: a word or a line always is, and a selection
/// that is not cannot be anchored to a viewport cell at all.
fn anchors_from(term: &Terminal<'_, '_>, selection: &Selection<'_>) -> Option<SelectionAnchors> {
    // Bound before the conversion: the grid references are borrowed, and a
    // temporary from `selection.start()` would not outlive the call.
    let (start, head) = (selection.start(), selection.end());
    let start = term
        .point_from_grid_ref(&start, PointSpace::Viewport)
        .ok()??;
    let head = term
        .point_from_grid_ref(&head, PointSpace::Viewport)
        .ok()??;
    Some(SelectionAnchors {
        start: term.track_grid_ref(Point::Viewport(start)).ok()?,
        head: term.track_grid_ref(Point::Viewport(head)).ok()?,
        deliberate: true,
    })
}

/// The word under a viewport cell, per the core's idea of a word boundary.
fn select_word(term: &Terminal<'_, '_>, x: u16, y: u16) -> Option<SelectionAnchors> {
    let grid_ref = viewport_ref(term, x, y)?;
    let options = SelectWordOptions::new(grid_ref);
    let selection = term.select_word(options).ok()??;
    anchors_from(term, &selection)
}

/// The line under a viewport cell, trimmed of the trailing blanks a terminal
/// fills the rest of the row with.
fn select_line(term: &Terminal<'_, '_>, x: u16, y: u16) -> Option<SelectionAnchors> {
    let grid_ref = viewport_ref(term, x, y)?;
    let options = SelectLineOptions::new(grid_ref);
    let selection = term.select_line(options).ok()??;
    anchors_from(term, &selection)
}

/// The URI of the cell at a viewport position, if it has one.
///
/// Asked on demand rather than carried in the frame: `hyperlink_uri` is one FFI
/// call per cell, and on a dense 3808-cell grid that is another ~30 µs on top of
/// a 41 µs publish — for a link nobody has hovered.
fn hyperlink_at(term: &Terminal<'_, '_>, x: u16, y: u16) -> Option<String> {
    let grid_ref = viewport_ref(term, x, y)?;
    let mut buf = vec![0u8; 256];
    let len = match grid_ref.hyperlink_uri(&mut buf) {
        Ok(len) => len,
        Err(libghostty_vt::Error::OutOfSpace { required }) if required > 0 => {
            buf.resize(required, 0);
            grid_ref.hyperlink_uri(&mut buf).ok()?
        }
        Err(_) => return None,
    };
    (len > 0).then(|| String::from_utf8_lossy(&buf[..len]).into_owned())
}

/// A grid reference at a viewport cell.
fn viewport_ref<'t>(
    term: &'t Terminal<'_, '_>,
    x: u16,
    y: u16,
) -> Option<libghostty_vt::screen::GridRef<'t>> {
    term.grid_ref(Point::Viewport(PointCoordinate { x, y: u32::from(y) }))
        .ok()
}

/// The highlight's linear span, or `None` for a press that has not moved.
///
/// A press that has not moved leaves the anchor exactly where the pointer went
/// down, and showing that as a selection makes every click look like it chose
/// something. A *deliberate* one-cell selection is different: double-clicking
/// the word `x`, or dragging back onto the cell you started on, really is one
/// character and has to paint.
fn selection_span(
    start: (u16, u16),
    head: (u16, u16),
    cols: u16,
    deliberate: bool,
) -> Option<(usize, usize)> {
    if start == head && !deliberate {
        return None;
    }
    let cols = usize::from(cols.max(1));
    Some((
        usize::from(start.1) * cols + usize::from(start.0),
        usize::from(head.1) * cols + usize::from(head.0),
    ))
}

/// A tracked reference's cell in the viewport, or `None` when it is no longer
/// in it.
///
/// `PointSpace::Viewport` answers in viewport coordinates, but it does *not*
/// bound them: a cell thirteen rows below a five-row viewport comes back as
/// `y = 13`. So the check is here, and nothing is clamped — a highlight
/// stretched to the edge would claim cells the selection does not cover.
fn viewport_cell(reference: &TrackedGridRef, rows: u16) -> Option<(u16, u16)> {
    let point = reference.point(PointSpace::Viewport).ok()??;
    let y = u16::try_from(point.y).ok()?;
    (y < rows).then_some((point.x, y))
}

/// Where the shell's prompts are.
///
/// `OSC 133 ; A` (and `; N`) is emitted by shell integration *before* the prompt
/// is drawn, so the cursor at that byte is the prompt's first cell. The position
/// is kept as a [`TrackedGridRef`] rather than a row number: it follows the page
/// list through scrolling, pruning and reflow, which is what a mark has to
/// survive.
///
/// The sequence arrives in the byte stream ahead of the parser, so a chunk is
/// written to the terminal in pieces around it. A sequence split across two
/// reads is held back unwritten until its terminator arrives — writing a
/// partial one and marking later would put the mark in the wrong place.
#[derive(Default)]
struct PromptMarks {
    marks: std::collections::VecDeque<TrackedGridRef>,
    /// An unterminated `ESC ] 133 ;` that has not been handed to the parser.
    carry: Vec<u8>,
}

impl PromptMarks {
    /// Prompts remembered; a jump and a capture only look at the last two, the
    /// rest is for a future "go back N commands".
    const KEEP: usize = 128;
    /// A held-back sequence longer than this is not one this understands, so it
    /// is flushed rather than stalling the session behind a bogus introducer.
    const CARRY_MAX: usize = 64;
    const INTRO: &'static [u8] = b"\x1b]133;";

    /// Write `bytes` to the terminal, recording every prompt in them.
    fn write(&mut self, term: &mut Terminal<'_, '_>, bytes: &[u8]) -> anyhow::Result<()> {
        let mut buf = std::mem::take(&mut self.carry);
        buf.extend_from_slice(bytes);

        let mut written = 0usize;
        let mut at = 0usize;
        while let Some(offset) = find(&buf[at..], Self::INTRO) {
            let start = at + offset;
            let body = start + Self::INTRO.len();
            // The command letter, then the terminator. Either can be in the
            // next read, and then the whole thing waits in the carry.
            let Some(&kind) = buf.get(body) else { break };
            let Some((offset, len)) = terminator(&buf[body..]) else {
                break;
            };
            let end = body + offset + len;

            term.vt_write(&buf[written..end]);
            written = end;
            if matches!(kind, b'A' | b'N') {
                self.mark(term)?;
            }
            at = end;
        }

        // Whatever is left is ordinary output, except for an introducer whose
        // sequence has not arrived yet — including one that has only partly
        // arrived, which a search for the whole introducer cannot see.
        let hold = find(&buf[written..], Self::INTRO)
            .or_else(|| partial_tail(&buf[written..], Self::INTRO))
            .map(|offset| written + offset);
        match hold {
            Some(start) if buf.len() - start <= Self::CARRY_MAX => {
                term.vt_write(&buf[written..start]);
                self.carry = buf[start..].to_vec();
            }
            _ => {
                term.vt_write(&buf[written..]);
                self.carry.clear();
            }
        }
        Ok(())
    }

    /// Record the prompt that starts at the cursor.
    fn mark(&mut self, term: &mut Terminal<'_, '_>) -> anyhow::Result<()> {
        // The spec's fresh-line: a prompt starts a line even if the shell did
        // not move there itself.
        if term.cursor_x()? > 0 {
            term.vt_write(b"\r\n");
        }
        let point = Point::Active(PointCoordinate {
            x: term.cursor_x()?,
            y: u32::from(term.cursor_y()?),
        });
        if let Ok(mark) = term.track_grid_ref(point) {
            self.marks.push_back(mark);
            while self.marks.len() > Self::KEEP {
                self.marks.pop_front();
            }
        }
        Ok(())
    }

    /// The prompt the last command started at: the current one when the cursor
    /// is sitting at a prompt, the running command's own prompt otherwise.
    fn block_start(&self, term: &Terminal<'_, '_>) -> Option<&TrackedGridRef> {
        let back = if term.is_cursor_at_prompt().unwrap_or(false) {
            2
        } else {
            1
        };
        let len = self.marks.len();
        (len >= back).then(|| &self.marks[len - back])
    }
}

/// Where a suffix that is a proper prefix of `needle` starts, if there is one.
///
/// A sequence arrives in whatever pieces the pty hands over, so `\x1b]1` can be
/// the end of a read. Searching for the whole introducer finds nothing there,
/// and writing the bytes would put the mark in the wrong place — or nowhere.
fn partial_tail(bytes: &[u8], needle: &[u8]) -> Option<usize> {
    (1..needle.len())
        .rev()
        .find(|len| bytes.ends_with(&needle[..*len]))
        .map(|len| bytes.len() - len)
}

/// Offset of the first `BEL` or `ESC \\` terminator, and its length.
fn terminator(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            0x07 => return Some((at, 1)),
            0x1b if bytes.get(at + 1) == Some(&b'\\') => return Some((at, 2)),
            // A bare ESC inside the sequence means it is malformed; scanning
            // past it would only misread the next one.
            0x1b => return None,
            _ => at += 1,
        }
    }
    None
}

/// The text of the block the last command produced: from the prompt it started
/// at to the cursor. `None` when the shell has not marked any prompt, or the
/// block has been pruned out of the scrollback.
fn capture_block(term: &Terminal<'_, '_>, marks: &PromptMarks) -> anyhow::Result<Option<String>> {
    let Some(start) = marks.block_start(term) else {
        return Ok(None);
    };
    let Some(start) = start.snapshot(term)? else {
        return Ok(None);
    };
    let end = term.grid_ref(Point::Active(PointCoordinate {
        x: term.cursor_x()?,
        y: u32::from(term.cursor_y()?),
    }))?;

    let selection = Selection::new(start, end, false);
    let options = FormatOptions::new()
        .with_selection(&selection)
        .with_trim(true)
        .with_unwrap(true);
    let Some(bytes) = term.format_selection_alloc(None, options)? else {
        return Ok(None);
    };
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

/// Watches the byte stream for OSC 7, the working-directory report.
///
/// `ESC ] 7 ; file://host/path BEL` (or `ESC \`) is what a shell sends when the
/// directory changes; there is no other portable way to know where a session is,
/// since `/proc` is Linux-only and a remote shell has no local process at all.
/// Chunks arrive at arbitrary offsets, so the tail of a partial sequence is kept.
#[derive(Default)]
struct CwdSniffer {
    buf: Vec<u8>,
}

/// One OSC 7 report: where the session says it is, and on which machine.
///
/// The host is kept rather than resolved away, because the two questions a path
/// answers are different: "may I use this as a local directory?" needs the host,
/// while "where does a reconnected session belong?" is a path on the session's
/// own machine whatever it is called.
struct CwdReport {
    host: String,
    path: PathBuf,
}

impl CwdSniffer {
    const MAX: usize = 8 * 1024;

    fn feed(&mut self, bytes: &[u8]) -> Option<CwdReport> {
        self.buf.extend_from_slice(bytes);
        let mut found = None;
        while let Some(start) = find(&self.buf, b"\x1b]7;") {
            let body = start + 4;
            let end = self.buf[body..]
                .iter()
                .position(|&b| b == 0x07 || b == 0x1b);
            let Some(len) = end else {
                // Incomplete: keep from the introducer and wait for more.
                self.buf.drain(..start);
                return found;
            };
            let payload = &self.buf[body..body + len];
            if let Some(report) = parse_file_uri(payload) {
                found = Some(report);
            }
            let terminator = match self.buf[body + len] {
                0x07 => 1,
                0x1b => 2, // ESC \ (ST)
                _ => 1,
            };
            self.buf.drain(..body + len + terminator);
        }
        // Anything else is just bytes we do not care about.
        let keep = self
            .buf
            .iter()
            .rposition(|&b| b == 0x1b)
            .filter(|&at| self.buf.len() - at < 8)
            .unwrap_or(self.buf.len());
        self.buf.drain(..keep);
        if self.buf.len() > Self::MAX {
            self.buf.clear();
        }
        found
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// `file://host/path`, with the host kept so the caller can decide what the path
/// means.
///
/// The host decides whether the path means anything here: empty, `localhost` or
/// this machine's own name is a local directory; anything else is a shell on
/// another host, and a path from there is not a local directory — using it would
/// make "new tab here" try to `cd` into a path that happens to exist on the wrong
/// machine. The rule itself is shared with the link opener, which had its own
/// copy of it and did not know about the machine's name.
fn parse_file_uri(payload: &[u8]) -> Option<CwdReport> {
    let text = std::str::from_utf8(payload).ok()?;
    let (host, path) = crate::uri::split(text)?;
    Some(CwdReport {
        host: host.to_owned(),
        path: PathBuf::from(path),
    })
}

fn fill_cells(
    snapshot: &Snapshot<'static, '_>,
    rows: &mut RowIterator<'static>,
    cells: &mut CellIterator<'static>,
    frame: &mut Frame,
) -> anyhow::Result<()> {
    let mut row_it = rows.update(snapshot)?;
    let mut text = String::with_capacity(16);
    while let Some(row) = row_it.next() {
        let mut cell_it = cells.update(row)?;
        while let Some(cell) = cell_it.next() {
            let mut out = Cell {
                fg: frame.default_fg,
                bg: frame.default_bg,
                ..Cell::default()
            };
            cell.graphemes_utf8(&mut text)?;
            out.text.push_str(&text);
            // The render iterator does not report cell width, so measure the
            // character: on a wcwidth grid, two cells means a wide glyph.
            out.wide = text
                .chars()
                .next()
                .is_some_and(|ch| unicode_width::UnicodeWidthChar::width(ch) == Some(2));

            if let Some(fg) = cell.fg_color()? {
                out.fg = rgb(fg);
            }
            if let Some(bg) = cell.bg_color()? {
                out.bg = rgb(bg);
                out.has_bg = true;
            }
            if cell.has_styling()? {
                let style = cell.style()?;
                out.bold = style.bold;
                out.italic = style.italic;
                if style.inverse {
                    std::mem::swap(&mut out.fg, &mut out.bg);
                    out.has_bg = true;
                }
                // After the swap, because faint dims what is drawn and an
                // inverse cell has already changed which colour that is.
                if style.faint {
                    out.fg = faint(out.fg, out.bg);
                }
            }
            frame.cells.push(out);
        }
    }
    Ok(())
}

fn rgb(c: libghostty_vt::style::RgbColor) -> [u8; 3] {
    [c.r, c.g, c.b]
}

/// How much of the foreground SGR 2 leaves behind, over the cell's background.
///
/// Ghostty does not draw faint either: its renderer turns the flag into the
/// foreground's alpha, `faint-opacity` (0.5 by default), composited over the
/// background — so a program that dims a ghost suggestion gets the same grey
/// here as in Ghostty. Blending is also the only version that works on a
/// palette: there is no "faint" colour to ask a theme for, and picking a fixed
/// grey would be wrong on half of them.
const FAINT_OPACITY: f32 = 0.5;

/// The colour an SGR 2 cell is drawn in: `fg` laid over `bg` at
/// [`FAINT_OPACITY`].
fn faint(fg: [u8; 3], bg: [u8; 3]) -> [u8; 3] {
    std::array::from_fn(|i| {
        let fg = f32::from(fg[i]);
        let bg = f32::from(bg[i]);
        fg.mul_add(FAINT_OPACITY, bg * (1.0 - FAINT_OPACITY))
            .round() as u8
    })
}

const DEMO: &[u8] = b"\x1b[32mdtr\x1b[0m \xe2\x80\x94 a serial-first terminal\r\n\r\n\
 normal  \x1b[1mbold\x1b[0m  \x1b[3mitalic\x1b[0m  \x1b[2mfaint\x1b[0m  \x1b[31mred\x1b[0m  \x1b[44m blue bg \x1b[0m  \x1b[7m inverse \x1b[0m\r\n\r\n\
 1  \xe2\x94\x8c\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x90\r\n\
 2  \xe2\x94\x82 box     \xe2\x94\x82\r\n\
 3  \xe2\x94\x94\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x98\r\n";

#[cfg(test)]
mod tests {
    use super::{Cell, Frame, frame_text};

    /// Copy from `start` to `end`, inclusive, as text.
    ///
    /// The range is two linear cell indices in either order; a partial first and
    /// last row is normal (a drag rarely starts at column zero), so rows are cut at
    /// the selection's own edges and only the trailing blanks are trimmed.
    fn frame_text_range(frame: &Frame, start: usize, end: usize) -> String {
        let cols = usize::from(frame.cols).max(1);
        let (first, last) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        let last = last.min(frame.cells.len().saturating_sub(1));
        let first = first.min(last);

        let (start_row, start_col) = (first / cols, first % cols);
        let (end_row, end_col) = (last / cols, last % cols);

        let mut lines = Vec::new();
        for row in start_row..=end_row {
            let from = if row == start_row { start_col } else { 0 };
            let to = if row == end_row { end_col } else { cols - 1 };
            let base = row * cols;
            if base + from >= frame.cells.len() {
                break;
            }
            let to = to.min(cols - 1);
            let line: String = frame.cells[base + from..=(base + to).min(frame.cells.len() - 1)]
                .iter()
                .map(|cell| cell.text.as_str())
                .collect();
            lines.push(line.trim_end().to_owned());
        }
        lines.join("\n")
    }

    fn frame(rows: &[&str]) -> Frame {
        let cols = 8u16;
        let mut cells = Vec::new();
        for row in rows {
            let mut chars: Vec<char> = row.chars().collect();
            chars.resize(cols as usize, ' ');
            for ch in chars {
                cells.push(Cell {
                    text: ch.to_string(),
                    wide: unicode_width::UnicodeWidthChar::width(ch) == Some(2),
                    fg: [255, 255, 255],
                    bg: [0, 0, 0],
                    has_bg: false,
                    bold: false,
                    italic: false,
                });
            }
        }
        Frame {
            cols,
            rows: rows.len() as u16,
            default_fg: [255, 255, 255],
            default_bg: [0, 0, 0],
            cursor: None,
            selection: None,
            cells,
        }
    }

    /// The mark a cursor leaves inside its cell, in pixels.
    ///
    /// A block is the cell; a bar is a quarter of the cell's width against its
    /// left edge (DECSCUSR's `ver25`, which is what nvim asks for in insert mode)
    /// and an underline a fifth of its height along the bottom (`hor20`). Both
    /// have a one-pixel floor, because a bar thinner than a pixel draws nothing
    /// and the user cannot then tell insert mode from no cursor at all.
    #[test]
    fn a_bar_cursor_is_a_stripe_and_not_a_cell() {
        use super::{Cursor, CursorShape};
        use crate::layout::Rect;

        let cell = Rect::new(10.0, 20.0, 8.0, 16.0);
        let mark = |shape| Cursor { x: 0, y: 0, shape }.mark(cell);

        assert_eq!(mark(CursorShape::Block), cell);
        // A quarter of 8 is 2, and it sits against the left edge of the cell.
        let bar = mark(CursorShape::Bar);
        assert_eq!((bar.x, bar.width, bar.height), (10.0, 2.0, 16.0));
        // A fifth of 16 is 3.2, rounded up to a whole pixel at the bottom.
        let underline = mark(CursorShape::Underline);
        assert_eq!(
            (underline.y, underline.width, underline.height),
            (32.0, 8.0, 4.0)
        );
        // And the floor: a three-pixel-wide cell in a two-pixel-tall font still
        // gets one pixel of bar rather than a sub-pixel nothing.
        let thin = Rect::new(0.0, 0.0, 3.0, 2.0);
        assert_eq!(
            Cursor {
                x: 0,
                y: 0,
                shape: CursorShape::Bar
            }
            .mark(thin)
            .width,
            1.0
        );
        assert_eq!(
            Cursor {
                x: 0,
                y: 0,
                shape: CursorShape::Underline
            }
            .mark(thin)
            .height,
            1.0
        );
    }

    #[test]
    fn the_vt_core_gives_back_the_characters_we_sent() {
        use libghostty_vt::render::{CellIterator, RowIterator};
        use libghostty_vt::{RenderState, Terminal};

        let mut term = Terminal::new(8, 2).unwrap();
        let mut state = RenderState::new().unwrap();
        let mut rows = RowIterator::new().unwrap();
        let mut cells = CellIterator::new().unwrap();
        term.vt_write(b"OK\r\n");
        let snapshot = state.update(&term).unwrap();
        let mut row_it = rows.update(&snapshot).unwrap();
        let row = row_it.next().unwrap();
        let mut cell_it = cells.update(row).unwrap();
        let mut text = String::new();
        let mut per_cell = Vec::new();
        while let Some(cell) = cell_it.next() {
            cell.graphemes_utf8(&mut text).unwrap();
            per_cell.push(text.clone());
        }
        assert_eq!(per_cell[0], "O", "cells: {per_cell:?}");
        assert_eq!(per_cell[1], "K", "cells: {per_cell:?}");
    }

    /// Faint is drawn, not dropped.
    ///
    /// A program that dims a ghost suggestion — fish's autosuggestion, an
    /// editor's inline completion — sends `\e[2m` and often nothing else, and a
    /// terminal that ignores the flag shows the ghost exactly as bright as the
    /// text it is suggesting against. The core parses it and hands back
    /// `style.faint`; the renderer used to throw that away, so the proof is a
    /// colour that is neither the default ink nor the ground.
    #[test]
    fn faint_text_is_a_dimmed_colour_and_not_the_default_ink() {
        use super::{faint, fill_cells, rgb};
        use libghostty_vt::render::{CellIterator, RowIterator};
        use libghostty_vt::{RenderState, Terminal};

        // `X` plain, `Y` faint, `Z` inverse and faint together.
        let mut term = Terminal::new(8, 1).unwrap();
        term.vt_write(b"X\x1b[2mY\x1b[0m\x1b[7;2mZ");
        let mut state = RenderState::new().unwrap();
        let mut rows = RowIterator::new().unwrap();
        let mut cells = CellIterator::new().unwrap();
        let snapshot = state.update(&term).unwrap();
        let colors = snapshot.colors().unwrap();
        let mut frame = Frame {
            cols: snapshot.cols().unwrap(),
            rows: snapshot.rows().unwrap(),
            default_fg: rgb(colors.foreground),
            default_bg: rgb(colors.background),
            cursor: None,
            selection: None,
            cells: Vec::new(),
        };
        fill_cells(&snapshot, &mut rows, &mut cells, &mut frame).unwrap();

        assert_eq!(frame.cells[0].text, "X");
        assert_eq!(frame.cells[1].text, "Y");
        assert_eq!(frame.cells[2].text, "Z");
        // The plain cell keeps the default ink: faint must not leak past `0m`.
        assert_eq!(frame.cells[0].fg, frame.default_fg);
        assert_eq!(frame.cells[1].fg, faint(frame.default_fg, frame.default_bg));
        assert_ne!(frame.cells[1].fg, frame.default_fg);
        // Inverse swaps first, so faint dims the colour that is actually drawn
        // — the background, which has become the foreground.
        assert!(frame.cells[2].has_bg);
        assert_eq!(frame.cells[2].fg, faint(frame.default_bg, frame.default_fg));

        // And the numbers, so a change to the blend is a diff and not a mood:
        // half of white over black, half of a mid colour, and the fixed point
        // that keeps a cell on a same-coloured ground exactly as it was.
        assert_eq!(faint([255, 255, 255], [0, 0, 0]), [128, 128, 128]);
        assert_eq!(faint([200, 100, 0], [0, 0, 0]), [100, 50, 0]);
        assert_eq!(faint([10, 20, 30], [10, 20, 30]), [10, 20, 30]);
    }

    #[test]
    fn copying_a_range_cuts_at_the_selection_edges() {
        // Two rows: "abcdefgh" and "ijklmnop".
        let frame = frame(&["abcdefgh", "ijklmnop"]);
        // From 'c' (index 2) to 'j' (index 9), both inclusive: the rest of the
        // first row, then the first two cells of the next.
        assert_eq!(frame_text_range(&frame, 2, 9), "cdefgh\nij");
    }

    #[test]
    fn copying_a_range_works_backwards() {
        // A drag can end before it starts; the range is the same either way.
        let frame = frame(&["abcdefgh", "ijklmnop"]);
        assert_eq!(
            frame_text_range(&frame, 9, 2),
            frame_text_range(&frame, 2, 9)
        );
    }

    #[test]
    fn copying_a_single_cell_gives_one_character() {
        let frame = frame(&["abcdefgh"]);
        assert_eq!(frame_text_range(&frame, 3, 3), "d");
    }

    #[test]
    fn a_range_past_the_end_is_clamped() {
        let frame = frame(&["ab"]);
        assert_eq!(frame_text_range(&frame, 0, 9999), "ab");
    }

    #[test]
    fn copying_a_screen_drops_padding_and_blank_tail() {
        let text = frame_text(&frame(&["hello", "", "  two"]));
        assert_eq!(text, "hello\n\n  two");
    }

    #[test]
    fn copying_an_empty_screen_gives_nothing() {
        assert_eq!(frame_text(&frame(&["", ""])), "");
    }
}

#[cfg(test)]
mod cwd_tests {
    use super::CwdSniffer;

    /// The path, when the report names a directory on this machine.
    fn path_of(input: &[u8]) -> Option<String> {
        let report = CwdSniffer::default().feed(input)?;
        crate::uri::is_local_host(&report.host).then(|| report.path.display().to_string())
    }

    /// The path as the pane keeps it for a reconnect: on whichever machine the
    /// session is, as text, because it is not a local path.
    fn reported(input: &[u8]) -> Option<String> {
        CwdSniffer::default()
            .feed(input)
            .map(|report| report.path.display().to_string())
    }

    #[test]
    fn a_bel_terminated_report_is_a_directory() {
        assert_eq!(
            path_of(b"\x1b]7;file:///home/barnabas/src\x07").as_deref(),
            Some("/home/barnabas/src")
        );
        assert_eq!(
            path_of(b"\x1b]7;file:///home/lab\x07").as_deref(),
            Some("/home/lab")
        );
    }

    #[test]
    fn an_st_terminated_report_works_too() {
        assert_eq!(
            path_of(b"\x1b]7;file:///tmp\x1b\\").as_deref(),
            Some("/tmp")
        );
    }

    #[test]
    fn a_sequence_split_across_chunks_is_still_read() {
        let mut sniffer = CwdSniffer::default();
        assert!(sniffer.feed(b"prompt \x1b]7;file:///ho").is_none());
        assert_eq!(
            sniffer
                .feed(b"me/barnabas\x07")
                .map(|report| report.path.display().to_string())
                .as_deref(),
            Some("/home/barnabas")
        );
    }

    #[test]
    fn another_host_is_not_a_local_directory() {
        // A remote shell's path means nothing here; adopting it would make
        // "new tab here" cd into a directory on the wrong machine.
        assert_eq!(path_of(b"\x1b]7;file://build-host/home/x\x07"), None);
        // It is still where the session is, and a connection that drops has to
        // come back to it.
        assert_eq!(
            reported(b"\x1b]7;file://build-host/home/x\x07").as_deref(),
            Some("/home/x")
        );
    }

    #[test]
    fn localhost_and_this_machine_are_local() {
        assert_eq!(
            path_of(b"\x1b]7;file://localhost/srv\x07").as_deref(),
            Some("/srv")
        );
        // A shell reports this machine by name more often than as `localhost`,
        // and the same name in a hyperlink is what `ls` writes.
        if let Some(name) = crate::uri::local_hostname() {
            let report = format!("\x1b]7;file://{name}/srv\x07");
            assert_eq!(path_of(report.as_bytes()).as_deref(), Some("/srv"));
        }
    }

    #[test]
    fn percent_escapes_are_decoded() {
        assert_eq!(
            path_of(b"\x1b]7;file:///home/a%20b\x07").as_deref(),
            Some("/home/a b")
        );
    }

    #[test]
    fn ordinary_output_is_not_a_directory() {
        assert_eq!(path_of(b"just a shell saying hello\x07"), None);
        assert_eq!(path_of(b"\x1b]0;a title\x07"), None);
    }
}

#[cfg(test)]
mod selection_tests {
    use super::{SelectionAnchors, viewport_cell};
    use libghostty_vt::Terminal;
    use libghostty_vt::terminal::{Point, PointCoordinate, ScrollViewport};

    /// Enough output to have somewhere to scroll, and a viewport at the bottom.
    fn terminal() -> Terminal<'static, 'static> {
        let mut term = Terminal::new(20, 5).unwrap();
        for line in 0..40 {
            term.vt_write(format!("line {line}\r\n").as_bytes());
        }
        term
    }

    fn head(
        term: &mut Terminal<'static, 'static>,
        cell: SelectionAnchors,
        x: u16,
        y: u16,
    ) -> SelectionAnchors {
        let mut cell = cell;
        cell.head
            .set(
                term,
                Point::Viewport(PointCoordinate { x, y: u32::from(y) }),
            )
            .unwrap();
        cell
    }

    /// The point of anchoring: the text a selection covers must not change when
    /// the view does. Cell indices fail this test by construction — that is
    /// what the old implementation stored.
    #[test]
    fn the_selected_text_survives_a_scroll() {
        let mut term = terminal();
        let start = SelectionAnchors::at(&term, 0, 3).unwrap();
        let cell = head(&mut term, start, 5, 3);
        let before = cell.text(&term).expect("a selection");
        assert!(before.starts_with("line "), "selected {before:?}");

        term.scroll_viewport(ScrollViewport::Delta(-3));
        assert_eq!(cell.text(&term).as_deref(), Some(before.as_str()));

        term.scroll_viewport(ScrollViewport::Top);
        assert_eq!(cell.text(&term).as_deref(), Some(before.as_str()));
    }

    /// ...and the highlight has to move with it, or the copy and the picture
    /// disagree about what is selected.
    #[test]
    fn the_highlight_moves_with_the_text() {
        let mut term = terminal();
        term.scroll_viewport(ScrollViewport::Top);
        let cell = SelectionAnchors::at(&term, 0, 2).unwrap();
        assert_eq!(viewport_cell(&cell.start, 5).map(|cell| cell.1), Some(2));

        term.scroll_viewport(ScrollViewport::Delta(1));
        assert_eq!(
            viewport_cell(&cell.start, 5).map(|cell| cell.1),
            Some(1),
            "one row up: the same line, one row higher"
        );
    }

    /// A double-click takes the word, using the core's idea of a boundary —
    /// not a split on spaces, so `--flag=value` and quoted words behave the way
    /// every other terminal does.
    #[test]
    fn a_double_click_takes_the_word_under_it() {
        let mut term = Terminal::new(30, 4).unwrap();
        term.vt_write(b"echo hello-world again\r\n");
        let cell = super::select_word(&term, 7, 0).expect("a word");
        assert_eq!(cell.text(&term).as_deref(), Some("hello-world"));
    }

    /// ...and the word is an anchor like any other, so it survives a scroll.
    #[test]
    fn a_word_selection_survives_a_scroll() {
        let mut term = terminal();
        term.vt_write(b"one two three\r\n");
        let cell = super::select_word(&term, 5, 3).expect("a word");
        let before = cell.text(&term);
        assert_eq!(before.as_deref(), Some("two"));
        term.scroll_viewport(ScrollViewport::Delta(-2));
        assert_eq!(cell.text(&term), before);
    }

    /// A selection over several lines is several lines. `unwrap` is set so that a
    /// *soft*-wrapped line does not come back with a break in it; a row the
    /// program ended on purpose is not a soft wrap, and flattening those turns a
    /// copied block into one long line wherever it is pasted.
    #[test]
    fn a_selection_over_several_lines_keeps_its_line_breaks() {
        let mut term = terminal();
        // Two whole lines: the last rows of the scrollback are `line 36` and
        // `line 37` (the terminal is 20x5 and 40 lines were written).
        let start = SelectionAnchors::at(&term, 0, 0).unwrap();
        let cell = head(&mut term, start, 9, 1);
        let text = cell.text(&term).expect("a selection");
        assert_eq!(text, "line 36\nline 37", "flattened to {text:?}");
    }

    /// OSC 8 gives a cell a URI. It is answered on ask, from the cell under the
    /// pointer, and a cell outside the link has none.
    #[test]
    fn a_hyperlink_is_answered_from_the_cell_under_it() {
        let mut term = Terminal::new(24, 3).unwrap();
        term.vt_write(b"\x1b]8;;file:///tmp/x.txt\x07link\x1b]8;;\x07 plain\r\n");
        assert_eq!(
            super::hyperlink_at(&term, 1, 0).as_deref(),
            Some("file:///tmp/x.txt")
        );
        assert_eq!(super::hyperlink_at(&term, 7, 0), None, "past the link");
        assert_eq!(super::hyperlink_at(&term, 0, 2), None, "an empty row");
    }

    /// A one-character word is still a word: clicking `b` in `a b` selects one
    /// cell, and it is a choice rather than a press, so it paints.
    #[test]
    fn a_one_character_word_still_selects() {
        let mut term = Terminal::new(20, 2).unwrap();
        term.vt_write(b"a b\r\n");
        // 'a' at 0, the space at 1, 'b' at 2 — the cell under the pointer.
        let cell = super::select_word(&term, 2, 0).expect("a word");
        assert_eq!(cell.text(&term).as_deref(), Some("b"));
        assert!(cell.deliberate, "a word is a choice, even one cell wide");
    }

    /// A triple-click takes the whole line, without the blanks that fill the
    /// rest of the row.
    #[test]
    fn a_triple_click_takes_the_line() {
        let mut term = Terminal::new(30, 4).unwrap();
        term.vt_write(b"first line\r\nsecond line\r\n");
        let cell = super::select_line(&term, 2, 0).expect("a line");
        assert_eq!(cell.text(&term).as_deref(), Some("first line"));
    }

    /// A press that has not moved is an anchor, not a selection: nothing is
    /// highlighted, so a click deselects instead of selecting one cell.
    #[test]
    fn a_press_that_never_moved_shows_no_selection() {
        assert_eq!(super::selection_span((4, 2), (4, 2), 80, false), None);
        assert_eq!(
            super::selection_span((4, 2), (5, 2), 80, false),
            Some((164, 165)),
            "one cell across is a selection, and the indices are row-major"
        );
        assert_eq!(
            super::selection_span((4, 2), (4, 2), 80, true),
            Some((164, 164)),
            "a chosen one-cell selection is the word `x`, and it paints"
        );
    }

    /// Once the selected characters leave the frame the selection is gone.
    /// Clamping the ends to the border would paint a highlight over cells the
    /// selection does not cover, and copying it would hand back a fragment
    /// that looks like the whole thing.
    #[test]
    fn a_selection_that_leaves_the_frame_is_dropped() {
        let mut term = terminal();
        let cell = SelectionAnchors::at(&term, 0, 3).unwrap();
        assert!(viewport_cell(&cell.start, 5).is_some());

        term.scroll_viewport(ScrollViewport::Delta(-10));
        assert_eq!(
            viewport_cell(&cell.start, 5),
            None,
            "scrolled out: no cell, and publish drops the selection"
        );

        // The other way: back to the live screen, past the selection entirely.
        term.scroll_viewport(ScrollViewport::Bottom);
        for line in 0..20 {
            term.vt_write(format!("pushed {line}\r\n").as_bytes());
        }
        assert_eq!(viewport_cell(&cell.start, 5), None);
    }
}

#[cfg(test)]
mod prompt_tests {
    use super::{PromptMarks, capture_block};
    use libghostty_vt::Terminal;

    fn terminal() -> Terminal<'static, 'static> {
        Terminal::new(40, 6).unwrap()
    }

    /// A shell that emits the semantic-prompt sequence one byte at a time is the
    /// normal case, not a corner: a pty hands over whatever the shell wrote, and
    /// a prompt written in small pieces arrives that way.
    #[test]
    fn a_sequence_split_across_reads_is_still_a_mark() {
        let mut term = terminal();
        let mut marks = PromptMarks::default();
        let bytes = b"$ \x1b]133;A\x07";
        for byte in bytes {
            marks.write(&mut term, &[*byte]).unwrap();
        }
        assert_eq!(marks.marks.len(), 1, "the prompt should have been marked");
        assert!(term.is_cursor_at_prompt().unwrap());
    }

    /// The spec's fresh-line: a prompt begins a line even when the shell did
    /// not move the cursor there itself.
    #[test]
    fn a_prompt_that_does_not_start_a_line_gets_one() {
        let mut term = terminal();
        let mut marks = PromptMarks::default();
        marks.write(&mut term, b"abc").unwrap();
        marks.write(&mut term, b"\x1b]133;A\x07").unwrap();
        assert_eq!(term.cursor_x().unwrap(), 0);
        assert_eq!(term.cursor_y().unwrap(), 1);
    }

    /// The block is the command, not just its output — that is what a pager
    /// wants to show, and it is why `select_output` is not used.
    #[test]
    fn the_block_runs_from_the_previous_prompt_to_here() {
        let mut term = terminal();
        let mut marks = PromptMarks::default();
        marks
            .write(
                &mut term,
                b"$ \x1b]133;A\x07$ \x1b]133;B\x07echo hi\r\n\x1b]133;C\x07hi\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ ",
            )
            .unwrap();

        let block = capture_block(&term, &marks).unwrap().expect("a block");
        assert!(block.contains("echo hi"), "no command line in {block:?}");
        assert!(block.contains("hi"), "no output in {block:?}");
        assert!(
            block.starts_with("$ "),
            "block does not start at the prompt: {block:?}"
        );
    }

    /// What fish actually sends, captured from a real `fish --no-config -i`:
    /// the command is followed by options and terminated with ST, not BEL.
    /// Reading the kind byte before the options is what makes this work.
    #[test]
    fn the_form_fish_sends_is_a_prompt() {
        let mut term = terminal();
        let mut marks = PromptMarks::default();
        marks
            .write(
                &mut term,
                b"\x1b]133;A;click_events=1\x1b\\$ \x1b]133;B\x1b\\ls\r\n\x1b]133;C;cmdline_url=ls\x1b\\a b\r\n\x1b]133;D;0\x1b\\\x1b]133;A;click_events=1\x1b\\$ ",
            )
            .unwrap();
        assert_eq!(marks.marks.len(), 2, "fish's markers should both be read");
        let block = capture_block(&term, &marks).unwrap().expect("a block");
        assert!(block.contains("ls"), "no command line in {block:?}");
        assert!(block.contains("a b"), "no output in {block:?}");
    }

    /// Without shell integration there are no marks, and capture says so rather
    /// than inventing a block.
    #[test]
    fn a_session_without_prompts_captures_nothing() {
        let mut term = terminal();
        let mut marks = PromptMarks::default();
        marks.write(&mut term, b"just output\r\n").unwrap();
        assert!(capture_block(&term, &marks).unwrap().is_none());
    }
}

#[cfg(test)]
mod gate_tests {
    use super::frame_budget;

    /// The gate is a latency floor as much as a work cap, so it has to follow
    /// the panel. A fixed 8 ms would serve a 240 Hz display at 125 Hz.
    #[test]
    fn the_gate_follows_the_refresh_rate() {
        assert_eq!(frame_budget(None).as_micros(), 8_333);
        assert_eq!(frame_budget(Some(60_000)).as_micros(), 8_333);
        assert_eq!(frame_budget(Some(144_000)).as_micros(), 3_472);
        assert_eq!(frame_budget(Some(240_000)).as_micros(), 2_083);
        // A monitor that will not say gets the 60 Hz default rather than zero.
        assert_eq!(frame_budget(Some(0)).as_micros(), 8_333);
        // And a very fast panel does not turn the gate into a busy loop.
        assert_eq!(frame_budget(Some(1_000_000)).as_micros(), 2_000);
    }
}

#[cfg(test)]
mod paste_tests {
    use super::pasted;

    /// A pty calls CR "Enter"; a clipboard holds LF, and a program that gets LF
    /// pasted into it sees a ctrl-J, which is not the same key. Every terminal
    /// normalises this, and doing it in the paste path (not the copy path) is
    /// what keeps a *copied* block byte-for-byte what was on screen.
    #[test]
    fn line_breaks_become_the_key_a_pty_calls_enter() {
        assert_eq!(pasted(false, b"one\ntwo"), b"one\rtwo");
        // CRLF is one break, not two: a paste that doubled every line would
        // land a blank line between each.
        assert_eq!(pasted(false, b"one\r\ntwo"), b"one\rtwo");
        // A lone CR is already the right byte.
        assert_eq!(pasted(false, b"one\rtwo"), b"one\rtwo");
        // Nothing else is touched, control characters included: an escape the
        // text uses on purpose is the text.
        assert_eq!(pasted(false, b"a\tb\x1b[C"), b"a\tb\x1b[C");
        assert_eq!(pasted(false, b""), b"");
    }

    /// The markers are what tell a program "this is a paste, not typing", so
    /// they have to go around the whole thing — an empty paste included, or a
    /// program that reads the start marker waits for an end that never comes.
    #[test]
    fn a_program_that_asked_for_bracketed_paste_gets_the_markers() {
        assert_eq!(pasted(true, b"one\ntwo"), b"\x1b[200~one\rtwo\x1b[201~");
        assert_eq!(pasted(true, b""), b"\x1b[200~\x1b[201~");
        assert_eq!(
            pasted(true, b"\x1b[31mred\x1b[0m"),
            b"\x1b[200~\x1b[31mred\x1b[0m\x1b[201~"
        );
    }
}

#[cfg(test)]
mod ending_tests {
    use super::{Backoff, Ending, ending};

    /// `exit` is the user ending the session; 255 is ssh saying the connection
    /// failed. This is the whole distinction between closing a tab and reopening
    /// one, and it is one line of code, so it is a test.
    #[test]
    fn ssh_s_255_is_a_drop_and_anything_else_is_the_user_leaving() {
        assert_eq!(ending(Some(255)), Ending::Dropped);
        // A signal is never somebody typing `exit`.
        assert_eq!(ending(None), Ending::Dropped);
        assert_eq!(ending(Some(0)), Ending::Chose);
        assert_eq!(ending(Some(1)), Ending::Chose);
    }

    /// A host that is down must not be asked once a second for ever, and a
    /// session that comes back must not stay slow for the rest of the day.
    #[test]
    fn the_backoff_grows_and_stops_growing() {
        let mut backoff = Backoff::default();
        let waits: Vec<u64> = (0..8).map(|_| backoff.next().as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30, 30]);
        backoff.reset();
        assert_eq!(backoff.next().as_secs(), 1);
    }
}

/// A shell pane with a real pty behind it: the only way to see a reconnection,
/// because the whole question is what the child's exit status means.
///
/// The "ssh" here is `/bin/sh -c`, and the config says it is remote — which is
/// the part the pane acts on. That keeps the test to a script and no network.
#[cfg(all(test, unix))]
mod session_tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use std::time::{Duration, Instant};
    use winit::keyboard::{KeyCode, ModifiersState, NamedKey};

    fn pane(script: String, remote: Option<&str>, args: Vec<String>) -> Pane {
        let mut config = pty::Config {
            program: Some("/bin/sh".to_owned()),
            remote: remote.map(str::to_owned),
            ..Default::default()
        };
        let mut all = vec!["-c".to_owned(), script];
        all.extend(args);
        config.args = all;
        let budget: FrameBudget = Arc::new(AtomicU64::new(frame_budget(None).as_nanos() as u64));
        Pane::spawn(Transport::Shell(config), None, None, budget, || {}).expect("a shell pane")
    }

    /// Poll the published frames until `needle` is on screen, and hand back what
    /// the screen said.
    fn wait_for(pane: &Pane, needle: &str, within: Duration) -> String {
        let deadline = Instant::now() + within;
        let mut text = String::new();
        loop {
            if let Some(frame) = pane.take_frame() {
                text = frame_text(&frame);
                if text.contains(needle) {
                    return text;
                }
            }
            assert!(
                Instant::now() < deadline,
                "`{needle}` never appeared; the screen said:\n{text}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_connection_that_drops_is_reopened_in_the_same_pane() {
        let dir = tempfile::tempdir().expect("tempdir");
        let counter = dir.path().join("n");
        let script = format!(
            "n=$(cat {n} 2>/dev/null || echo 0); n=$((n+1)); echo $n > {n}; \
             if [ $n = 1 ]; then echo first; exit 255; fi; echo second; exec sleep 30",
            n = counter.display()
        );
        let pane = pane(script, Some("test-host"), Vec::new());
        let text = wait_for(&pane, "second", Duration::from_secs(20));
        // It says what it is doing: the shell that dropped cannot.
        assert!(
            text.contains("reconnecting to test-host"),
            "the pane never said it was reconnecting:\n{text}"
        );
        // And the pane is still there, which is the whole point: a tab that
        // vanished on a dropped connection would take the scrollback with it.
        assert!(!pane.ended(), "a dropped connection ended the pane");
        assert!(
            pane.error().is_none(),
            "the pane failed: {:?}",
            pane.error()
        );
    }

    /// A host that never answers: the pane stays, keeps trying, and grows the
    /// wait. Closing the tab is how a person gives up, not the exit status of a
    /// connection that was never made.
    #[test]
    fn a_connection_that_never_comes_back_keeps_the_pane() {
        let dir = tempfile::tempdir().expect("tempdir");
        let attempts = dir.path().join("attempts");
        let script = format!("echo x >> {}; exit 255", attempts.display());
        let pane = pane(script, Some("test-host"), Vec::new());
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let made = std::fs::read_to_string(&attempts)
                .map(|log| log.lines().count())
                .unwrap_or(0);
            if made >= 2 {
                break;
            }
            assert!(Instant::now() < deadline, "only {made} attempts were made");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!pane.ended(), "a failed connection ended the pane");
        let text = wait_for(&pane, "reconnecting", Duration::from_secs(5));
        assert!(text.contains("reconnecting to test-host"), "{text}");
    }

    /// The directory the session reported comes back with it. The path is on the
    /// other machine, so it cannot be `cwd`: it is a string handed to ssh.
    #[test]
    fn a_reconnect_puts_the_session_back_where_it_was() {
        let dir = tempfile::tempdir().expect("tempdir");
        let seen = dir.path().join("args");
        let counter = dir.path().join("n");
        let script = format!(
            "n=$(cat {n} 2>/dev/null || echo 0); n=$((n+1)); echo $n > {n}; \
             if [ $n = 1 ]; then printf '\\033]7;file:///srv/app\\a'; exit 255; fi; \
             printf '%s\\n' \"$@\" > {seen}; echo second; exec sleep 30",
            n = counter.display(),
            seen = seen.display()
        );
        // The placeholder is `$0`, so the command ssh would run arrives as `$1`.
        let pane = pane(script, Some("test-host"), vec!["dtr".to_owned()]);
        wait_for(&pane, "second", Duration::from_secs(20));
        let args = std::fs::read_to_string(&seen).expect("the second connection recorded its argv");
        // What the *remote shell* would parse, not the spelling: the directory
        // has to arrive as one word, after a `cd`.
        let words = shlex::split(args.trim()).expect("the reopen command parses");
        assert_eq!(
            words.first().map(String::as_str),
            Some("cd"),
            "the reconnect did not go back to a directory: {args}"
        );
        assert_eq!(words.get(1).map(String::as_str), Some("/srv/app"), "{args}");
        // A shell, so the session is interactive rather than a dead `cd`.
        assert!(words.iter().any(|word| word == "exec"), "{args}");
    }

    /// A local shell has no connection to put back: `exit` still closes the tab.
    #[test]
    fn a_local_shell_that_exits_ends_the_pane() {
        let pane = pane("echo bye; exit 0".to_owned(), None, Vec::new());
        wait_for(&pane, "bye", Duration::from_secs(10));
        let deadline = Instant::now() + Duration::from_secs(10);
        while !pane.ended() {
            assert!(Instant::now() < deadline, "the pane never ended");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A program that died in the alternate screen must not leave the shell that
    /// replaces it inside one.
    #[test]
    fn a_reconnect_leaves_the_alternate_screen_and_its_modes() {
        let mut term = Terminal::new(COLS, ROWS).expect("a terminal");
        term.vt_write(b"\x1b[?1049h\x1b[?1000h\x1b[?2004h\x1b[?1h\x1b[>3u\x1b[?25l\x1b[6 q");
        assert!(term.mode(Mode::ALT_SCREEN_SAVE).expect("mode"));
        assert!(term.mode(Mode::NORMAL_MOUSE).expect("mode"));
        assert!(!term.mode(Mode::CURSOR_VISIBLE).expect("mode"));
        assert!(!term.kitty_keyboard_flags().expect("flags").is_empty());
        reset_modes(&mut term);
        assert!(!term.mode(Mode::ALT_SCREEN_SAVE).expect("mode"));
        assert!(!term.mode(Mode::NORMAL_MOUSE).expect("mode"));
        assert!(!term.mode(Mode::BRACKETED_PASTE).expect("mode"));
        assert!(!term.mode(Mode::DECCKM).expect("mode"));
        // And the one mode that comes back rather than going away: a shell with
        // no cursor at all reads as a terminal that has frozen.
        assert!(
            term.mode(Mode::CURSOR_VISIBLE).expect("mode"),
            "the cursor stayed hidden after a reconnect"
        );
        // The shape goes with the program, though, and it can only be read from a
        // snapshot: a connection that dropped inside insert mode must not leave the
        // shell that replaces it with a bar cursor.
        let mut state = RenderState::new().expect("render state");
        let snapshot = state.update(&term).expect("snapshot");
        assert_eq!(
            snapshot.cursor().expect("cursor").visual_style,
            CursorVisualStyle::Block,
            "the cursor shape outlived the program that set it"
        );
        // The kitty flags are a stack, not a mode, and a dead program's push
        // must not reach the shell that replaces it.
        assert!(
            term.kitty_keyboard_flags().expect("flags").is_empty(),
            "the kitty flags outlived the program that set them"
        );
    }

    /// The wrapper is the *program's* request, so it has to be read from the
    /// terminal rather than assumed: a program that never sent `?2004h` gets its
    /// text bare, and one that did gets the markers. A paste that ignored the
    /// mode would show the markers to a program that never asked for them.
    #[test]
    fn bracketed_paste_follows_the_program_that_asked_for_it() {
        for (prologue, expected) in [
            ("", "one\rtwo"),
            (r"printf '\033[?2004h'; ", "\x1b[200~one\rtwo\x1b[201~"),
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            let out = dir.path().join("pasted");
            // `cat` writes whatever reaches it, byte for byte, which is the only
            // assertion that says what the program was actually handed.
            let script = format!("{prologue}exec cat > {}", out.display());
            let pane = pane(script, None, Vec::new());
            // The program has started when it has said something (the mode
            // report) and its redirection exists.
            let deadline = Instant::now() + Duration::from_secs(10);
            while !out.exists() || (!prologue.is_empty() && pane.stats().rx() == 0) {
                assert!(Instant::now() < deadline, "the program never started");
                std::thread::sleep(Duration::from_millis(10));
            }
            // The mode bytes are parsed by the pane's own loop, which is a frame
            // behind the child that wrote them.
            std::thread::sleep(Duration::from_millis(150));

            pane.send(Command::Paste(b"one\ntwo".to_vec()));
            let mut got = Vec::new();
            while got.len() < expected.len() {
                assert!(
                    Instant::now() < deadline,
                    "pasted {got:?}, expected {expected:?} (with {prologue:?})"
                );
                got = std::fs::read(&out).unwrap_or_default();
                std::thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(
                String::from_utf8_lossy(&got),
                expected,
                "while the program said {prologue:?}"
            );
        }
    }

    /// The whole chain: a program that asks for application cursor keys gets
    /// `ESC O A` for an arrow, and one that does not gets `ESC [ A`. This is the
    /// test that would have caught htop's dead keyboard.
    #[test]
    fn an_arrow_key_is_encoded_for_the_program_that_asked() {
        for (prologue, expected) in [("", "\x1b[A"), (r"printf '\033[?1h'; ", "\x1bOA")] {
            let dir = tempfile::tempdir().expect("tempdir");
            let out = dir.path().join("keys");
            let script = format!("{prologue}exec cat > {}", out.display());
            let pane = pane(script, None, Vec::new());
            let deadline = Instant::now() + Duration::from_secs(10);
            // The program has started when it has said something (its mode
            // report) and its redirection exists.
            while !out.exists() || (!prologue.is_empty() && pane.stats().rx() == 0) {
                assert!(
                    Instant::now() < deadline,
                    "the program never started (expected {expected:?})"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            // The mode bytes are parsed by the pane's loop, one step behind the
            // child that wrote them.
            std::thread::sleep(Duration::from_millis(150));

            pane.send(Command::Key(Box::new(key(
                winit::keyboard::Key::Named(NamedKey::ArrowUp),
                KeyCode::ArrowUp,
                ModifiersState::empty(),
            ))));
            wait_for_bytes(&pane, &out, expected, &prologue);
        }
    }

    /// The kitty keyboard protocol, the reason this is an encoder rather than a
    /// table. `CSI > 1 u` is an application asking for disambiguated escape
    /// codes: from then on ctrl+letter is `CSI 97;5u` instead of a control byte,
    /// while a plain character still arrives as text.
    #[test]
    fn a_kitty_application_gets_kitty_encodings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("keys");
        let script = format!(r"printf '\033[>1u'; exec cat > {}", out.display());
        let pane = pane(script, None, Vec::new());
        let deadline = Instant::now() + Duration::from_secs(10);
        while !out.exists() || pane.stats().rx() == 0 {
            assert!(Instant::now() < deadline, "the program never started");
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(150));

        // ctrl+a is the case the protocol exists for: `^A` and "insert a
        // control byte" are the same bytes, and only a disambiguated report can
        // tell them apart.
        let mut ctrl_a = key(
            winit::keyboard::Key::Character("a".into()),
            KeyCode::KeyA,
            ModifiersState::CONTROL,
        );
        ctrl_a.text = Some("a".to_owned());
        pane.send(Command::Key(Box::new(ctrl_a)));
        wait_for_bytes(&pane, &out, "\x1b[97;5u", "kitty, ctrl+a");

        // A plain character is still text, not a report: the protocol does not
        // make typing slower or unreadable for a program that only wants keys.
        let mut typed = key(
            winit::keyboard::Key::Character("b".into()),
            KeyCode::KeyB,
            ModifiersState::empty(),
        );
        typed.text = Some("b".to_owned());
        pane.send(Command::Key(Box::new(typed)));
        wait_for_bytes(&pane, &out, "\x1b[97;5ub", "kitty, plain b");
    }

    /// A character the platform resolved after a shift is *that character*, not
    /// a report of the key plus a shift bit for the program to apply. `:` is
    /// shift+`;` and `#` is shift+`3`; Neovim uppercases letters and nothing
    /// else, so those two vanished while `A` arrived.
    #[test]
    fn a_kitty_application_gets_shifted_punctuation_as_a_character() {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("keys");
        let script = format!(r"printf '\033[>1u'; exec cat > {}", out.display());
        let pane = pane(script, None, Vec::new());
        let deadline = Instant::now() + Duration::from_secs(10);
        while !out.exists() || pane.stats().rx() == 0 {
            assert!(Instant::now() < deadline, "the program never started");
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(150));

        for (typed, code) in [
            (":", KeyCode::Semicolon),
            ("#", KeyCode::Digit3),
            ("A", KeyCode::KeyA),
        ] {
            let mut stroke = key(
                winit::keyboard::Key::Character(typed.into()),
                code,
                ModifiersState::SHIFT,
            );
            stroke.text = Some(typed.to_owned());
            pane.send(Command::Key(Box::new(stroke)));
        }

        // Exactly those three characters, in order: a `CSI 59;2u` report, or a
        // key that never arrived, shows up as a mismatch rather than as silence
        // on a screen nobody is looking at.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut got = String::new();
        while got.len() < 3 {
            assert!(
                Instant::now() < deadline,
                "the program got {got:?}, expected \":#A\""
            );
            got = std::fs::read_to_string(&out).unwrap_or_default();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(got, ":#A", "a shifted character has to arrive as itself");
    }

    /// Caps Lock is not a key a program ever asks for: everybody turns it into
    /// something else, and `caps:escape` is the common one. winit then reports
    /// the position `CapsLock` with the logical key `Escape`, and the position
    /// alone is a key the encoder has no bytes for — so the whole keystroke
    /// disappeared instead of cancelling whatever was on screen.
    #[test]
    fn the_layout_can_make_caps_lock_into_escape() {
        // Legacy encoding spells Escape as one byte.
        assert_eq!(
            remapped_escape_at_a_program(""),
            b"\x1b",
            "caps:escape has to arrive as ESC, not vanish"
        );
        // And the kitty protocol, which Neovim switches on, as a functional key
        // report. The key that went missing is a key in both encodings.
        assert_eq!(
            remapped_escape_at_a_program(r"printf '\033[>1u'; "),
            b"\x1b[27u",
            "a kitty program has to be told Escape too"
        );
    }

    /// Start a program that records its input, press the Caps Lock key as a
    /// layout with `caps:escape` reports it, and answer what the program saw.
    fn remapped_escape_at_a_program(prologue: &str) -> Vec<u8> {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("keys");
        let script = format!("{prologue}exec cat > {}", out.display());
        let pane = pane(script, None, Vec::new());
        let deadline = Instant::now() + Duration::from_secs(10);
        while !out.exists() {
            assert!(Instant::now() < deadline, "the program never started");
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(150));

        // The physical key is where the finger is, the logical key is what XKB
        // made of it.
        pane.send(Command::Key(Box::new(key(
            winit::keyboard::Key::Named(NamedKey::Escape),
            KeyCode::CapsLock,
            ModifiersState::empty(),
        ))));

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut got = Vec::new();
        while got.is_empty() {
            assert!(
                Instant::now() < deadline,
                "the remapped key sent nothing; the program should have seen ESC"
            );
            got = std::fs::read(&out).unwrap_or_default();
            std::thread::sleep(Duration::from_millis(20));
        }
        got
    }

    /// `CSI > 3 u` adds press *and release* reporting. A release is the one thing
    /// legacy encoding has nothing to say about, so it is the sharpest test that
    /// the flags reached the encoder.
    #[test]
    fn a_kitty_application_that_asks_for_events_gets_them() {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("keys");
        let script = format!(r"printf '\033[>3u'; exec cat > {}", out.display());
        let pane = pane(script, None, Vec::new());
        let deadline = Instant::now() + Duration::from_secs(10);
        while !out.exists() || pane.stats().rx() == 0 {
            assert!(Instant::now() < deadline, "the program never started");
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(150));

        let mut release = key(
            winit::keyboard::Key::Character("a".into()),
            KeyCode::KeyA,
            ModifiersState::empty(),
        );
        release.state = winit::event::ElementState::Released;
        release.text = None;
        pane.send(Command::Key(Box::new(release)));
        wait_for_bytes(&pane, &out, "\x1b[97;1:3u", "kitty, release");
    }

    /// Without the kitty protocol a ctrl combination is a control byte, and the
    /// text winit reports for it must not turn into a literal character: ctrl+a
    /// once typed an "a" because the encoder trusted `text`.
    #[test]
    fn legacy_ctrl_and_alt_still_send_what_they_always_did() {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("keys");
        let script = format!("exec cat > {}", out.display());
        let pane = pane(script, None, Vec::new());
        let deadline = Instant::now() + Duration::from_secs(10);
        while !out.exists() {
            assert!(Instant::now() < deadline, "the program never started");
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(150));

        let mut ctrl_a = key(
            winit::keyboard::Key::Character("a".into()),
            KeyCode::KeyA,
            ModifiersState::CONTROL,
        );
        ctrl_a.text = Some("a".to_owned());
        pane.send(Command::Key(Box::new(ctrl_a)));
        wait_for_bytes(&pane, &out, "\x01", "legacy ctrl+a");

        let mut alt_b = key(
            winit::keyboard::Key::Character("b".into()),
            KeyCode::KeyB,
            ModifiersState::ALT,
        );
        alt_b.text = Some("b".to_owned());
        pane.send(Command::Key(Box::new(alt_b)));
        wait_for_bytes(&pane, &out, "\x01\x1bb", "legacy alt+b");
    }

    /// A press and its release are one keystroke, not two. This is the "lsls"
    /// bug: with legacy encoding a release has nothing to send, and the encoder
    /// is what knows that.
    #[test]
    fn a_release_sends_nothing_unless_it_was_asked_for() {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("keys");
        let script = format!("exec cat > {}", out.display());
        let pane = pane(script, None, Vec::new());
        let deadline = Instant::now() + Duration::from_secs(10);
        while !out.exists() {
            assert!(Instant::now() < deadline, "the program never started");
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(150));

        let mut press = key(
            winit::keyboard::Key::Character("l".into()),
            KeyCode::KeyL,
            ModifiersState::empty(),
        );
        press.text = Some("l".to_owned());
        let mut release = press.clone();
        release.state = winit::event::ElementState::Released;
        release.text = None;
        pane.send(Command::Key(Box::new(press)));
        pane.send(Command::Key(Box::new(release)));
        wait_for_bytes(&pane, &out, "l", "one l");
        // And no second one: give the pane a moment to send something it should
        // not have.
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            std::fs::read_to_string(&out).unwrap_or_default(),
            "l",
            "the release was typed as well"
        );
    }

    /// An application *asks* before it uses the protocol (`CSI ? u`), and the
    /// answer is what makes fish and nvim switch over at all. The core builds
    /// the reply and dtr's job is not to lose it.
    #[test]
    fn the_kitty_query_is_answered() {
        for (prologue, expected) in [("", "\x1b[?0u"), (r"printf '\033[>1u'; ", "\x1b[?1u")] {
            let dir = tempfile::tempdir().expect("tempdir");
            let out = dir.path().join("reply");
            let script = format!("{prologue}printf '\\033[?u'; exec cat > {}", out.display());
            let pane = pane(script, None, Vec::new());
            wait_for_bytes(&pane, &out, expected, &format!("query with {prologue:?}"));
        }
    }

    /// A letter key with the text a keyboard would have produced.
    fn key(logical: winit::keyboard::Key, code: KeyCode, mods: ModifiersState) -> KeyStroke {
        KeyStroke {
            physical: Some(code),
            key: logical,
            text: None,
            state: winit::event::ElementState::Pressed,
            repeat: false,
            composing: false,
            mods,
        }
    }

    /// A key typed *while composing* must not send its text: the commit carries
    /// it. Sending both types the romaji as well as the kanji, and an IME is
    /// useless — which is why `composing` reaches the encoder at all.
    #[test]
    fn a_composing_key_sends_nothing_and_the_commit_does() {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("keys");
        let script = format!("exec cat > {}", out.display());
        let pane = pane(script, None, Vec::new());
        let deadline = Instant::now() + Duration::from_secs(10);
        while !out.exists() {
            assert!(Instant::now() < deadline, "the program never started");
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(150));

        let mut composing = key(
            winit::keyboard::Key::Character("n".into()),
            KeyCode::KeyN,
            ModifiersState::empty(),
        );
        composing.text = Some("n".to_owned());
        composing.composing = true;
        pane.send(Command::Key(Box::new(composing)));
        std::thread::sleep(Duration::from_millis(250));
        assert!(
            std::fs::read_to_string(&out).unwrap_or_default().is_empty(),
            "a composing key reached the program"
        );

        // What the composition decided on is typed text, not a key: it goes
        // through the same path a paste's contents do.
        pane.write("日本".as_bytes().to_vec());
        wait_for_bytes(&pane, &out, "日本", "the committed text");
    }

    /// Wait until the program has written `expected`, or fail with what it got.
    fn wait_for_bytes(pane: &Pane, path: &std::path::Path, expected: &str, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut got = String::new();
        while !got.contains(expected) {
            assert!(
                Instant::now() < deadline,
                "{what}: the program got {got:?}, expected {expected:?}"
            );
            got = std::fs::read_to_string(path).unwrap_or_default();
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = pane;
    }

    /// A pane-sized grid and the program's `cat`, with or without the modes it
    /// asks for first.
    ///
    /// `cat` is the assertion that matters: whatever reaches it is what the pane
    /// sent, byte for byte, and a program that read a click can be asked what it
    /// looked like.
    fn mouse_pane(prologue: &str, out: &std::path::Path) -> Pane {
        let script = format!("{prologue}exec cat > {}", out.display());
        let pane = pane(script, None, Vec::new());
        // A grid of 80x24 in 800x480 pixels: a ten-pixel cell, so the pointer
        // positions below are whole cells and the arithmetic is checkable by eye.
        pane.send(Command::Resize {
            cols: 80,
            rows: 24,
            width_px: 800,
            height_px: 480,
        });
        // The modes are parsed by the pane's loop, which runs a step behind the
        // child that wrote them; the flag flipping is the signal the UI uses, so
        // the test waits on the same one.
        let deadline = Instant::now() + Duration::from_secs(10);
        while (!prologue.is_empty() && !pane.stats().mouse()) || !out.exists() {
            assert!(
                Instant::now() < deadline,
                "the program never started (prologue {prologue:?})"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        pane
    }

    fn click(pane: &Pane, action: mouse::Action, button: mouse::Button, x: f32, y: f32) {
        pane.send(Command::Mouse {
            action,
            button: Some(button),
            mods: libghostty_vt::key::Mods::empty(),
            x,
            y,
            down: action != mouse::Action::Release,
        });
    }

    /// A click reaches the program in the form it asked for, at the cell it
    /// happened in.
    ///
    /// `?1002h` is button-event tracking — press, release, and motion while a
    /// button is held — and `?1006h` is the SGR form: readable, and what every
    /// modern full-screen program asks for. The cell is as important as the
    /// bytes, because a click reported one cell off is a click on the wrong
    /// character, which in an editor changes the wrong text.
    ///
    /// Ctrl is the other half: `Ctrl+click` is how nvim jumps to a definition, so
    /// the modifier has to reach the report as the 16 the protocol reserves for
    /// it.
    #[test]
    fn a_click_is_reported_where_it_happened() {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("mouse");
        let pane = mouse_pane(r"printf '\033[?1002h\033[?1006h'; ", &out);
        // Column 3, row 3: two cells in from the top-left corner.
        let at = (25.0, 45.0);
        click(&pane, mouse::Action::Press, mouse::Button::Left, at.0, at.1);
        wait_for_bytes(&pane, &out, "\x1b[<0;3;3M", "a left press");
        click(
            &pane,
            mouse::Action::Release,
            mouse::Button::Left,
            at.0,
            at.1,
        );
        wait_for_bytes(&pane, &out, "\x1b[<0;3;3m", "a left release");

        let mut ctrl = libghostty_vt::key::Mods::empty();
        ctrl.set(libghostty_vt::key::Mods::CTRL, true);
        pane.send(Command::Mouse {
            action: mouse::Action::Press,
            button: Some(mouse::Button::Left),
            mods: ctrl,
            x: at.0,
            y: at.1,
            down: true,
        });
        wait_for_bytes(&pane, &out, "\x1b[<16;3;3M", "a ctrl press");
        // And nothing else: a press is one report, not a press and a phantom
        // release, and the release is the lowercase `m` of the same button.
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            std::fs::read_to_string(&out).unwrap_or_default(),
            "\x1b[<0;3;3M\x1b[<0;3;3m\x1b[<16;3;3M",
        );
    }

    /// The original X10 form: one `ESC [ M` and three bytes, each 32 plus a
    /// number. `htop` and `less` still ask this way, and bytes a program did not
    /// expect become characters on its screen.
    ///
    /// Mode 9 is X10 tracking proper — presses and nothing else — and mode 1000
    /// is what everything has used since: the same bytes, but the release is one
    /// too, with button 3 in place of the button that came up.
    #[test]
    fn an_x10_report_is_the_bytes_and_the_release_its_mode_allows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("mouse");
        let pane = mouse_pane(r"printf '\033[?9h'; ", &out);
        // Column 3, row 3: 32 + button 0, 32 + 3, 32 + 3.
        click(&pane, mouse::Action::Press, mouse::Button::Left, 25.0, 45.0);
        wait_for_bytes(&pane, &out, "\x1b[M\x20##", "an X10 press");
        click(
            &pane,
            mouse::Action::Release,
            mouse::Button::Left,
            25.0,
            45.0,
        );
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            std::fs::read_to_string(&out).unwrap_or_default(),
            "\x1b[M\x20##",
            "an X10 program was sent a release it never asked for"
        );

        // The same bytes under normal tracking, where the release is part of it:
        // X10 spells a release as "no button", which is the 3 in the low bits.
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("mouse");
        let pane = mouse_pane(r"printf '\033[?1000h'; ", &out);
        click(&pane, mouse::Action::Press, mouse::Button::Left, 25.0, 45.0);
        wait_for_bytes(&pane, &out, "\x1b[M\x20##", "a normal press");
        click(
            &pane,
            mouse::Action::Release,
            mouse::Button::Left,
            25.0,
            45.0,
        );
        wait_for_bytes(&pane, &out, "\x1b[M##", "a normal release");
    }

    /// Motion is what `?1003h` adds and `?1002h` only reports while a button is
    /// held, which is the difference between a hover effect in an editor and a
    /// hundred reports a second for a pointer nobody is pressing.
    #[test]
    fn motion_is_reported_only_to_a_program_that_asked_for_it() {
        for (prologue, expected) in [
            // Any-event tracking: a pointer that has not been clicked is still
            // motion, and button 3 in the report means "no button".
            (r"printf '\033[?1003h\033[?1006h'; ", Some("\x1b[<35;11;6M")),
            // Button-event tracking without a button down: nothing to report.
            (r"printf '\033[?1002h\033[?1006h'; ", None),
            // Normal tracking: presses only, so motion is not its business.
            (r"printf '\033[?1000h\033[?1006h'; ", None),
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            let out = dir.path().join("mouse");
            let pane = mouse_pane(prologue, &out);
            pane.send(Command::Mouse {
                action: mouse::Action::Motion,
                button: None,
                mods: libghostty_vt::key::Mods::empty(),
                x: 105.0,
                y: 105.0,
                down: false,
            });
            match expected {
                Some(bytes) => wait_for_bytes(&pane, &out, bytes, prologue),
                None => {
                    std::thread::sleep(Duration::from_millis(300));
                    assert_eq!(
                        std::fs::read_to_string(&out).unwrap_or_default(),
                        "",
                        "motion reached a program that did not ask for it ({prologue:?})"
                    );
                }
            }
        }
    }

    /// A drag is motion *with* the button named: `?1002h` reports nothing for a
    /// pointer that is only hovering, and reports the button it is holding when
    /// it is not — which is how a program tells a drag from a hover.
    #[test]
    fn a_drag_names_the_button_it_is_holding() {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("mouse");
        let pane = mouse_pane(r"printf '\033[?1002h\033[?1006h'; ", &out);
        click(&pane, mouse::Action::Press, mouse::Button::Left, 25.0, 45.0);
        wait_for_bytes(
            &pane,
            &out,
            "\x1b[<0;3;3M",
            "the press that starts the drag",
        );
        pane.send(Command::Mouse {
            action: mouse::Action::Motion,
            button: Some(mouse::Button::Left),
            mods: libghostty_vt::key::Mods::empty(),
            x: 105.0,
            y: 105.0,
            down: true,
        });
        // 32 for motion plus the left button's own 0; column 11, row 6 is the
        // cell a ten-pixel-wide, twenty-pixel-tall grid puts (105, 105) in.
        wait_for_bytes(&pane, &out, "\x1b[<32;11;6M", "a drag");
    }

    /// The wheel is buttons 4 and 5, as a press and a release, because X10
    /// reporting has no wheel event of its own. That pair is how vim, less and
    /// every pager over ssh scroll when the mouse is theirs.
    #[test]
    fn the_wheel_is_buttons_four_and_five() {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("mouse");
        let pane = mouse_pane(r"printf '\033[?1000h\033[?1006h'; ", &out);
        for (button, code) in [(mouse::Button::Four, 64), (mouse::Button::Five, 65)] {
            let down = format!("\x1b[<{code};3;3M");
            let up = format!("\x1b[<{code};3;3m");
            click(&pane, mouse::Action::Press, button, 25.0, 45.0);
            wait_for_bytes(&pane, &out, &down, "a wheel press");
            click(&pane, mouse::Action::Release, button, 25.0, 45.0);
            wait_for_bytes(&pane, &out, &up, "a wheel release");
        }
    }

    /// The whole chain through a real program: nvim with `set mouse=a` asks for
    /// button-event tracking in the SGR form, and a click puts its cursor where
    /// the click was.
    ///
    /// This is the assertion a person cares about — not the bytes, but that the
    /// editor moved — and it is the one that would catch a mode the encoder
    /// honours but nvim actually spells differently. nvim is not in every
    /// sandbox, so without it the test returns rather than fails: the bytes above
    /// are the pinned part, and this is the extra mile.
    #[test]
    fn a_click_moves_a_real_editor_cursor() {
        let Some(editor) = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|dir| dir.join("nvim"))
            .find(|path| path.is_file())
        else {
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("text");
        // A line long enough that the column is unambiguous: a click past the end
        // of a short line is clamped by the editor itself, which is not what this
        // is testing.
        std::fs::write(&file, "one\nabcdefghijklmnopqrst\nthree\n").expect("the file nvim opens");
        // Through `sh -c` so the editor's state directories can be pointed at the
        // tempdir: this sandbox has a read-only `$HOME`, and an editor that stops
        // to complain about its log file sits at a hit-enter prompt where a click
        // is an answer to the prompt rather than a click in the buffer.
        let command = format!(
            "XDG_STATE_HOME={dir}/state XDG_DATA_HOME={dir}/data \
             XDG_CACHE_HOME={dir}/cache exec {editor} --clean -n -i NONE \
             -c 'set mouse=a' {file}",
            dir = dir.path().display(),
            editor = editor.display(),
            file = file.display()
        );
        let config = pty::Config {
            program: Some("/bin/sh".to_owned()),
            args: vec!["-c".to_owned(), command],
            ..Default::default()
        };
        let budget: FrameBudget = Arc::new(AtomicU64::new(frame_budget(None).as_nanos() as u64));
        let pane =
            Pane::spawn(Transport::Shell(config), None, None, budget, || {}).expect("an nvim pane");
        pane.send(Command::Resize {
            cols: 80,
            rows: 24,
            width_px: 800,
            height_px: 480,
        });
        // Ready when it has asked for the mouse, which is `set mouse=a` taking
        // effect rather than a guess about how long nvim takes to draw.
        let deadline = Instant::now() + Duration::from_secs(30);
        while !pane.stats().mouse() {
            assert!(
                Instant::now() < deadline,
                "nvim never asked for the mouse: {:?}",
                pane.take_frame().map(|frame| frame_text(&frame))
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        // The second line of the file, four cells in: a ten by twenty pixel cell
        // puts that at (45, 25).
        click(&pane, mouse::Action::Press, mouse::Button::Left, 45.0, 25.0);
        click(
            &pane,
            mouse::Action::Release,
            mouse::Button::Left,
            45.0,
            25.0,
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut seen: Option<(Option<(u16, u16)>, String)> = None;
        loop {
            if let Some(frame) = pane.take_frame() {
                let at = frame.cursor.map(|cursor| (cursor.x, cursor.y));
                let text = frame_text(&frame);
                if at == Some((4, 1)) {
                    break;
                }
                seen = Some((at, text));
            }
            assert!(
                Instant::now() < deadline,
                "nvim put the cursor at {:?}, not at the cell clicked; the screen \
                 said:\n{}",
                seen.as_ref().and_then(|(cursor, _)| *cursor),
                seen.as_ref().map(|(_, text)| text.as_str()).unwrap_or("")
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A program that never asked for the mouse hears nothing about it. This is
    /// the other half of the UI's gate: the click is a selection there, and the
    /// shell must not be sent bytes it would print as gibberish.
    #[test]
    fn a_click_goes_nowhere_when_the_program_never_asked() {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("mouse");
        let pane = mouse_pane("", &out);
        assert!(
            !pane.stats().mouse(),
            "tracking was on with no program asking"
        );
        click(&pane, mouse::Action::Press, mouse::Button::Left, 25.0, 45.0);
        click(
            &pane,
            mouse::Action::Release,
            mouse::Button::Left,
            25.0,
            45.0,
        );
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            std::fs::read_to_string(&out).unwrap_or_default(),
            "",
            "a click reached a program that did not ask for the mouse"
        );
    }

    /// A hidden cursor is not somewhere else.
    ///
    /// A program that paints the whole screen itself — btop, vim in the
    /// alternate screen, anything with a full-screen UI — sends `?25l` and
    /// leaves the position behind, and the terminal reports *where* the cursor
    /// is whether or not it is showing. Drawing that position puts a block in
    /// the middle of a picture that deliberately has none.
    #[test]
    fn a_hidden_cursor_is_not_drawn() {
        for (prologue, expected) in [("", true), (r"printf '\033[?25l'; ", false)] {
            let script = format!("{prologue}printf 'btop'; exec cat > /dev/null");
            let pane = pane(script, None, Vec::new());
            let deadline = Instant::now() + Duration::from_secs(10);
            let seen = loop {
                if let Some(frame) = pane.take_frame() {
                    if frame_text(&frame).contains("btop") {
                        break frame.cursor;
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "the program never said anything (with {prologue:?})"
                );
                std::thread::sleep(Duration::from_millis(20));
            };
            assert_eq!(
                seen.is_some(),
                expected,
                "the cursor under {prologue:?} was {seen:?}"
            );
        }
    }

    /// Hiding the cursor after drawing is a change of its own.
    ///
    /// A frame is only published when the render state is dirty, so a mode that
    /// changes nothing else has to dirty it too — otherwise the last frame, with
    /// its cursor, stays on screen for as long as the program is up.
    #[test]
    fn hiding_the_cursor_republishes_by_itself() {
        let script = r"printf 'btop'; sleep 0.3; printf '\033[?25l'; exec cat > /dev/null";
        let pane = pane(script.to_owned(), None, Vec::new());
        wait_for(&pane, "btop", Duration::from_secs(10));
        // Long enough for the mode to arrive and be parsed.
        std::thread::sleep(Duration::from_millis(500));
        let mut last = None;
        while let Some(frame) = pane.take_frame() {
            last = Some(frame.cursor);
        }
        assert!(
            last.is_some(),
            "hiding the cursor published no frame at all, so the screen kept the \
             cursor the last one had"
        );
        assert_eq!(last.flatten(), None, "a cursor survived ?25l: {last:?}");
    }

    /// DECSCUSR is how a program says "insert" instead of "replace".
    ///
    /// `CSI Ps SP q`: 1 and 2 are a block (blinking and steady), 3 and 4 a
    /// horizontal bar, 5 and 6 a vertical one. A terminal that draws every one of
    /// them as a block is telling the user they are about to overwrite a
    /// character when the editor is going to push one in.
    #[test]
    fn the_cursor_shape_follows_the_program() {
        for (prologue, expected) in [
            ("", CursorShape::Block),
            (r"printf '\033[2 q'; ", CursorShape::Block),
            (r"printf '\033[6 q'; ", CursorShape::Bar),
            (r"printf '\033[4 q'; ", CursorShape::Underline),
        ] {
            let script = format!("{prologue}printf 'x'; exec cat > /dev/null");
            let pane = pane(script, None, Vec::new());
            let deadline = Instant::now() + Duration::from_secs(10);
            let shape = loop {
                if let Some(frame) = pane.take_frame() {
                    if frame_text(&frame).contains('x') {
                        break frame.cursor.map(|cursor| cursor.shape);
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "the program never said anything (with {prologue:?})"
                );
                std::thread::sleep(Duration::from_millis(20));
            };
            assert_eq!(shape, Some(expected), "the cursor under {prologue:?}");
        }
    }

    /// The reason any of this matters, through the editor that asks for it: nvim
    /// sends `CSI 6 SP q` when it enters insert mode and `CSI 2 SP q` when it
    /// leaves, so the block has to become a bar and go back.
    #[test]
    fn nvim_insert_mode_gets_a_bar_cursor() {
        let Some(editor) = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|dir| dir.join("nvim"))
            .find(|path| path.is_file())
        else {
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("text");
        std::fs::write(&file, "one\ntwo\n").expect("the file nvim opens");
        let command = format!(
            "XDG_STATE_HOME={dir}/state XDG_DATA_HOME={dir}/data \
             XDG_CACHE_HOME={dir}/cache exec {editor} --clean -n -i NONE {file}",
            dir = dir.path().display(),
            editor = editor.display(),
            file = file.display()
        );
        let config = pty::Config {
            program: Some("/bin/sh".to_owned()),
            args: vec!["-c".to_owned(), command],
            ..Default::default()
        };
        let budget: FrameBudget = Arc::new(AtomicU64::new(frame_budget(None).as_nanos() as u64));
        let pane =
            Pane::spawn(Transport::Shell(config), None, None, budget, || {}).expect("an nvim pane");
        let shape = |pane: &Pane| {
            pane.take_frame()
                .and_then(|frame| frame.cursor)
                .map(|cursor| cursor.shape)
        };
        // Normal mode first: the block nvim starts in.
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut seen = None;
        while !matches!(seen, Some(CursorShape::Block)) {
            if let Some(shape) = shape(&pane) {
                seen = Some(shape);
            }
            assert!(
                Instant::now() < deadline,
                "nvim never showed a block cursor: {seen:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        // `i`, and the bar nvim says insert mode looks like.
        let mut typed = key(
            winit::keyboard::Key::Character("i".into()),
            KeyCode::KeyI,
            ModifiersState::empty(),
        );
        typed.text = Some("i".to_owned());
        pane.send(Command::Key(Box::new(typed)));
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut seen = None;
        while !matches!(seen, Some(CursorShape::Bar)) {
            if let Some(shape) = shape(&pane) {
                seen = Some(shape);
            }
            assert!(
                Instant::now() < deadline,
                "nvim did not switch to a bar cursor in insert mode: {seen:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        // And back out: Escape restores the block.
        pane.send(Command::Key(Box::new(key(
            winit::keyboard::Key::Named(NamedKey::Escape),
            KeyCode::Escape,
            ModifiersState::empty(),
        ))));
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut seen = Some(CursorShape::Bar);
        while !matches!(seen, Some(CursorShape::Block)) {
            if let Some(shape) = shape(&pane) {
                seen = Some(shape);
            }
            assert!(
                Instant::now() < deadline,
                "nvim stayed in a bar cursor after Escape: {seen:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
