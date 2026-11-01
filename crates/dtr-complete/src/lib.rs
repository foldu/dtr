//! Completion and ranking for the command palette.
//!
//! The palette is one input line that means different things depending on its
//! first word: `connect /dev/ttyUSB0`, `ssh jupiter.home`, `exec fish`. That
//! shape decides the design here — a *command* completes against a fixed list,
//! an *argument* completes against whatever that command knows about, and both
//! need to answer two different questions:
//!
//! - what should the list show, ranked (`complete`), and
//! - what single character suffix would finish the word (`inline`), which is
//!   what the grey ghost text in the input shows and Tab accepts.
//!
//! Inline completion is deliberately narrower than ranking: it only fires for a
//! prefix of a candidate. Offering to complete `jptr` to `jupiter` inline would
//! produce ghost text that shares no prefix with what was typed, which reads as
//! a glitch rather than a suggestion.

use std::ops::Range;

use nucleo_matcher::{Config, Matcher, Utf32Str};

/// What kind of thing a candidate is. The palette colours by kind, and the
/// caller uses it to decide what accepting the candidate *means*.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Command,
    Host,
    Port,
    Shell,
}

/// One candidate. `insert` is the whole input line after acceptance, not just
/// the word: the palette is a command line, and replacing the line wholesale
/// keeps the caller from re-implementing token splicing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub kind: Kind,
    pub label: String,
    pub insert: String,
    pub detail: String,
}

/// A ranked candidate, plus where it came from in the input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    pub item: Item,
    pub score: u32,
}

/// The suffix that would complete the current word, and the full line it would
/// produce.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inline {
    pub suffix: String,
    pub insert: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Argument {
    None,
    Hosts,
    Ports,
    Shells,
    Themes,
}

struct Command {
    name: &'static str,
    summary: &'static str,
    argument: Argument,
}

/// The palette's commands.
///
/// **Every one starts with a different letter**, and that is not a coincidence
/// to preserve: the first letter is the accelerator, so `s` is `ssh` and Tab on
/// one letter finishes the word. `every_command_starts_with_a_different_letter`
/// fails when a new command breaks it — rename the new one, do not merge the
/// two into a shared prefix.
///
/// A command that `palette::action_for` has no arm for belongs here only after
/// it has one: the list is an offer, and an offer that does nothing is worse
/// than a shorter list.
const COMMANDS: &[Command] = &[
    Command {
        name: "connect",
        summary: "open a serial port",
        argument: Argument::Ports,
    },
    Command {
        name: "ssh",
        summary: "open a session on an ssh host",
        argument: Argument::Hosts,
    },
    Command {
        name: "exec",
        summary: "launch a local shell",
        argument: Argument::Shells,
    },
    Command {
        name: "replay",
        summary: "play a captured byte stream",
        argument: Argument::None,
    },
    Command {
        name: "files",
        summary: "open a file browser pane",
        argument: Argument::None,
    },
    Command {
        name: "theme",
        summary: "apply a colour theme",
        argument: Argument::Themes,
    },
    Command {
        name: "demo",
        summary: "show the canned demo screen",
        argument: Argument::None,
    },
    Command {
        name: "quit",
        summary: "close the window",
        argument: Argument::None,
    },
];

/// A serial port as the picker sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortInfo {
    pub path: String,
    /// USB vendor/product or driver detail, when known.
    pub detail: String,
}

/// Everything the palette can draw from. Cheap to rebuild; the caller sets the
/// lists whenever the world changes (a device is replugged, ssh config edited).
#[derive(Default)]
pub struct Engine {
    hosts: Vec<(String, String)>,
    ports: Vec<PortInfo>,
    shells: Vec<String>,
    themes: Vec<String>,
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_hosts(&mut self, hosts: impl IntoIterator<Item = (String, String)>) {
        self.hosts = hosts.into_iter().collect();
        self.hosts.sort_by(|a, b| a.0.cmp(&b.0));
    }

