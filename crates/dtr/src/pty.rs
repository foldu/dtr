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
///
/// Shared with `crate::terminfo`, which connects to the same host for a very
/// different reason and must not be the one place with its own idea of "too
/// long to wait".
pub(crate) const CONNECT_TIMEOUT: &str = "ConnectTimeout=10";
/// Ask the far end for a sign of life this often, and give up after this many
/// unanswered — a minute of silence in total.
const KEEPALIVE: &str = "ServerAliveInterval=20";
const KEEPALIVE_COUNT: &str = "ServerAliveCountMax=3";

/// Run a POSIX script whatever the account's login shell is.
///
/// `sshd` hands a command to the *user's* shell, and that shell may be fish,
/// csh, ion or elvish: `"${SHELL:-/bin/sh}"` is a parse error in several of
/// them, so a reconnect command written as plain shell breaks on an account
/// whose shell is not POSIX. `sh -c '…'` is understood by every one of them —
/// single quotes are literal everywhere, and the script inside is parsed by
/// `sh`, which is the shell that can actually read it.
///
/// An embedded single quote is the one thing that has to be spelled out, with
/// the standard `'\''` dance, because a directory in a reconnect command comes
/// from the far end's own output and may contain anything but `/` and NUL.
pub(crate) fn sh_command(script: &str) -> String {
    format!("sh -c '{}'", script.replace('\'', r"'\''"))
}

