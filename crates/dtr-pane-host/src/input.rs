//! The keyboard a file pane speaks.
//!
//! A [`dtr_files::Key`] is what a browser acts on — `Up`, `Enter`, `Rename` — and
//! this is the one table from a window's key event to it. Both frontends import
//! it: the terminal's file pane (`crates/dtr/src/app.rs`) and the portal's
//! picker and manager windows (`crates/dtr-portal/src/window.rs`) each had their
//! own copy, and they had already drifted — the portal's copy never learned the
//! write chords F6 added, so its manager window could not rename or delete from
//! the keyboard while the terminal's could.
//!
//! **The table answers for a [`Scope`], not for a window.** A picker asks a
//! question; its listing is read-only and its footer owns `Enter` and `Escape`,
//! so it must not be offered rename, delete, paste or new-folder — a read-only
//! picker offering rename is the bug this types away. A manager over a backend
//! with no write verbs (`Caps::write == false`) is the same case: the menu has no
//! verbs, so the keyboard has none either.

use dtr_files::Key;
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};

/// What the pane in front of the keyboard is, and what it may do.
///
/// The frontend knows both facts; the table should not have to re-derive them
/// from a window kind, a mode or a browser borrow.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scope {
    /// The pane is a file picker: a question, whose footer owns `Enter` and
    /// `Escape` and whose listing is read-only.
    pub picker: bool,
    /// The pane's backend has real write verbs ([`dtr_vfs::Caps::write`]).
    pub write: bool,
}

impl Scope {
    /// A file manager's pane, writable when its backend says so.
    pub const fn manager(write: bool) -> Self {
        Self {
            picker: false,
            write,
        }
    }

    /// A picker's listing: movement and nothing that changes a file.
    pub const fn picker() -> Self {
        Self {
            picker: true,
            write: false,
        }
    }

    /// The scope a browser is in, from what it already knows about itself.
    pub fn of(browser: &dtr_files::Browser) -> Self {
        Self {
            picker: browser.is_picking(),
            write: browser.caps().write,
        }
    }

    /// Whether the write chords (`F2`, `Delete`, `Ctrl+C`, `Ctrl+V`,
    /// `Ctrl+Shift+N`) are this pane's.
    fn writes(self) -> bool {
        self.write && !self.picker
    }
}