    pub fn set_ports(&mut self, ports: impl IntoIterator<Item = PortInfo>) {
        self.ports = ports.into_iter().collect();
        self.ports.sort_by(|a, b| a.path.cmp(&b.path));
    }

    pub fn set_shells(&mut self, shells: impl IntoIterator<Item = String>) {
        self.shells = shells.into_iter().collect();
        self.shells.sort();
    }

    pub fn set_themes(&mut self, themes: impl IntoIterator<Item = String>) {
        self.themes = themes.into_iter().collect();
        self.themes.sort();
    }

    /// Ranked candidates for the whole input line.
    pub fn complete(&self, input: &str) -> Vec<Completion> {
        let parsed = Parsed::of(input);
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut out = Vec::new();

        match parsed.word {
            Word::Command => {
                for command in COMMANDS {
                    if let Some(score) = fuzzy(&mut matcher, command.name, parsed.needle) {
                        out.push(Completion {
                            item: Item {
                                kind: Kind::Command,
                                label: command.name.to_owned(),
                                insert: command.name.to_owned(),
                                detail: command.summary.to_owned(),
                            },
                            score,
                        });
                    }
                }
            }
            Word::Argument(command) => {
                for (value, detail) in self.arguments(&command.argument) {
                    if let Some(score) = fuzzy(&mut matcher, &value, parsed.needle) {
                        out.push(Completion {
                            item: Item {
                                kind: kind_of(&command.argument),
                                label: value.clone(),
                                insert: format!("{} {}", command.name, value),
                                detail,
                            },
                            score,
                        });
                    }
                }
            }
        }

        // Stable within equal scores: the lists are sorted already, so equal
        // candidates keep an order the user can learn.
        out.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.item.label.cmp(&b.item.label))
        });
        out
    }

    /// The ghost suffix for the current word, if one candidate extends it.
    ///
    /// Only prefixes qualify, and only when they are unambiguous enough to be
    /// useful: the top-ranked completion must start with what was typed.
    pub fn inline(&self, input: &str) -> Option<Inline> {
        let parsed = Parsed::of(input);
        if parsed.needle.is_empty() {
            return None;
        }
        let best = self.complete(input).into_iter().next()?;
        let candidate = best.item.label.as_str();
        let typed = parsed.needle.to_lowercase();
        if !candidate.to_lowercase().starts_with(&typed) || candidate.len() == parsed.needle.len() {
            return None;
        }
        Some(Inline {
            suffix: candidate[parsed.needle.len()..].to_owned(),
            insert: best.item.insert,
        })
    }

    fn arguments(&self, argument: &Argument) -> Vec<(String, String)> {
        match argument {
            Argument::None => Vec::new(),
            Argument::Hosts => self.hosts.clone(),
            Argument::Ports => self
                .ports
                .iter()
                .map(|port| (port.path.clone(), port.detail.clone()))
                .collect(),
            Argument::Shells => self
                .shells
                .iter()
                .map(|shell| (shell.clone(), "local shell".to_owned()))
                .collect(),
            Argument::Themes => self
                .themes
                .iter()
                .map(|theme| (theme.clone(), "theme".to_owned()))
                .collect(),
        }
    }
}

fn kind_of(argument: &Argument) -> Kind {
    match argument {
        Argument::Hosts => Kind::Host,
        Argument::Ports => Kind::Port,
        Argument::Shells => Kind::Shell,
        Argument::Themes => Kind::Command,
        Argument::None => Kind::Command,
    }
}

/// The word under the cursor, and what it is a word *of*.
enum Word {
    Command,
    Argument(&'static Command),
}

struct Parsed<'a> {
    word: Word,
    /// What is being completed — the command name or the argument being typed.
    needle: &'a str,
    #[allow(dead_code)]
    replace: Range<usize>,
}

