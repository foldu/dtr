//! Keyboard input → the terminal's own key encoder.
//!
//! This was a table, and the table was wrong in a way a table cannot fix:
//! legacy encoding is not a function of the key and the modifiers alone, but of
//! the *terminal's* modes. `DECCKM` makes an unmodified arrow key `ESC O A`
//! instead of `ESC [ A` — ncurses sends `smkx` for every program that calls
//! `keypad()`, so htop, less, man and mc all ask for it — `DECKPAM` does the same
//! for the keypad, mode 1036 makes alt an `ESC` prefix, and `modifyOtherKeys`
//! changes how ctrl and alt combinations are spelled. On top of that an
//! application can ask for the kitty keyboard protocol (`CSI > 1 u`), which
//! changes the encoding of *every* key and can turn on press and release
//! reporting.
//!
//! All of that is state of the terminal, and the terminal lives in the pane, so
//! the encoding happens there: `libghostty_vt::key::Encoder`, configured from the
//! terminal before each key (`set_options_from_terminal`). This module is only
//! the mapping from winit's vocabulary to the encoder's — which is where the two
//! libraries spell the same key differently.

use libghostty_vt::key::{Action, Event, Key, Mods};
use winit::event::ElementState;
use winit::keyboard::{Key as LogicalKey, KeyCode, ModifiersState, NamedKey};

use crate::keys::KeyStroke;

/// Describe one keystroke to the encoder.
///
/// Everything the encoder needs is here: which key (a *position*, not the
/// character a layout produces), which modifiers, whether it was a press, a
/// release or a repeat, and the text the platform resolved. The rest — the
/// escape sequence, and whether there is one at all — is the encoder's business.
pub fn fill(event: &mut Event<'_>, stroke: &KeyStroke) {
    let key = key_of(stroke);
    event.set_key(key);
    event.set_mods(mods_of(stroke.mods));
    event.set_action(match (stroke.state, stroke.repeat) {
        (ElementState::Released, _) => Action::Release,
        (ElementState::Pressed, true) => Action::Repeat,
        (ElementState::Pressed, false) => Action::Press,
    });
    // dtr has no composition state of its own: a composed character arrives as
    // text, and a composition still in progress produces no key event at all.
    event.set_composing(false);
    // The codepoint of the *key*, which is what a kitty report quotes: ctrl+a is
    // `CSI 97;5u`, not a control byte and not the text.
    if let Some(codepoint) = char_of_key(key) {
        event.set_unshifted_codepoint(codepoint);
    }
    // Text is what a *press* produced, and only when the text is what the key
    // sends. A ctrl or super combination has no text to send — the encoder's
    // `^A` is not the string "a" — and handing it the text is how `ctrl+a`
    // becomes a literal `a`. A *release* carries the same text in winit and
    // never had any of its own.
    let text = match stroke.state {
        ElementState::Pressed if !stroke.mods.control_key() && !stroke.mods.super_key() => {
            stroke.text.clone().filter(|text| !text.is_empty())
        }
        _ => None,
    };
    event.set_utf8(text);
}

/// The modifiers, as the encoder counts them.
///
/// winit does not say which side of the keyboard a modifier is on, so the side
/// bits stay unset; the encoder only uses them for kitty's `REPORT_ALTERNATES`.
pub fn mods_of(mods: ModifiersState) -> Mods {
    let mut out = Mods::empty();
    out.set(Mods::SHIFT, mods.shift_key());
    out.set(Mods::ALT, mods.alt_key());
    out.set(Mods::CTRL, mods.control_key());
    out.set(Mods::SUPER, mods.super_key());
    out
}

/// Which key the stroke was.
///
/// The *physical* key is the right answer, and the only one a terminal should
/// report: a key is a position, and the character it happens to produce is
/// `utf8`'s job. That is what keeps `ctrl+,` working on a layout where the
/// comma needs shift, and what lets the kitty protocol report the codepoint the
/// shell would have seen.
///
/// A scripted stroke has no physical key (`--keys text:;` is a character, not a
/// position), so the logical key is the fallback.
pub fn key_of(stroke: &KeyStroke) -> Key {
    if let Some(code) = stroke.physical {
        let key = key_of_code(code);
        if key != Key::Unidentified {
            return key;
        }
    }
    match &stroke.key {
        LogicalKey::Named(named) => key_of_named(*named),
        LogicalKey::Character(text) => text
            .chars()
            .next()
            .map_or(Key::Unidentified, key_of_char),
        _ => Key::Unidentified,
    }
}

