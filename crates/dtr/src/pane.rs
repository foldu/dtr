//! The emulation side of a session.
//!
//! A pane owns a `!Send` `libghostty_vt::Terminal` on its own thread, inside a
//! current-thread tokio runtime and a `LocalSet` (PLAN §2.5). What it publishes
//! is an owned [`Frame`] in a latest-value slot: the UI always draws the newest
//! one and never touches the terminal or blocks the emulation thread.

use std::cell::RefCell;
use std::io::Write as _;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use libghostty_vt::Terminal;
use libghostty_vt::render::{CellIterator, Dirty, RenderState, RowIterator, Snapshot};
use portable_pty::{Child, MasterPty};
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};

use crate::pty;
use crate::serial::{self, CrToCrlf};

pub const COLS: u16 = 80;
pub const ROWS: u16 = 24;

/// How long a burst of output may be parsed before a frame is published.
///
/// One publish rebuilds every cell of the grid, so it costs about 50 µs; a
/// flood arrives in chunks small enough that a publish per chunk would spend
/// the whole core rebuilding frames nobody can see. Half a frame at 60 Hz
/// keeps the picture current and cuts the publishes by three orders of
/// magnitude. A session with nothing queued publishes immediately, so a shell
/// prompt is never delayed by this.
const FRAME_BUDGET: std::time::Duration = std::time::Duration::from_millis(8);

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

#[derive(Clone, Debug)]
pub struct Frame {
    pub cols: u16,
    pub rows: u16,
    pub default_fg: [u8; 3],
    pub default_bg: [u8; 3],
    pub cursor: Option<(u16, u16)>,
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
}

