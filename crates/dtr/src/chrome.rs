//! The window frame dtr paints itself.
//!
//! With `[window] chrome = "client"` the tab strip *is* the title bar: the
//! window manager draws nothing, so moving the window, maximising it, and the
//! buttons that minimise and close it are ours. This module owns the geometry
//! and the decisions; `crate::ui::tab_bar` keeps the chips, because a chip is a
//! tab and not chrome.
//!
//! The buttons are drawn the way the platform draws its own — the metrics, the
//! glyph and the hover shape — but coloured from the strip's palette, so a dark
//! theme is never half light-mode grey. The one fixed colour is Windows'
//! close-hover red, because that is the detail the eye checks for.
//!
//! Two rules from elsewhere in this tree apply with force here, because a
//! caption is nothing but hit targets:
//!
//! - **the drag region starts where the chips stop.** egui gives a press to the
//!   topmost widget that senses clicks, so a drag area laid over the strip
//!   steals presses from the chips depending on allocation order — the
//!   "sometimes unresponsive" bug the tab strip already carries a comment
//!   about.
//! - **the drag begins on `drag_started`, not on `clicked`.** Wayland's
//!   `xdg_toplevel.move` takes the serial of the button press, so a drag
//!   requested after the button came up does nothing; and on X11 the request
//!   only works while the button is held.

use egui::{Color32, Id, Rect, Sense, Ui, Vec2};

use crate::ui::Look;

// The maximise button's hit test, the one thing here that has to be Win32:
// Windows 11 offers its snap layouts only for `HTMAXBUTTON`.
#[cfg(target_os = "windows")]
pub mod windows;

/// Windows 11's close-hover fill. Every other colour in the caption is derived
/// from the theme; this one is a platform fact, and the caption looks wrong
/// without it.
const CLOSE_HOVER: Color32 = Color32::from_rgb(0xC4, 0x2B, 0x1C);

/// Which platform's caption to draw.
///
/// A parameter rather than a `cfg!` inside the drawing code so both can be
/// rendered on one machine — which is how the metrics stay honest without a VM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// 46×32 full-height buttons, flat hover, the red close.
    Windows,
    /// 24×24 circles with 6-point gaps, a faint round hover, no red.
    Adwaita,
}

/// The caption of the platform this binary runs on.
pub fn kind() -> Kind {
    if cfg!(target_os = "windows") {
        Kind::Windows
    } else {
        Kind::Adwaita
    }
}

/// libadwaita's client-side title bar, in points.
///
/// The number is in the theme's CSS, and dtr does not parse CSS (`system/linux.rs`
/// says why), so it is measured out of libadwaita 1.9.3 and compiled in, the way
/// the palette's constants are: `window:not(.ssd-frame) > headerbar.titlebar
/// { min-height: 46px }`, and `headerbar { min-height: 47px }` for a header bar
/// that is not in the titlebar position. GNOME's own applications — Nautilus,
/// GNOME Terminal, Settings — are 46 points tall.
pub const ADWAITA_TITLEBAR: f32 = 46.0;

/// How tall a caption that *holds the tabs* is, in points.
///
/// The strip is not a plain title bar: the tabs live in it, so it has to be a
/// *tabbed* caption's height, and 46 points is the same number the GNOME header
/// bar asks for ([`ADWAITA_TITLEBAR`]) — the app's caption is one height on every
/// platform. It is a native height on Windows too: at 96 DPI Explorer's caption
/// row (the one its tabs live in) measures ~48 px and Windows Terminal's tab row
/// is 40 effective pixels (`TerminalWindow.cpp`: `static constexpr auto
/// titlebarHeight = 40`), so 46 sits inside the range a title bar with tabs in it
/// occupies. What it is *not* is a plain `SM_CAPTION` band: that is 31 px at
/// 96 DPI and *shrinks* in points as the display scales (31, 30.4, 30, 29 at
/// 96/120/144/192 DPI), so at 150 % it is a 45-pixel ribbon beside a tabbed
/// caption's 69 — which is exactly the report this answers.
pub const TABBED_TITLEBAR: f32 = 46.0;

/// How tall the title bar is, in points, as the platform would have drawn it.
///
/// With `chrome = "client"` the strip *is* the title bar, so it has to be as tall
/// as the one it replaces. 30 points was a tab row: beside a 46-point GNOME header
/// bar it read as a strip of afterthought, and on Windows it was the tab row's
/// height rather than the caption's.
///
/// - **Windows** asks. `SM_CAPTION + SM_CYSIZEFRAME + SM_CXPADDEDBORDER` at the
///   window's DPI is exactly where a standard window's client area starts, which
///   is the band a client-side caption stands in; `chrome/windows.rs` reads the
///   three metrics, and [`caption_band`] is the arithmetic with the guest's
///   numbers in its test. That band is a *floor*, not the answer: a caption with
///   tabs in it is a tabbed caption, and [`TABBED_TITLEBAR`] says why it is taller
///   than a plain one.
/// - **Adwaita** is [`ADWAITA_TITLEBAR`], measured out of the theme.
pub fn title_height(scale: f32) -> f32 {
    match kind() {
        Kind::Windows => windows_caption_height(caption_height(scale)),
        Kind::Adwaita => ADWAITA_TITLEBAR,
    }
}

