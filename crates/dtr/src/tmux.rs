//! tmux on the far end: the work lives in a server, the connection is a window
//! onto it.
//!
//! A dropped ssh connection takes its shell with it — the far end sends `SIGHUP`
//! to the process group the channel owned — so nothing dtr does on *this* side
//! can save a running build. Something has to be holding the work on the other
//! side, and the thing that is already there is tmux.
//!
//! Two halves, and they are different kinds of work. The session itself is a
//! remote command ([`crate::pty::Config::ssh_tmux`]): `new-session -A`, which
//! attaches when the session exists and creates it when it does not, so one
//! command serves both a new tab and a reconnection. Choosing *which* session is
//! a question to the host, so it is done before the session opens: one
//! `BatchMode` connection that asks what tmux has, [`Chooser`] for the answer
//! when there is something to choose between, and a plain login shell when there
//! is no tmux at all.
//!
//! The listing is best effort by design, the same trade [`crate::terminfo`]
//! makes: it is a second connection, and it gives up. A host that wants a
//! password for that connection, or one whose host key is not known yet, answers
//! nothing — so the tab opens as a plain shell, and says so rather than looking
//! like the feature worked.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use dtr_widgets::Card;

use crate::pty;
use crate::ui::Look;

/// What the far end runs to answer "what is in tmux here?".
///
/// The markers are not decoration. `tmux list-sessions` prints nothing and exits
/// non-zero when there is no server, which is indistinguishable from tmux not
/// being installed; and a host that prints a banner (an motd, `stty` noise) on a
/// command channel would look like sessions. Each name carries the `session `
/// prefix so a session literally called `dtr-end` cannot end the listing early.
const SCRIPT: &str = "\
command -v tmux >/dev/null 2>&1 || { echo dtr-no-tmux; exit 0; }
echo dtr-listing
tmux list-sessions -F 'session #{session_name}' 2>/dev/null
echo dtr-end
";

/// The prefix every listed session carries.
const SESSION: &str = "session ";

/// A question that has not been answered by now is not going to be: the person
/// is waiting in front of an empty strip, and a plain shell is better than a
/// longer wait.
const DEADLINE: Duration = Duration::from_secs(8);

/// How often the child is looked at while it runs. A connection that answers
/// quickly ends the wait immediately; this only bounds the poll itself.
const POLL: Duration = Duration::from_millis(25);

/// What the far end said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Listing {
    /// tmux is there, with these sessions (possibly none).
    Sessions(Vec<String>),
    /// No tmux on that host.
    Absent,
    /// The question could not be asked, and why.
    Failed(String),
}

/// What one line of the answer is worth.
#[derive(Debug, PartialEq, Eq)]
enum Answer {
    Sessions(Vec<String>),
    Absent,
    /// A listing that began and did not end: a connection that died mid-output
    /// could hide a name, and a hidden `dtr-1` is one a new session would be
    /// created *onto*.
    Incomplete,
}

fn parse(output: &str) -> Answer {
    let lines: Vec<&str> = output.lines().map(str::trim_end).collect();
    if lines.iter().any(|line| *line == "dtr-no-tmux") {
        return Answer::Absent;
    }
    let Some(start) = lines.iter().position(|line| *line == "dtr-listing") else {
        return Answer::Incomplete;
    };
    let mut names = Vec::new();
    for line in &lines[start + 1..] {
        if *line == "dtr-end" {
            return Answer::Sessions(names);
        }
        if let Some(name) = line.strip_prefix(SESSION) {
            // A session name cannot hold a newline, so one line is one name.
            if !name.is_empty() {
                names.push(name.to_owned());
            }
        }
    }
    Answer::Incomplete
}

/// The first `dtr-N` the far end does not already have.
///
/// Numbered rather than named after the window, because the name is the thing a
/// reconnection asks for and a tab's title belongs to the program inside it. The
/// scan is over *every* session, not just ours: a person who made `dtr-2` by
/// hand keeps it.
pub fn free_name(names: &[String]) -> String {
    (1..)
        .map(|n| format!("dtr-{n}"))
        .find(|candidate| !names.iter().any(|name| name == candidate))
        .expect("an unbounded range always yields a free name")
}

/// Ask the host what tmux has. Blocking, with a deadline; off the UI thread.
pub fn list(alias: &str) -> Listing {
    list_with(Command::new("ssh"), alias)
}