/// The key a file pane makes of one window key event, or `None` when it has
/// nothing to say about it.
///
/// `mods` is the window's current modifier state — winit reports consumed
/// modifiers separately, and this table does not need them: every chord here is
/// a modifier plus a key whose meaning does not depend on the layout.
pub fn pane_key(key: &WinitKey, mods: ModifiersState, scope: Scope) -> Option<Key> {
    let (ctrl, alt, shift, sup) = (
        mods.control_key(),
        mods.alt_key(),
        mods.shift_key(),
        mods.super_key(),
    );
    let writes = scope.writes();

    // A chord with a modifier is the window's or the write verbs', never the
    // movement keys' — except `Alt+Up`, the parent in a grid, where `Left` is a
    // movement. `Ctrl+Alt+Up` moves the *focus* between panes, so it must stay
    // the window's even inside a listing.
    if ctrl || alt || sup {
        if alt && !ctrl && !sup && matches!(key, WinitKey::Named(NamedKey::ArrowUp)) {
            return Some(Key::Parent);
        }
        // The desktop's new-folder chord. Its key event is the *character* `N`
        // because shift is held, which is why the comparison ignores case.
        if writes && ctrl && !alt && !sup && shift {
            if let WinitKey::Character(text) = key
                && text.eq_ignore_ascii_case("n")
            {
                return Some(Key::NewFolder);
            }
        }
        // Vim's yank and put, and the file manager's copy and paste. Without
        // shift, so `Ctrl+Shift+C` stays the terminal's copy selection.
        if writes && ctrl && !alt && !sup && !shift {
            if let WinitKey::Character(text) = key {
                if text.eq_ignore_ascii_case("c") {
                    return Some(Key::Copy);
                }
                if text.eq_ignore_ascii_case("v") {
                    return Some(Key::Paste);
                }
            }
        }
        return None;
    }

    // `Shift+Delete` asks first and destroys for good; plain `Delete` trashes
    // where the machine has a trash and asks where it does not.
    if shift && matches!(key, WinitKey::Named(NamedKey::Delete)) {
        return writes.then_some(Key::DeletePermanently);
    }

    match key {
        WinitKey::Named(NamedKey::F2) => writes.then_some(Key::Rename),
        WinitKey::Named(NamedKey::Delete) => writes.then_some(Key::Delete),
        WinitKey::Named(NamedKey::ArrowUp) => Some(Key::Up),
        WinitKey::Named(NamedKey::ArrowDown) => Some(Key::Down),
        WinitKey::Named(NamedKey::ArrowLeft) => Some(Key::Left),
        WinitKey::Named(NamedKey::ArrowRight) => Some(Key::Right),
        WinitKey::Named(NamedKey::PageUp) => Some(Key::PageUp),
        WinitKey::Named(NamedKey::PageDown) => Some(Key::PageDown),
        WinitKey::Named(NamedKey::Home) => Some(Key::Top),
        WinitKey::Named(NamedKey::End) => Some(Key::Bottom),
        WinitKey::Named(NamedKey::Enter) => Some(Key::Enter),
        // `Backspace` is the parent in both views; `h` is vim's left, which is
        // the parent in a list and a movement in a grid.
        WinitKey::Named(NamedKey::Backspace) => Some(Key::Parent),
        WinitKey::Character(text) => match text.as_str() {
            "h" => Some(Key::Left),
            "j" => Some(Key::Down),
            "k" => Some(Key::Up),
            "l" => Some(Key::Right),
            "r" => Some(Key::Reload),
            "g" => Some(Key::Top),
            "G" => Some(Key::Bottom),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none() -> ModifiersState {
        ModifiersState::empty()
    }

    fn mods(ctrl: bool, alt: bool, shift: bool) -> ModifiersState {
        let mut state = ModifiersState::empty();
        state.set(ModifiersState::CONTROL, ctrl);
        state.set(ModifiersState::ALT, alt);
        state.set(ModifiersState::SHIFT, shift);
        state
    }

    fn manager() -> Scope {
        Scope::manager(true)
    }

    /// A file pane's keys are its own, and the one with a modifier is the one
    /// every file manager has. The traps are the chords around it: `ctrl-alt-up`
    /// moves the *focus* between panes, so stealing it for the parent directory
    /// would make a window command stop working inside a listing.
    #[test]
    fn a_file_pane_gets_two_dimensional_keys_and_alt_up() {
        for (named, wanted) in [
            (NamedKey::ArrowRight, Key::Right),
            (NamedKey::ArrowLeft, Key::Left),
            (NamedKey::ArrowUp, Key::Up),
            (NamedKey::ArrowDown, Key::Down),
            (NamedKey::PageUp, Key::PageUp),
            (NamedKey::PageDown, Key::PageDown),
            (NamedKey::Home, Key::Top),
            (NamedKey::End, Key::Bottom),
            (NamedKey::Backspace, Key::Parent),
        ] {
            assert_eq!(
                pane_key(
                    &WinitKey::Named(named.clone()),
                    none(),
                    Scope::manager(true)
                ),
                Some(wanted),
                "{named:?} is not the key it should be"
            );
        }
        for (text, wanted) in [
            ("h", Key::Left),
            ("j", Key::Down),
            ("k", Key::Up),
            ("l", Key::Right),
        ] {
            assert_eq!(
                pane_key(&WinitKey::Character(text.into()), none(), manager()),
                Some(wanted),
                "{text} is not the key it should be"
            );
        }

        // Alt+Up is the parent; ctrl+alt+Up belongs to the window.
        assert_eq!(
            pane_key(
                &WinitKey::Named(NamedKey::ArrowUp),
                mods(false, true, false),
                manager()
            ),
            Some(Key::Parent)
        );
        assert_eq!(
            pane_key(
                &WinitKey::Named(NamedKey::ArrowUp),
                mods(true, true, false),
                manager()
            ),
            None
        );
        // And no other chord with a modifier is the pane's — except the six the
        // write verbs own. `ctrl-shift-c` in particular is **not** the pane's:
        // it is the terminal's copy selection, and a listing gets `ctrl-c`.
        assert_eq!(
            pane_key(
                &WinitKey::Character("c".into()),
                mods(true, false, false),
                manager()
            ),
            Some(Key::Copy)
        );
        assert_eq!(
            pane_key(
                &WinitKey::Character("C".into()),
                mods(true, false, true),
                manager()
            ),
            None,
            "ctrl-shift-c is the terminal's copy, not the listing's"
        );
    }

    /// The six chords a file pane owns, mapped as a pure function: which chord
    /// opens the field and which trashes a file is the pane's decision, and a
    /// listing is the only thing they mean anything to.
    #[test]
    fn the_file_pane_maps_its_write_chords() {
        let none = mods(false, false, false);
        let ctrl = mods(true, false, false);
        let shift = mods(false, false, true);
        let alt = mods(false, true, false);

        assert_eq!(
            pane_key(&WinitKey::Named(NamedKey::F2), none, manager()),
            Some(Key::Rename)
        );
        assert_eq!(
            pane_key(&WinitKey::Named(NamedKey::Delete), none, manager()),
            Some(Key::Delete)
        );
        assert_eq!(
            pane_key(&WinitKey::Named(NamedKey::Delete), shift, manager()),
            Some(Key::DeletePermanently),
            "shift is the difference between the trash and the delete"
        );
        assert_eq!(
            pane_key(&WinitKey::Named(NamedKey::Delete), ctrl, manager()),
            None,
            "ctrl+delete is not a binding"
        );
        assert_eq!(
            pane_key(
                &WinitKey::Character("n".into()),
                mods(true, false, true),
                manager()
            ),
            Some(Key::NewFolder)
        );
        // Shift makes the event report the character a keyboard produces.
        assert_eq!(
            pane_key(
                &WinitKey::Character("N".into()),
                mods(true, false, true),
                manager()
            ),
            Some(Key::NewFolder)
        );
        assert_eq!(
            pane_key(&WinitKey::Character("c".into()), ctrl, manager()),
            Some(Key::Copy)
        );
        assert_eq!(
            pane_key(&WinitKey::Character("v".into()), ctrl, manager()),
            Some(Key::Paste)
        );
        assert_eq!(
            pane_key(
                &WinitKey::Character("C".into()),
                mods(true, false, true),
                manager()
            ),
            None,
            "ctrl-shift-c stays the terminal's copy selection"
        );
        assert_eq!(
            pane_key(&WinitKey::Named(NamedKey::ArrowUp), alt, manager()),
            Some(Key::Parent),
            "the chord that was already there"
        );
        assert_eq!(
            pane_key(&WinitKey::Character("j".into()), none, manager()),
            Some(Key::Down)
        );
    }

    /// A picker's listing is read-only: movement still works, and every chord
    /// that would rename, delete, copy or paste is not the picker's. This is the
    /// drift the shared table fixes in the other direction — the portal's copy
    /// never learned the chords, and now gets them only where they are true.
    #[test]
    fn a_picker_gets_the_movement_keys_and_no_write_verbs() {
        let picker = Scope::picker();
        assert_eq!(
            pane_key(&WinitKey::Character("j".into()), none(), picker),
            Some(Key::Down)
        );
        assert_eq!(
            pane_key(&WinitKey::Named(NamedKey::F2), none(), picker),
            None
        );
        assert_eq!(
            pane_key(&WinitKey::Named(NamedKey::Delete), none(), picker),
            None
        );
        assert_eq!(
            pane_key(
                &WinitKey::Named(NamedKey::Delete),
                mods(false, false, true),
                picker
            ),
            None,
            "a permanent delete must never be one Enter away in a picker"
        );
        assert_eq!(
            pane_key(
                &WinitKey::Character("c".into()),
                mods(true, false, false),
                picker
            ),
            None
        );
        assert_eq!(
            pane_key(
                &WinitKey::Character("v".into()),
                mods(true, false, false),
                picker
            ),
            None
        );
        assert_eq!(
            pane_key(
                &WinitKey::Character("n".into()),
                mods(true, false, true),
                picker
            ),
            None
        );
    }

    /// A backend with no write verbs is the same case as a picker: the menu has
    /// no verbs, so the keyboard must not offer them either.
    #[test]
    fn a_read_only_pane_gets_no_write_chords() {
        let read_only = Scope::manager(false);
        assert_eq!(
            pane_key(&WinitKey::Named(NamedKey::F2), none(), read_only),
            None
        );
        assert_eq!(
            pane_key(&WinitKey::Named(NamedKey::Delete), none(), read_only),
            None
        );
        assert_eq!(
            pane_key(
                &WinitKey::Character("v".into()),
                mods(true, false, false),
                read_only
            ),
            None
        );
        assert_eq!(
            pane_key(&WinitKey::Character("j".into()), none(), read_only),
            Some(Key::Down),
            "movement is not a write verb"
        );
    }

    /// A manager window gets the chords the terminal's file pane has had since
    /// F6: this is what the portal's own copy was missing.
    #[test]
    fn a_manager_gets_the_write_chords_the_terminal_has() {
        let mut state = ModifiersState::empty();
        state.set(ModifiersState::CONTROL, true);
        state.set(ModifiersState::SHIFT, true);
        assert_eq!(
            pane_key(
                &WinitKey::Character("N".into()),
                state,
                Scope::manager(true)
            ),
            Some(Key::NewFolder)
        );
        assert_eq!(
            pane_key(
                &WinitKey::Named(NamedKey::Delete),
                ModifiersState::SHIFT,
                Scope::manager(true)
            ),
            Some(Key::DeletePermanently)
        );
    }
}
