//! Key sequences, in the shape Zed's keymap uses.
//!
//! A binding is written as text: `ctrl-shift-t`, `alt-1`, `ctrl-tab`, and a
//! two-chord binding as `ctrl-k ctrl-s`. Modifiers come first, the key last, and
//! the whole thing is case-insensitive. The point of borrowing the format is
//! that it is already documented elsewhere and reads the way people expect.
//!
//! This module is deliberately free of `winit`: the config file is parsed where
//! no window exists, and the app converts a real key event into a [`Chord`] to
//! match against these.

use std::fmt;

/// The modifiers a chord can carry. `super` is the platform key: command on a
/// Mac, Windows on Windows, super otherwise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub sup: bool,
}

impl Mods {
    pub const NONE: Self = Self {
        ctrl: false,
        alt: false,
        shift: false,
        sup: false,
    };
}

/// A key, either a character or one of the keys that has a name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// A character, stored lowercased: `ctrl-shift-t` and `ctrl-shift-T` are the
    /// same binding, because the shift is a modifier here, not a different key.
    Char(char),
    Named(Named),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Named {
    Tab,
    Enter,
    Escape,
    Space,
    Backspace,
    Delete,
    Insert,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Function(u8),
}

impl Named {
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "tab" => Self::Tab,
            "enter" | "return" => Self::Enter,
            "escape" | "esc" => Self::Escape,
            "space" => Self::Space,
            "backspace" => Self::Backspace,
            "delete" | "del" => Self::Delete,
            "insert" | "ins" => Self::Insert,
            "up" => Self::Up,
            "down" => Self::Down,
            "left" => Self::Left,
            "right" => Self::Right,
            "home" => Self::Home,
            "end" => Self::End,
            "pageup" | "pgup" => Self::PageUp,
            "pagedown" | "pgdn" => Self::PageDown,
            other => {
                let digits = other.strip_prefix('f')?;
                let number: u8 = digits.parse().ok()?;
                if !(1..=24).contains(&number) {
                    return None;
                }
                Self::Function(number)
            }
        })
    }
}

impl fmt::Display for Named {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tab => f.write_str("tab"),
            Self::Enter => f.write_str("enter"),
            Self::Escape => f.write_str("escape"),
            Self::Space => f.write_str("space"),
            Self::Backspace => f.write_str("backspace"),
            Self::Delete => f.write_str("delete"),
            Self::Insert => f.write_str("insert"),
            Self::Up => f.write_str("up"),
            Self::Down => f.write_str("down"),
            Self::Left => f.write_str("left"),
            Self::Right => f.write_str("right"),
            Self::Home => f.write_str("home"),
            Self::End => f.write_str("end"),
            Self::PageUp => f.write_str("pageup"),
            Self::PageDown => f.write_str("pagedown"),
            Self::Function(number) => write!(f, "f{number}"),
        }
    }
}

/// One keystroke: modifiers plus a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    pub mods: Mods,
    pub key: Key,
}

/// A binding: one chord, or several typed in order (`ctrl-k ctrl-s`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sequence {
    pub chords: Vec<Chord>,
}

impl Sequence {
    /// A binding that exists only to switch a default off.
    pub fn disabled() -> Self {
        Self { chords: Vec::new() }
    }

    pub fn is_disabled(&self) -> bool {
        self.chords.is_empty()
    }

    pub fn single(chord: Chord) -> Self {
        Self {
            chords: vec![chord],
        }
    }

    /// The chord that starts this sequence, for the prefix state machine.
    pub fn first(&self) -> Option<Chord> {
        self.chords.first().copied()
    }

    pub fn is_prefix_of(&self, other: &Sequence) -> bool {
        other.chords.len() > self.chords.len()
            && other.chords.starts_with(&self.chords[..])
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct ParseError {
    pub text: String,
    pub reason: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.reason, self.text)
    }
}

impl std::error::Error for ParseError {}

fn error(text: &str, reason: impl Into<String>) -> ParseError {
    ParseError {
        text: text.to_owned(),
        reason: reason.into(),
    }
}

/// Parse a full binding: one or more chords separated by spaces.
pub fn parse_sequence(text: &str) -> Result<Sequence, ParseError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(error(text, "empty binding"));
    }
    let mut chords = Vec::new();
    for part in trimmed.split_whitespace() {
        chords.push(parse_chord(part)?);
    }
    Ok(Sequence { chords })
}

