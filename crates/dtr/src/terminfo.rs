//! Carrying `xterm-ghostty` to the machine at the other end of an ssh session.
//!
//! dtr claims Ghostty's terminfo entry, which is honest — the VT core is
//! Ghostty's — and wrong wherever that entry is not installed: a vanilla Ubuntu
//! has no `ncurses-term` entry for it, so `clear`, `tput` and even `reset`
//! answer `unknown terminal type` inside a session that otherwise looks fine.
//!
//! The entry is four kilobytes of `tic` source and the far end already has
//! `tic`, so dtr copies it over its own ssh connection and compiles it there,
//! into the user's own directory (`$TERMINFO` when set, else `~/.terminfo`) —
//! what `kitten ssh` does, and for the same reason: a terminal that needs a file
//! on the far end cannot ask the person to go and install it.
//!
//! It is best effort and off the session's path: a connection of its own,
//! started before the session's so the two handshakes overlap, that gives up
//! after [`DEADLINE`] and only ever says anything in the log. A host that needs
//! a password for the extra connection simply does not get the entry, and its
//! `tput` says so itself.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::pty;

/// The entry, as `tic` source, dumped with `infocmp -x` from nixpkgs'
/// `ghostty-1.4.0-terminfo`; the file's own header says so.
const SOURCE: &str = include_str!("../assets/terminfo/xterm-ghostty");

/// What the far end runs, with the source on its stdin.
///
/// `tic` writes to the first *writable* directory of its search path, which
/// depends on the build; `-o` names the directory outright, so the entry lands
/// where ncurses will look for it and nothing root-owned is touched. The script
/// is POSIX `sh`, and it is handed to `sh` explicitly (`pty::sh_command`)
/// because the account's login shell may be fish or elvish, where this text is a
/// parse error rather than a script.
const SCRIPT: &str = "\
set -e
dir=${TERMINFO:-$HOME/.terminfo}
mkdir -p \"$dir\"
tic -x -o \"$dir\" -
";

/// A connection that has not finished by now is not going to be useful: the
/// session beside it is already open, and this is a convenience.
const DEADLINE: Duration = Duration::from_secs(15);

/// How often the child is looked at while it runs. A connection that answers
/// quickly ends the wait immediately; this only bounds the poll itself.
const POLL: Duration = Duration::from_millis(25);

/// `DTR_TERMINFO=0` turns the copy off, for a host where an extra connection
/// per tab is not wanted (it is one more line in its auth log).
const OFF: &str = "DTR_TERMINFO";

/// The hosts the entry has already been copied to in this run, so a second tab
/// to the same machine is not a second connection.
static DONE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::OnceLock::new();

/// The ssh invocation: the session's own connection arguments, with a pipe
/// where a terminal was and the script as the remote command.
///
/// Derived from [`pty::Config::ssh`] rather than written out again, because
/// this has to reach the *same host* the session beside it reaches — the same
/// `~/.ssh/config`, the same jump hosts, the same credentials. Two things
/// change: `-t` becomes `-T`, because nothing here needs a terminal and a tty
/// would want one to answer with; and `BatchMode` is added, because a password
/// prompt on a detached child with piped stdio is a wait that ends in nothing.
/// That is also why there is no `DTR_SSH_BATCH` escape hatch here, unlike
/// `dtr-sftp`: the interactive session beside this one *can* ask, and this one
/// must not.
pub fn ssh_args(alias: &str) -> Vec<String> {
    let mut args: Vec<String> = pty::Config::ssh(alias)
        .args
        .iter()
        .map(|arg| match arg.as_str() {
            "-t" => "-T".to_owned(),
            other => other.to_owned(),
        })
        .collect();
    // The destination is the last of them; ssh reads everything after it as the
    // remote command, so our option goes just before it.
    let at = args.len() - 1;
    args.splice(at..at, ["-o".to_owned(), "BatchMode=yes".to_owned()]);
    // Through `sh`, not straight at the login shell: fish and elvish cannot
    // parse `dir=${TERMINFO:-…}`, and `sshd` hands a command to the account's
    // shell, whatever it is.
    args.push(pty::sh_command(SCRIPT));
    args
}

