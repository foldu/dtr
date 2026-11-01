//! Local shell over a PTY — ConPTY on Windows, `openpty` on Unix.
//!
//! portable-pty gives a blocking reader/writer pair, which is the right shape
//! for us: the reader gets its own OS thread and hands bytes to the pane over a
//! channel, so nothing blocking ever runs on the pane's LocalSet.

use std::io::{Read, Write};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

pub const COLS: u16 = 80;
pub const ROWS: u16 = 24;

/// Ten seconds to reach the host, instead of the kernel's two minutes.
const CONNECT_TIMEOUT: &str = "ConnectTimeout=10";
/// Ask the far end for a sign of life this often, and give up after this many
/// unanswered — a minute of silence in total.
const KEEPALIVE: &str = "ServerAliveInterval=20";
const KEEPALIVE_COUNT: &str = "ServerAliveCountMax=3";

#[derive(Clone, Debug, Default)]
pub struct Config {
    /// Shell to launch; `None` walks a platform default list.
    pub program: Option<String>,
    /// Arguments for `program`, one per element.
    ///
    /// They live apart from the program string because an ssh remote command is
    /// one argument that contains spaces for the *remote* shell to parse;
    /// splitting it the way `program` is split would hand it over as four.
    pub args: Vec<String>,
    /// Where to start. A new tab inherits the directory its parent was in,
    /// which is what every other terminal does with "open a tab here".
    pub cwd: Option<std::path::PathBuf>,
    /// The pty is made at the default size and told the real one as soon as the
    /// UI resizes it. A connection that reopens after a drop is asked for at the
    /// size the window has *now*, so this can be set before the child starts
    /// rather than resized underneath its first prompt.
    pub size: Option<PtySize>,
    /// The `ssh` alias this session connects to, when it is one.
    ///
    /// A connection is not a session: the pane reopens it when it drops, and
    /// the tab holding it outlives the client. A local shell has no such thing —
    /// when it ends, the pane is over.
    pub remote: Option<String>,
}

pub struct Shell {
    /// Kept alive for as long as the pane lives: dropping it closes the pty.
    pub master: Box<dyn MasterPty + Send>,
    pub child: Box<dyn Child + Send + Sync>,
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
}

impl Config {
    /// An `ssh` session: the system client, in a pty.
    ///
    /// The system client and not an ssh library: it already knows the config
    /// file, the agent, jump hosts and known_hosts, and re-implementing those is
    /// a project rather than a feature. `-t` asks for a tty, which an
    /// interactive session needs.
    ///
    /// The three options are there because a client that never says anything is
    /// worse than one that fails. `ConnectTimeout` bounds an attempt that would
    /// otherwise sit in the kernel's connect for two minutes, so a host that is
    /// down is known to be down in ten seconds instead of in a hundred and
    /// twenty. The keepalives are what make a connection that went away
    /// *silently* — a lid closed, a route blackholed — exit with 255 instead of
    /// hanging on screen looking alive, which is exactly the failure that
    /// reconnect exists for: ssh's own default is no keepalive at all. Twenty
    /// seconds times three is a minute of total silence before the pane decides,
    /// slow enough not to kill a session on a congested link and fast enough
    /// that nobody has to wonder whether the pane is stuck.
    pub fn ssh(alias: &str) -> Self {
        Self {
            program: Some("ssh".to_owned()),
            // Before the destination: after it, ssh treats everything as a
            // command for the remote side.
            args: [
                "-t",
                "-o",
                CONNECT_TIMEOUT,
                "-o",
                KEEPALIVE,
                "-o",
                KEEPALIVE_COUNT,
                alias,
            ]
            .map(str::to_owned)
            .to_vec(),
            remote: Some(alias.to_owned()),
            ..Self::default()
        }
    }

    /// Reopen this session where it was.
    ///
    /// `dir` is the last directory the session reported with OSC 7, which is a
    /// path on the machine at the other end of the connection and means nothing
    /// here — hence a string, not a `PathBuf`.
    ///
    /// `ssh` with a command runs that command through the remote login shell
    /// instead of starting one, so the command starts one itself: a login shell,
    /// `exec`ed so that it *is* the session rather than a child of a wrapper.
    /// The `cd` comes first and is separated by `;`, not `&&`: a directory that
    /// has been deleted since should still leave a usable shell, rather than an
    /// ssh that exits with an error the pane would read as another drop.
    ///
    /// The path is quoted by `shlex`, a POSIX shell printer checked against bash,
    /// zsh, dash, ash, mksh and fish. The one thing it refuses is NUL, which no
    /// shell argument can carry: such a report is dropped and the session comes
    /// back at `$HOME`, rather than quoted into something a different shell would
    /// split.
    pub fn reopened(&self, dir: Option<&str>) -> Self {
        let mut next = self.clone();
        let Some(dir) = dir
            .map(str::trim)
            .filter(|dir| dir.starts_with('/') && self.remote.is_some())
        else {
            return next;
        };
        let Ok(dir) = shlex::try_quote(dir) else {
            tracing::warn!("the directory cannot be a shell argument; reopening at home");
            return next;
        };
        next.args
            .push(format!("cd {dir} ; exec \"${{SHELL:-/bin/sh}}\" -l"));
        next
    }