/// Parse one keystroke: `ctrl-shift-t`, `ctrl-tab`, `alt-1`, `f5`.
pub fn parse_chord(text: &str) -> Result<Chord, ParseError> {
    let lowered = text.trim().to_ascii_lowercase();
    if lowered.is_empty() {
        return Err(error(text, "empty chord"));
    }

    let mut mods = Mods::NONE;
    let mut segments: Vec<&str> = lowered.split('-').collect();
    // Consume modifiers from the left; whatever is left, joined back up, is the
    // key. Joining matters because `-` is itself a key: `ctrl--` splits into
    // three segments and only the last two are a minus.
    while segments.len() > 1 && !segments[0].is_empty() {
        match segments[0] {
            "ctrl" | "control" => mods.ctrl = true,
            "alt" | "option" | "opt" => mods.alt = true,
            "shift" => mods.shift = true,
            "super" | "cmd" | "command" | "win" | "meta" => mods.sup = true,
            other => {
                let unknown = other.to_owned();
                return Err(error(
                    text,
                    format!("`{unknown}` is not a modifier (ctrl, alt, shift, super)"),
                ));
            }
        }
        segments.remove(0);
    }
    let key_text = segments.join("-");
    if key_text.is_empty() {
        return Err(error(text, "no key after the modifiers"));
    }

    let key = if key_text.chars().count() == 1 {
        Key::Char(key_text.chars().next().unwrap_or(' '))
    } else {
        Key::Named(Named::parse(&key_text).ok_or_else(|| {
            error(
                text,
                format!("`{key_text}` is not a key name (tab, enter, escape, f1…)"),
            )
        })?)
    };

    // The text was lowercased above, so `ctrl-shift-T` and `ctrl-shift-t` are
    // the same binding: shift is a modifier here, not a different key.
    Ok(Chord { mods, key })
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.mods.ctrl {
            f.write_str("ctrl-")?;
        }
        if self.mods.alt {
            f.write_str("alt-")?;
        }
        if self.mods.sup {
            f.write_str("super-")?;
        }
        if self.mods.shift {
            f.write_str("shift-")?;
        }
        match self.key {
            Key::Char(ch) => write!(f, "{ch}"),
            Key::Named(named) => write!(f, "{named}"),
        }
    }
}

impl fmt::Display for Sequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, chord) in self.chords.iter().enumerate() {
            if index > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{chord}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Key {
        fn ch(self) -> char {
            match self {
                Key::Char(ch) => ch,
                Key::Named(named) => panic!("expected a character, got {named}"),
            }
        }
    }

    #[test]
    fn a_simple_chord_parses() {
        let chord = parse_chord("ctrl-shift-t").unwrap();
        assert!(chord.mods.ctrl && chord.mods.shift);
        assert!(!chord.mods.alt && !chord.mods.sup);
        assert_eq!(chord.key.ch(), 't');
    }

    #[test]
    fn modifiers_are_optional_and_order_does_not_matter() {
        assert_eq!(parse_chord("t").unwrap().mods, Mods::NONE);
        assert_eq!(
            parse_chord("shift-ctrl-t").unwrap(),
            parse_chord("ctrl-shift-t").unwrap()
        );
        assert_eq!(
            parse_chord("cmd-t").unwrap(),
            parse_chord("super-t").unwrap()
        );
    }

    #[test]
    fn symbols_survive_including_a_minus() {
        assert_eq!(parse_chord("ctrl-\\").unwrap().key.ch(), '\\');
        assert_eq!(parse_chord("ctrl--").unwrap().key.ch(), '-');
        assert_eq!(parse_chord("ctrl-shift--").unwrap().key.ch(), '-');
        assert_eq!(parse_chord("ctrl-,").unwrap().key.ch(), ',');
    }

    #[test]
    fn named_keys_parse() {
        assert_eq!(parse_chord("ctrl-tab").unwrap().key, Key::Named(Named::Tab));
        assert_eq!(
            parse_chord("shift-pageup").unwrap().key,
            Key::Named(Named::PageUp)
        );
        assert_eq!(parse_chord("f5").unwrap().key, Key::Named(Named::Function(5)));
        assert_eq!(parse_chord("esc").unwrap().key, Key::Named(Named::Escape));
    }

    #[test]
    fn nonsense_names_the_problem() {
        let err = parse_chord("hyper-t").unwrap_err().to_string();
        assert!(err.contains("not a modifier"), "{err}");
        let err = parse_chord("ctrl-nope").unwrap_err().to_string();
        assert!(err.contains("not a key name"), "{err}");
        assert!(parse_chord("").is_err());
    }

    #[test]
    fn a_sequence_is_chords_in_order() {
        let sequence = parse_sequence("ctrl-k ctrl-s").unwrap();
        assert_eq!(sequence.chords.len(), 2);
        assert_eq!(sequence.first().unwrap().key.ch(), 'k');
        assert!(sequence.is_prefix_of(&parse_sequence("ctrl-k ctrl-s ctrl-x").unwrap()));
        assert!(!sequence.is_prefix_of(&parse_sequence("ctrl-k ctrl-s").unwrap()));
    }

    #[test]
    fn display_round_trips() {
        for text in ["ctrl-shift-t", "ctrl-tab", "alt-1", "shift-pageup", "f12"] {
            let sequence = parse_sequence(text).unwrap();
            assert_eq!(sequence.to_string(), text, "{text}");
        }
    }
}
