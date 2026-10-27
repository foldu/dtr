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

#[derive(Clone, Debug, Default)]
pub struct Cell {
    pub text: String,
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
#[derive(Debug, Default)]
pub struct Stats {
    rx: AtomicU64,
    tx: AtomicU64,
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
    tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    stats: Arc<Stats>,
    /// A fatal pane error. The pane thread owns the terminal, so a failure to
    /// open a port happens out of sight; this is how it reaches the UI.
    error: Arc<Mutex<Option<String>>>,
}

impl Pane {
    /// Spawn the pane thread. `wake` is fired whenever a new frame is ready.
    ///
    /// With no serial config the pane shows a demo screen; with one it opens
    /// the port and drives it.
    pub fn spawn(
        transport: Transport,
        log: Option<PathBuf>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        let latest = Arc::new(Mutex::new(None::<Frame>));
        let published = Arc::clone(&latest);
        let stats = Arc::new(Stats::default());
        let counted = Arc::clone(&stats);
        let error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let failure = Arc::clone(&error);
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();

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
        })
    }

    /// Take the newest frame, if the emulator produced one since the last call.
    pub fn take_frame(&self) -> Option<Frame> {
        self.latest.lock().ok().and_then(|mut slot| slot.take())
    }

    pub fn stats(&self) -> &Arc<Stats> {
        &self.stats
    }

    /// The pane's fatal error, if it hit one.
    pub fn error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|slot| slot.clone())
    }

    /// Queue bytes for the emulator. Non-blocking; safe from the UI thread.
    pub fn write(&self, bytes: Vec<u8>) {
        let _ = self.tx.send(bytes);
    }
}

async fn run(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    out: Arc<Mutex<Option<Frame>>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    transport: Transport,
    stats: Arc<Stats>,
    log_path: Option<PathBuf>,
) -> anyhow::Result<()> {
    // Raw bytes as they arrive, before any mapping: this is what the wire said,
    // which is what you want when the point is to find out what the device did.
    let log: Option<Arc<Mutex<std::fs::File>>> = match &log_path {
        Some(path) => match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            Ok(file) => Some(Arc::new(Mutex::new(file))),
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
    term.on_pty_write(move |_term, data| {
        reply_sink.borrow_mut().extend_from_slice(data);
    })?;
    let mut state = RenderState::new()?;
    let mut rows = RowIterator::new()?;
    let mut cells = CellIterator::new()?;

    let (bytes_tx, mut bytes_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let mut sink: Option<Sink> = None;
    let mut source_alive = false;
    // The pty master and child must outlive the pane: dropping the master
    // closes the terminal and takes the shell with it.
    let mut pty_keep: Option<(Box<dyn MasterPty + Send>, Box<dyn Child + Send + Sync>)> = None;

    match transport {
        Transport::Serial(cfg) => {
            let stream = cfg.open()?;
            tracing::info!(port = %cfg.port, baud = cfg.baud, "serial open");
            let (read_half, write_half) = tokio::io::split(stream);
            sink = Some(Sink::Serial(write_half));
            let mapper = cfg.cr_to_crlf.then(CrToCrlf::default);
            source_alive = true;
            tokio::task::spawn_local(read_loop(read_half, mapper, bytes_tx, log.clone()));
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
            std::thread::Builder::new()
                .name("dtr-pty".into())
                .spawn(move || {
                    let mut buf = [0u8; 8192];
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                log_chunk(&pty_log, &buf[..n]);
                                if bytes_tx.send(buf[..n].to_vec()).is_err() {
                                    break;
                                }
                            }
                            Err(err) => {
                                tracing::warn!(%err, "pty read");
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
            let tx = bytes_tx;
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

    loop {
        tokio::select! {
            chunk = bytes_rx.recv(), if source_alive => {
                match chunk {
                    Some(bytes) => {
                        stats.add_rx(bytes.len());
                        term.vt_write(&bytes);
                        flush_replies(&replies, &mut sink).await;
                        publish(&mut term, &mut state, &mut rows, &mut cells, &out, &wake, false)?;
                    }
                    None => source_alive = false,
                }
            }
            cmd = rx.recv() => {
                let Some(bytes) = cmd else { break };
                if let Some(sink) = sink.as_mut() {
                    match sink.write(&bytes).await {
                        Ok(()) => stats.add_tx(bytes.len()),
                        Err(err) => tracing::warn!(%err, "write to session"),
                    }
                }
            }
        }
    }

    let _ = &pty_keep;
    Ok(())
}

/// Append the raw bytes to the session log, if one is open.
fn log_chunk(log: &Option<Arc<Mutex<std::fs::File>>>, bytes: &[u8]) {
    let Some(log) = log else { return };
    match log.lock() {
        Ok(mut file) => {
            if let Err(err) = file.write_all(bytes) {
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
    log: Option<Arc<Mutex<std::fs::File>>>,
) {
    let mut buf = [0u8; 8192];
    let mut out = Vec::with_capacity(8192);
    loop {
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

    match out.lock() {
        Ok(mut slot) => *slot = Some(frame),
        Err(err) => tracing::error!("frame slot poisoned: {err}"),
    }
    wake();
    Ok(())
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
