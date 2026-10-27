//! dtr — a serial-first terminal.

mod app;
mod explorer;
mod input;
mod layout;
mod logbook;
mod modem;
mod palette;
mod profile;
mod render;
mod pane;
mod pty;
mod screenshot;
mod serial;
mod sources;
mod theme;
mod ui;

use pane::Transport;

use std::path::PathBuf;

use clap::Parser;

/// The default Windows allocator is slow and contends badly between the pane
/// thread, the PTY reader and the UI; mimalloc is the usual answer. It is the
/// allocator on every platform so a bug here shows up where it is easier to see.
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Non-interactive commands. They are real subcommands rather than more flat
/// flags because `theme` is the start of a family (`list`, `cp` and `edit` are
/// already in the memo) and a flag per verb would sprawl; the existing flat
/// flags keep working unchanged.
#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Work with theme files
    Theme {
        #[command(subcommand)]
        action: ThemeAction,
    },
}

#[derive(Debug, clap::Subcommand)]
enum ThemeAction {
    /// Convert an iTerm2 .itermcolors file into a dtr theme
    Import {
        /// The .itermcolors file to read
        file: PathBuf,

        /// Theme name to write (default: the input file name without extension)
        #[arg(long, value_name = "NAME")]
        name: Option<String>,

        /// Overwrite an existing theme file
        #[arg(long)]
        force: bool,
    },
}

#[derive(Debug, Parser)]
#[command(name = "dtr", version, about = "A serial-first terminal")]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,

    /// Prefer the GPU whose name contains TEXT, e.g. --gpu nvidia
    #[arg(long, value_name = "TEXT")]
    gpu: Option<String>,

    /// Close the window after MS milliseconds (shutdown smoke test)
    #[arg(long, value_name = "MS")]
    exit_after: Option<u64>,

    /// Write a CPU flamegraph here on exit (Linux; samples this process)
    #[arg(long, value_name = "PATH")]
    cpu_profile: Option<PathBuf>,

    /// Verbose logging to stderr: -v for debug, -vv for trace (RUST_LOG overrides)
    #[arg(short = 'v', long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Serial port path, e.g. /dev/ttyUSB0 or COM7
    #[arg(short, long)]
    port: Option<String>,

    /// Baud rate (remembered per port when omitted)
    #[arg(short, long)]
    baud: Option<u32>,

    /// Shell program to launch when no port is given (default: $SHELL, else bash/pwsh)
    #[arg(long, value_name = "PROGRAM")]
    shell: Option<String>,

    /// Draw the captured frame as a split (for reviewing the layout)
    #[arg(long, value_name = "h|v")]
    split: Option<String>,

    /// Scroll N lines into the scrollback before capturing
    #[arg(long, value_name = "N", default_value_t = 0)]
    scroll: i64,

    /// Select cells START..=END (linear indices) in the screenshot
    #[arg(long, value_name = "START:END")]
    select: Option<String>,

    /// Show a canned screen instead of launching anything
    #[arg(long)]
    demo: bool,

    /// Open an ssh session on HOST, using the system's ssh client
    #[arg(long, value_name = "HOST")]
    ssh: Option<String>,

    /// Open the command palette on start
    #[arg(long)]
    palette: bool,

    /// Open a file-browser pane next to the initial session, at PATH if given
    #[arg(long, value_name = "PATH", num_args = 0..=1, default_missing_value = "")]
    files: Option<String>,

    /// Config file (default: the platform's config dir, `dtr/config.toml`)
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Print the config file path and exit
    #[arg(long)]
    config_path: bool,

    /// List the themes found in the search paths and exit
    #[arg(long)]
    themes: bool,

    /// Write a commented default config file, if there is none
    #[arg(long)]
    config_init: bool,

    /// Set one value (`section.key value`) in the config file and exit
    #[arg(long, num_args = 2, value_names = ["KEY", "VALUE"])]
    config_set: Option<Vec<String>>,

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

