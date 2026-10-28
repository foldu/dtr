//! Local shell over a PTY — ConPTY on Windows, `openpty` on Unix.
//!
//! portable-pty gives a blocking reader/writer pair, which is the right shape
//! for us: the reader gets its own OS thread and hands bytes to the pane over a
//! channel, so nothing blocking ever runs on the pane's LocalSet.

use std::io::{Read, Write};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

pub const COLS: u16 = 80;
pub const ROWS: u16 = 24;

#[derive(Clone, Debug, Default)]
pub struct Config {
    /// Shell to launch; `None` walks a platform default list.
    pub program: Option<String>,
    /// Where to start. A new tab inherits the directory its parent was in,
    /// which is what every other terminal does with "open a tab here".
    pub cwd: Option<std::path::PathBuf>,
}

pub struct Shell {
    /// Kept alive for as long as the pane lives: dropping it closes the pty.
    pub master: Box<dyn MasterPty + Send>,
    pub child: Box<dyn Child + Send + Sync>,
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
}

impl Config {
    pub fn open(&self) -> anyhow::Result<Shell> {
        match &self.program {
            Some(program) => spawn(program, self.cwd.as_deref()),
            None => {
                let mut last = None;
                for program in candidates() {
                    match spawn(&program, self.cwd.as_deref()) {
                        Ok(shell) => return Ok(shell),
                        Err(err) => {
                            tracing::debug!(%program, %err, "shell candidate failed");
                            last = Some(err);
                        }
                    }
                }
                Err(last.unwrap_or_else(|| anyhow::anyhow!("no shell to launch")))
            }
        }
    }
}

fn spawn(program: &str, cwd: Option<&std::path::Path>) -> anyhow::Result<Shell> {
    let system = native_pty_system();
    let pair = system.openpty(PtySize {
        rows: ROWS,
        cols: COLS,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    // A shell may be given with arguments (`bash -l`); no quoting, M0.
    let mut parts = program.split_whitespace();
    let exe = parts.next().unwrap_or(program);
    let mut cmd = CommandBuilder::new(exe);
    if let Some(dir) = cwd {
        cmd.cwd(dir);
    }
    for arg in parts {
        cmd.arg(arg);
    }
    // We are a libghostty terminal; ghostty's terminfo entry is the honest
    // identity. (nixpkgs and the ghostty installer both provide it.)
    cmd.env("TERM", "xterm-ghostty");

    let child = pair.slave.spawn_command(cmd)?;
    // The pane owns the master; the slave must be closed or the pty never EOFs.
    drop(pair.slave);

    let reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;
    tracing::info!(%program, "shell started");

    Ok(Shell {
        master: pair.master,
        child,
        reader,
        writer,
    })
}

fn candidates() -> Vec<String> {
    #[cfg(windows)]
    {
        let mut list = Vec::new();
        if let Ok(shell) = std::env::var("DTR_SHELL") {
            list.push(shell);
        }
        list.push("pwsh.exe".to_owned());
        list.push("powershell.exe".to_owned());
        if let Ok(comspec) = std::env::var("COMSPEC") {
            list.push(comspec);
        }
        list.push("cmd.exe".to_owned());
        list
    }

    #[cfg(not(windows))]
    {
        let mut list = Vec::new();
        if let Ok(shell) = std::env::var("DTR_SHELL") {
            list.push(shell);
        }
        if let Ok(shell) = std::env::var("SHELL") {
            list.push(shell);
        }
        list.push("/bin/bash".to_owned());
        list.push("/bin/sh".to_owned());
        list
    }
}