/// The Windows strip's height in points: a tabbed caption's, raised to the
/// platform's own band when that is taller.
///
/// `None` is a platform that answered nothing (the offscreen path), and the
/// tabbed caption is dtr's own look rather than a measurement, so it stands on
/// its own — the band can only ever raise the answer, never pull it down. That is
/// the invariant the report needed: **the strip is never shorter than the
/// platform's own caption at any scale**.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn windows_caption_height(platform_band: Option<f32>) -> f32 {
    platform_band.unwrap_or(0.0).max(TABBED_TITLEBAR)
}

#[cfg(target_os = "windows")]
fn caption_height(scale: f32) -> Option<f32> {
    windows::caption_height(scale)
}

#[cfg(not(target_os = "windows"))]
fn caption_height(_scale: f32) -> Option<f32> {
    None
}

/// Where a standard Windows window's client area starts, in points, from the
/// three metrics its caption is made of.
///
/// Measured in the Windows 11 guest with `GetSystemMetricsForDpi`:
///
/// | DPI | `SM_CYCAPTION` | `SM_CYSIZEFRAME` | `SM_CXPADDEDBORDER` | pixels | points |
/// | --- | --- | --- | --- | --- | --- |
/// | 96 (100 %) | 23 | 4 | 4 | 31 | 31 |
/// | 120 (125 %) | 29 | 4 | 5 | 38 | 30.4 |
/// | 144 (150 %) | 34 | 5 | 6 | 45 | 30 |
/// | 192 (200 %) | 45 | 5 | 8 | 58 | 29 |
///
/// A WinForms window in the same guest put its client area 31 pixels below the
/// window's top at 96 DPI — `caption + frame + padded`, not the 32 some sources
/// quote (that count includes the 1-pixel top border, which dtr draws itself, and
/// which is part of the strip's own hairline). `AdjustWindowRectExForDpi` on the
/// guest's own window style gives the same 31/38/45/58, so the sum is not losing
/// a pixel at the fractional scales either; `chrome/windows.rs` asks that API
/// because it is the OS's answer for the style winit kept.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn caption_band(caption: i32, frame: i32, padded: i32, scale: f32) -> Option<f32> {
    if caption <= 0 || frame < 0 || padded < 0 {
        return None;
    }
    band_points(caption + frame + padded, scale)
}

/// A caption band's physical pixels as points on a `scale` display.
///
/// The band is a *measurement in pixels*; everything egui draws is in points and
/// egui applies the factor itself, so this division and egui's multiplication
/// must be the only two places the scale appears. A band of no pixels or a
/// scale of nothing is `None` — a strip of no height is not a fallback.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn band_points(pixels: i32, scale: f32) -> Option<f32> {
    (pixels > 0 && scale > 0.0).then(|| pixels as f32 / scale)
}

/// The physical pixels a strip of `title_height` points reserves above the grid,
/// gap included.
///
/// The points→pixels boundary, crossed exactly once on the reservation side:
/// `fit_panes` sizes the pty in pixels, egui draws the strip in points, and
/// `top_inset` is the difference. `(height + gap) * scale`, never
/// `height * scale * scale` — a second factor is what put the status bar over
/// the bottom row on every display above 1× (`docs/windows.md` §2.2), and the
/// caption is reserved and drawn by the same rule.
pub fn reserved_pixels(title_height: f32, gap: f32, scale: f32) -> f32 {
    (title_height + gap) * scale
}

/// The physical pixels the strip is *drawn* at, from its height in points.
///
/// The other side of the same boundary: egui is handed points and applies the
/// factor itself (`.exact_size(title_height)`), so this is only what the log and
/// the tests compare the reservation against — it is not a second multiplication
/// to draw with.
pub fn strip_pixels(title_height: f32, scale: f32) -> f32 {
    title_height * scale
}

/// A window button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Minimize,
    /// Doubles as restore: the caller knows whether the window is maximised.
    Maximize,
    Close,
}

/// What a press on the empty part of the strip asked the window to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drag {
    /// The window is being dragged by its caption.
    Move,
    /// A double-click on the caption: maximise, or restore.
    ToggleMaximize,
}

/// The caption's own metrics, per platform.
#[derive(Clone, Copy, Debug)]
struct Tokens {
    width: f32,
    height: f32,
    gap: f32,
    glyph: f32,
    /// Space between the last button and the window's edge.
    inset: f32,
    round: bool,
}

fn tokens(kind: Kind, strip_height: f32) -> Tokens {
    match kind {
        Kind::Windows => Tokens {
            // Windows' own caption buttons are 46 wide and the full height of
            // the caption; butting them together is what makes a run of three
            // read as one control instead of three buttons.
            width: 46.0,
            height: strip_height,
            gap: 0.0,
            glyph: 10.0,
            inset: 0.0,
            round: false,
        },
        Kind::Adwaita => Tokens {
            width: 24.0,
            height: 24.0,
            gap: 6.0,
            glyph: 16.0,
            inset: 8.0,
            round: true,
        },
    }
}

