//! Key bindings: names for what the terminal can do, and the map from chords to
//! those names.
//!
//! The config file carries the chords (see `dtr_config::keys`), so this module
//! owns the *vocabulary*: every action has a name that can be written in TOML,
//! and a default binding that the config can override or switch off with
//! `false`.

use std::collections::BTreeMap;

use dtr_config::keys::{Chord, Sequence};

/// Everything a key can be bound to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Palette,
    Settings,
    NewTab,
    ClosePane,
    NextTab,
    PreviousTab,
    SplitHorizontal,
    SplitVertical,
    Copy,
    Paste,
    ScrollPageUp,
    ScrollPageDown,
    ScrollTop,
    ScrollBottom,
    /// Walk the viewport to the previous prompt (the shell has to mark them).
    PreviousPrompt,
    NextPrompt,
    /// Capture the last command's output block for the pager.
    CaptureBlock,
    ModemDtr,
    ModemRts,
    ModemBreak,
    Echo,
}

impl Action {
    /// The name this action carries in `[keys]`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Palette => "palette",
            Self::Settings => "settings",
            Self::NewTab => "new-tab",
            Self::ClosePane => "close-pane",
            Self::NextTab => "next-tab",
            Self::PreviousTab => "previous-tab",
            Self::SplitHorizontal => "split-horizontal",
            Self::SplitVertical => "split-vertical",
            Self::Copy => "copy",
            Self::Paste => "paste",
            Self::ScrollPageUp => "scroll-page-up",
            Self::ScrollPageDown => "scroll-page-down",
            Self::ScrollTop => "scroll-top",
            Self::ScrollBottom => "scroll-bottom",
            Self::PreviousPrompt => "previous-prompt",
            Self::NextPrompt => "next-prompt",
            Self::CaptureBlock => "capture-block",
            Self::ModemDtr => "modem-dtr",
            Self::ModemRts => "modem-rts",
            Self::ModemBreak => "modem-break",
            Self::Echo => "echo",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        ALL.iter()
            .copied()
            .find(|action| action.name() == name)
    }
}

/// Every action, in the order the template lists them.
pub const ALL: [Action; 21] = [
    Action::Palette,
    Action::Settings,
    Action::NewTab,
    Action::ClosePane,
    Action::NextTab,
    Action::PreviousTab,
    Action::SplitHorizontal,
    Action::SplitVertical,
    Action::Copy,
    Action::Paste,
    Action::ScrollPageUp,
    Action::ScrollPageDown,
    Action::ScrollTop,
    Action::ScrollBottom,
    Action::PreviousPrompt,
    Action::NextPrompt,
    Action::CaptureBlock,
    Action::ModemDtr,
    Action::ModemRts,
    Action::ModemBreak,
    Action::Echo,
];

/// What the terminal does out of the box, as `(action, chords)`.
///
/// Written in the same syntax the config file uses, so the template and the
/// defaults cannot drift: a binding documented there is a binding here.
pub const DEFAULTS: &[(&str, &str)] = &[
    ("palette", "ctrl-shift-p"),
    ("settings", "ctrl-,"),
    ("new-tab", "ctrl-shift-t"),
    ("close-pane", "ctrl-shift-w"),
    ("next-tab", "ctrl-tab"),
    ("previous-tab", "ctrl-shift-tab"),
    // Shifted punctuation is written as the character it produces: pressing
    // ctrl+shift+\\ reports `|`, and `ctrl-shift-\\` would never match.
    ("split-horizontal", "ctrl-|"),
    ("split-vertical", "ctrl-_"),
    ("copy", "ctrl-shift-c"),
    ("paste", "ctrl-shift-v"),
    ("scroll-page-up", "shift-pageup"),
    ("scroll-page-down", "shift-pagedown"),
    ("scroll-top", "shift-home"),
    ("scroll-bottom", "shift-end"),
    // Only do anything when the shell emits OSC 133, which is what marks a
    // prompt; without it the core has no prompts to walk to.
    ("previous-prompt", "ctrl-shift-up"),
    ("next-prompt", "ctrl-shift-down"),
    ("capture-block", "ctrl-shift-o"),
    ("modem-dtr", "ctrl-shift-d"),
    ("modem-rts", "ctrl-shift-r"),
    ("modem-break", "ctrl-shift-b"),
    ("echo", "ctrl-shift-e"),
];

/// What a chord means right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Matched {
    /// Nothing is bound to this chord.
    Nothing,
    /// A complete binding.
    Action(Action),
    /// The chord starts a longer binding; the next key decides.
    Prefix,
}

