//! The caption's one Win32 message.
//!
//! Windows 11 shows its snap-layout flyout when the pointer is over a region the
//! window procedure reports as `HTMAXBUTTON` — and only then. winit answers no
//! hit tests (it has no hook for them: `with_msg_hook` sees *posted* messages,
//! and `WM_NCHITTEST` is sent), so the window procedure is subclassed for that
//! single message.
//!
//! Answering `HTMAXBUTTON` also means the OS treats the button as part of the
//! non-client area: clicks there are performed by `DefWindowProc` exactly as
//! they are on a native caption, so dtr does not handle the click, only the hit
//! test. The cost is that egui never sees the pointer over that button, so it
//! does not paint its hover — the flyout is the feedback instead.
//!
//! Every other button stays a client-area widget and behaves like the tabs.

use std::sync::atomic::{AtomicI32, Ordering};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    HTMAXBUTTON, IsZoomed, PostMessageW, SC_MAXIMIZE, SC_RESTORE, WM_DWMCOLORIZATIONCOLORCHANGED,
    WM_NCHITTEST, WM_NCLBUTTONDOWN, WM_SETTINGCHANGE, WM_SYSCOMMAND, WM_THEMECHANGED,
};

/// Identifies dtr's subclass, in case anything else subclasses the window too.
const SUBCLASS: usize = 0x6474_7200;

/// The maximise button's client rectangle, in physical pixels, written by the
/// drawing code every frame and read by the window procedure.
///
/// Atomics rather than a message because the two live on the same thread but the
/// window procedure cannot borrow the renderer; an unset rectangle (all zeroes)
/// is "no button", which is also what a window with no tabs has.
static MAXIMIZE: [AtomicI32; 4] = [
    AtomicI32::new(0),
    AtomicI32::new(0),
    AtomicI32::new(0),
    AtomicI32::new(0),
];

/// Record where the maximise button is, in points, for this frame.
pub fn set_maximize_rect(rect: egui::Rect, scale: f32) {
    MAXIMIZE[0].store((rect.left() * scale).round() as i32, Ordering::Relaxed);
    MAXIMIZE[1].store((rect.top() * scale).round() as i32, Ordering::Relaxed);
    MAXIMIZE[2].store((rect.right() * scale).round() as i32, Ordering::Relaxed);
    MAXIMIZE[3].store((rect.bottom() * scale).round() as i32, Ordering::Relaxed);
}

/// Take over the window procedure for the hit test.
///
/// # Safety
///
/// `hwnd` must be a live top-level window on the calling thread, which is what
/// the caller passes immediately after winit created it.
pub unsafe fn install(hwnd: HWND) {
    // SAFETY: the caller guarantees a live window; the procedure only reads
    // atomics and otherwise defers to the previous one.
    unsafe {
        SetWindowSubclass(hwnd, Some(procedure), SUBCLASS, 0);
    }
}

unsafe extern "system" fn procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if message == WM_NCHITTEST {
        let (left, top, right, bottom) = (
            MAXIMIZE[0].load(Ordering::Relaxed),
            MAXIMIZE[1].load(Ordering::Relaxed),
            MAXIMIZE[2].load(Ordering::Relaxed),
            MAXIMIZE[3].load(Ordering::Relaxed),
        );
        if right > left && bottom > top {
            // The point arrives in screen coordinates; the rectangle is in
            // client pixels.
            let mut point = POINT {
                x: (lparam & 0xffff) as u16 as i16 as i32,
                y: ((lparam >> 16) & 0xffff) as u16 as i16 as i32,
            };
            // SAFETY: `hwnd` is the window this procedure was called for, and
            // `point` is a live local.
            unsafe { ScreenToClient(hwnd, &mut point) };
            if point.x >= left && point.x < right && point.y >= top && point.y < bottom {
                return HTMAXBUTTON as LRESULT;
            }
        }
    }
    if message == WM_NCLBUTTONDOWN && wparam as u32 == HTMAXBUTTON {
        // Answering the hit test with `HTMAXBUTTON` moves the click into the
        // non-client area, and winit's window procedure does not forward a
        // non-client button down to `DefWindowProc` — so the button would be a
        // dead rectangle. Send the command a native maximise button sends.
        //
        // Posted, not sent: this runs inside a hit-test/click sequence and the
        // window must not be maximised under the mouse's own message.
        let command: WPARAM = if unsafe { IsZoomed(hwnd) } != 0 {
            SC_RESTORE as WPARAM
        } else {
            SC_MAXIMIZE as WPARAM
        };
        // SAFETY: `hwnd` is the window this procedure was called for.
        unsafe { PostMessageW(hwnd, WM_SYSCOMMAND, command, 0) };
        return 0;
    }
    if matches!(
        message,
        WM_SETTINGCHANGE | WM_DWMCOLORIZATIONCOLORCHANGED | WM_THEMECHANGED
    ) {
        // The desktop's colours changed — the scheme, the accent, or both — and
        // Windows has three messages for it: `WM_SETTINGCHANGE` (the theme, and on
        // Windows 11 the accent, both as `ImmersiveColorSet`),
        // `WM_DWMCOLORIZATIONCOLORCHANGED` (the accent, classically) and
        // `WM_THEMECHANGED` (a theme file swapped). winit forwards only the first,
        // and only when the light/dark answer differs, so the other two are heard
        // here or not at all. Re-reading two registry values costs less than
        // deciding which message meant what.
        tracing::debug!(message, "the desktop says its colours changed");
        crate::system::changed();
    }
    // SAFETY: passing the message through to the previous procedure, which is
    // what makes this a subclass and not a replacement.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}
