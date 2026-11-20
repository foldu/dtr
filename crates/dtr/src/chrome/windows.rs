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

use std::sync::atomic::{AtomicI32, AtomicIsize, AtomicU32, Ordering};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{InvalidateRect, ScreenToClient};
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetSystemMetricsForDpi};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GWL_STYLE, GetWindowLongW, HTMAXBUTTON, IsZoomed, PostMessageW, SC_MAXIMIZE,
    SC_RESTORE, SM_CXPADDEDBORDER, SM_CYCAPTION, SM_CYSIZEFRAME, WM_DWMCOLORIZATIONCOLORCHANGED,
    WM_NCHITTEST, WM_NCLBUTTONDOWN, WM_NCMOUSELEAVE, WM_SETTINGCHANGE, WM_SYSCOMMAND,
    WM_THEMECHANGED,
};

/// `WM_MOUSELEAVE`. windows-sys files it under `UI::Controls`, and this crate
/// pulls in only the hand it needs, so the one constant is spelled here.
const WM_MOUSELEAVE: u32 = 0x02A3;

/// Where a standard window's client area starts, in points, at the DPI a window
/// with `scale` has — i.e. how tall Windows' own caption band is there.
///
/// `GetSystemMetricsForDpi` and not `GetSystemMetrics`: winit's windows are
/// per-monitor DPI aware, so a window dragged to a 125 % monitor has 120-DPI
/// metrics even when the system is 96, and `GetSystemMetrics` would keep
/// answering for the system's monitor — a caption that is the wrong height on
/// exactly the display the window is on.
///
/// The answer is the OS's own, asked through [`client_origin`], because that is
/// the number the window manager's `WM_NCCALCSIZE` calculation starts from for
/// the style winit actually kept. The three metrics are the fallback for a
/// window that is not there yet (the offscreen capture path never has one) and
/// for a call that refuses. On the Windows 11 guest (build 26300) the two agree
/// at 96/120/144/192 DPI — measured, so the sum is not losing a pixel at a
/// fractional scale — which is why this is a robustness change and not what
/// makes a 150 % caption taller.
pub fn caption_height(scale: f32) -> Option<f32> {
    let dpi = (f64::from(scale) * 96.0).round().clamp(48.0, 768.0) as u32;
    // SAFETY: the three indices are Windows' own constants and the call only
    // reads a system metric. An index it does not know returns 0, which
    // `caption_band` refuses rather than drawing a strip of no height.
    let (caption, frame, padded) = unsafe {
        (
            GetSystemMetricsForDpi(SM_CYCAPTION, dpi),
            GetSystemMetricsForDpi(SM_CYSIZEFRAME, dpi),
            GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi),
        )
    };
    let sum = caption + frame + padded;
    let client = client_origin(dpi);
    // Once per display, not once per frame: this is the line that says whether a
    // caption is short because the OS answered short or because we drew it short.
    if LOGGED_DPI.swap(dpi, Ordering::Relaxed) != dpi {
        tracing::debug!(
            dpi,
            caption,
            frame,
            padded,
            sum,
            client_origin = client,
            "caption band"
        );
    }
    super::band_points(client.unwrap_or(sum), scale)
}

/// The DPI whose caption band was last logged, so `-v` gets one line per display.
static LOGGED_DPI: AtomicU32 = AtomicU32::new(0);

/// Where *this* window's client area would start, in physical pixels, for the
/// style winit retained.
///
/// A window with `decorations(false)` keeps `WS_CAPTION | WS_BORDER | WS_SYSMENU
/// | WS_SIZEBOX` and hides the frame in `WM_NCCALCSIZE` (`docs/chrome.md` §2),
/// so the question dtr has to answer — where would the client area start? — is
/// the one `AdjustWindowRectExForDpi` already answers, with the window's own
/// style rather than a generic one this file would have to guess. `None` before
/// the window exists, or if the call refuses; the caller falls back to summing
/// `SM_CYCAPTION`, `SM_CYSIZEFRAME` and `SM_CXPADDEDBORDER`.
fn client_origin(dpi: u32) -> Option<i32> {
    let hwnd = WINDOW.load(Ordering::Relaxed) as HWND;
    if hwnd.is_null() {
        return None;
    }
    // SAFETY: `hwnd` is the live window `install` was handed, on this thread; the
    // style reads and the rect adjustment only read per-window state.
    unsafe {
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // A terminal has no menu bar, so `bmenu` is false.
        if AdjustWindowRectExForDpi(&mut rect, style, 0, ex, dpi) == 0 {
            return None;
        }
        let top = -rect.top;
        (top > 0).then_some(top)
    }
}

/// The window this module was installed on, as an integer: the atomics the window
/// procedure reads cannot carry a raw pointer, and this is only ever used on the
/// thread that created the window.
static WINDOW: AtomicIsize = AtomicIsize::new(0);

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

/// Whether the pointer is over the maximise button, as last reported by the
/// window's own hit test.
///
/// Answering `HTMAXBUTTON` moves the pointer into the non-client area for that
/// rectangle, which means egui never sees a hover there: Windows sends
/// `WM_NCMOUSEMOVE`, not the client message egui-winit translates. The hit test
/// is therefore also the only place that can notice the hover, and this flag is
/// how the drawing code learns what it saw.
static MAXIMIZE_HOVER: AtomicI32 = AtomicI32::new(0);

/// Did the last hit test land on the maximise button?
pub fn maximize_hovered() -> bool {
    MAXIMIZE_HOVER.load(Ordering::Relaxed) != 0
}

/// Record the hover the hit test saw, and ask for a frame when it changed.
///
/// The hit test runs inside the mouse message, not the paint loop, so writing
/// the flag alone would leave the button the old colour until something else
/// happened to redraw the window — which is a hover that arrives a second late
/// or never. Invalidating turns the change into a `WM_PAINT`, which winit
/// delivers as a redraw request.
fn set_maximize_hover(hwnd: HWND, inside: bool) {
    let was = MAXIMIZE_HOVER.swap(i32::from(inside), Ordering::Relaxed) != 0;
    if was == inside {
        return;
    }
    if inside {
        // Without this Windows never sends `WM_NCMOUSELEAVE` for the button,
        // and a pointer that leaves the window straight from it would leave the
        // fill lit. Tracking is per-entry and one-shot, so it is asked for here
        // rather than kept on.
        let mut track = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE | TME_NONCLIENT,
            hwndTrack: hwnd,
            dwHoverTime: 0,
        };
        // SAFETY: `track` is a live local with the layout the API expects.
        unsafe { TrackMouseEvent(&mut track) };
    }
    // SAFETY: `hwnd` is the live window the hit test was called for.
    unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
}

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
    // Kept for the caption band: `AdjustWindowRectExForDpi` needs the style the
    // window really has, and this is the one place that is handed the handle.
    WINDOW.store(hwnd as isize, Ordering::Relaxed);
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
            let inside = point.x >= left && point.x < right && point.y >= top && point.y < bottom;
            // Every mouse move over the window asks this question, so the
            // answer is also the hover state: it goes false again as soon as
            // the pointer leaves the button, without a second message to say
            // so.
            set_maximize_hover(hwnd, inside);
            if inside {
                return HTMAXBUTTON as LRESULT;
            }
        } else {
            set_maximize_hover(hwnd, false);
        }
    }
    if message == WM_MOUSELEAVE || message == WM_NCMOUSELEAVE {
        // Leaving the window is not another hit test, so clear the hover here
        // or a pointer that left over the button leaves it lit.
        set_maximize_hover(hwnd, false);
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