/// One key event, in the shape the handler needs.
///
/// winit's `KeyEvent` cannot be constructed outside the window library (its
/// platform-specific payload is private), so the handler takes this instead.
/// The window fills it from a real event; `--keys` fills it from a script, and
/// both run the same code.
#[derive(Clone, Debug)]
pub struct KeyStroke {
    /// The key's *position*, which is what a terminal reports and what the key
    /// encoder wants: a layout decides the character, not the key. `None` for a
    /// scripted stroke, which has no keyboard behind it.
    pub physical: Option<winit::keyboard::KeyCode>,
    pub key: winit::keyboard::Key,
    pub text: Option<String>,
    pub state: winit::event::ElementState,
    pub repeat: bool,
    /// The modifiers that were down. A real event takes them from the window;
    /// a scripted one from the chord, because a script has no keyboard.
    pub mods: winit::keyboard::ModifiersState,
}

impl KeyStroke {
    /// A press of `chord`, as a key event would have reported it.
    pub fn press(chord: Chord) -> Self {
        let (key, text) = match chord.key {
            dtr_config::keys::Key::Char(ch) => {
                // Shift shows up in the character a keyboard produces: the
                // event says `|`, not shift and backslash.
                let produced = if chord.mods.shift {
                    shifted(ch)
                } else {
                    ch
                };
                (
                    winit::keyboard::Key::Character(produced.to_string().into()),
                    Some(produced.to_string()),
                )
            }
            dtr_config::keys::Key::Named(named) => (named_key(named), None),
        };
        // `ModifiersState` is bitflags: build the bits rather than pretend a
        // keyboard is down.
        let mut mods = winit::keyboard::ModifiersState::empty();
        mods.set(winit::keyboard::ModifiersState::CONTROL, chord.mods.ctrl);
        mods.set(winit::keyboard::ModifiersState::ALT, chord.mods.alt);
        mods.set(winit::keyboard::ModifiersState::SHIFT, chord.mods.shift);
        mods.set(winit::keyboard::ModifiersState::SUPER, chord.mods.sup);
        Self {
            physical: None,
            key,
            text,
            state: winit::event::ElementState::Pressed,
            repeat: false,
            mods,
        }
    }

    /// A typed character, as the shell should receive it.
    pub fn typed(ch: char) -> Self {
        Self {
            physical: None,
            key: winit::keyboard::Key::Character(ch.to_string().into()),
            text: Some(ch.to_string()),
            state: winit::event::ElementState::Pressed,
            repeat: false,
            mods: winit::keyboard::ModifiersState::empty(),
        }
    }

    /// The release of a typed character. A real keyboard sends both, and the
    /// handler must send bytes for one of them only: encoding the release as
    /// well is exactly the "lsls" bug.
    pub fn typed_release(ch: char) -> Self {
        Self {
            state: winit::event::ElementState::Released,
            ..Self::typed(ch)
        }
    }
}

/// Parse a `--keys` script into strokes.
///
/// `;` separates tokens, not `,`: a chord is allowed to be a comma
/// (`ctrl-,` opens the menu), so the separator cannot be one.
///
/// A token is a chord (`ctrl-shift-t`), a named key (`enter`, `down`), `+`
/// between the chords of one binding (`ctrl-k+ctrl-s`), or `text:` followed by
/// characters to type. The tokens exist so a test can say "open the menu, move
/// down twice, save" in one argument.
pub fn parse_script(script: &str) -> Vec<KeyStroke> {
    let mut strokes = Vec::new();
    for token in script.split(';') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        if let Some(text) = token.strip_prefix("text:") {
            for ch in text.chars() {
                // Press and release, like a keyboard: a script that only
                // pressed would fail to catch the doubled-keystroke bug.
                strokes.push(KeyStroke::typed(ch));
                strokes.push(KeyStroke::typed_release(ch));
            }
            continue;
        }
        // `ctrl-k+ctrl-s`: `+` joins the chords of one binding, because `;`
        // already separates tokens.
        let chords: Vec<&str> = token.split('+').collect();
        if chords.len() > 1 {
            for part in chords {
                if let Ok(sequence) = dtr_config::keys::parse_sequence(part) {
                    for chord in sequence.chords {
                        strokes.push(KeyStroke::press(chord));
                    }
                }
            }
            continue;
        }
        match dtr_config::keys::parse_sequence(token) {
            Ok(sequence) => {
                for chord in sequence.chords {
                    strokes.push(KeyStroke::press(chord));
                }
            }
            Err(err) => tracing::warn!(token, %err, "keys: cannot parse script token"),
        }
    }
    strokes
}