/// Something the UI asks the pane to do. Keystrokes are only one of these:
/// the modem lines and the viewport live on the pane side too.
pub enum Command {
    Write(Vec<u8>),
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
    CopyRange(usize, usize),
    Paste(Vec<u8>),
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
            Transport::Shell(config) => match &config.program {
                Some(program) => format!("shell {program}"),
                None => "shell".to_owned(),
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

pub struct Pane {
    latest: Arc<Mutex<Option<Frame>>>,
    tx: tokio::sync::mpsc::UnboundedSender<Command>,
    stats: Arc<Stats>,
    /// The last thing the pane copied, for the UI to put on the clipboard.
    copied: Arc<Mutex<Option<String>>>,
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
        wake: impl Fn() + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        let latest = Arc::new(Mutex::new(None::<Frame>));
        let published = Arc::clone(&latest);
        let stats = Arc::new(Stats::default());
        let counted = Arc::clone(&stats);
        let copied: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let copy_sink = Arc::clone(&copied);
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
                        Arc::clone(&ended_flag),
                        Arc::clone(&cwd_seen),
                        theme,
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

async fn run(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<Command>,
    out: Arc<Mutex<Option<Frame>>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    transport: Transport,
    stats: Arc<Stats>,
    log_path: Option<(PathBuf, serial::LogFormat)>,
    copied: Arc<Mutex<Option<String>>>,
    ended: Arc<std::sync::atomic::AtomicBool>,
    cwd: Arc<Mutex<Option<std::path::PathBuf>>>,
    theme: Option<crate::theme::Theme>,
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
                None => write.reply(Err(libghostty_vt::terminal::ClipboardWriteError::Unsupported), false),
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
    let paste_tick = matches!(transport, Transport::Serial(_))
        .then(|| std::time::Duration::from_millis(20));
    let mut paste: Vec<u8> = Vec::new();
    let mut paste_at: Option<tokio::time::Instant> = None;
    let (reconnect_tx, mut reconnect_rx) =
        tokio::sync::mpsc::unbounded_channel::<tokio_serial::SerialStream>();
    // The pty master and child must outlive the pane: dropping the master
    // closes the terminal and takes the shell with it.
    let mut pty_keep: Option<(Box<dyn MasterPty + Send>, Box<dyn Child + Send + Sync>)> = None;

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
        Transport::Shell(cfg) => {
            let shell = cfg.open()?;
            let pty::Shell {
                master,
                child,
                mut reader,
                writer,
            } = shell;
            pty_keep = Some((master, child));
            sink = Some(Sink::Pty(writer));
            source_alive = true;
            let pty_log = log.clone();
            let pty_tx = bytes_tx.clone();
            let pty_ended = Arc::clone(&ended);
            let pty_wake = Arc::clone(&wake);
            std::thread::Builder::new()
                .name("dtr-pty".into())
                .spawn(move || {
                    let mut buf = [0u8; 8192];
                    loop {
                        match reader.read(&mut buf) {
                            // EOF: the child closed the pty, so it has exited.
                            Ok(0) => {
                                pty_ended.store(true, std::sync::atomic::Ordering::Relaxed);
                                pty_wake();
                                break;
                            }
                            Ok(n) => {
                                log_chunk(&pty_log, &buf[..n]);
                                if pty_tx.send(buf[..n].to_vec()).is_err() {
                                    break;
                                }
                            }
                            Err(err) => {
                                tracing::warn!(%err, "pty read");
                                pty_ended.store(true, std::sync::atomic::Ordering::Relaxed);
                                pty_wake();
                                break;
                            }
                        }
                    }
                })?;
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

    // Always publish once, even if the terminal is still clean: a pane that
    // has produced no output yet still has an (empty) grid to show.
    publish(&mut term, &mut state, &mut rows, &mut cells, &out, &wake, true)?;

    let mut sniffer = CwdSniffer::default();
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
                        let deadline = tokio::time::Instant::now() + FRAME_BUDGET;
                        let mut bytes = bytes;
                        let (mut chunks, mut total) = (0u32, 0usize);
                        loop {
                            chunks += 1;
                            total += bytes.len();
                            stats.add_rx(bytes.len());
                            if let Some(dir) = sniffer.feed(&bytes) {
                                if let Ok(mut slot) = cwd.lock() {
                                    *slot = Some(dir);
                                }
                            }
                            term.vt_write(&bytes);
                            if tokio::time::Instant::now() >= deadline {
                                break;
                            }
                            match bytes_rx.try_recv() {
                                Ok(next) => bytes = next,
                                Err(_) => break,
                            }
                        }
                        flush_replies(&replies, &mut sink).await;
                        tracing::debug!(chunks, bytes = total, "burst parsed");
                        if tokio::time::Instant::now() >= next_publish {
                            publish(&mut term, &mut state, &mut rows, &mut cells, &out, &wake, false)?;
                            next_publish = tokio::time::Instant::now() + FRAME_BUDGET;
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
                publish(&mut term, &mut state, &mut rows, &mut cells, &out, &wake, false)?;
                next_publish = tokio::time::Instant::now() + FRAME_BUDGET;
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
                        // Typing while scrolled back means you want the live
                        // screen: follow the cursor rather than swallowing keys.
                        if stats.scrollback() > 0 {
                            term.scroll_viewport(libghostty_vt::terminal::ScrollViewport::Bottom);
                            note_scrollback(&term, &stats);
                        }
                        if let Some(sink) = sink.as_mut() {
                            match sink.write(&bytes).await {
                                Ok(()) => stats.add_tx(bytes.len()),
                                Err(err) => tracing::warn!(%err, "write to session"),
                            }
                        }
                        // Local echo is for devices that do not echo: the bytes
                        // are shown without being sent anywhere else.
                        if echo {
                            term.vt_write(&bytes);
                            publish(&mut term, &mut state, &mut rows, &mut cells, &out, &wake, false)?;
                        }
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
                        if let Err(err) = term.resize(new_cols, new_rows, width_px, height_px) {
                            tracing::warn!(%err, "resize the grid");
                        }
                        // The shell needs the size too, or it wraps for the old
                        // width and the terminal shows a staircase.
                        if let Some((pty, _)) = pty_keep.as_mut() {
                            let size = portable_pty::PtySize {
                                rows: new_rows,
                                cols: new_cols,
                                pixel_width: u16::try_from(width_px).unwrap_or(u16::MAX),
                                pixel_height: u16::try_from(height_px).unwrap_or(u16::MAX),
                            };
                            if let Err(err) = pty.resize(size) {
                                tracing::warn!(%err, "resize the pty");
                            }
                        }
                        tracing::debug!(cols = new_cols, rows = new_rows, "resized");
                        publish(&mut term, &mut state, &mut rows, &mut cells, &out, &wake, true)?;
                    }
                    Command::Scroll(lines) => {
                        term.scroll_viewport(libghostty_vt::terminal::ScrollViewport::Delta(lines));
                        // Forced: a viewport move does not necessarily mark the
                        // render state dirty, and a scroll that does not repaint
                        // looks like a broken wheel.
                        publish(&mut term, &mut state, &mut rows, &mut cells, &out, &wake, true)?;
                        note_scrollback(&term, &stats);
                    }
                    Command::ScrollEdge { top } => {
                        term.scroll_viewport(if top {
                            libghostty_vt::terminal::ScrollViewport::Top
                        } else {
                            libghostty_vt::terminal::ScrollViewport::Bottom
                        });
                        publish(&mut term, &mut state, &mut rows, &mut cells, &out, &wake, true)?;
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
                                    publish(&mut term, &mut state, &mut rows, &mut cells, &out, &wake, true)?;
                                }
                                Err(err) => tracing::warn!(%err, "could not switch theme"),
                            }
                        }
                    }
                    Command::Copy | Command::CopyRange(_, _) => {
                        // Read the text out of the terminal now. The published
                        // frame cannot be used: the UI takes it out of the slot
                        // to draw it, so a copy would find nothing there — which
                        // is exactly the "copied 0 chars" that shipped.
                        let text = match copy_text(&mut term, &mut state, &mut rows, &mut cells, &command) {
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
                    Command::Paste(bytes) => match paste_tick {
                        // No pacing: hand the whole thing over at once.
                        None => {
                            if let Some(sink) = sink.as_mut() {
                                match sink.write(&bytes).await {
                                    Ok(()) => stats.add_tx(bytes.len()),
                                    Err(err) => tracing::warn!(%err, "paste write"),
                                }
                            }
                        }
                        Some(_) => {
                            paste = bytes;
                            paste_at = Some(tokio::time::Instant::now());
                        }
                    },
                }
            }
        }
    }

    let _ = &pty_keep;
    Ok(())
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
    command: &Command,
) -> anyhow::Result<String> {
    let snapshot = state.update(term)?;
    let mut frame = Frame {
        cols: snapshot.cols()?,
        rows: snapshot.rows()?,
        default_fg: [0; 3],
        default_bg: [0; 3],
        cursor: None,
        cells: Vec::with_capacity(
            usize::from(snapshot.cols()?) * usize::from(snapshot.rows()?),
        ),
    };
    fill_cells(&snapshot, rows, cells, &mut frame)?;
    Ok(match command {
        Command::CopyRange(start, end) => frame_text_range(&frame, *start, *end),
        _ => frame_text(&frame),
    })
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

/// Copy from `start` to `end`, inclusive, as text.
///
/// The range is two linear cell indices in either order; a partial first and
/// last row is normal (a drag rarely starts at column zero), so rows are cut at
/// the selection's own edges and only the trailing blanks are trimmed.
pub(crate) fn frame_text_range(frame: &Frame, start: usize, end: usize) -> String {
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
) -> anyhow::Result<()> {
    let started = std::time::Instant::now();
    let snapshot = state.update(term)?;

    // A clean frame means the UI already has the current contents.
    if !force && matches!(snapshot.dirty()?, Dirty::Clean) {
        return Ok(());
    }

    let colors = snapshot.colors()?;
    let mut frame = Frame {
        cols: snapshot.cols()?,
        rows: snapshot.rows()?,
        default_fg: rgb(colors.foreground),
        default_bg: rgb(colors.background),
        cursor: snapshot.cursor_viewport()?.map(|c| (c.x, c.y)),
        cells: Vec::with_capacity(usize::from(snapshot.cols()?) * usize::from(snapshot.rows()?)),
    };

    fill_cells(&snapshot, rows, cells, &mut frame)?;

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
    wake();
    Ok(())
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

impl CwdSniffer {
    const MAX: usize = 8 * 1024;

    fn feed(&mut self, bytes: &[u8]) -> Option<std::path::PathBuf> {
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
            if let Some(path) = parse_file_uri(payload) {
                found = Some(path);
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

/// This machine's name, for the host part of an OSC 7 URI. A shell that reports
/// `file://<hostname>/path` on the local machine is still local; a shell on
/// another host is not, and its path must never be treated as a local directory.
fn local_hostname() -> Option<&'static str> {
    static NAME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        std::fs::read_to_string("/etc/hostname")
            .ok()
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .or_else(|| std::env::var("HOSTNAME").ok())
    })
    .as_deref()
}

/// `file://host/path` to a path.
///
/// The host decides whether the path means anything here: empty, `localhost` or
/// this machine's own name is a local directory; anything else is a shell on
/// another host, and a path from there is not a local directory — returning it
/// would make "new tab here" try to `cd` into a path that happens to exist on the
/// wrong machine.
fn parse_file_uri(payload: &[u8]) -> Option<std::path::PathBuf> {
    let text = std::str::from_utf8(payload).ok()?;
    let rest = text.strip_prefix("file://")?;
    let slash = rest.find('/')?;
    let host = &rest[..slash];
    let local = host.is_empty()
        || host.eq_ignore_ascii_case("localhost")
        || local_hostname().is_some_and(|name| host.eq_ignore_ascii_case(name));
    if !local {
        return None;
    }
    let mut path = String::with_capacity(rest.len() - slash);
    let mut chars = rest[slash..].chars();
    while let Some(ch) = chars.next() {
        if ch == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            let byte = u8::from_str_radix(&hex, 16).ok()?;
            path.push(byte as char);
        } else {
            path.push(ch);
        }
    }
    Some(std::path::PathBuf::from(path))
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
            }
            frame.cells.push(out);
        }
    }
    Ok(())
}

