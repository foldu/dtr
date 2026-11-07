//! A segmented choice: one chip per option, the chosen one filled with the
//! accent.
//!
//! The chips are painted rather than built from `Button`s so that "chosen" can be
//! a fill *and* the whole row can share one gap, instead of inheriting whatever a
//! widget theme thinks a pressed button looks like. A caller passes the group's
//! name for the accessibility nodes: the label alone (`7`) is not unique in a
//! sheet that also has a stop bit of `7`.

use egui::{Sense, Stroke, StrokeKind, Ui, Vec2};

use crate::skin::Skin;

/// Draw the options and report the one the pointer chose.
pub fn segmented<T: Copy + PartialEq>(
    ui: &mut Ui,
    skin: &Skin,
    group: &str,
    value: T,
    options: &[(T, &str)],
) -> Option<T> {
    let palette = skin.palette;
    let style = &skin.style;
    let mut picked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (option, label) in options {
            let selected = *option == value;
            let galley = ui.painter().layout_no_wrap(
                (*label).to_owned(),
                style.value_font.clone(),
                palette.text,
            );
            let (rect, response) = ui.allocate_exact_size(
                Vec2::new(galley.size().x + style.padding.x * 2.0, style.row),
                Sense::click(),
            );
            let hovered = response.hovered();
            let (fill, border, colour) = match (selected, hovered) {
                (true, _) => (palette.accent, palette.accent, palette.on_accent),
                (false, true) => (palette.control_hover, palette.border_strong, palette.text),
                (false, false) => (palette.control, palette.control, palette.dim),
            };

            let painter = ui.painter();
            painter.rect_filled(rect, style.chip_radius, fill);
            painter.rect_stroke(
                rect,
                style.chip_radius,
                Stroke::new(style.border, border),
                StrokeKind::Inside,
            );
            painter.galley(
                egui::pos2(
                    rect.center().x - galley.size().x / 2.0,
                    rect.center().y - galley.size().y / 2.0,
                ),
                galley,
                colour,
            );

            let name = format!("{group} {label}");
            response.widget_info(|| {
                egui::WidgetInfo::selected(egui::Role::RadioButton, true, selected, &name)
            });
            if response.has_focus() {
                response.surrender_focus();
            }
            // Enter answers nothing here: the sheet's keys are the sheet's.
            if super::pointer_only(response).clicked() {
                picked = Some(*option);
            }
        }
    });
    picked
}