/// The character a shifted key produces on a US layout, for the few keys a
/// terminal binding is likely to use.
fn shifted(ch: char) -> char {
    match ch {
        '1' => '!',
        '2' => '@',
        '3' => '#',
        '4' => '$',
        '5' => '%',
        '6' => '^',
        '7' => '&',
        '8' => '*',
        '9' => '(',
        '0' => ')',
        '-' => '_',
        '=' => '+',
        '[' => '{',
        ']' => '}',
        '\\' => '|',
        ';' => ':',
        '\'' => '"',
        ',' => '<',
        '.' => '>',
        '/' => '?',
        '`' => '~',
        other => other,
    }
}

fn named_key(named: dtr_config::keys::Named) -> winit::keyboard::Key {
    use dtr_config::keys::Named as N;
    use winit::keyboard::{Key, NamedKey};
    let named = match named {
        N::Tab => NamedKey::Tab,
        N::Enter => NamedKey::Enter,
        N::Escape => NamedKey::Escape,
        N::Space => NamedKey::Space,
        N::Backspace => NamedKey::Backspace,
        N::Delete => NamedKey::Delete,
        N::Insert => NamedKey::Insert,
        N::Up => NamedKey::ArrowUp,
        N::Down => NamedKey::ArrowDown,
        N::Left => NamedKey::ArrowLeft,
        N::Right => NamedKey::ArrowRight,
        N::Home => NamedKey::Home,
        N::End => NamedKey::End,
        N::PageUp => NamedKey::PageUp,
        N::PageDown => NamedKey::PageDown,
        N::Function(_) => NamedKey::F1,
    };
    Key::Named(named)
}

/// Turn a real key event into a chord, or `None` for keys that stay with the
/// terminal.
///
/// A bare character is not a binding: typing `t` must reach the shell, so only
/// chords with a modifier (or a named key that is not text) are matched.
pub fn chord_from_event(
    key: &winit::keyboard::Key,
    modifiers: winit::keyboard::ModifiersState,
) -> Option<Chord> {
    use dtr_config::keys::{Key as Spec, Mods, Named};
    use winit::keyboard::{Key, NamedKey};

    let mods = Mods {
        ctrl: modifiers.control_key(),
        alt: modifiers.alt_key(),
        shift: modifiers.shift_key(),
        sup: modifiers.super_key(),
    };
    let spec = match key {
        Key::Named(named) => {
            let named = match named {
                NamedKey::Tab => Named::Tab,
                NamedKey::Enter => Named::Enter,
                NamedKey::Escape => Named::Escape,
                NamedKey::Space => Named::Space,
                NamedKey::Backspace => Named::Backspace,
                NamedKey::Delete => Named::Delete,
                NamedKey::Insert => Named::Insert,
                NamedKey::ArrowUp => Named::Up,
                NamedKey::ArrowDown => Named::Down,
                NamedKey::ArrowLeft => Named::Left,
                NamedKey::ArrowRight => Named::Right,
                NamedKey::Home => Named::Home,
                NamedKey::End => Named::End,
                NamedKey::PageUp => Named::PageUp,
                NamedKey::PageDown => Named::PageDown,
                other => {
                    let text = format!("{other:?}");
                    let digits = text.strip_prefix('F')?;
                    Named::Function(digits.parse().ok()?)
                }
            };
            Spec::Named(named)
        }
        Key::Character(text) => {
            let mut chars = text.chars();
            let ch = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            Spec::Char(ch.to_ascii_lowercase())
        }
        _ => return None,
    };
    Some(Chord { mods, key: spec })
}

/// Chords to actions, with the config's overrides applied.
pub struct Keymap {
    bindings: Vec<(Sequence, Action)>,
    /// Chords bound to more than one action. The first binding wins at dispatch
    /// time, so this is a diagnostic rather than an error: a user who rebinds
    /// onto a default deserves to be told, not stopped.
    conflicts: Vec<(Sequence, Action, Action)>,
}