fn rgb(c: libghostty_vt::style::RgbColor) -> [u8; 3] {
    [c.r, c.g, c.b]
}

const DEMO: &[u8] = b"\x1b[32mdtr\x1b[0m \xe2\x80\x94 a serial-first terminal\r\n\r\n\
 normal  \x1b[1mbold\x1b[0m  \x1b[3mitalic\x1b[0m  \x1b[31mred\x1b[0m  \x1b[44m blue bg \x1b[0m  \x1b[7m inverse \x1b[0m\r\n\r\n\
 1  \xe2\x94\x8c\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x90\r\n\
 2  \xe2\x94\x82 box     \xe2\x94\x82\r\n\
 3  \xe2\x94\x94\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x98\r\n";

#[cfg(test)]
mod tests {
    use super::{Cell, Frame, frame_text};

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
            cells,
        }
    }

    #[test]
    fn the_vt_core_gives_back_the_characters_we_sent() {
        use libghostty_vt::{RenderState, Terminal};
        use libghostty_vt::render::{CellIterator, RowIterator};

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

    #[test]
    fn copying_a_range_cuts_at_the_selection_edges() {
        // Two rows: "abcdefgh" and "ijklmnop".
        let frame = frame(&["abcdefgh", "ijklmnop"]);
        // From 'c' (index 2) to 'j' (index 9), both inclusive: the rest of the
        // first row, then the first two cells of the next.
        assert_eq!(super::frame_text_range(&frame, 2, 9), "cdefgh\nij");
    }

    #[test]
    fn copying_a_range_works_backwards() {
        // A drag can end before it starts; the range is the same either way.
        let frame = frame(&["abcdefgh", "ijklmnop"]);
        assert_eq!(super::frame_text_range(&frame, 9, 2), super::frame_text_range(&frame, 2, 9));
    }

    #[test]
    fn copying_a_single_cell_gives_one_character() {
        let frame = frame(&["abcdefgh"]);
        assert_eq!(super::frame_text_range(&frame, 3, 3), "d");
    }

    #[test]
    fn a_range_past_the_end_is_clamped() {
        let frame = frame(&["ab"]);
        assert_eq!(super::frame_text_range(&frame, 0, 9999), "ab");
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

    fn path_of(input: &[u8]) -> Option<String> {
        CwdSniffer::default()
            .feed(input)
            .map(|path| path.display().to_string())
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
                .map(|path| path.display().to_string())
                .as_deref(),
            Some("/home/barnabas")
        );
    }

    #[test]
    fn another_host_is_not_a_local_directory() {
        // A remote shell's path means nothing here; adopting it would make
        // "new tab here" cd into a directory on the wrong machine.
        assert_eq!(path_of(b"\x1b]7;file://build-host/home/x\x07"), None);
    }

    #[test]
    fn localhost_and_this_machine_are_local() {
        assert_eq!(
            path_of(b"\x1b]7;file://localhost/srv\x07").as_deref(),
            Some("/srv")
        );
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
