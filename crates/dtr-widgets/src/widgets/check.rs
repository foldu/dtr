//! A checkbox: one target that is a box and a word, and a tick drawn from two
//! strokes.
//!
//! The tick is drawn rather than borrowed for two reasons that have both bitten
//! this tree already: egui's own faces have no check mark (a `✓` in a label
//! panicked the harness), and an icon dependency would make every consumer
//! install an image loader before a checkbox could be drawn. Two line segments
//! scale with `Style::check_size` and take their colour from the palette, like
//! the tab strip's close cross.

use egui::{Color32, Painter, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};

use crate::skin::Skin;

/// The gap between the box and its word. Fixed rather than a token: dtr's sheet
/// and a system dialog both put the label one short step from the box, and a
/// style that moved it would be a style nobody asked for.
const LABEL_GAP: f32 = 8.0;

/// Draw a checkbox and report the value it should now hold.
///
/// `None` means nobody touched it; `Some(!on)` means the pointer did. Like every
/// painted widget here it declares an accessibility node and refuses the Enter
/// that egui turns into a click.
pub fn check(ui: &mut Ui, skin: &Skin, on: bool, label: &str) -> Option<bool> {
    boxed(ui, skin, on, Some(label), label)
}

/// A checkbox with no visible label: the row it sits in already says what it is,
/// and the name is only for the accessibility node.
///
/// The settings menu is the shape this exists for — a label on the left, the
/// control on the right — and a checkbox that repeated the row's name there
/// would be the second node with that name.
pub fn check_box(ui: &mut Ui, skin: &Skin, on: bool, name: &str) -> Option<bool> {
    boxed(ui, skin, on, None, name)
}

fn boxed(ui: &mut Ui, skin: &Skin, on: bool, visible: Option<&str>, name: &str) -> Option<bool> {
    let palette = skin.palette;
    let style = &skin.style;
    let size = style.check_size;

    let galley = visible.map(|label| {
        ui.painter()
            .layout_no_wrap(label.to_owned(), style.value_font.clone(), palette.text)
    });
    let width = size + galley.as_ref().map_or(0.0, |g| LABEL_GAP + g.size().x);
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(width, style.row.max(size)), Sense::click());
    let box_rect = Rect::from_center_size(
        Pos2::new(rect.left() + size / 2.0, rect.center().y),
        Vec2::splat(size),
    );
    let (fill, border) = match (on, response.hovered()) {
        (true, false) => (palette.accent, palette.accent),
        (true, true) => (palette.accent_hover, palette.accent_hover),
        (false, false) => (palette.control, palette.control_strong),
        (false, true) => (palette.control_hover, palette.border_strong),
    };

    let painter = ui.painter();
    painter.rect_filled(box_rect, style.check_radius, fill);
    painter.rect_stroke(
        box_rect,
        style.check_radius,
        Stroke::new(style.border, border),
        StrokeKind::Inside,
    );
    if on {
        tick(painter, box_rect, palette.on_accent);
    }
    if let Some(galley) = galley {
        painter.galley(
            Pos2::new(
                box_rect.right() + LABEL_GAP,
                rect.center().y - galley.size().y / 2.0,
            ),
            galley,
            palette.text,
        );
    }

    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, on, name));
    if response.has_focus() {
        response.surrender_focus();
    }
    // Enter is the sheet's own key, so only the pointer toggles the box.
    super::pointer_only(response).clicked().then_some(!on)
}

/// Two segments scaled to the box: down from the left shoulder, then up to the
/// right, which is where a tick carries its weight.
pub(super) fn tick(painter: &Painter, box_rect: Rect, colour: Color32) {
    let centre = box_rect.center();
    let r = box_rect.width() / 2.0;
    let stroke = Stroke::new((box_rect.width() * 0.115).max(1.4), colour);
    let shoulder = Pos2::new(centre.x - r * 0.46, centre.y + r * 0.02);
    let corner = Pos2::new(centre.x - r * 0.14, centre.y + r * 0.34);
    let tip = Pos2::new(centre.x + r * 0.48, centre.y - r * 0.38);
    painter.line_segment([shoulder, corner], stroke);
    painter.line_segment([corner, tip], stroke);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::Palette;
    use egui_kittest::kittest::{NodeT, Queryable};
    use std::cell::RefCell;
    use std::rc::Rc;

    fn skin() -> Skin {
        Skin::dtr(Palette::dtr(
            [0x0b, 0x0f, 0x0e],
            [0xdc, 0xdf, 0xdd],
            [0xff, 0xb0, 0x00],
        ))
    }

    /// The whole control is one target, so pressing the word toggles it — a
    /// checkbox you have to aim at to use is a checkbox nobody uses, and the
    /// box's 16 points are smaller than the label beside it.
    #[test]
    fn pressing_the_label_toggles_it() {
        let skin = skin();
        let seen: Rc<RefCell<Vec<bool>>> = Rc::new(RefCell::new(Vec::new()));
        let seen_in = Rc::clone(&seen);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(Vec2::new(240.0, 60.0))
            .build_ui(move |ui| {
                skin.apply(ui.ctx());
                if let Some(value) = check(ui, &skin, false, "auto reconnect") {
                    seen_in.borrow_mut().push(value);
                }
            });
        harness.run();
        harness.get_by_label("auto reconnect").click();
        harness.run();
        assert_eq!(seen.borrow().as_slice(), &[true]);
    }

    /// The node carries the value as well as the name: a screen reader (and the
    /// harness) has to be able to tell a set checkbox from a clear one.
    #[test]
    fn the_box_reports_whether_it_is_set() {
        let skin = skin();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(Vec2::new(240.0, 60.0))
            .build_ui(move |ui| {
                skin.apply(ui.ctx());
                check(ui, &skin, true, "CR to CRLF");
            });
        harness.run();
        let node = harness.get_by_label("CR to CRLF");
        assert_eq!(
            node.accesskit_node().toggled(),
            Some(egui::accesskit::Toggled::True)
        );
    }
}