/// Copy the entry to `alias`, on its own thread, saying nothing on screen.
///
/// Detached on purpose: the pane must not wait for it (the session opens beside
/// it), and closing the tab must not wait for it either — a runtime that owned
/// this would hold teardown until the deadline. Nothing is lost if it never
/// runs, because the failure it prevents is one `tput` reports itself.
pub fn copy(alias: &str) {
    if std::env::var(OFF).as_deref() == Ok("0") {
        return;
    }
    // One connection per host per run: a second tab to the same machine would
    // copy the same four kilobytes again, and the entry is on its disk.
    let done = DONE.get_or_init(Default::default);
    if done
        .lock()
        .map(|done| done.contains(alias))
        .unwrap_or(false)
    {
        return;
    }
    let alias = alias.to_owned();
    let spawned = std::thread::Builder::new()
        .name(format!("terminfo {alias}"))
        .spawn(move || match install(&alias) {
            Ok(()) => {
                // Only a success is remembered: a connection refused because
                // the vault was locked is worth another try on the next tab.
                if let Ok(mut done) = done.lock() {
                    done.insert(alias.clone());
                }
                tracing::info!(alias = %alias, "terminfo entry copied");
            }
            Err(err) => tracing::warn!(
                alias = %alias,
                %err,
                "could not copy the terminfo entry; `tput` there will not know xterm-ghostty"
            ),
        });
    if let Err(err) = spawned {
        // A thread that will not start is not a reason to refuse a session.
        tracing::warn!(%err, "could not start the terminfo copy");
    }
}

/// The blocking half: run `ssh`, hand it the source, and read its answer.
fn install(alias: &str) -> Result<(), String> {
    install_with(Command::new("ssh"), alias)
}