/// The three hit rectangles, in drawing order: minimise, maximise, close.
///
/// The order is the same on both platforms — Windows and GNOME both put close at
/// the outer edge — so only the sizes differ.
fn boxes(strip: Rect, kind: Kind) -> [(Hit, Rect); 3] {
    let t = tokens(kind, strip.height());
    let total = t.width * 3.0 + t.gap * 2.0;
    let left = strip.right() - t.inset - total;
    let top = if t.round {
        strip.center().y - t.height / 2.0
    } else {
        strip.top()
    };
    [Hit::Minimize, Hit::Maximize, Hit::Close].map(|hit| {
        let index = match hit {
            Hit::Minimize => 0.0,
            Hit::Maximize => 1.0,
            Hit::Close => 2.0,
        };
        let x = left + index * (t.width + t.gap);
        (
            hit,
            Rect::from_min_size(egui::pos2(x, top), Vec2::new(t.width, t.height)),
        )
    })
}

/// The area the buttons take, so the drag region can stop before it.
pub fn buttons_rect(strip: Rect, kind: Kind) -> Rect {
    let t = tokens(kind, strip.height());
    let total = t.width * 3.0 + t.gap * 2.0;
    Rect::from_min_max(
        egui::pos2(strip.right() - t.inset - total, strip.top()),
        strip.max,
    )
}

/// The maximise button alone, for Windows' hit test to answer `HTMAXBUTTON`.
#[cfg(target_os = "windows")]
pub fn maximize_rect(strip: Rect, kind: Kind) -> Rect {
    boxes(strip, kind)[1].1
}

/// Whether the platform's own hit test is currently over the maximise button.
///
/// Only Windows has one, and the button there is part of the non-client area
/// (that is what offers the snap layouts), so egui cannot report its hover; the
/// hit test in `chrome/windows.rs` is the only witness.
fn platform_maximize_hover() -> bool {
    #[cfg(target_os = "windows")]
    {
        windows::maximize_hovered()
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

/// Draw the window buttons, right-aligned in `strip`, and report a press.
pub fn buttons(
    ui: &mut Ui,
    strip: Rect,
    maximized: bool,
    look: &Look,
    glyphs: Glyphs,
) -> Option<Hit> {
    buttons_for(ui, strip, maximized, look, kind(), glyphs)
}

/// The same, for an explicitly chosen platform and glyph source — what the tests
/// need to render both captions on one machine.
pub fn buttons_for(
    ui: &mut Ui,
    strip: Rect,
    maximized: bool,
    look: &Look,
    kind: Kind,
    glyphs: Glyphs,
) -> Option<Hit> {
    let t = tokens(kind, strip.height());
    let fg = look.skin().palette.text;
    let mut hit = None;

    for (action, rect) in boxes(strip, kind) {
        let name = match action {
            Hit::Minimize => "minimize window",
            Hit::Maximize if maximized => "restore window",
            Hit::Maximize => "maximize window",
            Hit::Close => "close window",
        };
        let response = ui.interact(
            rect,
            Id::unique(("dtr-window-button", name)),
            Sense::click(),
        );
        // Like the chips: a click-sensing widget is activated by Enter once it
        // has focus, and Enter belongs to the shell.
        if response.has_focus() {
            response.surrender_focus();
        }

        let hovered = response.hovered()
            || response.is_pointer_button_down_on()
            || (action == Hit::Maximize && platform_maximize_hover());
        let close = action == Hit::Close;
        let fill = if hovered && close {
            // `#C42B1C`, with the glyph going white so it stays legible on it.
            Some(CLOSE_HOVER)
        } else if hovered {
            Some(Color32::from_rgba_unmultiplied(fg.r(), fg.g(), fg.b(), 26))
        } else {
            None
        };
        if let Some(fill) = fill {
            if t.round {
                ui.painter()
                    .circle_filled(rect.center(), t.width / 2.0, fill);
            } else {
                ui.painter().rect_filled(rect, 0.0, fill);
            }
        }
        let glyph_colour = if hovered && close { Color32::WHITE } else { fg };
        let glyph_box = Rect::from_center_size(rect.center(), Vec2::splat(t.glyph));
        paint_glyph(ui, glyph_box, action, maximized, kind, glyphs, glyph_colour);

        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, name));
        if response.clicked() && ui.input(|i| i.pointer.any_click()) {
            hit = Some(action);
        }
    }
    hit
}

/// Where the caption's glyphs come from.
///
/// Decided once, at startup, by whether the platform's icon font resolved — not
/// per frame, and never by hoping a family exists: egui panics on a missing
/// glyph, which in a caption means the window dies while closing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyphs {
    /// The platform's own icon font (Windows' Segoe Fluent Icons).
    Font,
    /// Drawn in this file: Adwaita's shapes, or Lucide as the last resort.
    Painted,
}