impl Keymap {
    /// Build the map: defaults, then `[keys]` on top.
    ///
    /// An override replaces every default binding of that action, so rebinding
    /// `new-tab` leaves nothing on `ctrl-shift-t`. `false` in the config turns
    /// the action's bindings off entirely, which is how a chord is handed back
    /// to the shell.
    pub fn new(overrides: &BTreeMap<String, Sequence>) -> Self {
        let mut bindings: Vec<(Sequence, Action)> = Vec::new();
        for (name, text) in DEFAULTS {
            let Some(action) = Action::from_name(name) else {
                continue;
            };
            if overrides.contains_key(*name) {
                continue;
            }
            if let Ok(sequence) = dtr_config::keys::parse_sequence(text) {
                bindings.push((sequence, action));
            }
        }
        for (name, sequence) in overrides {
            let Some(action) = Action::from_name(name) else {
                // An unknown name is reported by the caller, which has a log;
                // refusing to start over a typo would be worse.
                continue;
            };
            if sequence.is_disabled() {
                continue;
            }
            bindings.push((sequence.clone(), action));
        }

        let mut conflicts = Vec::new();
        for (index, (sequence, action)) in bindings.iter().enumerate() {
            for (other_sequence, other) in bindings.iter().skip(index + 1) {
                if sequence == other_sequence && action != other {
                    conflicts.push((sequence.clone(), *action, *other));
                }
            }
        }
        Self {
            bindings,
            conflicts,
        }
    }

    /// Chords that more than one action claims.
    pub fn conflicts(&self) -> &[(Sequence, Action, Action)] {
        &self.conflicts
    }

    /// Action names in the config that no action answers to.
    pub fn unknown_names(overrides: &BTreeMap<String, Sequence>) -> Vec<String> {
        overrides
            .keys()
            .filter(|name| Action::from_name(name).is_none())
            .cloned()
            .collect()
    }

    /// Resolve a chord, given the chords already typed towards a longer binding.
    ///
    /// Shift on punctuation is not a modifier in practice: pressing
    /// ctrl+shift+backslash reports the character `|` with shift held, and a
    /// binding written `ctrl-shift-\\` describes a key that no keyboard sends.
    /// The lookup therefore retries without shift for a non-alphabetic
    /// character, so both spellings work and neither silently does nothing.
    pub fn resolve(&self, pending: &[Chord], chord: Chord) -> Matched {
        let matched = self.resolve_exact(pending, chord);
        if matched != Matched::Nothing {
            return matched;
        }
        if let dtr_config::keys::Key::Char(ch) = chord.key {
            if !ch.is_alphabetic() && chord.mods.shift {
                let without = Chord {
                    mods: dtr_config::keys::Mods {
                        shift: false,
                        ..chord.mods
                    },
                    key: chord.key,
                };
                return self.resolve_exact(pending, without);
            }
        }
        Matched::Nothing
    }

    fn resolve_exact(&self, pending: &[Chord], chord: Chord) -> Matched {
        let mut typed = pending.to_vec();
        typed.push(chord);

        // Exact first: a sequence that is both a prefix and a complete binding
        // must act on the complete one.
        if let Some((_, action)) = self.bindings.iter().find(|(seq, _)| seq.chords == typed) {
            return Matched::Action(*action);
        }
        if self
            .bindings
            .iter()
            .any(|(seq, _)| seq.chords.len() > typed.len() && seq.chords.starts_with(&typed))
        {
            return Matched::Prefix;
        }
        Matched::Nothing
    }

