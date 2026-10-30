//! Keyboard input → the byte sequences a terminal sends.
//!
//! M0 speaks legacy VT/xterm: CSI sequences for cursor keys, `ESC O` for the
//! first four function keys, and the usual control characters. The kitty
//! keyboard protocol that libghostty implements is a later milestone — it
//! requires mode negotiation, not another row in a table.

use winit::event::KeyEvent;
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// The terminal state that changes what a key *means*, as opposed to what it
/// types.
///
/// `DECCKM` is the one that matters in practice. ncurses turns it on with
/// `smkx` (`ESC [ ? 1 h`) for every program that calls `keypad()` — htop, less,
/// man, mc, aptitude — and from then on an unmodified arrow key is `ESC O A`,
/// not `ESC [ A`: `kcud1=\EOB` in the terminfo entry, because the terminal is
/// what is supposed to change. A terminal that ignores the mode types arrows the
/// program cannot recognise, which is "the arrow keys don't work in htop".
///
/// The state lives in the pane's terminal, which is why it arrives here as a
/// plain value rather than being asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modes {
    /// `DECCKM`: application cursor keys.
    pub cursor_keys: bool,
}

/// Encode a winit key event for the session, if it produces any bytes.
///
/// Releases are dropped here rather than at the call site: a release event
/// carries the same `logical_key` as its press (only `text` differs, and it
/// falls back to the character), so encoding both sends every keystroke twice.
/// That is exactly what "typing `ls` shows `lsls`" was.
#[allow(dead_code)]
pub fn encode(event: &KeyEvent, mods: ModifiersState, modes: Modes) -> Option<Vec<u8>> {
    if event.state != winit::event::ElementState::Pressed {
        return None;
    }
    encode_with(&event.logical_key, event.text.as_deref(), mods, modes)
}

/// The table's own spelling: a key with no modes set.
///
/// Test-only, because the app always has a terminal to ask — this is what makes
/// the table itself readable in a test without repeating `Modes::default()`
/// fifteen times.
#[cfg(test)]
pub fn encode_key(logical: &Key, text: Option<&str>, mods: ModifiersState) -> Option<Vec<u8>> {
    encode_with(logical, text, mods, Modes::default())
}

/// The testable core: key, resolved text, modifiers, and what the terminal
/// asked for.
pub fn encode_with(
    logical: &Key,
    text: Option<&str>,
    mods: ModifiersState,
    modes: Modes,
) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    // Alt is an ESC prefix in xterm; the modifiers are not stripped from what
    // follows, so Alt+letter arrives as ESC + letter.
    if mods.alt_key() {
        out.push(0x1b);
    }

    match logical {
        Key::Named(named) => out.extend_from_slice(&named_bytes(*named, mods, modes)?),
        Key::Character(chars) => {
            let character = chars.chars().next()?;
            if mods.control_key() {
                out.extend_from_slice(&control_byte(character)?);
            } else {
                // `text` is the shifted, dead-key-resolved form; fall back to
                // the raw character when the platform gives us nothing.
                let text = text.filter(|t| !t.is_empty()).unwrap_or(chars);
                out.extend_from_slice(text.as_bytes());
            }
        }
        Key::Dead(_) | Key::Unidentified(_) => return None,
    }

    Some(out)
}

/// `^A`..`^Z`, plus the punctuation that has a control code.
fn control_byte(character: char) -> Option<Vec<u8>> {
    let upper = character.to_ascii_uppercase();
    let byte = match upper {
        '@' | ' ' => 0x00,
        'A'..='Z' => u8::try_from(upper).ok()? - b'A' + 1,
        '[' => 0x1b,
        '\\' => 0x1c,
        ']' => 0x1d,
        '^' => 0x1e,
        '_' => 0x1f,
        '?' => 0x7f,
        _ => return None,
    };
    Some(vec![byte])
}

/// xterm's modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
fn modifier_code(mods: ModifiersState) -> u8 {
    1 + u8::from(mods.shift_key())
        + 2 * u8::from(mods.alt_key())
        + 4 * u8::from(mods.control_key())
}

/// `CSI <final>` when unmodified, `CSI 1 ; <mods> <final>` otherwise.
fn csi(final_byte: u8, mods: ModifiersState) -> Vec<u8> {
    let m = modifier_code(mods);
    if m == 1 {
        vec![0x1b, b'[', final_byte]
    } else {
        format!("\x1b[1;{m}{}", final_byte as char).into_bytes()
    }
}

/// `CSI <n> ~` when unmodified, `CSI <n> ; <mods> ~` otherwise.
fn csi_tilde(n: u8, mods: ModifiersState) -> Vec<u8> {
    let m = modifier_code(mods);
    if m == 1 {
        format!("\x1b[{n}~").into_bytes()
    } else {
        format!("\x1b[{n};{m}~").into_bytes()
    }
}