/// Draw one button's symbol inside `box_`.
fn paint_glyph(
    ui: &Ui,
    box_: Rect,
    action: Hit,
    maximized: bool,
    kind: Kind,
    glyphs: Glyphs,
    colour: Color32,
) {
    match (kind, glyphs) {
        (Kind::Windows, Glyphs::Font) => {
            let glyph = match action {
                Hit::Minimize => "\u{E921}",
                Hit::Maximize if maximized => "\u{E923}",
                Hit::Maximize => "\u{E922}",
                Hit::Close => "\u{E8BB}",
            };
            ui.painter().text(
                box_.center(),
                egui::Align2::CENTER_CENTER,
                glyph,
                egui::FontId::new(
                    box_.height(),
                    egui::FontFamily::Name(crate::fonts::ICON_FAMILY.into()),
                ),
                colour,
            );
        }
        // Lucide when the platform's font is not there: the same three shapes,
        // drawn from the SVG corpus the rest of the UI uses.
        (Kind::Windows, Glyphs::Painted) => {
            let source = match action {
                Hit::Minimize => egui::include_image!("../assets/icons/minus.svg"),
                Hit::Maximize if maximized => egui::include_image!("../assets/icons/copy.svg"),
                Hit::Maximize => egui::include_image!("../assets/icons/square.svg"),
                Hit::Close => egui::include_image!("../assets/icons/x.svg"),
            };
            // Painted, not added as a widget: an image on top of the button is a
            // second click target and wins the press half the time.
            egui::Image::new(source)
                .fit_to_exact_size(box_.size())
                .tint(colour)
                .paint_at(ui, box_);
        }
        (Kind::Adwaita, _) => paint_adwaita(ui, box_, action, maximized, colour),
    }
}

/// Adwaita's four symbols, on its own 16-unit grid with a 1.5-point stroke.
///
/// Drawn rather than embedded: Adwaita's symbolic icons are CC-BY-SA/LGPL and
/// dtr is `MIT OR Unlicense`, and the shapes are a line, two squares and a
/// cross — the licence would cost more than the four paths.
fn paint_adwaita(ui: &Ui, box_: Rect, action: Hit, maximized: bool, colour: Color32) {
    let stroke = egui::Stroke::new(1.5, colour);
    let at = |x: f32, y: f32| {
        egui::pos2(
            box_.left() + x / 16.0 * box_.width(),
            box_.top() + y / 16.0 * box_.height(),
        )
    };
    let line = |a: egui::Pos2, b: egui::Pos2| {
        ui.painter().line_segment([a, b], stroke);
        // egui strokes have square caps; at 1.5 points the notch shows, so a dot
        // at each end is Adwaita's round cap.
        for end in [a, b] {
            ui.painter().circle_filled(end, 0.75, colour);
        }
    };
    let square = |from: (f32, f32), to: (f32, f32)| {
        ui.painter().rect_stroke(
            Rect::from_min_max(at(from.0, from.1), at(to.0, to.1)),
            0.0,
            stroke,
            egui::StrokeKind::Inside,
        );
    };
    match action {
        Hit::Minimize => line(at(3.5, 8.0), at(12.5, 8.0)),
        Hit::Maximize if maximized => {
            square((3.5, 6.5), (10.5, 13.0));
            // The window behind it: a corner, not a second full square, which is
            // how Adwaita draws restore.
            line(at(6.0, 3.5), at(12.5, 3.5));
            line(at(12.5, 3.5), at(12.5, 10.0));
        }
        Hit::Maximize => square((3.5, 3.5), (12.5, 12.5)),
        Hit::Close => {
            line(at(4.0, 4.0), at(12.0, 12.0));
            line(at(12.0, 4.0), at(4.0, 12.0));
        }
    }
}

/// The application's mark at the far left of a client-side strip.
///
/// Both platforms put it where their own header bar does: Windows 8 points in,
/// in a 16-point square, where its caption icon goes and where the window's
/// system menu sits; a GNOME header bar beside the first tab, a size up at 20,
/// as the mark rather than a button.
pub fn caption_icon_rect(strip: Rect) -> Option<Rect> {
    let (inset, side) = match kind() {
        Kind::Windows => (8.0, 16.0),
        Kind::Adwaita => (8.0, 20.0),
    };
    Some(Rect::from_center_size(
        egui::pos2(strip.left() + inset + side / 2.0, strip.center().y),
        Vec2::splat(side),
    ))
}

/// How far the chips start from the left edge.
///
/// A kind that shows the mark reserves 8 points either side of it first; the
/// strip's own 6 points are what is left when it does not.
pub fn chips_left() -> f32 {
    match kind() {
        // 8 points in, the icon's square, 8 points out.
        Kind::Windows => 32.0,
        Kind::Adwaita => 36.0,
    }
}

