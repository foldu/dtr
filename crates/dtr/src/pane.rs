//! The emulation side of a session.
//!
//! A pane owns a `!Send` `libghostty_vt::Terminal` on its own thread, inside a
//! current-thread tokio runtime and a `LocalSet` (PLAN §2.5). What it publishes
//! is an owned [`Frame`] in a latest-value slot: the UI always draws the newest
//! one and never touches the terminal or blocks the emulation thread.

use std::sync::{Arc, Mutex};
use std::thread;

use libghostty_vt::Terminal;
use libghostty_vt::render::{CellIterator, Dirty, RenderState, RowIterator, Snapshot};
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};

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

pub struct Pane {
    latest: Arc<Mutex<Option<Frame>>>,
    tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
}

impl Pane {
    /// Spawn the pane thread. `wake` is fired whenever a new frame is ready.
    ///
    /// With no serial config the pane shows a demo screen; with one it opens
    /// the port and drives it.
    pub fn spawn(
        serial: Option<serial::Config>,
        wake: impl Fn() + Send + 'static,
    ) -> anyhow::Result<Self> {
        let latest = Arc::new(Mutex::new(None::<Frame>));
        let published = Arc::clone(&latest);
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
                    if let Err(err) = run(rx, published, wake, serial).await {
                        tracing::error!(%err, "pane");
                    }
                });
            })?;

        Ok(Self { latest, tx })
    }

    /// Take the newest frame, if the emulator produced one since the last call.
    pub fn take_frame(&self) -> Option<Frame> {
        self.latest.lock().ok().and_then(|mut slot| slot.take())
    }

    /// Queue bytes for the emulator. Non-blocking; safe from the UI thread.
    pub fn write(&self, bytes: Vec<u8>) {
        let _ = self.tx.send(bytes);
    }
}

async fn run(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    out: Arc<Mutex<Option<Frame>>>,
    wake: impl Fn(),
    serial: Option<serial::Config>,
) -> anyhow::Result<()> {
    let mut term = Terminal::new(COLS, ROWS)?;
    let mut state = RenderState::new()?;
    let mut rows = RowIterator::new()?;
    let mut cells = CellIterator::new()?;

    let mut writer: Option<WriteHalf<tokio_serial::SerialStream>> = None;
    let (bytes_tx, mut bytes_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let mut serial_alive = false;

    match &serial {
        Some(cfg) => {
            // A failed open is fatal: the user asked for this port by name.
            let stream = cfg.open()?;
            tracing::info!(port = %cfg.port, baud = cfg.baud, "serial open");
            let (read_half, write_half) = tokio::io::split(stream);
            writer = Some(write_half);
            let mapper = cfg.cr_to_crlf.then(CrToCrlf::default);
            serial_alive = true;
            tokio::task::spawn_local(read_loop(read_half, mapper, bytes_tx));
        }
        None => term.vt_write(DEMO),
    }

    publish(&mut term, &mut state, &mut rows, &mut cells, &out, &wake)?;

    loop {
        tokio::select! {
            chunk = bytes_rx.recv(), if serial_alive => {
                match chunk {
                    Some(bytes) => {
                        term.vt_write(&bytes);
                        publish(&mut term, &mut state, &mut rows, &mut cells, &out, &wake)?;
                    }
                    None => serial_alive = false,
                }
            }
            cmd = rx.recv() => {
                let Some(bytes) = cmd else { break };
                if let Some(w) = writer.as_mut() {
                    if let Err(err) = w.write_all(&bytes).await {
                        tracing::warn!(%err, "serial write");
                    }
                }
            }
        }
    }
    Ok(())
}

/// Read the port until it fails or closes, mapping bytes on the way through.
async fn read_loop(
    mut reader: ReadHalf<tokio_serial::SerialStream>,
    mut mapper: Option<CrToCrlf>,
    tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
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
    wake: &impl Fn(),
) -> anyhow::Result<()> {
    let snapshot = state.update(term)?;

    // A clean frame means the UI already has the current contents.
    if matches!(snapshot.dirty()?, Dirty::Clean) {
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