/// `SS3 <final>` (F1–F4) when unmodified, `CSI 1 ; <mods> <final>` otherwise.
fn ss3(final_byte: u8, mods: ModifiersState) -> Vec<u8> {
    let m = modifier_code(mods);
    if m == 1 {
        vec![0x1b, b'O', final_byte]
    } else {
        format!("\x1b[1;{m}{}", final_byte as char).into_bytes()
    }
}

/// A cursor key: `SS3 <final>` in application mode, `CSI <final>` otherwise.
///
/// Only the *unmodified* key changes form. A modified one stays
/// `CSI 1 ; <mods> <final>` in both modes — that is what xterm does, and what
/// ncurses' own table expects (`kUP5=\E[1;5A`), so a shift-arrow keeps working
/// in a `keypad()`ed program even while plain arrows have switched.
fn cursor(final_byte: u8, mods: ModifiersState, modes: Modes) -> Vec<u8> {
    if modes.cursor_keys && modifier_code(mods) == 1 {
        vec![0x1b, b'O', final_byte]
    } else {
        csi(final_byte, mods)
    }
}

fn named_bytes(key: NamedKey, mods: ModifiersState, modes: Modes) -> Option<Vec<u8>> {
    Some(match key {
        NamedKey::Enter => b"\r".to_vec(),
        // The terminal sends DEL, not BS; applications delete on DEL.
        NamedKey::Backspace => vec![0x7f],
        NamedKey::Tab if mods.shift_key() => b"\x1b[Z".to_vec(),
        NamedKey::Tab => vec![b'\t'],
        NamedKey::Escape => vec![0x1b],
        NamedKey::Space => vec![b' '],
        NamedKey::ArrowUp => cursor(b'A', mods, modes),
        NamedKey::ArrowDown => cursor(b'B', mods, modes),
        NamedKey::ArrowRight => cursor(b'C', mods, modes),
        NamedKey::ArrowLeft => cursor(b'D', mods, modes),
        NamedKey::Home => cursor(b'H', mods, modes),
        NamedKey::End => cursor(b'F', mods, modes),
        NamedKey::Insert => csi_tilde(2, mods),
        NamedKey::Delete => csi_tilde(3, mods),
        NamedKey::PageUp => csi_tilde(5, mods),
        NamedKey::PageDown => csi_tilde(6, mods),
        NamedKey::F1 => ss3(b'P', mods),
        NamedKey::F2 => ss3(b'Q', mods),
        NamedKey::F3 => ss3(b'R', mods),
        NamedKey::F4 => ss3(b'S', mods),
        NamedKey::F5 => csi_tilde(15, mods),
        NamedKey::F6 => csi_tilde(17, mods),
        NamedKey::F7 => csi_tilde(18, mods),
        NamedKey::F8 => csi_tilde(19, mods),
        NamedKey::F9 => csi_tilde(20, mods),
        NamedKey::F10 => csi_tilde(21, mods),
        NamedKey::F11 => csi_tilde(23, mods),
        NamedKey::F12 => csi_tilde(24, mods),
        _ => return None,
    })
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
    use super::{Modes, encode_key, encode_with, unescape};
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    fn ch(text: &'static str) -> Key {
        Key::Character(text.into())
    }

    fn application() -> Modes {
        Modes {
            cursor_keys: true,
        }
    }

    #[test]
    fn plain_characters_pass_through() {
        assert_eq!(
            encode_key(&ch("a"), Some("a"), ModifiersState::empty()),
            Some(b"a".to_vec())
        );
    }

    #[test]
    fn shifted_characters_use_the_resolved_text() {
        assert_eq!(
            encode_key(&ch("A"), Some("A"), ModifiersState::SHIFT),
            Some(b"A".to_vec())
        );
    }

    #[test]
    fn ctrl_letters_become_control_codes() {
        assert_eq!(
            encode_key(&ch("c"), None, ModifiersState::CONTROL),
            Some(vec![0x03])
        );
        assert_eq!(
            encode_key(&ch("["), None, ModifiersState::CONTROL),
            Some(vec![0x1b])
        );
        assert_eq!(
            encode_key(&ch(" "), None, ModifiersState::CONTROL),
            Some(vec![0x00])
        );
    }

    #[test]
    fn alt_prefixes_escape() {
        assert_eq!(
            encode_key(&ch("b"), Some("b"), ModifiersState::ALT),
            Some(b"\x1bb".to_vec())
        );
    }

    #[test]
    fn enter_backspace_and_tab() {
        let none = ModifiersState::empty();
        assert_eq!(
            encode_key(&Key::Named(NamedKey::Enter), None, none),
            Some(b"\r".to_vec())
        );
        assert_eq!(
            encode_key(&Key::Named(NamedKey::Backspace), None, none),
            Some(vec![0x7f])
        );
        assert_eq!(
            encode_key(&Key::Named(NamedKey::Tab), None, none),
            Some(b"\t".to_vec())
        );
        assert_eq!(
            encode_key(&Key::Named(NamedKey::Tab), None, ModifiersState::SHIFT),
            Some(b"\x1b[Z".to_vec())
        );
    }

    #[test]
    fn arrows_carry_modifiers_as_csi_parameters() {
        let up = Key::Named(NamedKey::ArrowUp);
        assert_eq!(
            encode_key(&up, None, ModifiersState::empty()),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            encode_key(&up, None, ModifiersState::SHIFT),
            Some(b"\x1b[1;2A".to_vec())
        );
        assert_eq!(
            encode_key(
                &Key::Named(NamedKey::ArrowRight),
                None,
                ModifiersState::CONTROL
            ),
            Some(b"\x1b[1;5C".to_vec())
        );
    }

    #[test]
    fn function_keys_use_ss3_and_tilde_forms() {
        assert_eq!(
            encode_key(&Key::Named(NamedKey::F1), None, ModifiersState::empty()),
            Some(b"\x1bOP".to_vec())
        );
        assert_eq!(
            encode_key(&Key::Named(NamedKey::F5), None, ModifiersState::empty()),
            Some(b"\x1b[15~".to_vec())
        );
        assert_eq!(
            encode_key(&Key::Named(NamedKey::Delete), None, ModifiersState::SHIFT),
            Some(b"\x1b[3;2~".to_vec())
        );
    }

    /// `DECCKM` is what ncurses asks for with `smkx`, and what makes an arrow
    /// key `ESC O A`. htop, less, man and mc enable it; a terminal that always
    /// sends `ESC [ A` types arrows they cannot match, which is a dead keyboard.
    #[test]
    fn application_cursor_keys_use_ss3() {
        let none = ModifiersState::empty();
        for (key, plain, app) in [
            (NamedKey::ArrowUp, b"\x1b[A".as_slice(), b"\x1bOA".as_slice()),
            (NamedKey::ArrowDown, b"\x1b[B", b"\x1bOB"),
            (NamedKey::ArrowRight, b"\x1b[C", b"\x1bOC"),
            (NamedKey::ArrowLeft, b"\x1b[D", b"\x1bOD"),
            (NamedKey::Home, b"\x1b[H", b"\x1bOH"),
            (NamedKey::End, b"\x1b[F", b"\x1bOF"),
        ] {
            let key = Key::Named(key);
            assert_eq!(encode_with(&key, None, none, Modes::default()), Some(plain.to_vec()));
            assert_eq!(encode_with(&key, None, none, application()), Some(app.to_vec()));
        }
    }

    /// A *modified* cursor key keeps its CSI form in both modes: that is what
    /// xterm sends, and ncurses' `kUP5=\E[1;5A` is the only spelling it knows,
    /// so switching those to SS3 would break shift-arrow in the very programs
    /// this fixes.
    #[test]
    fn modified_cursor_keys_stay_csi_in_application_mode() {
        assert_eq!(
            encode_with(
                &Key::Named(NamedKey::ArrowUp),
                None,
                ModifiersState::SHIFT,
                application()
            ),
            Some(b"\x1b[1;2A".to_vec())
        );
        assert_eq!(
            encode_with(
                &Key::Named(NamedKey::ArrowLeft),
                None,
                ModifiersState::CONTROL | ModifiersState::ALT,
                application()
            ),
            Some(b"\x1b\x1b[1;7D".to_vec())
        );
    }

    /// The mode is about cursor keys only. Page keys and the function keys have
    /// one form each, and a program that turned `DECCKM` on did not ask for
    /// those to move.
    #[test]
    fn other_keys_ignore_the_cursor_mode() {
        for key in [
            NamedKey::PageUp,
            NamedKey::PageDown,
            NamedKey::Insert,
            NamedKey::Delete,
            NamedKey::F1,
            NamedKey::F5,
            NamedKey::Enter,
            NamedKey::Tab,
            NamedKey::Backspace,
        ] {
            let key = Key::Named(key);
            assert_eq!(
                encode_with(&key, None, ModifiersState::empty(), application()),
                encode_with(&key, None, ModifiersState::empty(), Modes::default()),
                "{key:?} changed with DECCKM"
            );
        }
    }

    #[test]
    fn unescape_handles_the_small_vocabulary() {
        assert_eq!(unescape(r"ls\r"), b"ls\r".to_vec());
        assert_eq!(unescape(r"a\tb\e"), b"a\tb\x1b".to_vec());
        assert_eq!(unescape(r"keep\z"), b"keep\\z".to_vec());
    }
}