/// The arguments for a helper connection beside a session.
///
/// Derived from [`Config::ssh`] rather than written out again, because a helper
/// has to reach the *same host* the session beside it reaches — the same
/// `~/.ssh/config`, the same jump hosts, the same credentials. Two things
/// change: `-t` becomes `-T`, because nothing here needs a terminal and a tty
/// would want one to answer with; and `BatchMode=yes` is added, because a
/// password prompt on a child with piped stdio is a wait that ends in nothing.
///
/// The alias is last and nothing may follow it: ssh reads the rest as a command
/// for the far end, so a caller appends its own command and nothing else.
pub(crate) fn batch_args(alias: &str) -> Vec<String> {
    let mut args: Vec<String> = Config::ssh(alias)
        .args
        .iter()
        .map(|arg| match arg.as_str() {
            "-t" => "-T".to_owned(),
            other => other.to_owned(),
        })
        .collect();
    let at = args.len() - 1;
    args.splice(at..at, ["-o".to_owned(), "BatchMode=yes".to_owned()]);
    args
}

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
    /// The tmux session this connection runs, when it is one.
    ///
    /// Beside `remote` because it is the same kind of fact, and it is what makes
    /// a reconnection "the same session" rather than "a new one": `ssh` is a
    /// transport, and a transport that dies takes its shell with it unless
    /// something on the far side is holding the work. `None` is a plain session
    /// — the far end starts its login shell, as it always did.
    pub tmux: Option<String>,
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
        let mut args: Vec<String> = Vec::new();
        // The same escape hatch `dtr-sftp` has, for the same failure: one
        // root-owned drop-in in `/etc/ssh/ssh_config` makes every connection die
        // with "Bad owner or permissions". `-F` replaces the *system* file as
        // well as the user one, which is what gets past it — and it is also why
        // the terminfo copy derives its arguments from here: both have to reach
        // the host the same way.
        if let Ok(config) = std::env::var("DTR_SSH_CONFIG") {
            args.push("-F".to_owned());
            args.push(config);
        }
        args.extend(
            // Before the destination: after it, ssh treats everything as a
            // command for the remote side.
            [
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
        );
        Self {
            program: Some("ssh".to_owned()),
            args,
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
    /// instead of starting one, so the command starts one itself — or, when this
    /// is a tmux session, attaches to it. The `cd` comes first and is separated
    /// by `;`, not `&&`: a directory that has been deleted since should still
    /// leave a usable session, rather than an ssh that exits with an error the
    /// pane would read as another drop.
    ///
    /// The path is quoted by `shlex`, a POSIX shell printer checked against bash,
    /// zsh, dash, ash, mksh and fish. The one thing it refuses is NUL, which no
    /// shell argument can carry: such a report is dropped and the session comes
    /// back at `$HOME`, rather than quoted into something a different shell would
    /// split.
    pub fn reopened(&self, dir: Option<&str>) -> Self {
        let dir = dir
            .map(str::trim)
            .filter(|dir| dir.starts_with('/') && self.remote.is_some());
        let quoted = dir.and_then(|dir| match shlex::try_quote(dir) {
            Ok(dir) => Some(dir.into_owned()),
            Err(_) => {
                tracing::warn!("the directory cannot be a shell argument; reopening at home");
                None
            }
        });
        self.clone().wire(quoted.as_deref())
    }

    /// An `ssh` session that runs a tmux session on the far end.
    ///
    /// The session is created when it does not exist and attached to when it
    /// does — `new-session -A`, one command for both, which is also what makes a
    /// reconnection land back in the same session. That is the whole point of
    /// the wrapping: the work lives in a server that the connection merely looks
    /// at, so the connection dying is not the work dying.
    pub fn ssh_tmux(alias: &str, session: &str) -> Self {
        let mut config = Self::ssh(alias);
        config.tmux = Some(session.to_owned());
        config.wire(None)
    }

    /// Put this session's remote command on the arguments, replacing whatever
    /// command they already carry.
    ///
    /// `dir` is already quoted for the remote shell; `None` means wherever the
    /// far end starts, which is `$HOME` at login. Idempotent, because a
    /// reconnection is built from a config that already has a command on it.
    fn wire(mut self, dir: Option<&str>) -> Self {
        let Some(alias) = self.remote.clone() else {
            // A local shell has no command to wrap: its directory is its own,
            // and appending an ssh command to `bash` would be nonsense.
            return self;
        };
        // Everything after the destination is the command, and the destination
        // is the last argument that equals the alias — the command is one
        // argument, so it cannot hide another copy of the alias.
        if let Some(at) = self.args.iter().rposition(|arg| *arg == alias) {
            self.args.truncate(at + 1);
        }
        let Some(script) = self.script(dir) else {
            return self;
        };
        self.args.push(sh_command(&script));
        self
    }

    /// The remote command for this session, or `None` when it needs none.
    fn script(&self, dir: Option<&str>) -> Option<String> {
        match &self.tmux {
            Some(session) => {
                let session = shlex::try_quote(session)
                    .map(|quoted| quoted.into_owned())
                    .unwrap_or_else(|_| session.clone());
                let mut script = String::new();
                if let Some(dir) = dir {
                    // `cd` on its own line and without an `&&`: a directory that
                    // has been deleted since should leave the session where the
                    // far end starts, which is what `$PWD` below then carries.
                    script.push_str(&format!("cd {dir} 2>/dev/null\n"));
                }
                script.push_str(
                    // A host with no tmux is a shell, not a failure: the tab has
                    // to work on a machine nobody has set up, and an `exec` that
                    // cannot find its program exits non-zero — which the pane
                    // would read as `exit` and close the tab over.
                    "command -v tmux >/dev/null 2>&1 || exec \"${SHELL:-/bin/sh}\" -l\n\
                     infocmp xterm-ghostty >/dev/null 2>&1 || { TERM=xterm-256color; export TERM; }\n",
                );
                // `-A` is attach-or-create, so one command serves a new tab and
                // a reconnection alike. `-c` is where the session starts, and
                // tmux ignores it when it attaches. The `infocmp` line above is
                // because tmux has to *draw* on this terminal: a host that has
                // not got dtr's entry yet cannot be handed a name its ncurses
                // does not know.
                script.push_str(&format!(
                    "exec tmux new-session -A -s {session} -c \"$PWD\""
                ));
                Some(script)
            }
            // A plain ssh session: a login shell, `exec`ed so that it *is* the
            // session rather than a child of a wrapper.
            None => dir.map(|dir| format!("cd {dir} ; exec \"${{SHELL:-/bin/sh}}\" -l")),
        }
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

    // The tty the child gets is *not* raw, and both readers of this mode are
    // why.
    //
    // The kernel's line discipline is not a second terminal we could do
    // without: it is where a line feed becomes a carriage return. Turn that off
    // and every program that writes `\n` — `ls`, `cat`, a shell script, Ubuntu's
    // motd — draws a staircase, and "one\ntwo\n" comes out as `one` with `two`
    // starting where `one` ended. The VT core cannot recover what the kernel
    // did not do, and the core doing it instead would be the wrong layer: a
    // program that wants bare newlines asks the tty for them itself.
    //
    // The other reader is `ssh`. It sends *this* tty's modes to the far end in
    // its `pty-req`, so a raw local pty is a raw remote one: no `ONLCR` and, the
    // part people notice first, no `ECHO` — nothing typed at the remote shell
    // appears, and the fix is `reset`, which is `tcsetattr(sane)` at the other
    // end and therefore looks like it does nothing at all.
    //
    // This costs what it costs: `n_tty` rewrites each newline, measured at 3.9 s
    // for a 32 MiB flood of two-byte lines against 37 ms raw. Every other
    // terminal pays it on the same payload, and one that does not is not a
    // terminal.
    #[cfg(unix)]
    if let Some(fd) = pair.master.as_raw_fd() {
        set_sane(fd)?;
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

/// Give the child the tty a terminal gives it: canonical input, echo, signals,
/// 8-bit data, and output post-processing so that a newline is also a carriage
/// return.
///
/// These are the kernel's defaults — and `stty sane`'s — set explicitly so that
/// the mode does not depend on what a pty inherits. A program that needs
/// something else sets it itself; that is what `readline`, `vim`, `less` and
/// `ssh` all do.
#[cfg(unix)]
fn set_sane(fd: std::os::unix::io::RawFd) -> anyhow::Result<()> {
    // SAFETY: `fd` is an open pty master owned by this call's caller, and
    // `termios` is plain old data the kernel fills in.
    let mut termios = std::mem::MaybeUninit::<libc::termios>::uninit();
    if unsafe { libc::tcgetattr(fd, termios.as_mut_ptr()) } != 0 {
        anyhow::bail!("tcgetattr: {}", std::io::Error::last_os_error());
    }
    let mut termios = unsafe { termios.assume_init() };

    // Input: CR from the keyboard reads as NL, ^S/^Q flow control works, and
    // nothing is stripped to seven bits.
    termios.c_iflag |= libc::ICRNL | libc::IXON;
    termios.c_iflag &= !(libc::PARMRK | libc::ISTRIP | libc::INLCR | libc::IGNCR | libc::IXOFF);
    // Output: the one conversion a terminal cannot do for the kernel. See the
    // comment at the call site — `ssh` forwards these flags as well.
    termios.c_oflag |= libc::OPOST | libc::ONLCR;
    // Line discipline: lines, echo (so a session that has not taken the tty
    // over still shows what was typed), ^C to the foreground process group.
    termios.c_lflag |=
        libc::ICANON | libc::ECHO | libc::ECHOE | libc::ECHOK | libc::ISIG | libc::IEXTEN;
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
mod tty_tests {
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

    /// The trap this guards: a pty whose modes were made raw is a pty that
    /// cannot render a plain newline, and `ssh` copies these flags to the far
    /// end, where the same raw mode shows up as a session with no echo.
    ///
    /// A program that wants raw sets it itself, so the flags asserted here are
    /// the *starting* ones, and the cost (the kernel rewriting each newline) is
    /// paid on purpose.
    #[test]
    fn the_session_starts_sane() {
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

        assert_ne!(
            termios.c_oflag & libc::OPOST,
            0,
            "a newline has to stay a newline and a carriage return"
        );
        assert_ne!(
            termios.c_oflag & libc::ONLCR,
            0,
            "`\\n` alone would draw a staircase, and ssh would copy that to the far end"
        );
        assert_ne!(
            termios.c_lflag & libc::ICANON,
            0,
            "a session that has not taken the tty over reads lines"
        );
        assert_ne!(
            termios.c_lflag & libc::ECHO,
            0,
            "without echo, a remote shell shows nothing until `reset`"
        );
        assert_ne!(
            termios.c_iflag & libc::ICRNL,
            0,
            "the Enter key arrives as CR and has to read as NL"
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

    /// The script the far end's `sh` is given. The command is `sh -c '…'`, so
    /// the assertion that matters is on the *inner* text: the login shell only
    /// has to pass it through, and the `sh` on the other side parses it.
    fn script(config: &Config) -> String {
        let command = shlex::split(config.args.last().expect("a reopen adds a command"))
            .expect("the command parses");
        assert_eq!(
            command[..2],
            ["sh", "-c"],
            "the command is not run through sh: {command:?}"
        );
        command[2].clone()
    }

    /// The words the remote `sh` would see in the command.
    fn words(config: &Config) -> Vec<String> {
        shlex::split(&script(config)).expect("the script parses")
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

    /// An outer layer of quoting is only worth having if it is exact, and the
    /// case that breaks naive single quoting is a quote in the directory, so
    /// that one is executed for real: the login shell (whatever it is) hands
    /// `sh` the text, and what `sh` receives has to be the script.
    #[test]
    fn a_quote_in_the_directory_survives_the_outer_shell() {
        let hostile = "/tmp/it's here";
        let script = format!("printf %s {}", shlex::try_quote(hostile).expect("quotable"));
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(sh_command(&script))
            .output()
            .expect("sh");
        assert!(out.status.success(), "sh said no: {out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), hostile);
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

    /// A tmux session is a command from the *first* connection, not only from a
    /// reopen: the tab is inside the session before anything has dropped, which
    /// is the whole point of asking the host what it has before opening one.
    #[test]
    fn a_tmux_session_is_created_or_attached_from_the_first_connection() {
        let config = Config::ssh_tmux("jupyter", "dtr-2");
        assert_eq!(config.remote.as_deref(), Some("jupyter"));
        assert_eq!(config.tmux.as_deref(), Some("dtr-2"));
        let words = words(&config);
        assert!(
            words.windows(3).any(|w| w == ["tmux", "new-session", "-A"]),
            "attach-or-create, or a new tab lands on someone else's session: {words:?}"
        );
        assert!(
            words.windows(2).any(|w| w == ["-s", "dtr-2"]),
            "the session is named: {words:?}"
        );
        // A host with no tmux gets the login shell this always started, because
        // a tab that cannot find tmux is still a tab.
        assert!(
            words
                .windows(3)
                .any(|w| w == ["exec", "${SHELL:-/bin/sh}", "-l"]),
            "{words:?}"
        );
    }

    #[test]
    fn a_tmux_reconnect_goes_back_to_the_same_session_in_the_same_directory() {
        let next = Config::ssh_tmux("jupyter", "dtr-2").reopened(Some("/srv/app"));
        assert_eq!(next.tmux.as_deref(), Some("dtr-2"));
        let words = words(&next);
        assert_eq!(words[..3], ["cd", "/srv/app", "2>/dev/null"], "{words:?}");
        assert!(words.windows(2).any(|w| w == ["-s", "dtr-2"]), "{words:?}");
        // Idempotent: a second reopen is the same command, not a second one.
        assert_eq!(next.reopened(Some("/srv/app")).args, next.args);
    }

    /// The same quoting, on the path that also carries a session name.
    #[test]
    fn a_hostile_directory_is_still_one_word_inside_a_tmux_command() {
        for dir in ["/tmp/it's here", "/tmp/a b", "/tmp/$(touch /tmp/pwned)"] {
            let words = words(&Config::ssh_tmux("jupyter", "dtr-1").reopened(Some(dir)));
            assert_eq!(words[1], dir, "while quoting {dir:?}: {words:?}");
        }
    }

    /// The command a tmux tab runs, against a real tmux: the flags are accepted,
    /// the session lands where it was asked to, and a second connection finds it
    /// instead of making another.
    ///
    /// The argument vector is taken *from the script* rather than written out
    /// again here, so the two cannot disagree about a flag; the directory is
    /// substituted and `-d` is added, because a test has no terminal to attach
    /// and killing a client to get out of one is a different experiment than
    /// this one.
    #[cfg(unix)]
    #[test]
    fn the_tmux_command_is_accepted_and_lands_where_it_was_asked() {
        let Ok(version) = std::process::Command::new("tmux").arg("-V").output() else {
            return;
        };
        if !version.status.success() {
            return;
        }
        let sockets = tempfile::tempdir().expect("socket dir");
        let dir = tempfile::tempdir().expect("start dir");
        let start = dir.path().to_string_lossy().into_owned();

        let config = Config::ssh_tmux("jupyter", "dtr-9").reopened(Some(&start));
        let words = words(&config);
        // The last one: the first is `command -v tmux`, which is the guard
        // rather than the command.
        let at = words
            .iter()
            .rposition(|word| word == "tmux")
            .expect("the script runs tmux");
        let mut argv: Vec<String> = words[at + 1..].to_vec();
        assert_eq!(
            argv.first().map(String::as_str),
            Some("new-session"),
            "the test is reading something other than the session command: {argv:?}"
        );
        for word in &mut argv {
            if *word == "$PWD" {
                *word = start.clone();
            }
        }
        argv.push("-d".to_owned());

        // A socket of its own, so a test cannot see (or kill) a session the
        // person running it actually has.
        let tmux = |args: &[String]| {
            std::process::Command::new("tmux")
                .args(args)
                .env("TMUX_TMPDIR", sockets.path())
                .output()
                .expect("tmux")
        };
        let list = || {
            let out = tmux(&[
                "list-sessions".to_owned(),
                "-F".to_owned(),
                "#{session_name} #{session_id} #{pane_current_path}".to_owned(),
            ]);
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };

        let first = tmux(&argv);
        assert!(
            first.status.success(),
            "tmux refused {argv:?}: {}",
            String::from_utf8_lossy(&first.stderr)
        );
        // Not "the only session": a person's `tmux.conf` runs when a server
        // starts, and this machine's starts one of its own.
        let created = list();
        let ours = created
            .lines()
            .find(|line| line.starts_with("dtr-9 "))
            .unwrap_or_else(|| panic!("no dtr-9 among {created:?}"));
        let mut parts = ours.split(' ');
        parts.next();
        let id = parts.next().expect("a session id").to_owned();
        assert_eq!(
            parts.next().map(str::trim_end),
            Some(start.as_str()),
            "the session started where it was asked to: {ours:?}"
        );

        // The same command again. It cannot succeed here — the session exists,
        // so `-A` goes down the *attach* path, and a test has no terminal to
        // attach to — and that is the point: what has to be true is that it
        // found the session rather than making another.
        let second = tmux(&argv);
        let complaint = String::from_utf8_lossy(&second.stderr).to_lowercase();
        assert!(
            !complaint.contains("duplicate"),
            "a missing `-A` looks exactly like this, and the count alone cannot tell: {complaint}"
        );
        let again = list();
        assert_eq!(
            again
                .lines()
                .filter(|line| line.starts_with("dtr-9 "))
                .count(),
            1,
            "attaching made a second session: {created:?} -> {again:?}"
        );
        assert!(
            again.contains(&id),
            "the session was replaced rather than found: {created:?} -> {again:?}"
        );

        let _ = tmux(&["kill-server".to_owned()]);
    }

    /// The script itself, run by a real `sh` with a `tmux` that only records:
    /// what the far end sees just before a session starts, and what happens on a
    /// host where `command -v tmux` finds nothing.
    ///
    /// The word lists above say what the script *is*; this says that it runs —
    /// that `$PWD` is the directory the connection started in, that a path with
    /// a space in it survives the `cd`, and that a host without tmux gets a login
    /// shell rather than a tab that closes on exit 127.
    #[cfg(unix)]
    #[test]
    fn the_script_runs_tmux_where_it_stands_or_falls_back_to_a_shell() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().expect("tempdir");
        let bin = home.path().join("bin");
        std::fs::create_dir(&bin).expect("bin");
        let record = home.path().join("record");
        let tool = |name: &str| {
            let path = bin.join(name);
            std::fs::write(
                &path,
                format!(
                    "#!/bin/sh\nprintf '%s\\0' \"$PWD\" \"$@\" >> {}\n",
                    shlex::try_quote(&record.to_string_lossy()).expect("a temp path is quotable")
                ),
            )
            .expect("write the stand-in");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            path
        };
        tool("tmux");
        let shell = tool("shell");
        let recorded = || -> Vec<String> {
            let bytes = std::fs::read(&record).unwrap_or_default();
            bytes
                .split(|byte| *byte == 0)
                .filter(|field| !field.is_empty())
                .map(|field| String::from_utf8_lossy(field).into_owned())
                .collect()
        };
        let start = home.path().join("it's here");
        std::fs::create_dir(&start).expect("start dir");

        // A new tab: no `cd`, so the directory is the one the connection is in.
        let run = |config: &Config, path: &std::ffi::OsStr| {
            // Absolute: `PATH` below is the stand-ins alone, and this is the
            // one thing the test needs to find for real.
            let _ = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(script(config))
                .current_dir(&start)
                .env("PATH", path)
                .env("SHELL", &shell)
                .output();
            recorded()
        };
        let with_tmux = run(&Config::ssh_tmux("jupyter", "dtr-1"), bin.as_os_str());
        assert_eq!(
            with_tmux,
            [
                start.to_string_lossy().into_owned(),
                "new-session".to_owned(),
                "-A".to_owned(),
                "-s".to_owned(),
                "dtr-1".to_owned(),
                "-c".to_owned(),
                start.to_string_lossy().into_owned(),
            ],
            "the session is created where the connection started"
        );

        // A reconnection: the same session, in the directory the far end
        // reported — a path with a space and a quote in it.
        let _ = std::fs::remove_file(&record);
        let reconnected = run(
            &Config::ssh_tmux("jupyter", "dtr-1").reopened(Some(&start.to_string_lossy())),
            bin.as_os_str(),
        );
        let here = start.to_string_lossy().into_owned();
        assert_eq!(reconnected[0], here, "{reconnected:?}");
        assert_eq!(
            reconnected[reconnected.len() - 2..],
            ["-c".to_owned(), here.clone()],
            "{reconnected:?}"
        );

        // And a host with no tmux: the login shell this tab always started.
        let _ = std::fs::remove_file(&record);
        let empty = home.path().join("empty");
        std::fs::create_dir(&empty).expect("empty bin");
        let plain = run(&Config::ssh_tmux("jupyter", "dtr-1"), empty.as_os_str());
        assert_eq!(
            plain,
            [start.to_string_lossy().into_owned(), "-l".to_owned()],
            "a host without tmux still gets a shell"
        );
    }
}