impl<'a> Parsed<'a> {
    fn of(input: &'a str) -> Self {
        // The trailing space counts: `ssh ` is already asking about the
        // *argument*, and trimming it away made the palette fall back to
        // completing commands after the space where hosts should appear.
        let trimmed = input.trim_start();
        match trimmed.split_once(char::is_whitespace) {
            None => Self {
                word: Word::Command,
                needle: trimmed,
                replace: 0..input.len(),
            },
            Some((name, rest)) => {
                let needle = rest.trim_start();
                let start = trimmed.len() - needle.len();
                let command = COMMANDS.iter().find(|command| {
                    command.name.eq_ignore_ascii_case(name)
                        || command.name.starts_with(&name.to_lowercase())
                });
                match command {
                    Some(command) => Self {
                        word: Word::Argument(command),
                        needle,
                        replace: start..input.len(),
                    },
                    // An unknown command completes as a command word, so a typo
                    // suggests the command it was meant to be.
                    None => Self {
                        word: Word::Command,
                        needle: name,
                        replace: 0..input.len(),
                    },
                }
            }
        }
    }
}

fn fuzzy(matcher: &mut Matcher, candidate: &str, needle: &str) -> Option<u32> {
    if needle.is_empty() {
        return Some(0);
    }
    let mut haystack_buf = Vec::new();
    let mut needle_buf = Vec::new();
    let haystack = Utf32Str::new(candidate, &mut haystack_buf);
    let needle = Utf32Str::new(needle, &mut needle_buf);
    matcher.fuzzy_match(haystack, needle).map(u32::from)
}

#[cfg(test)]
mod tests {
    use super::{Engine, Kind, PortInfo};

    fn engine() -> Engine {
        let mut engine = Engine::new();
        engine.set_hosts([
            ("jupiter.home.5kw.li".to_owned(), "user barnabas".to_owned()),
            ("saturn.home.5kw.li".to_owned(), String::new()),
            ("mars.lab.5kw.li".to_owned(), String::new()),
        ]);
        engine.set_ports([
            PortInfo {
                path: "/dev/ttyUSB0".to_owned(),
                detail: "FTDI FT232R".to_owned(),
            },
            PortInfo {
                path: "/dev/ttyACM0".to_owned(),
                detail: "Arduino".to_owned(),
            },
        ]);
        engine.set_shells(["fish".to_owned(), "bash".to_owned()]);
        engine.set_themes(["kanagawa wave".to_owned(), "dracula".to_owned()]);
        engine
    }

    /// r#"The space is the signal."#;
    #[test]
    fn a_space_after_ssh_offers_hosts_rather_than_commands() {
        let engine = engine();
        let completions = engine.complete("ssh ");
        assert!(!completions.is_empty(), "hosts should be offered");
        assert!(
            completions
                .iter()
                .all(|completion| completion.item.label.contains(".li")),
            "every candidate is a host, not a command: {:?}",
            completions
                .iter()
                .map(|completion| completion.item.label.clone())
                .collect::<Vec<_>>()
        );
        // And a prefix narrows them.
        let narrowed = engine.complete("ssh jup");
        assert_eq!(narrowed.len(), 1);
        assert_eq!(narrowed[0].item.label, "jupiter.home.5kw.li");
    }

    #[test]
    fn a_bare_prefix_lists_commands() {
        let completions = engine().complete("conn");
        assert_eq!(completions[0].item.label, "connect");
        assert_eq!(completions[0].item.kind, Kind::Command);
    }

    #[test]
    fn a_typo_still_suggests_the_command() {
        let completions = engine().complete("conect");
        assert_eq!(completions[0].item.label, "connect");
    }

    #[test]
    fn arguments_complete_from_the_command_s_own_list() {
        let completions = engine().complete("connect tty");
        let labels: Vec<_> = completions.iter().map(|c| c.item.label.as_str()).collect();
        assert!(labels.contains(&"/dev/ttyUSB0"));
        assert!(labels.contains(&"/dev/ttyACM0"));
        // A port command must not offer hosts.
        assert!(!labels.iter().any(|label| label.contains("jupiter")));
    }

