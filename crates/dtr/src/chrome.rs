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

/// Draw the window buttons, right-aligned in `strip`, and report a press.
pub fn buttons(ui: &mut Ui, strip: Rect, maximized: bool, look: &Look) -> Option<Hit> {
    buttons_for(ui, strip, maximized, look, kind())
}

/// The same, for an explicitly chosen platform — what the tests and a future
/// "draw the other platform's caption" option need.
pub fn buttons_for(
    ui: &mut Ui,
    strip: Rect,
    maximized: bool,
    look: &Look,
    kind: Kind,
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
        let glyph = if hovered && close { Color32::WHITE } else { fg };
        let source = match action {
            Hit::Minimize => egui::include_image!("../assets/icons/minus.svg"),
            Hit::Maximize if maximized => egui::include_image!("../assets/icons/copy.svg"),
            Hit::Maximize => egui::include_image!("../assets/icons/square.svg"),
            Hit::Close => egui::include_image!("../assets/icons/x.svg"),
        };
        let box_ = Rect::from_center_size(rect.center(), Vec2::splat(t.glyph));
        // Painted, not added as a widget: an image on top of the button is a
        // second click target and wins the press half the time.
        egui::Image::new(source)
            .fit_to_exact_size(box_.size())
            .tint(glyph)
            .paint_at(ui, box_);

        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, name));
        if response.clicked() && ui.input(|i| i.pointer.any_click()) {
            hit = Some(action);
        }
    }
    hit
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
            if let Some(hit) = buttons_for(ui, strip, false, &look, Kind::Windows) {
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