/// Draw the mark, and on Windows report where it was clicked, in points.
pub fn caption_icon(ui: &mut Ui, strip: Rect, look: &Look) -> Option<egui::Pos2> {
    let rect = caption_icon_rect(strip)?;
    if kind() == Kind::Adwaita {
        // A header bar's mark is not a button: it takes no pointer, no focus and
        // no accessible name, and says what the window is the way a title does.
        // The SVG rather than the PNG the Windows caption uses, because 20 points
        // is 40 pixels at 2x and a 32-pixel raster would be soft there — and this
        // is the caption that is measured in points at every scale.
        egui::Image::new(egui::include_image!("../../../assets/logo/dtr-icon.svg"))
            .fit_to_exact_size(rect.size())
            .paint_at(ui, rect);
        return None;
    }
    let response = ui.interact(rect, Id::unique("dtr-caption-icon"), Sense::click());
    if response.has_focus() {
        response.surrender_focus();
    }
    if response.hovered() {
        ui.painter()
            .rect_filled(rect.expand(2.0), 0.0, look.skin().palette.control_hover);
    }
    // The app's own icon, painted untinted: it is artwork, not a mask. The same
    // file the window and the executable carry, so the caption, the taskbar and
    // Explorer never show three different marks.
    egui::Image::new(egui::include_image!("../../../assets/logo/dtr-icon-32.png"))
        .fit_to_exact_size(rect.size())
        .paint_at(ui, rect);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "window menu"));
    (response.clicked() && ui.input(|i| i.pointer.any_click())).then_some(rect.center())
}

/// The part of the strip a press can drag: everything the tab bar and the
/// buttons did not take.
///
/// `None` when the chips filled the row (a narrow window, or many tabs): there
/// is then no caption to grab, which is the same as a native window whose title
/// text reaches the buttons.
pub fn drag_rect(strip: Rect, used: Rect) -> Option<Rect> {
    let left = used.right().max(strip.left());
    let drag = Rect::from_min_max(egui::pos2(left, strip.top()), strip.max);
    (drag.width() >= 1.0 && drag.height() >= 1.0).then_some(drag)
}

/// Width of the border that grabs a resize, in points.
///
/// A frameless window has no window manager to resize it, and the OS's own
/// grip is gone with the frame; this is the whole of what replaces it.
const RESIZE_EDGE: f32 = 4.0;

/// The corner boxes, which are bigger than the edge band so a diagonal drag is
/// easy to hit.
const RESIZE_CORNER: f32 = 10.0;

/// The window's resize borders, as interactive strips on `screen`.
///
/// Drawn by egui rather than by the OS, so this is where a frameless window's
/// resizing comes from on Linux. On Windows the retained frame still hit-tests
/// the outer pixels and takes the press first, which is why this is correct to
/// run there too: it never wins, it just does not have to.
///
/// The cursor is set from the zone under the pointer, because an invisible
/// border with no cursor is a border nobody finds.
pub fn resize_edges(ui: &mut Ui, screen: Rect) -> Option<egui::ResizeDirection> {
    use egui::{CursorIcon, ResizeDirection as Dir};
    let e = RESIZE_EDGE;
    let c = RESIZE_CORNER;
    let (l, t, r, b) = (screen.left(), screen.top(), screen.right(), screen.bottom());
    // Edges first, corners last: a press in a corner is inside both, and the
    // later widget is the one egui gives the press to.
    let zones = [
        (
            Rect::from_min_max(egui::pos2(l + c, t), egui::pos2(r - c, t + e)),
            Dir::North,
            "north",
            CursorIcon::ResizeNorth,
        ),
        (
            Rect::from_min_max(egui::pos2(l + c, b - e), egui::pos2(r - c, b)),
            Dir::South,
            "south",
            CursorIcon::ResizeSouth,
        ),
        (
            Rect::from_min_max(egui::pos2(l, t + c), egui::pos2(l + e, b - c)),
            Dir::West,
            "west",
            CursorIcon::ResizeWest,
        ),
        (
            Rect::from_min_max(egui::pos2(r - e, t + c), egui::pos2(r, b - c)),
            Dir::East,
            "east",
            CursorIcon::ResizeEast,
        ),
        (
            Rect::from_min_max(egui::pos2(l, t), egui::pos2(l + c, t + c)),
            Dir::NorthWest,
            "north-west",
            CursorIcon::ResizeNorthWest,
        ),
        (
            Rect::from_min_max(egui::pos2(r - c, t), egui::pos2(r, t + c)),
            Dir::NorthEast,
            "north-east",
            CursorIcon::ResizeNorthEast,
        ),
        (
            Rect::from_min_max(egui::pos2(l, b - c), egui::pos2(l + c, b)),
            Dir::SouthWest,
            "south-west",
            CursorIcon::ResizeSouthWest,
        ),
        (
            Rect::from_min_max(egui::pos2(r - c, b - c), egui::pos2(r, b)),
            Dir::SouthEast,
            "south-east",
            CursorIcon::ResizeSouthEast,
        ),
    ];
    for (rect, direction, name, cursor) in zones {
        let response = ui.interact(rect, Id::unique(("dtr-resize", name)), Sense::drag());
        if response.hovered() {
            ui.ctx().set_cursor_icon(cursor);
        }
        if response.drag_started() {
            return Some(direction);
        }
    }
    None
}