/// The names the two libraries agree on.
///
/// They were both generated from the same list of key names, so most of the
/// mapping is an identity — writing each arm by hand is how the two drift apart.
macro_rules! same_name {
    ($value:expr, $from:ident, $to:ident, [$($name:ident),* $(,)?]) => {
        match $value {
            $($from::$name => $to::$name,)*
            _ => $to::Unidentified,
        }
    };
}

fn key_of_code(code: KeyCode) -> Key {
    match code {
        KeyCode::KeyA => Key::A,
        KeyCode::KeyB => Key::B,
        KeyCode::KeyC => Key::C,
        KeyCode::KeyD => Key::D,
        KeyCode::KeyE => Key::E,
        KeyCode::KeyF => Key::F,
        KeyCode::KeyG => Key::G,
        KeyCode::KeyH => Key::H,
        KeyCode::KeyI => Key::I,
        KeyCode::KeyJ => Key::J,
        KeyCode::KeyK => Key::K,
        KeyCode::KeyL => Key::L,
        KeyCode::KeyM => Key::M,
        KeyCode::KeyN => Key::N,
        KeyCode::KeyO => Key::O,
        KeyCode::KeyP => Key::P,
        KeyCode::KeyQ => Key::Q,
        KeyCode::KeyR => Key::R,
        KeyCode::KeyS => Key::S,
        KeyCode::KeyT => Key::T,
        KeyCode::KeyU => Key::U,
        KeyCode::KeyV => Key::V,
        KeyCode::KeyW => Key::W,
        KeyCode::KeyX => Key::X,
        KeyCode::KeyY => Key::Y,
        KeyCode::KeyZ => Key::Z,
        // winit says Super where the encoder says Meta, for the same key.
        KeyCode::SuperLeft => Key::MetaLeft,
        KeyCode::SuperRight => Key::MetaRight,
        other => same_name!(
            other,
            KeyCode,
            Key,
            [
                Backquote,
                Backslash,
                BracketLeft,
                BracketRight,
                Comma,
                Digit0,
                Digit1,
                Digit2,
                Digit3,
                Digit4,
                Digit5,
                Digit6,
                Digit7,
                Digit8,
                Digit9,
                Equal,
                IntlBackslash,
                IntlRo,
                IntlYen,
                Minus,
                Period,
                Quote,
                Semicolon,
                Slash,
                AltLeft,
                AltRight,
                Backspace,
                CapsLock,
                ContextMenu,
                ControlLeft,
                ControlRight,
                Enter,
                ShiftLeft,
                ShiftRight,
                Space,
                Tab,
                Convert,
                KanaMode,
                NonConvert,
                Delete,
                End,
                Help,
                Home,
                Insert,
                PageDown,
                PageUp,
                ArrowDown,
                ArrowLeft,
                ArrowRight,
                ArrowUp,
                NumLock,
                Numpad0,
                Numpad1,
                Numpad2,
                Numpad3,
                Numpad4,
                Numpad5,
                Numpad6,
                Numpad7,
                Numpad8,
                Numpad9,
                NumpadAdd,
                NumpadBackspace,
                NumpadComma,
                NumpadDecimal,
                NumpadDivide,
                NumpadEnter,
                NumpadEqual,
                NumpadMultiply,
                NumpadParenLeft,
                NumpadParenRight,
                NumpadSubtract,
                Escape,
                F1,
                F2,
                F3,
                F4,
                F5,
                F6,
                F7,
                F8,
                F9,
                F10,
                F11,
                F12,
                F13,
                F14,
                F15,
                F16,
                F17,
                F18,
                F19,
                F20,
                F21,
                F22,
                F23,
                F24,
                Fn,
                FnLock,
                PrintScreen,
                ScrollLock,
                Pause,
                BrowserBack,
                BrowserFavorites,
                BrowserForward,
                BrowserHome,
                BrowserRefresh,
                BrowserSearch,
                BrowserStop,
                Eject,
                LaunchApp1,
                LaunchApp2,
                LaunchMail,
                MediaPlayPause,
                MediaSelect,
                MediaStop,
                MediaTrackNext,
                MediaTrackPrevious,
                Power,
                Sleep,
                AudioVolumeDown,
                AudioVolumeMute,
                AudioVolumeUp,
                WakeUp,
                Copy,
                Cut,
                Paste,
            ]
        ),
    }
}

