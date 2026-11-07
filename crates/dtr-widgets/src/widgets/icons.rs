//! The two icons the chrome draws itself: a close cross and a plus.
//!
//! Both are strokes rather than images. That is not minimalism for its own sake:
//! egui's bundled faces have no `×` (a label with one panics the harness and
//! draws an empty box in a window), and an embedded SVG makes every consumer
//! install an image loader and every harness call `install_image_loaders` before
//! a chip can be drawn. Two lines are the shape at any size.
//!
//! The returned [`egui::Response`] has had the synthetic Enter click stripped,
//! so `clicked()` on it can only have come from the pointer. A focused
//! click-sensing widget is otherwise activated by Enter, and Enter belongs to the
//! shell: without this, typing at a prompt switched tabs.

use egui::{Rect, Response, Sense, Stroke, Ui, Vec2};

use crate::skin::Skin;

/// A close cross in a square target, laid out in the caller's flow.
pub fn cross(ui: &mut Ui, skin: &Skin, label: &str, side: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
    paint_cross(ui, skin, rect, &response);
    finish(response, label)
}

/// A close cross in a rect the caller has already reserved — the right-hand end
/// of a tab chip, where the chip and the cross must be *disjoint* targets.
pub fn cross_at(ui: &mut Ui, skin: &Skin, rect: Rect, id: egui::Id, label: &str) -> Response {
    let response = ui.interact(rect, id, Sense::click());
    paint_cross(ui, skin, rect, &response);
    finish(response, label)
}

/// A plus, in a target of the caller's shape: the tab strip's new-tab button is
/// wider than it is tall, and the glyph stays square inside it.
pub fn plus(ui: &mut Ui, skin: &Skin, label: &str, size: Vec2) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let palette = skin.palette;
    let style = &skin.style;
    ui.painter().rect_filled(
        rect,
        style.chip_radius,
        if response.hovered() {
            palette.control_hover
        } else {
            palette.control
        },
    );
    let arm = rect.height() * 0.25;
    let centre = rect.center();
    let stroke = Stroke::new(style.border.max(1.3), palette.text);
    for axis in [Vec2::X, Vec2::Y] {
        ui.painter()
            .line_segment([centre - axis * arm, centre + axis * arm], stroke);
    }
    finish(response, label)
}

fn paint_cross(ui: &Ui, skin: &Skin, rect: Rect, response: &Response) {
    let palette = skin.palette;
    let style = &skin.style;
    let hovered = response.hovered();
    if hovered {
        ui.painter()
            .rect_filled(rect, style.chip_radius, palette.control_hover);
    }
    let arm = rect.width().min(rect.height()) * 0.25;
    let centre = rect.center();
    let stroke = Stroke::new(
        style.border.max(1.3),
        if hovered { palette.text } else { palette.dim },
    );
    for (dx, dy) in [(-1.0, -1.0), (-1.0, 1.0)] {
        ui.painter().line_segment(
            [
                egui::pos2(centre.x + dx * arm, centre.y + dy * arm),
                egui::pos2(centre.x - dx * arm, centre.y - dy * arm),
            ],
            stroke,
        );
    }
}

/// Declare the accessibility node, give the focus back — so the next Enter
/// reaches the shell — and make the response pointer-only.
fn finish(mut response: Response, label: &str) -> Response {
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    if response.has_focus() {
        response.surrender_focus();
    }
    response
        .flags
        .remove(egui::response::Flags::FAKE_PRIMARY_CLICKED);
    response
}
