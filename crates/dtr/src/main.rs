//! dtr — a serial-first terminal.

mod about;
mod app;
mod chrome;
mod explorer;
mod fonts;
mod ime;
mod input;
mod keymap;
mod keys;
mod layout;
mod linkmenu;
mod logbook;
mod modem;
mod pager;
mod palette;
mod pane;
mod profile;
mod pty;
mod render;
mod screenshot;
mod serial;
mod serial_dialog;
mod settings;
mod sources;
mod sprite;
mod system;
mod terminfo;
mod theme;
mod thumb;
mod ui;
mod uri;
mod watch;

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

    /// Open the settings menu on start (also how it gets into screenshots)
    #[arg(long)]
    settings: bool,

    /// Open the keyboard map on start: every binding, on one card
    #[arg(long)]
    keymap: bool,

    /// List the installed families that can be a terminal grid, then exit
    #[arg(long)]
    fonts: bool,

    /// Print the glyph pipeline's decisions for a fixed sample — cell metrics,
    /// and the face, size and rectangle behind each character — then exit
    #[arg(long)]
    font_report: bool,

    /// Prefer the GPU whose name contains TEXT, e.g. --gpu nvidia
    #[arg(long, value_name = "TEXT")]
    gpu: Option<String>,

    /// What the controls look like: "native" (the platform's own) or "dtr".
    /// Overrides `[ui] widgets` in the config.
    #[arg(long, value_name = "STYLE")]
    widgets: Option<String>,

    /// Write a PNG of the live window once, MS after it opens. This is the only
    /// way to capture what the window draws: the offscreen path is a different
    /// renderer state and has disagreed with the window before.
    #[arg(long, value_name = "PATH", requires = "capture_after")]
    capture: Option<std::path::PathBuf>,

    /// How long after opening to take the `--capture` PNG
    #[arg(long, value_name = "MS", default_value_t = 1200)]
    capture_after: u64,

    /// Type a script of keys once the window opens, then carry on as usual.
    ///
    /// Tokens are separated by `;`: a chord such as `ctrl-shift-t`, a named key
    /// such as `enter` or `down`, `+` between the chords of one binding, or
    /// `text:` followed by characters to type. One token is applied per frame,
    /// so a following `--capture` photographs the result. Example:
    /// `--keys "ctrl-,;down;down" --capture /tmp/menu.png`.
    #[arg(long, value_name = "SCRIPT")]
    keys: Option<String>,

    /// Drag across the terminal once it opens: `x1,y1,x2,y2` in pixels. The
    /// pointer path is otherwise only exercisable by hand, and selecting text
    /// is how a copy happens.
    #[arg(long, value_name = "X1,Y1,X2,Y2")]
    drag: Option<String>,

    /// Click at `x,y` once it opens. Unlike a drag this hovers first and
    /// presses on the next frame, because a link is only known once the pane
    /// has answered what is under the pointer.
    #[arg(long, value_name = "X,Y")]
    click: Option<String>,

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

    /// Show an IME composition as if it were being typed (`--screenshot`)
    #[arg(long, value_name = "TEXT")]
    preedit: Option<String>,

    /// Show a canned screen instead of launching anything
    #[arg(long)]
    demo: bool,

    /// Open an ssh session on HOST, using the system's ssh client
    #[arg(long, value_name = "HOST")]
    ssh: Option<String>,

    /// Open the command palette on start
    #[arg(long)]
    palette: bool,

    /// Open the serial connection sheet for PORT on start. The port is not
    /// opened until the sheet is answered; this is also how it gets into
    /// `--screenshot`.
    #[arg(long, value_name = "PORT", num_args = 0..=1, default_missing_value = "/dev/ttyUSB0")]
    serial_dialog: Option<String>,

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

    /// Paste TEXT through the paste path once the session is attached, as if it
    /// came from the clipboard: escapes are expanded, line endings are the
    /// terminal's business, and a program that asked for bracketed paste gets
    /// the markers. `--send` is typing; this is pasting.
    #[arg(long, value_name = "TEXT")]
    paste: Option<String>,

    /// Wait this long before capturing, so a session can produce output
    #[arg(long, default_value_t = 300, value_name = "MS")]
    screenshot_delay: u64,
}

