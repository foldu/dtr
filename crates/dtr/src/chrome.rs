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
    let fg = Look::colour(look.foreground);
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

        let hovered = response.hovered() || response.is_pointer_button_down_on();
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

/// The app icon at the far left of a Windows caption.
///
/// It sits where the OS puts it, and it does what the OS does with it: a click
/// opens the window's system menu. GNOME header bars have no app icon, so on
/// anything but Windows this draws nothing and takes no space.
pub fn caption_icon_rect(strip: Rect) -> Option<Rect> {
    (kind() == Kind::Windows).then(|| {
        Rect::from_center_size(
            egui::pos2(strip.left() + 8.0 + 8.0, strip.center().y),
            Vec2::splat(16.0),
        )
    })
}

/// How far the chips start from the left edge.
///
/// Windows reserves the icon's square first; everyone else gives the chips the
/// same 6 points the strip always had.
pub fn chips_left() -> f32 {
    if kind() == Kind::Windows {
        // 8 points in, the 16-point icon, 8 points out.
        32.0
    } else {
        6.0
    }
}

/// Draw the caption icon and report where it was clicked, in points.
pub fn caption_icon(ui: &mut Ui, strip: Rect, look: &Look) -> Option<egui::Pos2> {
    let rect = caption_icon_rect(strip)?;
    let response = ui.interact(rect, Id::unique("dtr-caption-icon"), Sense::click());
    if response.has_focus() {
        response.surrender_focus();
    }
    if response.hovered() {
        let fg = Look::colour(look.foreground);
        ui.painter().rect_filled(
            rect.expand(2.0),
            0.0,
            Color32::from_rgba_unmultiplied(fg.r(), fg.g(), fg.b(), 26),
        );
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
            crate::ui::tab_bar(ui, &labels, 0, &[], &look);
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
            let bar = crate::ui::tab_bar(ui, &labels, 0, &[], &look);
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
