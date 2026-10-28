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

    // A pty is born in the kernel's cooked mode, and that mode is a second
    // line discipline sitting in front of ours: every byte the session writes
    // is scanned for post-processing (each `\n` becomes `\r\n`), input is line
    // buffered and echoed by the kernel rather than by the program, and flow
    // control eats ^S. A terminal does all of that itself — the VT core *is*
    // the line discipline — so the session is put in raw mode before the child
    // starts, the way every other terminal starts one.
    //
    // Measured on a 32 MiB flood of two-byte lines: 37 ms raw, 3.9 s cooked.
    // The 3.9 s is `n_tty` rewriting each newline one character at a time, and
    // it is paid by the *session*, not by dtr — which is why a dumb pass-through
    // like `script` is just as slow on the same payload.
    //
    // `ISIG` is the one flag kept: ^C should still interrupt the foreground
    // process, and a program that wants the byte itself (readline) turns it off
    // for the duration, as it always has.
    #[cfg(unix)]
    if let Some(fd) = pair.master.as_raw_fd() {
        set_raw(fd)?;
    }

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

/// Put the pty into raw mode: no output post-processing, no kernel echo, no
/// line buffering, no flow control. `ISIG` is kept, so ^C still signals the
/// foreground process group; a program that wants the byte turns it off itself.
#[cfg(unix)]
fn set_raw(fd: std::os::unix::io::RawFd) -> anyhow::Result<()> {
    // SAFETY: `fd` is an open pty master owned by this call's caller, and
    // `termios` is plain old data the kernel fills in.
    let mut termios = std::mem::MaybeUninit::<libc::termios>::uninit();
    if unsafe { libc::tcgetattr(fd, termios.as_mut_ptr()) } != 0 {
        anyhow::bail!("tcgetattr: {}", std::io::Error::last_os_error());
    }
    let mut termios = unsafe { termios.assume_init() };

    termios.c_iflag &= !(libc::IGNBRK
        | libc::BRKINT
        | libc::PARMRK
        | libc::ISTRIP
        | libc::INLCR
        | libc::IGNCR
        | libc::ICRNL
        | libc::IXON);
    termios.c_oflag &= !libc::OPOST;
    termios.c_lflag &= !(libc::ECHO | libc::ECHONL | libc::ICANON | libc::IEXTEN);
    termios.c_cflag &= !(libc::CSIZE | libc::PARENB);
    termios.c_cflag |= libc::CS8;

    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &termios) } != 0 {
        anyhow::bail!("tcsetattr: {}", std::io::Error::last_os_error());
    }
    Ok(())
}

fn candidates() -> Vec<String> {    #[cfg(windows)]
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

#[cfg(all(test, unix))]
mod raw_tests {
    use super::*;

    /// The trap this guards: a pty is cooked until somebody says otherwise, and
    /// cooked means the kernel runs a line discipline in front of ours. The
    /// expensive half is output post-processing — every newline of the session's
    /// output is rewritten to CR LF one character at a time, which measured
    /// 3.9 s against 37 ms for the same 32 MiB flood.
    #[test]
    fn the_session_starts_raw() {
        let shell = Config {
            program: Some("true".to_owned()),
            ..Default::default()
        }
        .open()
        .expect("a pty with `true` on it");
        let fd = shell.master.as_raw_fd().expect("a unix master has an fd");

        let mut termios = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: an open master fd, and a buffer the kernel fills in.
        assert_eq!(unsafe { libc::tcgetattr(fd, termios.as_mut_ptr()) }, 0);
        let termios = unsafe { termios.assume_init() };

        assert_eq!(termios.c_oflag & libc::OPOST, 0, "the kernel post-processes output");
        assert_eq!(termios.c_lflag & libc::ICANON, 0, "the kernel line buffers input");
        assert_eq!(termios.c_lflag & libc::ECHO, 0, "the kernel echoes input");
        assert_eq!(termios.c_iflag & libc::ICRNL, 0, "the kernel rewrites input newlines");
        assert_ne!(termios.c_lflag & libc::ISIG, 0, "^C must still signal the foreground");
    }
}
