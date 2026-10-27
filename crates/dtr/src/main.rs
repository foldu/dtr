//! dtr — a serial-first terminal.

mod app;
mod pane;
mod screenshot;
mod ui;

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

    /// Render one frame offscreen to a PNG and exit, without opening a window
    #[arg(long, value_name = "PATH")]
    screenshot: Option<PathBuf>,

    /// Frame size for --screenshot, in pixels
    #[arg(long, default_value = "900x620", value_name = "WxH")]
    size: String,
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

    if let Some(path) = args.screenshot.as_deref() {
        let (width, height) = parse_size(&args.size)?;
        return app::capture_headless(path, width, height);
    }

    app::run()
}
