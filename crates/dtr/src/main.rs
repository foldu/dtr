//! dtr — a serial-first terminal.

mod app;
mod pane;
mod pty;
mod screenshot;
mod serial;
mod ui;

use pane::Transport;

use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "dtr", version, about = "A serial-first terminal")]
struct Args {
    /// Serial port path, e.g. /dev/ttyUSB0 or COM7
    #[arg(short, long)]
    port: Option<String>,

    /// Baud rate
    #[arg(short, long, default_value_t = 115_200)]
    baud: u32,

    /// Shell program to launch when no port is given (default: $SHELL, else bash/pwsh)
    #[arg(long, value_name = "PROGRAM")]
    shell: Option<String>,

    /// Show a canned screen instead of launching anything
    #[arg(long)]
    demo: bool,

    /// Serial byte mapping: `cr=crlf` (default) or `none`
    #[arg(long, value_name = "MAP", default_value = "cr=crlf")]
    imap: String,

    /// Render one frame offscreen to a PNG and exit, without opening a window
    #[arg(long, value_name = "PATH")]
    screenshot: Option<PathBuf>,

    /// Frame size for --screenshot, in pixels
    #[arg(long, default_value = "900x620", value_name = "WxH")]
    size: String,

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

    let transport = if args.demo {
        Transport::Demo
    } else if let Some(port) = args.port.clone() {
        Transport::Serial(serial::Config {
            port,
            baud: args.baud,
            cr_to_crlf: parse_imap(&args.imap)?,
        })
    } else {
        Transport::Shell(pty::Config {
            program: args.shell.clone(),
        })
    };

    if let Some(path) = args.screenshot.as_deref() {
        let (width, height) = parse_size(&args.size)?;
        let delay = std::time::Duration::from_millis(args.screenshot_delay);
        return app::capture_headless(path, width, height, transport, delay);
    }

    app::run(transport)
}