/// The same keys, when the window only reported a logical key.
fn key_of_named(named: NamedKey) -> Key {
    same_name!(
        named,
        NamedKey,
        Key,
        [
            Backspace,
            CapsLock,
            ContextMenu,
            Enter,
            Space,
            Tab,
            Convert,
            KanaMode,
            NonConvert,
            Delete,
            End,
            Help,
            Home,
            Insert,
            PageDown,
            PageUp,
            ArrowDown,
            ArrowLeft,
            ArrowRight,
            ArrowUp,
            Escape,
            F1,
            F2,
            F3,
            F4,
            F5,
            F6,
            F7,
            F8,
            F9,
            F10,
            F11,
            F12,
            F13,
            F14,
            F15,
            F16,
            F17,
            F18,
            F19,
            F20,
            F21,
            F22,
            F23,
            F24,
            Fn,
            FnLock,
            NumLock,
            PrintScreen,
            ScrollLock,
            Pause,
            BrowserBack,
            BrowserFavorites,
            BrowserForward,
            BrowserHome,
            BrowserRefresh,
            BrowserSearch,
            BrowserStop,
            Eject,
            MediaPlayPause,
            MediaStop,
            MediaTrackNext,
            MediaTrackPrevious,
            Power,
            AudioVolumeDown,
            AudioVolumeMute,
            AudioVolumeUp,
            WakeUp,
            Copy,
            Cut,
            Paste,
        ]
    )
}

/// What a key position spells on a US layout, and back.
///
/// One table for both directions, because the directions must not disagree:
/// `key_of_char(';')` and `char_of_key(Key::Semicolon)` are the same fact, and
/// the kitty protocol quotes the codepoint of the *key*, so a typo in one shows
/// up as a wrong report in the other.
///
/// US-layout on purpose: a position is all a character can imply when no
/// keyboard is behind it (`--keys text:;`), and the encoder has the text for a
/// real one.
const KEY_CHARS: &[(char, Key)] = &[
    ('a', Key::A),
    ('b', Key::B),
    ('c', Key::C),
    ('d', Key::D),
    ('e', Key::E),
    ('f', Key::F),
    ('g', Key::G),
    ('h', Key::H),
    ('i', Key::I),
    ('j', Key::J),
    ('k', Key::K),
    ('l', Key::L),
    ('m', Key::M),
    ('n', Key::N),
    ('o', Key::O),
    ('p', Key::P),
    ('q', Key::Q),
    ('r', Key::R),
    ('s', Key::S),
    ('t', Key::T),
    ('u', Key::U),
    ('v', Key::V),
    ('w', Key::W),
    ('x', Key::X),
    ('y', Key::Y),
    ('z', Key::Z),
    ('0', Key::Digit0),
    ('1', Key::Digit1),
    ('2', Key::Digit2),
    ('3', Key::Digit3),
    ('4', Key::Digit4),
    ('5', Key::Digit5),
    ('6', Key::Digit6),
    ('7', Key::Digit7),
    ('8', Key::Digit8),
    ('9', Key::Digit9),
    ('`', Key::Backquote),
    ('\\', Key::Backslash),
    ('[', Key::BracketLeft),
    (']', Key::BracketRight),
    (',', Key::Comma),
    ('=', Key::Equal),
    ('-', Key::Minus),
    ('.', Key::Period),
    ('\'', Key::Quote),
    (';', Key::Semicolon),
    ('/', Key::Slash),
    (' ', Key::Space),
];

/// A character with no position behind it.
///
/// Anything not in the table is `Unidentified`, and the encoder falls back to
/// the text, which is the correct answer for a layout it cannot know.
fn key_of_char(ch: char) -> Key {
    let ch = ch.to_ascii_lowercase();
    KEY_CHARS
        .iter()
        .find(|(character, _)| *character == ch)
        .map_or(Key::Unidentified, |(_, key)| *key)
}

/// The codepoint a key position stands for, for the encoder's kitty reports.
fn char_of_key(key: Key) -> Option<char> {
    KEY_CHARS
        .iter()
        .find(|(_, position)| *position == key)
        .map(|(character, _)| *character)
}

