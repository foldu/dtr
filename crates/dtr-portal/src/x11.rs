//! The one thing a portal window needs from X11 that winit does not expose.
//!
//! `parent_window` is an opaque string the frontend sends (`docs/portal.md`
//! §2, and the xdg-desktop-portal interface): `x11:0x1234` on X11 and
//! `wayland:<token>` on Wayland. Making a dialog **transient for** the asking
//! window is what puts it above its parent, keeps it out of the taskbar, and
//! lets the window manager centre it there; `modal` adds the state that stops
//! the parent taking input.
//!
//! winit 0.30 has no API for either. Its X11 extension can only *embed* a
//! window in a parent at creation (`with_embed_parent_window`), which reparents
//! this one into the application's window — a picker clipped to somebody else's
//! client area is worse than one that is merely on top. `WindowExtX11` is
//! otherwise empty and there is no `set_transient_for`. So the two properties
//! are written over the X protocol with [`x11rb`], using the window id winit
//! hands out through `raw-window-handle`:
//!
//! ```text
//! WM_TRANSIENT_FOR   WINDOW    the parent id
//! _NET_WM_STATE      ATOM      [_NET_WM_STATE_MODAL] when modal
//! ```
//!
//! EWMH's rule for a window's initial state is to set the property before the
//! window is mapped, and the picker's window is created hidden and only mapped
//! when a request arrives (`src/window.rs`), so these writes land in the right
//! window of the protocol — and a later request from a different application
//! rewrites them before the window is mapped again.
//!
//! **Wayland is the documented gap.** A `wayland:` handle is not a window id: it
//! is an xdg-foreign token, and winit exposes no way to attach one. There the
//! picker is not transient and the frontend's own "unparented dialog" path
//! applies; that is written down rather than passed off as a flag
//! (`docs/manager.md` §5, `docs/portal.md` §8).

/// The X11 window a portal `parent_window` names, if it names one.
///
/// The portal's format is `x11:` followed by the window id in hexadecimal,
/// conventionally with `0x` (`x11:0x3a00007`); both spellings are accepted
/// because the prefix is the contract and `0x` is a habit. A `wayland:` handle,
/// an empty string and a non-hex id are all `None` — the picker then simply is
/// not transient, which is the honest answer for a handle this process cannot
/// use.
pub fn parent_window(handle: &str) -> Option<u32> {
    let rest = handle.strip_prefix("x11:")?;
    let hex = rest
        .strip_prefix("0x")
        .or_else(|| rest.strip_prefix("0X"))
        .unwrap_or(rest);
    if hex.is_empty() || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(hex, 16).ok().filter(|id| *id != 0)
}

/// Make `window` transient for `parent`, and modal when asked.
///
/// `parent` is the id [`parent_window`] returned. `None` clears the hints: the
/// window is reused for every request (`src/window.rs`), so a request from an
/// application that sent no handle must not inherit the last one's parent.
///
/// Errors are the caller's to log and carry on from: a picker that is not
/// transient still picks files. The common one is a Wayland window, where this
/// is unsupported by construction.
pub fn set_transient_for(
    window: &winit::window::Window,
    parent: Option<u32>,
    modal: bool,
) -> Result<(), String> {
    use winit::raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, PropMode};

    let handle = window
        .window_handle()
        .map_err(|err| format!("this window has no raw handle: {err}"))?;
    let RawWindowHandle::Xlib(xlib) = handle.as_raw() else {
        return Err("not an X11 window: a Wayland parent_window is opaque and \
                    winit exposes no xdg-foreign"
            .to_owned());
    };
    let id = xlib.window as u32;

    let (connection, _screen) =
        x11rb::connect(None).map_err(|err| format!("X11 connect: {err}"))?;
    let intern = |name: &[u8]| -> Result<u32, String> {
        connection
            .intern_atom(false, name)
            .map_err(|err| format!("intern_atom: {err}"))?
            .reply()
            .map(|reply| reply.atom)
            .map_err(|err| format!("intern_atom reply: {err}"))
    };
    let transient = intern(b"WM_TRANSIENT_FOR")?;
    let state = intern(b"_NET_WM_STATE")?;
    let modal_state = intern(b"_NET_WM_STATE_MODAL")?;

    // `format = 32` property data travels as CARD32s in the connection's byte
    // order, which x11rb negotiated as native; that is why these are
    // `to_ne_bytes` and not `to_le_bytes`.
    let write = |atom: u32, kind: u32, values: &[u32]| -> Result<(), String> {
        let data: Vec<u8> = values
            .iter()
            .flat_map(|value| value.to_ne_bytes())
            .collect();
        connection
            .change_property(
                PropMode::REPLACE,
                id,
                atom,
                kind,
                32,
                values.len() as u32,
                &data,
            )
            .map_err(|err| format!("change_property: {err}"))?
            .check()
            .map_err(|err| format!("change_property: {err}"))
    };

    match parent {
        Some(parent) => write(transient, AtomEnum::WINDOW.into(), &[parent])?,
        None => {
            // Absent, not zeroed: `WM_TRANSIENT_FOR` with a 0 window is a broken
            // hint, and a window manager is free to ignore the dialog for it.
            connection
                .delete_property(id, transient)
                .map_err(|err| format!("delete_property: {err}"))?
                .check()
                .map_err(|err| format!("delete_property: {err}"))?;
        }
    }
    let states: &[u32] = if modal { &[modal_state] } else { &[] };
    write(state, AtomEnum::ATOM.into(), states)?;

    tracing::debug!(window = id, ?parent, modal, "set the picker's X11 hints");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parent_window;

    #[test]
    fn a_portal_x11_handle_is_the_window_id_in_hex() {
        assert_eq!(parent_window("x11:0x3a00007"), Some(0x3a0_0007));
        // The prefix is the contract; `0x` is a habit.
        assert_eq!(parent_window("x11:3a00007"), Some(0x3a0_0007));
        assert_eq!(parent_window("x11:0X1"), Some(1));
        // A real toolkit writes uppercase hex as readily as lowercase.
        assert_eq!(parent_window("x11:0xAB"), Some(0xab));
    }

    #[test]
    fn handles_this_process_cannot_use_are_not_parents() {
        // Wayland's handle is an xdg-foreign token, not an id.
        assert_eq!(parent_window("wayland:5c3d2f0e"), None);
        assert_eq!(parent_window(""), None);
        assert_eq!(parent_window("x11:"), None);
        assert_eq!(parent_window("x11:0x"), None);
        // X window 0 is `None`: the root is not a window a dialog can be for.
        assert_eq!(parent_window("x11:0x0"), None);
        // A prefix without the colon, a decimal-looking typo, and junk.
        assert_eq!(parent_window("x110x12"), None);
        assert_eq!(parent_window("x11:12g4"), None);
        assert_eq!(parent_window("x11: 1"), None);
    }
}