/// Pick the settings a serial port is opened with.
///
/// The config file is not consulted: baud and CR translation describe a device,
/// so they are remembered per port in the store. Precedence is the command line,
/// then what this port was last opened with, then 8N1 at 115200 with CR→CRLF —
/// the setting every serial console has assumed since forever.
fn resolve_serial(
    cli_baud: Option<u32>,
    cli_imap: Option<&str>,
    remembered: Option<&dtr_store::SerialSettings>,
) -> anyhow::Result<dtr_store::SerialSettings> {
    // Start from the whole remembered record: framing, signals and the profile
    // name are facts about this device just as the rate is. The command line
    // can override the two it has flags for; everything else survives.
    let mut settings = remembered.cloned().unwrap_or_default();
    if let Some(baud) = cli_baud {
        settings.baud = baud;
    }
    if let Some(spec) = cli_imap {
        settings.cr_to_crlf = parse_imap(spec)?;
    }
    Ok(settings)
}

#[cfg(test)]
mod serial_precedence {
    use super::*;

    #[test]
    fn a_remembered_port_is_reopened_the_way_it_was_left() {
        let remembered = dtr_store::SerialSettings {
            baud: 9600,
            parity: dtr_store::Parity::Even,
            profile: Some("gps".to_owned()),
            cr_to_crlf: false,
            ..dtr_store::SerialSettings::default()
        };
        let picked = resolve_serial(None, None, Some(&remembered)).unwrap();
        assert_eq!(picked.baud, 9600);
        assert_eq!(picked.parity, dtr_store::Parity::Even);
        assert_eq!(picked.profile.as_deref(), Some("gps"));
        assert!(!picked.cr_to_crlf);
    }

    #[test]
    fn a_port_never_opened_gets_8n1_at_115200() {
        let picked = resolve_serial(None, None, None).unwrap();
        assert_eq!(picked.baud, 115_200);
        assert!(picked.cr_to_crlf);
    }

    #[test]
    fn the_command_line_beats_memory() {
        let remembered = dtr_store::SerialSettings {
            baud: 9600,
            parity: dtr_store::Parity::Even,
            cr_to_crlf: false,
            ..dtr_store::SerialSettings::default()
        };
        let picked = resolve_serial(Some(57_600), Some("none"), Some(&remembered)).unwrap();
        assert_eq!(picked.baud, 57_600);
        assert_eq!(
            picked.parity,
            dtr_store::Parity::Even,
            "the command line only overrides what it names"
        );
        assert!(
            !picked.cr_to_crlf,
            "the command line asked for no translation"
        );
    }
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
    // Libraries that predate `tracing` say what went wrong through `log` — the
    // file dialog is one, and "the picker never appeared" is exactly the kind of
    // failure that needs a cause in the log. `init` above sets this up only when
    // `tracing-subscriber`'s `tracing-log` feature is on, so it is asked for
    // directly and a second registration is not an error.
    let _ = tracing_log::LogTracer::init();
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
    // A key table with a typo in it is silent otherwise: the binding simply
    // never fires. Say which names were not understood and which chords are
    // claimed twice, once, at startup.
    for name in crate::keys::Keymap::unknown_names(&config.keys) {
        tracing::warn!(%name, "keys: no action by that name");
    }
    {
        let keymap = crate::keys::Keymap::new(&config.keys);
        for (sequence, first, second) in keymap.conflicts() {
            tracing::warn!(
                binding = %sequence,
                first = first.name(),
                second = second.name(),
                "keys: two actions want this binding; the first wins"
            );
        }
    }

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
    if args.fonts {
        for family in crate::fonts::usable_families() {
            println!("{family}");
        }
        return Ok(());
    }

    let select = match args.select.as_deref() {
        Some(spec) => {
            let (start, end) = spec
                .split_once(':')
                .ok_or_else(|| anyhow::anyhow!("expected START:END, got {spec:?}"))?;
            Some((start.trim().parse::<usize>()?, end.trim().parse::<usize>()?))
        }
        None => None,
    };
    let preset = match args.widgets.as_deref() {
        Some("dtr") => dtr_widgets::Preset::Dtr,
        Some("native") => dtr_widgets::Preset::Native,
        Some(other) => {
            anyhow::bail!("--widgets is {other:?}; it must be \"dtr\" or \"native\"")
        }
        None => match config.ui.widgets {
            dtr_config::Widgets::Dtr => dtr_widgets::Preset::Dtr,
            dtr_config::Widgets::Native => dtr_widgets::Preset::Native,
        },
    };
    let mut look = ui::Look::themed(
        config.font.size,
        config.font.family.clone(),
        config.font.path.clone(),
        theme.as_ref(),
        preset,
    );
    let skin = look.skin();
    // Where the platform's answer came from, and why there is none where there is
    // none: "asked for native and got dtr" with no cause is the log line this pair
    // exists for. The `skin()` above is what performs the read.
    let native = crate::system::resolution();
    // The hover the platform's own ladder chose, when there is one: it is the
    // difference between "read the system accent" and "read the system's hover for
    // it", and only one of those is visible without the log.
    let hover = crate::system::hex(skin.palette.accent_hover);
    match &native.reason {
        Some(reason) => tracing::debug!(
            skin = crate::system::describe(&skin),
            requested = look.preset.as_str(),
            accent = crate::system::accent_hex(&skin),
            hover = ?hover,
            source = native.source,
            reason = %reason,
            "chrome"
        ),
        None => tracing::debug!(
            skin = crate::system::describe(&skin),
            requested = look.preset.as_str(),
            accent = crate::system::accent_hex(&skin),
            hover = ?hover,
            source = native.source,
            "chrome"
        ),
    }
    look.fallback = config.font.fallback.clone();

