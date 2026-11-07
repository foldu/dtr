//! A button: one word in a box, primary or not.
//!
//! A primary button is the answer the dialog is asking for, so it carries the
//! accent; a secondary one is any other answer and carries the control surface.
//! The difference is not decoration — a sheet with two accent-filled buttons has
//! no primary, which is what the serial sheet had before there was a skin.

use egui::{Sense, Stroke, StrokeKind, Ui, Vec2};

use crate::skin::Skin;

/// A button, before it is drawn.
pub struct Button<'a> {
    label: &'a str,
    primary: bool,
    width: Option<f32>,
    enabled: bool,
}

impl<'a> Button<'a> {
    pub fn new(label: &'a str) -> Self {
        Self {
            label,
            primary: false,
            width: None,
            enabled: true,
        }
    }

    /// The answer the sheet is asking for.
    pub fn primary(self) -> Self {
        Self {
            primary: true,
            ..self
        }
    }

    /// A minimum width, so two buttons in a footer can be the same size. The
    /// label wins if it needs more, because a clipped word is not a button.
    pub fn width(self, width: f32) -> Self {
        Self {
            width: Some(width),
            ..self
        }
    }

    pub fn enabled(self, enabled: bool) -> Self {
        Self { enabled, ..self }
    }

    /// Draw it, and report whether the pointer pressed it.
    pub fn show(self, ui: &mut Ui, skin: &Skin) -> bool {
        let palette = skin.palette;
        let style = &skin.style;
        let (text_colour, fill, border) = if !self.enabled {
            (palette.disabled, palette.well, palette.border)
        } else if self.primary {
            (palette.on_accent, palette.accent, palette.accent)
        } else {
            (palette.text, palette.control, palette.border)
        };

        let galley = ui.painter().layout_no_wrap(
            self.label.to_owned(),
            style.value_font.clone(),
            text_colour,
        );
        let width = self
            .width
            .unwrap_or(0.0)
            .max(galley.size().x + style.padding.x * 2.0);
        let (rect, response) = ui.allocate_exact_size(
            Vec2::new(width, style.row),
            if self.enabled {
                Sense::click()
            } else {
                Sense::hover()
            },
        );

        let hovered = self.enabled && response.hovered();
        let clicked = self.enabled && response.is_pointer_button_down_on();
        let (fill, border) = if !self.enabled {
            (fill, border)
        } else if self.primary {
            match (hovered, clicked) {
                (_, true) => (palette.accent_active, palette.accent_active),
                (true, false) => (palette.accent_hover, palette.accent_hover),
                (false, false) => (fill, border),
            }
        } else {
            match (hovered, clicked) {
                (_, true) => (palette.control_active, palette.border_strong),
                (true, false) => (palette.control_hover, palette.border_strong),
                (false, false) => (fill, border),
            }
        };

        let painter = ui.painter();
        painter.rect_filled(rect, style.control_radius, fill);
        painter.rect_stroke(
            rect,
            style.control_radius,
            Stroke::new(style.border, border),
            StrokeKind::Inside,
        );
        painter.galley(
            egui::pos2(
                rect.center().x - galley.size().x / 2.0,
                rect.center().y - galley.size().y / 2.0,
            ),
            galley,
            text_colour,
        );

        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::Button, self.enabled, self.label)
        });
        if response.has_focus() {
            response.surrender_focus();
        }
        // Enter belongs to the sheet, not to a button that happens to be focused.
        self.enabled && super::pointer_only(response).clicked()
    }
}