/// The same, with the client named — a test hands in a script that records what
/// it was given instead of opening a connection.
fn install_with(mut command: Command, alias: &str) -> Result<(), String> {
    let mut child = command
        .args(ssh_args(alias))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("cannot start the copy: {err}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        // A host that refuses before reading its input closes the pipe early;
        // its exit status is the answer, and this error is the same fact.
        if let Err(err) = stdin.write_all(SOURCE.as_bytes()) {
            tracing::debug!(%err, "the host stopped reading the terminfo source");
        }
    }

    let deadline = Instant::now() + DEADLINE;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(err) => return Err(format!("waiting for ssh: {err}")),
        }
        if Instant::now() >= deadline {
            // `kill` on a child that has already exited is a no-op, and the
            // wait is what reaps it either way.
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("no answer in {} s", DEADLINE.as_secs()));
        }
        std::thread::sleep(POLL);
    };

    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    if status.success() {
        return Ok(());
    }
    // The last line is the one that says why; earlier ones are banners.
    let last = stderr.lines().last().unwrap_or("").trim().to_owned();
    Err(if last.is_empty() {
        format!("ssh exited with {status}")
    } else {
        last
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// An option after the destination is not an option: ssh hands the rest to
    /// the remote side as its command, so one option in the wrong place turns
    /// the copy into "run `-o` on that host".
    #[test]
    fn every_option_precedes_the_alias_and_the_script_follows_it() {
        let args = ssh_args("jupyter");
        let alias = args
            .iter()
            .position(|arg| arg == "jupyter")
            .expect("the alias is in the arguments");
        assert_eq!(
            args.last().map(String::as_str),
            Some(pty::sh_command(SCRIPT).as_str()),
            "the script has to be the command, and through sh"
        );
        assert_eq!(alias, args.len() - 2, "nothing may follow the destination");
        assert_eq!(
            args[alias - 1],
            "BatchMode=yes",
            "the destination is where the command begins: {args:?}"
        );
        for option in ["-T", "-o", pty::CONNECT_TIMEOUT, "BatchMode=yes"] {
            let at = args
                .iter()
                .position(|arg| arg == option)
                .unwrap_or_else(|| panic!("{option} is not an argument"));
            assert!(at < alias, "{option} comes after the destination");
        }
    }

    /// The plumbing, without a network: a stand-in `ssh` records its argv and
    /// keeps its stdin, which are the two things that can be wrong — the entry
    /// has to arrive on stdin intact, and the script has to be the command.
    #[test]
    fn the_entry_travels_on_stdin_and_the_script_is_the_command() {
        let dir = tempfile::tempdir().expect("tempdir");
        let stand_in = StandIn::new(dir.path(), "exit 0");
        install_with(stand_in.command(), "jupyter").expect("the stand-in succeeds");

        let argv = stand_in.argv();
        assert_eq!(
            argv.last(),
            Some(&pty::sh_command(SCRIPT)),
            "the script is not the command: {argv:?}"
        );
        assert!(
            argv.last()
                .is_some_and(|command| command.starts_with("sh -c '")),
            "the command has to go through sh, not the login shell: {argv:?}"
        );
        assert!(
            argv.contains(&"jupyter".to_owned()),
            "no destination in {argv:?}"
        );
        assert_eq!(
            stand_in.stdin(),
            SOURCE.as_bytes(),
            "the entry did not arrive whole"
        );
    }

    /// A refusal is the far end's answer, and the last line of its stderr is
    /// the one that says why: that is what the log quotes instead of "ssh
    /// exited with 255".
    #[test]
    fn a_failure_quotes_the_last_line_of_stderr() {
        let dir = tempfile::tempdir().expect("tempdir");
        let stand_in = StandIn::new(
            dir.path(),
            "printf 'banner\\ncannot do that\\n' >&2\nexit 255",
        );
        let err = install_with(stand_in.command(), "jupyter").expect_err("255 is a failure");
        assert_eq!(err, "cannot do that");
    }

    /// The command is what `sshd` hands the account's login shell, which may be
    /// fish or elvish; the script is what `sh` then parses. Both have to be
    /// valid, and the outer one must not need a POSIX shell to read it.
    #[test]
    fn the_command_and_the_script_parse_as_sh() {
        let command = pty::sh_command(SCRIPT);
        assert!(
            command.starts_with("sh -c '") && command.ends_with('\''),
            "the command is not a single-quoted sh -c: {command:?}"
        );
        for script in [command, SCRIPT.to_owned()] {
            let child = Command::new("sh")
                .arg("-n")
                .arg("-c")
                .arg(&script)
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .expect("sh");
            let out = child.wait_with_output().expect("wait for sh -n");
            assert!(
                out.status.success(),
                "sh -n: {} for {script:?}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    /// The embedded entry is the entry: `tic` compiles it into a directory
    /// nothing can shadow, and what comes out is xterm-ghostty with the
    /// capabilities the VT core actually implements. A hand-edited or truncated
    /// asset would compile into something subtly wrong instead of failing, so
    /// this reads it back rather than trusting the exit status.
    #[test]
    fn the_entry_compiles_back_into_ghostty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut child = Command::new("tic")
            .args(["-x", "-o"])
            .arg(dir.path())
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("tic (pkgs.ncurses is a test input)");
        child
            .stdin
            .take()
            .expect("a stdin pipe")
            .write_all(SOURCE.as_bytes())
            .expect("write the source");
        let out = child.wait_with_output().expect("wait for tic");
        assert!(
            out.status.success(),
            "tic: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let dump = Command::new("infocmp")
            .args(["-x", "-A"])
            .arg(dir.path())
            .arg("xterm-ghostty")
            .output()
            .expect("infocmp");
        assert!(
            dump.status.success(),
            "the entry is not where tic wrote it: {}",
            String::from_utf8_lossy(&dump.stderr)
        );
        let text = String::from_utf8_lossy(&dump.stdout);
        assert!(
            text.contains("xterm-ghostty|ghostty"),
            "not the ghostty entry:\n{text}"
        );
        // Extended capabilities, which only survive `tic -x`: without them the
        // copy is a plain xterm and `reset` would half-work.
        for capability in ["Smulx=", "Se=", "E3="] {
            assert!(
                text.contains(capability),
                "no {capability} in the entry:\n{text}"
            );
        }
    }

    /// A stand-in `ssh`: records its argv, keeps its stdin, then runs `then`.
    ///
    /// Run through `sh` rather than made executable, because exec'ing a file
    /// that was written a moment ago races every other test's `fork`: a child
    /// that has not reached `execve` yet still holds the write descriptor, and
    /// Linux answers that with `ETXTBSY`.
    struct StandIn {
        script: std::path::PathBuf,
        argv: std::path::PathBuf,
        stdin: std::path::PathBuf,
    }

    impl StandIn {
        fn new(dir: &std::path::Path, then: &str) -> Self {
            let quoted = |path: &std::path::Path| {
                shlex::try_quote(&path.to_string_lossy())
                    .expect("a temp path is quotable")
                    .into_owned()
            };
            let script = dir.join("ssh.sh");
            let argv = dir.join("argv");
            let stdin = dir.join("stdin");
            std::fs::write(
                &script,
                format!(
                    // NUL-separated: the script is one argument with newlines in
                    // it, so a line per argument would not read back.
                    "#!/bin/sh\nprintf '%s\\0' \"$@\" > {}\ncat > {}\n{}\n",
                    quoted(&argv),
                    quoted(&stdin),
                    then
                ),
            )
            .expect("write the stand-in");
            Self {
                script,
                argv,
                stdin,
            }
        }

        fn command(&self) -> Command {
            let mut command = Command::new("sh");
            command.arg(&self.script);
            command
        }

        fn argv(&self) -> Vec<String> {
            std::fs::read_to_string(&self.argv)
                .expect("the stand-in records its argv")
                // The last element is empty: the recorded list ends with a NUL.
                .split('\0')
                .filter(|arg| !arg.is_empty())
                .map(str::to_owned)
                .collect()
        }

        fn stdin(&self) -> Vec<u8> {
            std::fs::read(&self.stdin).unwrap_or_default()
        }
    }
}