/// The same, with the client named — a test hands in a script that answers
/// instead of opening a connection.
fn list_with(mut command: Command, alias: &str) -> Listing {
    let mut args = pty::batch_args(alias);
    args.push(pty::sh_command(SCRIPT));
    let mut child = match command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => return Listing::Failed(format!("cannot start ssh: {err}")),
    };

    let deadline = Instant::now() + DEADLINE;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(err) => return Listing::Failed(format!("waiting for ssh: {err}")),
        }
        if Instant::now() >= deadline {
            // `kill` on a child that has already exited is a no-op, and the
            // wait is what reaps it either way.
            let _ = child.kill();
            let _ = child.wait();
            return Listing::Failed(format!("no answer in {} s", DEADLINE.as_secs()));
        }
        std::thread::sleep(POLL);
    };

    let mut output = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_string(&mut output);
    }
    match parse(&output) {
        Answer::Sessions(names) => Listing::Sessions(names),
        Answer::Absent => Listing::Absent,
        Answer::Incomplete => {
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            // The last line is the one that says why; earlier ones are banners.
            let last = stderr.lines().last().unwrap_or("").trim().to_owned();
            Listing::Failed(if last.is_empty() {
                format!("ssh exited with {status}")
            } else {
                last
            })
        }
    }
}

/// Ask on a thread of its own and answer with [`crate::app::UserEvent::Tmux`].
///
/// A thread and not the worker: this is one child process with a deadline, the
/// same shape as the terminfo copy, and the UI thread must not wait for it.
pub fn spawn_list(alias: &str, proxy: winit::event_loop::EventLoopProxy<crate::app::UserEvent>) {
    let alias = alias.to_owned();
    let fallback = proxy.clone();
    let asked = alias.clone();
    let spawned = std::thread::Builder::new()
        .name(format!("tmux {alias}"))
        .spawn(move || {
            let listing = list(&asked);
            match &listing {
                Listing::Sessions(names) => {
                    tracing::info!(alias = %asked, sessions = names.len(), "tmux sessions listed")
                }
                Listing::Absent => tracing::info!(alias = %asked, "no tmux on the host"),
                Listing::Failed(why) => {
                    tracing::warn!(alias = %asked, %why, "could not list tmux sessions")
                }
            }
            let _ = proxy.send_event(crate::app::UserEvent::Tmux {
                alias: asked,
                listing,
            });
        });
    if let Err(err) = spawned {
        // The answer still has to arrive: whoever asked is holding a slot for
        // it, and a listing nobody is making would wait forever.
        tracing::warn!(%err, "could not start the tmux listing thread");
        let _ = fallback.send_event(crate::app::UserEvent::Tmux {
            alias,
            listing: Listing::Failed(format!("{err}")),
        });
    }
}

/// What the chooser was asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    /// Start a new session, under a name nothing on that host has.
    Create(String),
    /// Attach to one that is already there.
    Attach(String),
    /// Dismissed without choosing.
    Close,
}

/// Which session to open, when there is a choice to make.
///
/// A card and not a popup: the answer decides what the window will contain, it
/// has to be reachable from the keyboard, and the number of sessions a host has
/// is unbounded — the card's body scrolls.
pub struct Chooser {
    open: bool,
    alias: String,
    names: Vec<String>,
}

impl Default for Chooser {
    fn default() -> Self {
        Self {
            open: false,
            alias: String::new(),
            names: Vec::new(),
        }
    }
}

impl Chooser {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// The host this is choosing for.
    pub fn alias(&self) -> &str {
        &self.alias
    }

    /// Ask about `names` on `alias`.
    pub fn open(&mut self, alias: &str, names: Vec<String>) {
        self.open = true;
        self.alias = alias.to_owned();
        self.names = names;
    }

    pub fn ui(&self, ui: &mut egui::Ui, look: &Look) -> Option<Pick> {
        if !self.open {
            return None;
        }
        if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            return Some(Pick::Close);
        }
        let skin = look.skin();
        let count = match self.names.len() {
            0 => "none".to_owned(),
            1 => "1 session".to_owned(),
            n => format!("{n} sessions"),
        };
        let picked = std::cell::RefCell::new(None::<Pick>);
        let output = Card::new(egui::Id::unique("dtr-tmux-sessions"), &self.alias)
            .size(360.0, 300.0)
            .min(300.0, 240.0)
            .body_min(240.0)
            .kicker("tmux sessions")
            .trailing(&count)
            .close("close the session list")
            .show(ui, &skin, |ui, _inner| {
                if dtr_widgets::menu_row(ui, &skin, "New session", false) {
                    *picked.borrow_mut() = Some(Pick::Create(free_name(&self.names)));
                }
                // Separated, because "New session" is a verb and the rest are
                // names: a run of rows with one verb among them is a row of
                // near-misses under the pointer.
                ui.separator();
                for name in &self.names {
                    if dtr_widgets::menu_row(ui, &skin, name, false) {
                        *picked.borrow_mut() = Some(Pick::Attach(name.clone()));
                    }
                }
            });
        // The card's own cross is a dismissal, not a choice.
        if output.close {
            return Some(Pick::Close);
        }
        picked.into_inner()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listing_is_the_names_between_the_markers() {
        let output = "dtr-listing\nsession work\nsession dtr-1\ndtr-end\n";
        assert_eq!(
            parse(output),
            Answer::Sessions(vec!["work".to_owned(), "dtr-1".to_owned()])
        );
    }