    #[test]
    fn accepting_an_argument_replaces_the_whole_line() {
        let completions = engine().complete("ssh jup");
        let best = &completions[0];
        assert_eq!(best.item.insert, "ssh jupiter.home.5kw.li");
    }

    #[test]
    fn inline_completes_a_prefix_of_a_host() {
        let inline = engine().inline("ssh jupiter.h").expect("a suffix");
        assert_eq!(inline.suffix, "ome.5kw.li");
        assert_eq!(inline.insert, "ssh jupiter.home.5kw.li");
    }

    #[test]
    fn inline_declines_when_only_fuzzy_matches() {
        // `jptr` ranks jupiter first, but it is not a prefix, so ghost text
        // would appear from nowhere.
        assert!(engine().inline("ssh jptr").is_none());
    }

    #[test]
    fn inline_needs_something_to_add() {
        assert!(engine().inline("ssh jupiter.home.5kw.li").is_none());
        assert!(engine().inline("ssh ").is_none());
    }

    #[test]
    fn fuzzy_matching_reaches_inside_a_name() {
        let completions = engine().complete("ssh 5kw.li");
        assert!(completions.len() >= 3);
        assert!(completions.iter().all(|c| c.item.kind == Kind::Host));
    }

    #[test]
    fn themes_complete_after_the_theme_command() {
        let completions = engine().complete("theme kan");
        assert_eq!(completions[0].item.label, "kanagawa wave");
        assert_eq!(completions[0].item.insert, "theme kanagawa wave");
    }

    #[test]
    fn a_theme_name_completes_inline() {
        let inline = engine().inline("theme kanagawa").expect("a suffix");
        assert_eq!(inline.suffix, " wave");
    }

    #[test]
    fn shells_complete() {
        let completions = engine().complete("exec fi");
        assert_eq!(completions[0].item.label, "fish");
        assert_eq!(completions[0].item.kind, Kind::Shell);
    }

    #[test]
    fn files_is_a_command() {
        let completions = engine().complete("fil");
        assert_eq!(completions[0].item.label, "files");
        assert_eq!(completions[0].item.kind, Kind::Command);
    }

    #[test]
    fn a_command_without_arguments_offers_nothing() {
        assert!(engine().complete("demo x").is_empty());
        // After the space the *argument* is the word being completed, and this
        // command has none — offering the command back would be noise, and it
        // is what made `ssh ` complete commands instead of hosts.
        assert!(engine().complete("demo ").is_empty());
    }

    #[test]
    fn empty_input_ranks_every_command() {
        let completions = engine().complete("");
        assert_eq!(completions.len(), super::COMMANDS.len());
    }

    /// The palette's rule: the first letter is the accelerator, so `s` is `ssh`
    /// and nothing else may start with `s`. Two commands sharing a letter make
    /// `s<Tab>` a guess, and the list a list of near-misses.
    #[test]
    fn every_command_starts_with_a_different_letter() {
        let mut owner: std::collections::BTreeMap<char, &'static str> = Default::default();
        for command in super::COMMANDS {
            let letter = command
                .name
                .chars()
                .next()
                .expect("a command has a name")
                .to_ascii_lowercase();
            if let Some(other) = owner.insert(letter, command.name) {
                panic!(
                    "`{other}` and `{}` both start with `{letter}`",
                    command.name
                );
            }
        }
    }

    /// The rule's consequence: one letter, one command, and Tab finishes the
    /// word. This is also the ranking assertion — an inner fuzzy match must not
    /// outrank the command that starts with the letter, or `c<Tab>` would
    /// complete `exec`.
    #[test]
    fn one_letter_completes_to_its_command() {
        for command in super::COMMANDS {
            let letter = &command.name[..1];
            let inline = engine()
                .inline(letter)
                .unwrap_or_else(|| panic!("`{letter}` completed to nothing"));
            assert_eq!(
                format!("{letter}{}", inline.suffix),
                command.name,
                "`{letter}` should complete to the one command that starts with it"
            );
        }
    }
}