/// Expand the backslash escapes accepted by `--send`.
///
/// A small vocabulary on purpose: it exists to type into a session from a test
/// or a script, not to be a second shell.
pub fn unescape(input: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            out.extend_from_slice(character.to_string().as_bytes());
            continue;
        }
        match chars.next() {
            Some('n') => out.push(b'\n'),
            Some('r') => out.push(b'\r'),
            Some('t') => out.push(b'\t'),
            Some('e') => out.push(0x1b),
            Some('\\') => out.push(b'\\'),
            Some(other) => {
                // Unknown escape: keep both characters, don't swallow input.
                out.push(b'\\');
                out.extend_from_slice(other.to_string().as_bytes());
            }
            None => out.push(b'\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{key_of, mods_of, unescape};
    use crate::keys::KeyStroke;
    use libghostty_vt::key::{Key, Mods};
    use winit::event::ElementState;
    use winit::keyboard::{Key as LogicalKey, KeyCode, ModifiersState, NamedKey};

    fn stroke(physical: Option<KeyCode>, logical: LogicalKey) -> KeyStroke {
        KeyStroke {
            physical,
            key: logical,
            text: None,
            state: ElementState::Pressed,
            repeat: false,
            mods: ModifiersState::empty(),
        }
    }

    /// The physical key decides, and a letter is a *position*: `Key` names the
    /// key, not the character, which is what the encoder reports to a kitty
    /// application.
    #[test]
    fn a_physical_key_is_its_position() {
        let cases = [
            (KeyCode::KeyA, Key::A),
            (KeyCode::KeyZ, Key::Z),
            (KeyCode::Digit7, Key::Digit7),
            (KeyCode::Semicolon, Key::Semicolon),
            (KeyCode::ArrowUp, Key::ArrowUp),
            (KeyCode::Home, Key::Home),
            (KeyCode::F5, Key::F5),
            (KeyCode::Enter, Key::Enter),
            (KeyCode::Escape, Key::Escape),
            (KeyCode::Numpad5, Key::Numpad5),
            (KeyCode::NumpadEnter, Key::NumpadEnter),
            (KeyCode::SuperLeft, Key::MetaLeft),
        ];
        for (code, expected) in cases {
            // The logical key is deliberately something else: the position wins,
            // so a layout that produces `q` on that key still reports the key.
            let stroke = stroke(Some(code), LogicalKey::Character("q".into()));
            assert_eq!(key_of(&stroke), expected, "{code:?}");
        }
    }

    /// A scripted stroke has no position, so the character has to stand in for
    /// one — with a US layout's answer, because no other is available.
    #[test]
    fn a_character_stands_in_for_the_position() {
        let cases = [
            ('a', Key::A),
            ('A', Key::A),
            (';', Key::Semicolon),
            ('7', Key::Digit7),
        ];
        for (ch, expected) in cases {
            let stroke = stroke(None, LogicalKey::Character(ch.to_string().into()));
            assert_eq!(key_of(&stroke), expected, "{ch:?}");
        }
        // A character no keyboard position implies is not guessed at.
        let stroke = stroke(None, LogicalKey::Character("é".into()));
        assert_eq!(key_of(&stroke), Key::Unidentified);
    }

    /// And a named key with no position still maps, or Escape and the arrows
    /// would vanish from `--keys` scripts.
    #[test]
    fn a_named_key_survives_a_missing_position() {
        for (named, expected) in [
            (NamedKey::ArrowDown, Key::ArrowDown),
            (NamedKey::Escape, Key::Escape),
            (NamedKey::PageUp, Key::PageUp),
            (NamedKey::Delete, Key::Delete),
        ] {
            let stroke = stroke(None, LogicalKey::Named(named));
            assert_eq!(key_of(&stroke), expected, "{named:?}");
        }
    }

    /// winit and the encoder both count modifiers one bit at a time.
    #[test]
    fn modifiers_map_one_to_one() {
        assert_eq!(mods_of(ModifiersState::empty()), Mods::empty());
        assert_eq!(
            mods_of(ModifiersState::CONTROL | ModifiersState::SHIFT),
            Mods::CTRL | Mods::SHIFT
        );
        assert_eq!(
            mods_of(ModifiersState::ALT | ModifiersState::SUPER),
            Mods::ALT | Mods::SUPER
        );
    }

    #[test]
    fn unescape_handles_the_small_vocabulary() {
        assert_eq!(unescape(r"ls\r"), b"ls\r".to_vec());
        assert_eq!(unescape(r"a\tb\e"), b"a\tb\x1b".to_vec());
        assert_eq!(unescape(r"keep\z"), b"keep\\z".to_vec());
    }
}
