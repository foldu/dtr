//! dtr — a serial-first terminal.

mod app;
mod input;
mod logbook;
mod modem;
mod palette;
mod pane;
mod pty;
mod screenshot;
mod serial;
mod sources;
mod ui;

use pane::Transport;

use std::path::PathBuf;

use clap::Parser;

/// The default Windows allocator is slow and contends badly between the pane
/// thread, the PTY reader and the UI; mimalloc is the usual answer. It is the
/// allocator on every platform so a bug here shows up where it is easier to see.
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Debug, Parser)]
#[command(name = "dtr", version, about = "A serial-first terminal")]
struct Args {
    /// Serial port path, e.g. /dev/ttyUSB0 or COM7
    #[arg(short, long)]
    port: Option<String>,

    /// Baud rate (remembered per port when omitted)
    #[arg(short, long)]
    baud: Option<u32>,

    /// Shell program to launch when no port is given (default: $SHELL, else bash/pwsh)
    #[arg(long, value_name = "PROGRAM")]
    shell: Option<String>,

    /// Show a canned screen instead of launching anything
    #[arg(long)]
    demo: bool,

    /// Open the command palette on start
    #[arg(long)]
    palette: bool,

    /// Serial byte mapping: `cr=crlf` (default) or `none` (remembered per port)
    #[arg(long, value_name = "MAP")]
    imap: Option<String>,

    /// Path to the state database
    #[arg(long, value_name = "PATH")]
    db: Option<PathBuf>,

    /// Do not read or write remembered settings
    #[arg(long)]
    no_remember: bool,

    /// Replay a captured byte stream instead of opening a device
    #[arg(long, value_name = "PATH")]
    replay: Option<PathBuf>,

    /// Append received bytes to this file
    #[arg(long, value_name = "PATH")]
    log: Option<PathBuf>,

    /// Session log shape: `raw` (replayable) or `timestamped` (readable)
    #[arg(long, value_name = "FORMAT", default_value = "raw")]
    log_format: String,

    /// Why this session happened; stored in the logbook
    #[arg(long, value_name = "TEXT")]
    note: Option<String>,

    /// List recorded sessions and exit
    #[arg(long)]
    logbook: bool,

    /// Render one frame offscreen to a PNG and exit, without opening a window
    #[arg(long, value_name = "PATH")]
    screenshot: Option<PathBuf>,

    /// Frame size for --screenshot, in pixels
    #[arg(long, default_value = "900x620", value_name = "WxH")]
    size: String,

    /// Send these bytes once the session is attached (escapes: \n \r \t \e \\)
    #[arg(long, value_name = "TEXT")]
    send: Option<String>,

    /// Wait this long before capturing, so a session can produce output
    #[arg(long, default_value_t = 300, value_name = "MS")]
    screenshot_delay: u64,
}

fn parse_imap(spec: &str) -> anyhow::Result<bool> {
    match spec {
        "none" => Ok(false),
        "cr=crlf" => Ok(true),
        other => anyhow::bail!("unknown imap {other:?}; expected `cr=crlf` or `none`"),
    }
}