    /// A session called `dtr-end` is a name, not the end of the listing.
    #[test]
    fn a_session_named_like_a_marker_is_still_a_session() {
        let output = "dtr-listing\nsession dtr-end\nsession dtr-2\ndtr-end\n";
        assert_eq!(
            parse(output),
            Answer::Sessions(vec!["dtr-end".to_owned(), "dtr-2".to_owned()])
        );
    }

    #[test]
    fn a_host_without_tmux_says_so() {
        assert_eq!(parse("dtr-no-tmux\n"), Answer::Absent);
    }

    /// Silence is not "no sessions": it is a question that was not answered, and
    /// guessing "none" would make the next new session land on top of an
    /// existing one.
    #[test]
    fn silence_and_half_an_answer_are_not_a_listing() {
        assert_eq!(parse(""), Answer::Incomplete);
        assert_eq!(
            parse("Welcome to Ubuntu\n"),
            Answer::Incomplete,
            "a banner is not a listing"
        );
        assert_eq!(
            parse("dtr-listing\nsession work\n"),
            Answer::Incomplete,
            "a truncated listing must not pass for a short one"
        );
    }

    #[test]
    fn a_new_session_takes_the_first_free_number() {
        assert_eq!(free_name(&[]), "dtr-1");
        assert_eq!(free_name(&["dtr-1".to_owned()]), "dtr-2");
        // A hand-made `dtr-2` is not a gap to step into.
        assert_eq!(
            free_name(&["work".to_owned(), "dtr-2".to_owned()]),
            "dtr-1",
            "only the names that are taken matter"
        );
        assert_eq!(
            free_name(&["dtr-1".to_owned(), "dtr-2".to_owned(), "dtr-3".to_owned()]),
            "dtr-4"
        );
    }

    /// The plumbing without a network: a stand-in `ssh` answers the script, and
    /// what it was handed is checked — the alias has to be the destination and
    /// the script has to be the command.
    #[cfg(unix)]
    #[test]
    fn the_answer_is_read_from_a_connection_that_asked_the_script() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("argv");
        let client = dir.path().join("ssh");
        std::fs::write(
            &client,
            format!(
                // NUL-separated: the script argument has newlines in it, and one
                // argument per line would count those as more arguments.
                "#!/bin/sh\nprintf '%s\\0' \"$@\" > {}\nprintf 'dtr-listing\\nsession work\\ndtr-end\\n'\n",
                shlex::try_quote(&log.to_string_lossy()).expect("a temp path is quotable")
            ),
        )
        .expect("write the stand-in");
        std::fs::set_permissions(&client, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let listing = list_with(Command::new(&client), "jupyter");
        assert_eq!(listing, Listing::Sessions(vec!["work".to_owned()]));

        let recorded = std::fs::read(&log).expect("argv");
        let argv: Vec<String> = recorded
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| String::from_utf8_lossy(arg).into_owned())
            .collect();
        assert_eq!(
            argv.last().map(String::as_str),
            Some(pty::sh_command(SCRIPT).as_str()),
            "the script has to be the command, and through sh"
        );
        let alias = argv
            .iter()
            .position(|arg| arg == "jupyter")
            .expect("the alias is an argument");
        assert_eq!(
            alias,
            argv.len() - 2,
            "nothing may follow the destination but the command: {argv:?}"
        );
        assert_eq!(
            argv[alias - 1],
            "BatchMode=yes",
            "the question must not stop to ask for a password"
        );
        assert!(
            argv.contains(&"-T".to_owned()) && !argv.contains(&"-t".to_owned()),
            "a helper has no terminal: {argv:?}"
        );
    }

    /// A client that fails is a question that was not answered, and the reason
    /// is the one ssh printed.
    #[cfg(unix)]
    #[test]
    fn a_failed_connection_says_why() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let client = dir.path().join("ssh");
        std::fs::write(
            &client,
            "#!/bin/sh\necho 'Permission denied (publickey).' >&2\nexit 255\n",
        )
        .expect("write the stand-in");
        std::fs::set_permissions(&client, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        assert_eq!(
            list_with(Command::new(&client), "jupyter"),
            Listing::Failed("Permission denied (publickey).".to_owned())
        );
    }
}