    pub fn open(&self) -> anyhow::Result<Shell> {
        match self.program.as_deref() {
            Some(program) => spawn(self, program),
            None => {
                let mut last = None;
                for program in candidates() {
                    match spawn(self, &program) {
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

/// The directory a session starts in: the one asked for when it exists, else
/// `$HOME` when *it* exists, else the directory dtr was started from.
///
/// The fallbacks are not politeness. portable-pty uses `$HOME` as the child's
/// working directory when no `cwd` is set, and `current_dir` on a path that
/// does not exist fails the whole spawn with a bare `ENOENT`.
fn start_dir(wanted: Option<&std::path::Path>) -> std::path::PathBuf {
    wanted
        .filter(|dir| dir.is_dir())
        .map(std::path::Path::to_path_buf)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .filter(|dir| dir.is_dir())
        })
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| std::path::PathBuf::from("/"))
}

fn spawn(config: &Config, program: &str) -> anyhow::Result<Shell> {
    let system = native_pty_system();
    let pair = system.openpty(config.size.unwrap_or(PtySize {
        rows: ROWS,
        cols: COLS,
        pixel_width: 0,
        pixel_height: 0,
    }))?;

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
    // Always give a directory, because portable-pty's own fallback is `$HOME`
    // and it passes that to `current_dir` without checking: a `$HOME` that no
    // longer exists makes the spawn fail with a bare `ENOENT`, which reads as
    // "the shell is missing". A stale home must not stop a terminal from
    // opening a shell.
    cmd.cwd(start_dir(config.cwd.as_deref()));
    for arg in parts {
        cmd.arg(arg);
    }
    // Verbatim, one per element: this is how a remote command that the *other*
    // machine parses stays one argument here.
    for arg in &config.args {
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

#[cfg(all(test, unix))]
mod raw_tests {
    use super::*;

    /// A directory that is asked for and exists is the one used, so "open a tab
    /// in this directory" keeps working.
    #[test]
    fn an_existing_start_directory_is_the_one_used() {
        let wanted = std::env::temp_dir();
        assert_eq!(start_dir(Some(&wanted)), wanted);
    }

    /// The sandbox case: a `$HOME` that does not exist used to fail the spawn
    /// with `ENOENT`, which reads as "the shell is missing". Whatever the fall
    /// chain lands on has to be a real directory, or `current_dir` fails again.
    #[test]
    fn a_start_directory_that_is_gone_still_lands_somewhere_real() {
        let dir = start_dir(Some(std::path::Path::new("/nonexistent/dtr-test")));
        assert!(dir.is_dir(), "{dir:?} is not a directory");
    }

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

        assert_eq!(
            termios.c_oflag & libc::OPOST,
            0,
            "the kernel post-processes output"
        );
        assert_eq!(
            termios.c_lflag & libc::ICANON,
            0,
            "the kernel line buffers input"
        );
        assert_eq!(termios.c_lflag & libc::ECHO, 0, "the kernel echoes input");
        assert_eq!(
            termios.c_iflag & libc::ICRNL,
            0,
            "the kernel rewrites input newlines"
        );
        assert_ne!(
            termios.c_lflag & libc::ISIG,
            0,
            "^C must still signal the foreground"
        );
    }

    /// `args` is a list rather than part of the program string so that one
    /// argument can contain spaces. A shell reads them as its own argv.
    #[test]
    fn arguments_reach_the_program_unsplit() {
        let shell = Config {
            program: Some("/bin/sh".to_owned()),
            args: vec!["-c".to_owned(), "printf 'two words'".to_owned()],
            ..Default::default()
        }
        .open()
        .expect("a pty with sh on it");
        let mut reader = shell.reader;
        let mut got = Vec::new();
        let mut buf = [0u8; 256];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !String::from_utf8_lossy(&got).contains("two words") {
            assert!(std::time::Instant::now() < deadline, "no output: {got:?}");
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => got.extend_from_slice(&buf[..n]),
                Err(err) => panic!("read: {err}"),
            }
        }
        assert_eq!(String::from_utf8_lossy(&got), "two words");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ssh_session_is_a_connection() {
        let config = Config::ssh("jupyter");
        assert_eq!(config.program.as_deref(), Some("ssh"));
        // The alias is what makes the pane treat a lost client as a lost
        // connection rather than the end of the session.
        assert_eq!(config.remote.as_deref(), Some("jupyter"));
        assert_eq!(config.args.last().map(String::as_str), Some("jupyter"));
    }

    /// An ssh command takes options *before* the destination; after it every
    /// word is a command for the far end, so an option would become a command
    /// there. The keepalives are what let the pane notice a connection that went
    /// away silently, and the connect timeout is why a dead host is known to be
    /// dead in ten seconds rather than in the kernel's hundred and twenty.
    #[test]
    fn the_timeouts_come_before_the_destination_and_survive_a_reopen() {
        let config = Config::ssh("jupyter");
        let alias = config
            .args
            .iter()
            .position(|arg| arg == "jupyter")
            .expect("the alias");
        for option in [CONNECT_TIMEOUT, KEEPALIVE, KEEPALIVE_COUNT] {
            let at = config
                .args
                .iter()
                .position(|arg| arg == option)
                .unwrap_or_else(|| panic!("{option} is missing"));
            assert!(at < alias, "{option} comes after the destination");
            assert_eq!(config.args[at - 1], "-o", "-o introduces {option}");
        }
        // A reconnection is the same connection, so the options come with it.
        let reopened = config.reopened(Some("/srv/app"));
        assert_eq!(reopened.args[..alias + 1], config.args[..]);
    }

    /// The words the remote shell would see in the command. Parsed with the same
    /// crate that quoted it, which is the only assertion that says what matters:
    /// the directory arrives as *one* word, and the rest of the command is
    /// intact.
    fn words(config: &Config) -> Vec<String> {
        shlex::split(config.args.last().expect("a reopen adds a command"))
            .expect("the command parses")
    }

    #[test]
    fn reopening_puts_the_session_back_where_it_was() {
        let next = Config::ssh("jupyter").reopened(Some("/srv/app"));
        assert_eq!(next.remote.as_deref(), Some("jupyter"));
        assert_eq!(
            words(&next),
            ["cd", "/srv/app", ";", "exec", "${SHELL:-/bin/sh}", "-l"]
        );
    }

    /// A path can hold everything the quoting is made of. Left alone any of it
    /// would end the word and run whatever followed on the other machine, so the
    /// assertion is on the words a shell would parse, not on the spelling.
    #[test]
    fn a_hostile_directory_is_still_one_word() {
        for dir in [
            "/tmp/it's here",
            "/tmp/a b",
            "/tmp/$(touch /tmp/pwned)",
            "/tmp/`id`",
            "/tmp/a;rm -rf /",
            "/tmp/\"quoted\"",
            "/tmp/dollar$HOME",
            "/tmp/new\nline",
            "/tmp/back\\slash",
            "/tmp/ünïcode",
            "/tmp/-leading-dash",
            "/tmp/tilde~hash#star*",
        ] {
            let next = Config::ssh("jupyter").reopened(Some(dir));
            assert_eq!(
                words(&next),
                ["cd", dir, ";", "exec", "${SHELL:-/bin/sh}", "-l"],
                "while quoting {dir:?}"
            );
        }
    }

    /// NUL is the one byte a shell argument cannot carry at all, so a report
    /// holding one is dropped — the session comes back at `$HOME` — rather than
    /// trusted to whatever a different shell would make of it.
    #[test]
    fn a_directory_a_shell_cannot_carry_is_dropped() {
        let base = Config::ssh("jupyter");
        assert_eq!(base.reopened(Some("/tmp/a\0b")).args, base.args);
    }

    /// OSC 7 reports absolute paths, so a relative one is a broken prompt rather
    /// than a directory: guessing at it would put a reconnect somewhere random.
    #[test]
    fn a_directory_that_is_not_absolute_is_ignored() {
        let base = Config::ssh("jupyter");
        assert_eq!(base.reopened(None).args, base.args);
        assert_eq!(base.reopened(Some("  ")).args, base.args);
        assert_eq!(base.reopened(Some("srv/app")).args, base.args);
        assert_eq!(base.reopened(Some("./srv")).args, base.args);
    }

    /// A local shell has no connection to put back: the directory is already
    /// its own, and appending an ssh command to `bash` would be nonsense.
    #[test]
    fn a_local_shell_is_not_reopened_with_a_remote_command() {
        let local = Config {
            program: Some("bash".to_owned()),
            ..Default::default()
        };
        assert_eq!(local.reopened(Some("/srv/app")).args, local.args);
    }
}