fn parse_size(size: &str) -> anyhow::Result<(u32, u32)> {
    let (w, h) = size
        .split_once('x')
        .ok_or_else(|| anyhow::anyhow!("expected WxH, got {size:?}"))?;
    Ok((w.trim().parse()?, h.trim().parse()?))
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    tracing::debug!(?args, "starting");

    // Never fatal: a broken or read-only state file must not stop a terminal
    // from opening a port.
    let store: Option<std::sync::Arc<dtr_store::Store>> = if args.no_remember {
        None
    } else {
        let path = args.db.clone().or_else(dtr_store::Store::default_path);
        match path {
            Some(path) => match dtr_store::Store::open(&path) {
                Ok(store) => Some(std::sync::Arc::new(store)),
                Err(err) => {
                    tracing::warn!(path = %path.display(), %err, "no remembered settings");
                    None
                }
            },
            None => None,
        }
    };

    let transport = if args.demo {
        Transport::Demo
    } else if let Some(path) = args.replay.clone() {
        Transport::Replay(path)
    } else if let Some(port) = args.port.clone() {
        let remembered = store
            .as_ref()
            .and_then(|store| store.serial(&port).ok().flatten())
            .unwrap_or_default();

        // Precedence: what was asked for, then what was remembered, then the
        // built-in 115200 8N1 with CR→CRLF on.
        let settings = dtr_store::SerialSettings {
            baud: args.baud.unwrap_or(remembered.baud),
            cr_to_crlf: match args.imap.as_deref() {
                Some(spec) => parse_imap(spec)?,
                None => remembered.cr_to_crlf,
            },
        };

        if let Some(store) = store.as_ref() {
            if let Err(err) = store.remember_serial(&port, &settings) {
                tracing::warn!(%err, "could not remember the port settings");
            }
            let _ = store.set_last_port(&port);
        }

        Transport::Serial(serial::Config {
            port,
            baud: settings.baud,
            cr_to_crlf: settings.cr_to_crlf,
            auto_reconnect: true,
        })
    } else {
        Transport::Shell(pty::Config {
            program: args.shell.clone(),
        })
    };

    if args.logbook {
        let Some(store) = store.as_ref() else {
            anyhow::bail!("--logbook needs the state database; drop --no-remember");
        };
        return logbook::print(store);
    }

    let log = match args.log.clone() {
        Some(path) => {
            let format = match args.log_format.as_str() {
                "raw" => serial::LogFormat::Raw,
                "timestamped" => serial::LogFormat::Timestamped,
                other => anyhow::bail!("unknown log format {other:?}; expected `raw` or `timestamped`"),
            };
            Some((path, format))
        }
        None => None,
    };

    let send = args.send.as_deref().map(input::unescape).unwrap_or_default();

    // A session is recorded when it starts, so a crash leaves a row with no
    // end — which is exactly what `unfinished` in the listing means.
    let session = match (store.as_deref(), &transport) {
        (_, Transport::Demo) | (None, _) => None,
        (Some(store), transport) => {
            let mut record = dtr_store::SessionRecord {
                label: transport.describe(),
                note: args.note.clone(),
                started_at: dtr_store::now_ms(),
                ..dtr_store::SessionRecord::default()
            };
            if let Transport::Serial(config) = transport {
                record.port = Some(config.port.clone());
                record.baud = Some(config.baud);
                record.imap = Some(if config.cr_to_crlf { "cr=crlf" } else { "none" }.to_owned());
            }
            match store.begin_session(&record) {
                Ok(id) => Some(id),
                Err(err) => {
                    tracing::warn!(%err, "could not record the session");
                    None
                }
            }
        }
    };

    if let Some(path) = args.screenshot.as_deref() {
        let (width, height) = parse_size(&args.size)?;
        let delay = std::time::Duration::from_millis(args.screenshot_delay);
        let startup = app::Startup {
            transport,
            send,
            log: log.clone(),
            store: store.clone(),
            palette: args.palette,
        };
        let result = app::capture_headless(startup, path, width, height, delay);
        finish(store.as_deref(), session, "screenshot", &result);
        return result;
    }

    let startup = app::Startup {
        transport,
        send,
        log: log.clone(),
        store: store.clone(),
        palette: args.palette,
    };
    let result = app::run(startup);
    finish(store.as_deref(), session, "closed", &result);
    result
}

/// Close the logbook row, whatever happened.
fn finish(
    store: Option<&dtr_store::Store>,
    session: Option<u64>,
    clean: &str,
    result: &anyhow::Result<()>,
) {
    let (Some(store), Some(id)) = (store, session) else {
        return;
    };
    let reason = match result {
        Ok(()) => clean.to_owned(),
        Err(err) => format!("error: {err}"),
    };
    if let Err(err) = store.end_session(id, dtr_store::now_ms(), &reason) {
        tracing::warn!(%err, "could not close the logbook row");
    }
}
