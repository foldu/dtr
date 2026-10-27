use clap::Parser;

/// dtr — a serial-first terminal.
#[derive(Debug, Parser)]
#[command(name = "dtr", version, about = "A serial-first terminal")]
struct Args {
    /// Serial port path, e.g. /dev/ttyUSB0 or COM7
    #[arg(short, long)]
    port: Option<String>,

    /// Baud rate
    #[arg(short, long, default_value_t = 115_200)]
    baud: u32,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Prove the pinned wrapper links against the nix-built libghostty-vt.
    let mut term = libghostty_vt::Terminal::new(80, 24).expect("create terminal");
    let _ = term.vt_write(b"\x1b[32mdtr\x1b[0m 80x24\r\n");

    println!("{args:?}");
    Ok(())
}