    /// The action a half-typed binding would fire if the user stopped now.
    ///
    /// Zed waits a second for the next chord and then does this; a prefix that
    /// is also a complete binding must still work on its own, or typing the
    /// first chord of `ctrl-k ctrl-s` would make `ctrl-k` dead.
    pub fn flush(&self, pending: &[Chord]) -> Option<Action> {
        (1..=pending.len()).rev().find_map(|length| {
            self.bindings
                .iter()
                .find(|(sequence, _)| sequence.chords == pending[..length])
                .map(|(_, action)| *action)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dtr_config::keys::parse_sequence;

    #[test]
    fn the_defaults_parse_and_are_unique() {
        let mut seen: Vec<Sequence> = Vec::new();
        for (name, text) in DEFAULTS {
            assert!(Action::from_name(name).is_some(), "{name} is not an action");
            let sequence = parse_sequence(text).unwrap_or_else(|err| panic!("{name}: {err}"));
            assert!(
                !seen.contains(&sequence),
                "{name} reuses the binding {sequence}"
            );
            seen.push(sequence);
        }
        assert_eq!(seen.len(), ALL.len(), "every action has a default");
    }

    #[test]
    fn an_override_replaces_the_default() {
        let mut overrides = BTreeMap::new();
        overrides.insert("new-tab".to_owned(), parse_sequence("ctrl-t").unwrap());
        let keymap = Keymap::new(&overrides);

        let old = parse_sequence("ctrl-shift-t").unwrap();
        assert_eq!(
            keymap.resolve(&[], old.chords[0]),
            Matched::Nothing,
            "the default binding should be gone"
        );
        let new = parse_sequence("ctrl-t").unwrap();
        assert_eq!(
            keymap.resolve(&[], new.chords[0]),
            Matched::Action(Action::NewTab)
        );
    }

    #[test]
    fn false_switches_a_binding_off() {
        let mut overrides = BTreeMap::new();
        overrides.insert("copy".to_owned(), Sequence::disabled());
        let keymap = Keymap::new(&overrides);
        let chord = parse_sequence("ctrl-shift-c").unwrap();
        assert_eq!(keymap.resolve(&[], chord.chords[0]), Matched::Nothing);
    }

    #[test]
    fn a_two_chord_binding_waits_for_the_second_key() {
        let mut overrides = BTreeMap::new();
        overrides.insert("new-tab".to_owned(), parse_sequence("ctrl-k ctrl-t").unwrap());
        let keymap = Keymap::new(&overrides);

        let first = parse_sequence("ctrl-k").unwrap().chords[0];
        assert_eq!(keymap.resolve(&[], first), Matched::Prefix);

        let second = parse_sequence("ctrl-t").unwrap().chords[0];
        assert_eq!(
            keymap.resolve(&[first], second),
            Matched::Action(Action::NewTab)
        );
    }

    #[test]
    fn shifted_punctuation_matches_either_spelling() {
        // The default says ctrl-|; the key event says shift-| because the user
        // held shift to type it. Both have to reach the same action.
        let keymap = Keymap::new(&BTreeMap::new());
        let pressed = parse_sequence("ctrl-shift-|").unwrap().chords[0];
        assert_eq!(
            keymap.resolve(&[], pressed),
            Matched::Action(Action::SplitHorizontal)
        );
        // And the spelling the docs warn about still works.
        let written = parse_sequence("ctrl-shift-\\").unwrap().chords[0];
        assert_eq!(keymap.resolve(&[], written), Matched::Nothing);
    }

    #[test]
    fn a_pending_prefix_flushes_to_the_shorter_binding() {
        let mut overrides = BTreeMap::new();
        overrides.insert(
            "new-tab".to_owned(),
            parse_sequence("ctrl-k ctrl-t").unwrap(),
        );
        // `ctrl-k` is only a prefix, so nothing fires immediately...
        let keymap = Keymap::new(&overrides);
        let first = parse_sequence("ctrl-k").unwrap().chords[0];
        assert_eq!(keymap.resolve(&[], first), Matched::Prefix);
        // ...and a timeout has nothing shorter to fall back to.
        assert_eq!(keymap.flush(&[first]), None);

        // But a binding that is both fires on the timeout.
        overrides.insert(
            "palette".to_owned(),
            parse_sequence("ctrl-k ctrl-p").unwrap(),
        );
        let keymap = Keymap::new(&overrides);
        assert_eq!(keymap.resolve(&[], first), Matched::Prefix);
        assert_eq!(keymap.flush(&[first]), None, "ctrl-k still means nothing");
    }

    #[test]
    fn an_unknown_name_is_reported_not_fatal() {
        let mut overrides = BTreeMap::new();
        overrides.insert("newtab".to_owned(), parse_sequence("ctrl-t").unwrap());
        let unknown = Keymap::unknown_names(&overrides);
        assert_eq!(unknown, vec!["newtab".to_owned()]);
        // And the map still works for everything else.
        let keymap = Keymap::new(&overrides);
        let chord = parse_sequence("ctrl-shift-t").unwrap();
        assert_eq!(
            keymap.resolve(&[], chord.chords[0]),
            Matched::Action(Action::NewTab)
        );
    }
}

#[cfg(test)]
mod tab_through {
    use super::*;
    use dtr_config::keys::parse_sequence;

    /// Plain Tab belongs to the shell: it is completion, and a tab key that
    /// switched tabs would make `cd lo<TAB>` leave the session it was typed in.
    #[test]
    fn a_bare_tab_is_not_next_tab() {
        let overrides = std::collections::BTreeMap::new();
        let keymap = Keymap::new(&overrides);

        let tab = parse_sequence("tab").expect("tab");
        assert_eq!(
            keymap.resolve(&[], tab.chords[0]),
            Matched::Nothing,
            "Tab must reach the shell, not the tab bar"
        );

        let ctrl_tab = parse_sequence("ctrl-tab").expect("ctrl-tab");
        assert_eq!(
            keymap.resolve(&[], ctrl_tab.chords[0]),
            Matched::Action(Action::NextTab),
            "ctrl-tab still switches"
        );
    }
}