/// Run one non-interactive command and exit. These never open a terminal.
fn run_command(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Theme { action } => match action {
            ThemeAction::Import { file, name, force } => {
                let mut import = theme::import_itermcolors(&file)?;
                // Alpha is the one thing the conversion has to throw away;
                // say so instead of silently changing the colour.
                for warning in &import.warnings {
                    eprintln!("warning: {warning}");
                }
                if let Some(name) = name {
                    import.theme.name = name;
                }
                let path = theme::write_theme(&import.theme, force)?;
                println!("wrote {}", path.display());
            }
        },
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // `-v`/`-vv` win when given, because a flag on the command line is the more
    // deliberate request; otherwise `RUST_LOG`, otherwise INFO. A stale
    // `RUST_LOG=warn` in the environment silently swallowing `-v` is exactly the
    // kind of thing that makes someone conclude logging is broken.
    let level = match args.verbose {
        0 => "info",
        1 => "dtr=debug",
        _ => "dtr=trace",
    };
    let filter = if args.verbose > 0 {
        tracing_subscriber::EnvFilter::new(level)
    } else {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level))
    };
    let effective = if args.verbose > 0 {
        level.to_owned()
    } else {
        std::env::var("RUST_LOG").unwrap_or_else(|_| level.to_owned())
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
    // Said out loud so an empty log is never a mystery: if this line is missing
    // from the log, the binary predates it and needs rebuilding.
    tracing::info!(filter = %effective, version = env!("CARGO_PKG_VERSION"), "logging");

    tracing::debug!(?args, "starting");

    if let Some(command) = args.command {
        return run_command(command);
    }

    let config_path = args.config.clone().unwrap_or_else(dtr_config::default_path);
    if args.config_path {
        println!("{}", config_path.display());
        return Ok(());
    }
    if let Some(pair) = &args.config_set {
        dtr_config::set_value(&config_path, &pair[0], &pair[1])?;
        println!("{}: {} = {}", config_path.display(), pair[0], pair[1]);
        return Ok(());
    }
    if args.themes {
        let names = theme::names();
        if names.is_empty() {
            println!("no themes found; looked in:");
            for path in theme::search_paths() {
                println!("  {}", path.display());
            }
        } else {
            for name in names {
                println!("{name}");
            }
        }
        return Ok(());
    }
    if args.config_init {
        if config_path.exists() {
            println!("{} already exists", config_path.display());
        } else {
            if let Some(parent) = config_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&config_path, dtr_config::TEMPLATE)?;
            println!("wrote {}", config_path.display());
        }
        return Ok(());
    }

    // A broken config is worth complaining about but not worth refusing to
    // start a terminal over: the defaults are always usable.
    let config = match dtr_config::Config::load(&config_path) {
        Ok(config) => config,
        Err(err) => {
            tracing::warn!(path = %config_path.display(), %err, "config file");
            dtr_config::Config::default()
        }
    };
    // A theme that cannot be found is worth saying loudly once, then carrying
    // on with the built-in colours: a wrong name must not stop a session.
    let theme = match config.theme.as_deref() {
        Some(name) => match theme::load(name) {
            Ok(theme) => Some(theme),
            Err(err) => {
                tracing::warn!(%err, "theme");
                None
            }
        },
        None => None,
    };
    let split = match args.split.as_deref() {
        None => None,
        Some("h") => Some(crate::layout::Axis::Horizontal),
        Some("v") => Some(crate::layout::Axis::Vertical),
        Some(other) => anyhow::bail!("expected h or v for --split, got {other:?}"),
    };
    // `DTR_GPU` exists so the packaged binary can be pointed at a card without
    // a rebuild; the flag wins when both are given.
    let gpu = args
        .gpu
        .clone()
        .or_else(|| std::env::var("DTR_GPU").ok())
        .filter(|name| !name.trim().is_empty());
    let select = match args.select.as_deref() {
        Some(spec) => {
            let (start, end) = spec
                .split_once(':')
                .ok_or_else(|| anyhow::anyhow!("expected START:END, got {spec:?}"))?;
            Some((start.trim().parse::<usize>()?, end.trim().parse::<usize>()?))
        }
        None => None,
    };
    let look = ui::Look::themed(config.font.size, config.font.path.clone(), theme.as_ref());

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
    } else if let Some(host) = args.ssh.clone() {
        // The system client, in a pty: it already speaks the config file, the
        // agent, jump hosts and known_hosts.
        Transport::Shell(pty::Config {
            program: Some(format!("ssh -t {host}")),
        })
    } else if let Some(path) = args.replay.clone() {
        Transport::Replay(path)
    } else if let Some(port) = args.port.clone() {
        let remembered = store
            .as_ref()
            .and_then(|store| store.serial(&port).ok().flatten())
            .unwrap_or_default();

        // Precedence: the command line, then the config file, then what was
        // remembered for this port, then the built-in 115200 8N1 with CR→CRLF.
        // The config file outranks memory because it is a declaration, not a
        // side effect of the last session.
        let settings = dtr_store::SerialSettings {
            baud: args
                .baud
                .or(config.serial.baud.into())
                .unwrap_or(remembered.baud),
            cr_to_crlf: match args.imap.as_deref() {
                Some(spec) => parse_imap(spec)?,
                None => config.serial.cr_to_crlf,
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
            auto_reconnect: config.serial.auto_reconnect,
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
    let files = args.files.as_deref().map(|path| {
        if path.is_empty() {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        } else {
            PathBuf::from(path)
        }
    });

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
        // Profiling a capture is how the setup itself gets tested: same
        // sampling, no window needed.
        let profiler = args
            .cpu_profile
            .as_ref()
            .and_then(|path| crate::profile::Guard::start(path));

        let startup = app::Startup {
            transport,
            send,
            log: log.clone(),
            store: store.clone(),
            palette: args.palette,
            look: look.clone(),
            theme: theme.clone(),
            select,
            scroll: args.scroll,
            split,
            files,
            exit_after: args.exit_after,
            gpu: gpu.clone(),
        };
        let result = app::capture_headless(startup, path, width, height, delay);
        if let (Some(path), Some(profiler)) = (args.cpu_profile.clone(), profiler) {
            profiler.finish(&path);
        }
        finish(store.as_deref(), session, "screenshot", &result);
        return result;
    }

    let profiler = args
        .cpu_profile
        .as_ref()
        .and_then(|path| crate::profile::Guard::start(path));

    let startup = app::Startup {
        transport,
        send,
        log: log.clone(),
        store: store.clone(),
        palette: args.palette,
        look,
        theme,
        select,
        scroll: args.scroll,
        split,
        files,
        exit_after: args.exit_after,
        gpu,
    };
    let result = app::run(startup);
    if let Some(profiler) = profiler {
        if let Some(path) = args.cpu_profile.clone() {
            profiler.finish(&path);
        }
    }
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

// timing probe