/// Draw the drag region into `strip` and report what the pointer asked for.
///
/// `used` is what `ui::tab_bar` allocated, so a press that lands on a chip or
/// on `+` never reaches this. `strip` must already exclude the buttons —
/// [`buttons_rect`] is how.
pub fn drag_area(ui: &mut Ui, strip: Rect, used: Rect) -> Option<Drag> {
    let drag = drag_rect(strip, used)?;
    let response = ui.interact(
        drag,
        Id::unique("dtr-caption-drag"),
        Sense::click_and_drag(),
    );
    if response.drag_started() {
        return Some(Drag::Move);
    }
    // The pointer check matters for the same reason it does on a chip: egui
    // turns Enter into a click for a focused click-sensing widget, and a window
    // must not maximise because a key was pressed in the shell.
    if response.double_clicked() && ui.input(|i| i.pointer.any_click()) {
        return Some(Drag::ToggleMaximize);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;

    fn strip() -> Rect {
        Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(800.0, 30.0))
    }

    /// The strip is a title bar, so it is as tall as the platform's own — and a
    /// platform that answers nothing gets the tab row rather than no strip.
    #[test]
    fn the_strip_is_a_title_bar() {
        assert_eq!(title_height(1.0), ADWAITA_TITLEBAR);
        assert!(
            title_height(1.0) > crate::ui::TAB_HEIGHT,
            "46 points against the 30 a tab row had"
        );
        assert_eq!(caption_band(0, 0, 0, 1.0), None, "a metric of nothing");
        assert_eq!(caption_band(23, 4, 4, 0.0), None, "no scale");
        assert_eq!(caption_band(-1, 4, 4, 1.0), None, "a negative caption");
        assert_eq!(caption_band(23, -1, 4, 1.0), None, "a negative frame");
        assert_eq!(caption_band(23, 4, -1, 1.0), None, "a negative padding");
        assert_eq!(band_points(0, 1.0), None, "no pixels");
        assert_eq!(band_points(-31, 1.0), None, "negative pixels");
        assert_eq!(band_points(31, 0.0), None, "no scale");
        assert_eq!(band_points(31, -1.5), None, "a negative scale");
    }

    /// Windows' caption band, for the four DPIs the guest was measured at, and
    /// `AdjustWindowRectExForDpi` — asked with the window's own retained style —
    /// returned the same pixel count at every one of them (Windows 11 build
    /// 26300). So a caption that reads short at 150 % is not a pixel lost to the
    /// per-DPI rounding of three metrics; the band really is 45 px there, and the
    /// comparison target is taller than a plain `SM_CYCAPTION` title bar.
    #[test]
    fn the_windows_caption_is_the_guests_band_at_every_dpi() {
        for (dpi, caption, frame, padded) in [
            (96, 23, 4, 4),
            (120, 29, 4, 5),
            (144, 34, 5, 6),
            (192, 45, 5, 8),
        ] {
            let scale = f64::from(dpi) as f32 / 96.0;
            let pixels = (caption + frame + padded) as f32;
            assert_eq!(
                caption_band(caption, frame, padded, scale),
                Some(pixels / scale),
                "the band at {dpi} DPI"
            );
            // The chip follows the strip: never smaller than the 20-point chip,
            // never taller than Adwaita's own tab.
            let chip = crate::ui::tab_chip_height(pixels / scale);
            assert!(
                (crate::ui::TAB_CHIP_HEIGHT..=26.0).contains(&chip),
                "{dpi} DPI: a chip of {chip} points"
            );
        }
    }

    /// The Windows strip is a *tabbed* caption: 46 points at every scale, the same
    /// height as the GNOME header bar, and the platform's own band only raises it
    /// when a person has made their title bars taller than that — it can never
    /// shorten it.
    #[test]
    fn the_windows_caption_is_a_tabbed_one() {
        assert_eq!(windows_caption_height(None), TABBED_TITLEBAR);
        for (dpi, caption, frame, padded) in [
            (96, 23, 4, 4),
            (120, 29, 4, 5),
            (144, 34, 5, 6),
            (192, 45, 5, 8),
        ] {
            let scale = f64::from(dpi) as f32 / 96.0;
            let band = caption_band(caption, frame, padded, scale).unwrap();
            let strip = windows_caption_height(Some(band));
            assert_eq!(strip, TABBED_TITLEBAR, "at {dpi} DPI");
            assert!(
                strip >= band,
                "the strip is shorter than the native band at {dpi} DPI"
            );
            assert_eq!(
                crate::ui::tab_chip_height(strip),
                26.0,
                "a tabbed caption gets Adwaita's tab, not a pill"
            );
        }
        assert_eq!(
            windows_caption_height(Some(50.0)),
            50.0,
            "a system caption a person made taller wins"
        );
        // At 150 %: 69 pixels over the platform's 45, and 75 reserved with the gap.
        assert_eq!(strip_pixels(windows_caption_height(Some(30.0)), 1.5), 69.0);
        assert_eq!(
            reserved_pixels(windows_caption_height(Some(30.0)), crate::ui::GRID_GAP, 1.5),
            75.0
        );
    }

    /// The strip is reserved in pixels and drawn in points, and each crosses the
    /// boundary once. A second factor on either side is the status bar's
    /// grow-by-scale² bug (`docs/windows.md` §2.2) moved to the caption: at 200 %
    /// a 31-point caption is 31 points tall over 70 pixels reserved, not 124.
    #[test]
    fn the_caption_is_scaled_once_on_each_side_of_the_boundary() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let height = 31.0;
            let drawn = strip_pixels(height, scale);
            let reserved = reserved_pixels(height, crate::ui::GRID_GAP, scale);
            assert!(
                (reserved - drawn - crate::ui::GRID_GAP * scale).abs() < 1e-3,
                "at {scale}: reserved {reserved} is not the strip {drawn} plus the gap"
            );
            assert!(
                drawn <= reserved,
                "at {scale}: the strip is drawn over the grid: {drawn} > {reserved}"
            );
        }
        assert_eq!(strip_pixels(31.0, 2.0), 62.0);
        assert_eq!(
            reserved_pixels(31.0, crate::ui::GRID_GAP, 2.0),
            70.0,
            "a scale applied twice would reserve 124"
        );
        // And at 150 %, the guest's 45-pixel band is 30 points, drawn 45 pixels.
        let band = caption_band(34, 5, 6, 1.5).unwrap();
        assert_eq!(band, 30.0);
        assert_eq!(strip_pixels(band, 1.5), 45.0);
        assert_eq!(
            reserved_pixels(band, crate::ui::GRID_GAP, 1.5),
            51.0,
            "45 px of strip and 6 px of gap"
        );
    }

    /// The Windows buttons are the platform's: 46 points wide and the full height
    /// of the caption band, so at 150 % they draw as 69 × 45 pixels — the
    /// rectangle Explorer's own caption buttons occupy.
    #[test]
    fn windows_caption_buttons_are_46_points_by_the_band() {
        let strip = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(800.0, 30.0));
        for (_, rect) in boxes(strip, Kind::Windows) {
            assert_eq!(rect.width(), 46.0);
            assert_eq!(rect.height(), strip.height());
            assert_eq!(strip_pixels(rect.width(), 1.5), 69.0);
            assert_eq!(strip_pixels(rect.height(), 1.5), 45.0);
        }
    }

    #[test]
    fn the_drag_region_begins_where_the_chips_end() {
        let used = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(240.0, 20.0));
        let drag = drag_rect(strip(), used).expect("there is empty strip beside the chips");
        assert_eq!(drag.left(), 240.0);
        assert_eq!(drag.top(), 0.0);
        assert_eq!(drag.right(), 800.0);
        assert_eq!(drag.bottom(), 30.0);
    }

    #[test]
    fn a_strip_full_of_chips_has_nothing_to_drag() {
        let used = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 20.0));
        assert_eq!(drag_rect(strip(), used), None);
    }

    #[test]
    fn a_used_rect_wider_than_the_strip_does_not_invert_the_region() {
        // Chips overflow a narrow window: `used` can reach past the strip.
        let used = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(900.0, 20.0));
        assert_eq!(drag_rect(strip(), used), None);
    }

    /// The rectangles have to be disjoint as well as right-aligned: overlapping
    /// click targets are the bug this module exists to avoid, and a test of the
    /// union alone would not see it.
    #[test]
    fn both_platforms_stack_three_disjoint_buttons_at_the_right_edge() {
        for kind in [Kind::Windows, Kind::Adwaita] {
            let boxes = boxes(strip(), kind);
            assert_eq!(
                boxes.map(|(hit, _)| hit),
                [Hit::Minimize, Hit::Maximize, Hit::Close]
            );
            for pair in boxes.windows(2) {
                assert!(
                    pair[0].1.right() <= pair[1].1.left(),
                    "{kind:?}: the buttons overlap"
                );
            }
            let all = buttons_rect(strip(), kind);
            assert_eq!(all.right(), strip().right());
            assert_eq!(all.left(), boxes[0].1.left());
            assert!(all.contains_rect(boxes[2].1));
        }
    }

    /// The mark sits between the edge and the chips, not under them: a chip that
    /// starts on top of the icon is "sometimes unresponsive" with a picture on
    /// it, and the two are laid out by two functions that must agree.
    #[test]
    fn the_app_mark_is_clear_of_the_first_chip() {
        let strip = strip();
        let mark = caption_icon_rect(strip).expect("a client frame shows the mark");
        assert!(
            mark.left() >= strip.left() + 6.0,
            "the mark touches the edge: {mark:?}"
        );
        assert!(
            mark.right() + 8.0 <= chips_left(),
            "the chips start on top of the mark: {mark:?} against {}",
            chips_left()
        );
        assert!(
            mark.top() >= strip.top() && mark.bottom() <= strip.bottom(),
            "the mark is outside the strip: {mark:?}"
        );
    }

    #[test]
    fn the_drag_region_ends_where_the_buttons_begin() {
        for kind in [Kind::Windows, Kind::Adwaita] {
            let buttons = buttons_rect(strip(), kind);
            let caption =
                Rect::from_min_max(strip().min, egui::pos2(buttons.left(), strip().bottom()));
            let used = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 20.0));
            let drag = drag_rect(caption, used).expect("space between the chips and the buttons");
            assert!(
                drag.right() <= buttons.left(),
                "{kind:?}: the drag runs under the buttons"
            );
        }
    }

    /// The borders are invisible, so the only proof they exist is that a drag
    /// there asks for a resize — and for the right edge of it.
    #[test]
    fn a_drag_on_a_border_asks_for_that_resize() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let seen: Rc<RefCell<Vec<egui::ResizeDirection>>> = Rc::default();
        let sink = Rc::clone(&seen);
        let rects: Rc<RefCell<Option<Rect>>> = Rc::default();
        let seen_rects = Rc::clone(&rects);
        let mut harness = egui_kittest::Harness::new_ui(move |ui| {
            let screen = ui.max_rect();
            *seen_rects.borrow_mut() = Some(screen);
            if let Some(direction) = resize_edges(ui, screen) {
                sink.borrow_mut().push(direction);
            }
        });
        harness.run();
        let screen = rects.borrow().expect("the window was drawn");

        for (pos, expected) in [
            (
                screen.left_top() + egui::vec2(2.0, 2.0),
                egui::ResizeDirection::NorthWest,
            ),
            (
                screen.right_bottom() - egui::vec2(2.0, 2.0),
                egui::ResizeDirection::SouthEast,
            ),
            (
                egui::pos2(screen.center().x, screen.top() + 2.0),
                egui::ResizeDirection::North,
            ),
            (
                egui::pos2(screen.center().x, screen.bottom() - 2.0),
                egui::ResizeDirection::South,
            ),
            (
                egui::pos2(screen.left() + 2.0, screen.center().y),
                egui::ResizeDirection::West,
            ),
            (
                egui::pos2(screen.right() - 2.0, screen.center().y),
                egui::ResizeDirection::East,
            ),
        ] {
            // Move towards the middle, so the drag never leaves the window.
            let toward = (screen.center() - pos).normalized() * 24.0;
            seen.borrow_mut().clear();
            harness.drag_at(pos);
            harness.run();
            harness.hover_at(pos + toward);
            harness.run();
            assert_eq!(*seen.borrow(), vec![expected], "a drag at {pos:?}");
            harness.drop_at(pos + toward);
            harness.run();
        }
    }

    /// The strip is drawn and a button pressed in one pass, the way the window
    /// does it — every button reports itself, and none of them is a tab chip.
    #[test]
    fn each_button_reports_itself() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let seen: Rc<RefCell<Vec<Hit>>> = Rc::default();
        let sink = Rc::clone(&seen);
        let labels = vec!["demo".to_owned()];
        let look = Look::default();
        let mut harness = egui_kittest::Harness::new_ui(move |ui| {
            let strip = ui.max_rect();
            crate::ui::tab_bar(ui, &labels, 0, &[], &look, crate::ui::TAB_CHIP_HEIGHT, &[]);
            if let Some(hit) = buttons_for(ui, strip, false, &look, Kind::Windows, Glyphs::Painted)
            {
                sink.borrow_mut().push(hit);
            }
        });
        harness.run();

        for (label, expected) in [
            ("minimize window", Hit::Minimize),
            ("maximize window", Hit::Maximize),
            ("close window", Hit::Close),
        ] {
            seen.borrow_mut().clear();
            harness.get_by_label(label).click();
            harness.run();
            assert_eq!(*seen.borrow(), vec![expected], "{label}");
        }
    }

    /// The caption's own mousedown must not reach the grid underneath, and the
    /// drag region must not reach the buttons.
    #[test]
    fn a_drag_moves_the_window_from_the_empty_strip_and_not_from_a_chip() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let seen: Rc<RefCell<Vec<Drag>>> = Rc::default();
        let sink = Rc::clone(&seen);
        let rects: Rc<RefCell<Option<(Rect, Rect)>>> = Rc::default();
        let seen_rects = Rc::clone(&rects);
        let labels = vec!["demo".to_owned()];
        let look = Look::default();
        let mut harness = egui_kittest::Harness::new_ui(move |ui| {
            let strip = ui.max_rect();
            let bar =
                crate::ui::tab_bar(ui, &labels, 0, &[], &look, crate::ui::TAB_CHIP_HEIGHT, &[]);
            *seen_rects.borrow_mut() = Some((strip, bar.used));
            if let Some(drag) = drag_area(ui, strip, bar.used) {
                sink.borrow_mut().push(drag);
            }
        });
        harness.run();

        let (strip, used) = rects.borrow().expect("the strip was drawn");
        assert!(
            used.right() < strip.right(),
            "the test needs empty strip beside the chip"
        );

        // Empty strip, right of the chips: a press and a little movement.
        let x = (used.right() + strip.right()) / 2.0;
        let y = strip.center().y;
        harness.drag_at(egui::pos2(x, y));
        harness.run();
        harness.hover_at(egui::pos2(x + 24.0, y));
        harness.run();
        assert_eq!(*seen.borrow(), vec![Drag::Move]);
        harness.drop_at(egui::pos2(x + 24.0, y));
        harness.run();

        // The chip: the same gesture must leave the window alone.
        seen.borrow_mut().clear();
        let chip = egui::pos2(used.left() + 8.0, y);
        harness.drag_at(chip);
        harness.run();
        harness.hover_at(egui::pos2(chip.x + 24.0, y));
        harness.run();
        assert!(
            seen.borrow().is_empty(),
            "a press on a chip is the chip's, not the caption's: {:?}",
            seen.borrow()
        );
    }
}