    // After the Look, so the report describes the font the terminal would
    // actually use — config, family and theme included — and before anything
    // opens a window or a port.
    if args.font_report {
        render::font_report(&look);
        return Ok(());
    }

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
        // agent, jump hosts and known_hosts. It is a connection, not a session,
        // so the pane reopens it when it drops.
        Transport::Shell(pty::Config::ssh(&host))
    } else if let Some(path) = args.replay.clone() {
        Transport::Replay(path)
    } else if let Some(port) = args.port.clone() {
        let remembered = store
            .as_ref()
            .and_then(|store| store.serial(&port).ok().flatten());

        // Precedence: the command line, then what this port was last opened
        // with, then the config file, then 115200 8N1 with CR→CRLF.
        //
        // Memory outranks the config because the config describes *ports in
        // general* — the default a port you have never opened should get — while
        // the remembered rate is a fact about this device. The old order had the
        // config first, which made the remembered value unreachable: `baud` was
        // not optional, so it was always present and always won.
        let mut settings = resolve_serial(args.baud, args.imap.as_deref(), remembered.as_ref())?;
        // The config file's `auto_reconnect` is the default for a port that
        // has never been opened; a remembered port keeps the policy it was last
        // connected with, the same way it keeps its framing.
        if remembered.is_none() {
            settings.auto_reconnect = config.serial.auto_reconnect;
        }

        if let Some(store) = store.as_ref() {
            if let Err(err) = store.remember_serial(&port, &settings) {
                tracing::warn!(%err, "could not remember the port settings");
            }
            let _ = store.set_last_port(&port);
        }

        Transport::Serial(serial::Config::from_settings(port, &settings))
    } else {
        Transport::Shell(pty::Config {
            program: args.shell.clone(),
            ..Default::default()
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
                other => {
                    anyhow::bail!("unknown log format {other:?}; expected `raw` or `timestamped`")
                }
            };
            Some((path, format))
        }
        None => None,
    };

    let send = args
        .send
        .as_deref()
        .map(input::unescape)
        .unwrap_or_default();
    let paste = args
        .paste
        .as_deref()
        .map(input::unescape)
        .unwrap_or_default();
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
            paste: paste.clone(),
            log: log.clone(),
            store: store.clone(),
            palette: args.palette,
            look: look.clone(),
            theme: theme.clone(),
            scrollback: config.terminal.scrollback_limit,
            select,
            scroll: args.scroll,
            split,
            files,
            exit_after: args.exit_after,
            capture: args.capture.clone(),
            capture_after: args.capture_after,
            keys: args.keys.clone(),
            drag: args.drag.clone(),
            click: args.click.clone(),
            gpu: gpu.clone(),
            settings: args.settings,
            keymap: args.keymap,
            serial_dialog: args.serial_dialog.as_deref().map(|port| {
                crate::serial_dialog::SerialDialog::from_store(port, "", store.as_deref())
            }),
            config: config.clone(),
            config_path: config_path.clone(),
            preedit: args.preedit.clone(),
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
        paste,
        log: log.clone(),
        store: store.clone(),
        palette: args.palette,
        look,
        theme,
        scrollback: config.terminal.scrollback_limit,
        select,
        scroll: args.scroll,
        split,
        files,
        exit_after: args.exit_after,
        capture: args.capture.clone(),
        capture_after: args.capture_after,
        keys: args.keys.clone(),
        drag: args.drag.clone(),
        click: args.click.clone(),
        gpu,
        settings: args.settings,
        keymap: args.keymap,
        serial_dialog: args
            .serial_dialog
            .as_deref()
            .map(|port| crate::serial_dialog::SerialDialog::from_store(port, "", store.as_deref())),
        config,
        config_path,
        preedit: args.preedit.clone(),
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
