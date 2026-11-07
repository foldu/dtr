//! A close cross: two strokes in a square target.
//!
//! Drawn rather than taken from a font, for the reason the tab strip learned the
//! hard way: egui's faces have no `×`, so a label with one in it panics the
//! harness and draws an empty box in a window. Two strokes are the shape at any
//! size, and they take their colour from the palette.

use egui::{Pos2, Sense, Stroke, Ui};

use crate::skin::Skin;

/// Draw a close cross and report whether the pointer pressed it.
///
/// `side` is the *target*, not the glyph: a 24-point target with a 12-point cross
/// is what the serial sheet has, and the tab strip's chips are smaller. The glyph
/// is half the target, which keeps the press area generous without drawing a
/// cross that fills it.
pub fn cross(ui: &mut Ui, skin: &Skin, label: &str, side: f32) -> bool {
    let palette = skin.palette;
    let style = &skin.style;
    let (rect, response) = ui.allocate_exact_size(egui::Vec2::splat(side), Sense::click());
    let hovered = response.hovered();
    if hovered {
        ui.painter()
            .rect_filled(rect, style.chip_radius, palette.control_hover);
    }
    let arm = side * 0.25;
    let centre = rect.center();
    let stroke = Stroke::new(
        style.border.max(1.3),
        if hovered { palette.text } else { palette.dim },
    );
    for (dx, dy) in [(-1.0, -1.0), (-1.0, 1.0)] {
        ui.painter().line_segment(
            [
                Pos2::new(centre.x + dx * arm, centre.y + dy * arm),
                Pos2::new(centre.x - dx * arm, centre.y - dy * arm),
            ],
            stroke,
        );
    }

    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    if response.has_focus() {
        response.surrender_focus();
    }
    response.clicked() && ui.input(|input| input.pointer.any_click())
}
