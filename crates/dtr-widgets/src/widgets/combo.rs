//! An editable dropdown: a text field with a chevron that opens its choices.
//!
//! Two controls in the serial sheet are this one: the baud rate (type a rate, or
//! pick a standard one) and the profile (name this connection, or pick a saved
//! name). They were built separately — a bespoke field for the rate and a combo
//! box plus a separate name row for the profile — and the profile pair existed
//! only because a combo box cannot be typed into.
//!
//! The field is the control and the arrow is a second, *disjoint* hit target:
//! egui gives a press to the topmost widget that senses clicks, so an arrow laid
//! over the end of the field would take the presses meant for the text, depending
//! on allocation order.
//!
//! It reports the two ways the text can change differently, because callers act
//! on them differently: a [`Edit::Picked`] choice means "load that profile"; a
//! [`Edit::Typed`] character means "this is the name being written", and a name
//! that happens to equal an existing profile's must not silently load it.

use egui::{Color32, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};

use crate::skin::Skin;
use crate::style::Focus;

/// What the user did to the field this frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    /// A character was typed or deleted: the text is the whole new value, not a
    /// delta, and it may not parse — a half-typed rate must stay visible.
    Typed(String),
    /// One of the choices was clicked.
    Picked(String),
}

/// A text field with a dropdown arrow. See the module docs.
pub struct Editable<'a> {
    id: &'a str,
    text: &'a str,
    label: &'a str,
    choices_label: Option<&'a str>,
    width: Option<f32>,
    valid: bool,
}

impl<'a> Editable<'a> {
    /// `id` must be unique in the surface and stable across frames; `text` is the
    /// value to show; `label` is the field's accessibility name.
    pub fn new(id: &'a str, text: &'a str, label: &'a str) -> Self {
        Self {
            id,
            text,
            label,
            choices_label: None,
            width: None,
            valid: true,
        }
    }

    /// The arrow's accessibility name, when "the same as the field" is wrong — a
    /// test (and a screen reader) has to be able to tell the two apart.
    pub fn choices_label(mut self, label: &'a str) -> Self {
        self.choices_label = Some(label);
        self
    }

    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// False colours the border with the accent, which is how a half-typed rate
    /// says "this will not connect" without replacing what was typed.
    pub fn valid(mut self, valid: bool) -> Self {
        self.valid = valid;
        self
    }

    pub fn show<S: AsRef<str>>(self, ui: &mut Ui, skin: &Skin, choices: &[S]) -> Option<Edit> {
        let p = skin.palette;
        let s = &skin.style;
        let width = self
            .width
            .unwrap_or_else(|| ui.available_width().max(120.0));
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, s.row), Sense::hover());

        // The arrow is its own target, and it starts where the text stops.
        let arrow_width = (s.row * 0.95).min(26.0).max(18.0);
        let arrow_rect =
            Rect::from_min_max(Pos2::new(rect.right() - arrow_width, rect.top()), rect.max);
        let text_rect = Rect::from_min_max(
            Pos2::new(rect.left() + 1.0, rect.top() + 1.0),
            Pos2::new(arrow_rect.left() - 1.0, rect.bottom() - 1.0),
        );

        let hovered = ui.rect_contains_pointer(rect);
        let fill = if hovered { p.control_hover } else { p.control };
        let border = if !self.valid {
            p.accent
        } else if hovered {
            p.border_strong
        } else {
            p.border
        };
        let painter = ui.painter();
        painter.rect_filled(rect, s.control_radius, fill);
        painter.rect_stroke(
            rect,
            s.control_radius,
            Stroke::new(s.border, border),
            StrokeKind::Inside,
        );

        let mut buffer = self.text.to_owned();
        let response = ui.put(
            text_rect.shrink2(Vec2::new(7.0, 2.0)),
            egui::TextEdit::singleline(&mut buffer)
                .id_salt((self.id, "text"))
                .frame(egui::Frame::NONE)
                .margin(egui::Margin::ZERO)
                .font(egui::TextStyle::Body)
                .desired_width((text_rect.width() - 14.0).max(20.0)),
        );
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, self.label));

        // A focused native field shows an accent underline, not a ring; the dtr
        // style says `Focus::None` and paints nothing.
        if s.focus == Focus::Ring && response.has_focus() {
            ui.painter().line_segment(
                [
                    Pos2::new(rect.left() + 1.0, rect.bottom() - 1.0),
                    Pos2::new(rect.right() - 1.0, rect.bottom() - 1.0),
                ],
                Stroke::new(2.0, p.accent),
            );
        }

        let arrow = ui.interact(
            arrow_rect,
            ui.make_persistent_id((self.id, "arrow")),
            Sense::click(),
        );
        let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&arrow));
        let arrow_hovered = arrow.hovered();
        if arrow_hovered || open {
            ui.painter().rect_filled(
                arrow_rect.shrink(2.0),
                s.control_radius.saturating_sub(1),
                p.control_hover,
            );
        }
        let chevron = if arrow_hovered || open { p.text } else { p.dim };
        paint_chevron(ui.painter(), arrow_rect.center(), chevron);
        // The arrow cannot be called the same thing as the field: two nodes with
        // one name cannot be addressed from a test or a screen reader, and the
        // default is where that trap lives.
        let choices_label = match self.choices_label {
            Some(label) => label.to_owned(),
            None => format!("{} choices", self.label),
        };
        arrow.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, &choices_label));
        if arrow.has_focus() {
            arrow.surrender_focus();
        }

        let mut picked = None;
        // The list spans the field, not the arrow: `BOTTOM_END` puts its right
        // edge on the arrow's right (the field's right), and the width makes up
        // the rest. Anchoring to the arrow and forgetting the alignment is how a
        // dropdown ends up hanging off the control's right-hand corner.
        egui::Popup::menu(&arrow)
            .align(egui::RectAlign::BOTTOM_END)
            .width(width.max(120.0))
            .show(|ui| {
                if choices.is_empty() {
                    ui.label(
                        egui::RichText::new("no choices")
                            .font(s.value_font.clone())
                            .color(p.dim),
                    );
                }
                for choice in choices {
                    let label = choice.as_ref();
                    let selected = label == self.text;
                    let text = egui::RichText::new(label).font(s.value_font.clone());
                    if ui.selectable_label(selected, text).clicked() {
                        picked = Some(Edit::Picked(label.to_owned()));
                        ui.close();
                    }
                }
            });

        // Typed first: a pick sets the text on the *next* frame, so the two
        // cannot be the same frame in practice — but a pick is the more specific
        // answer if they ever are.
        if let Some(edit) = picked {
            return Some(edit);
        }
        response.changed().then(|| {
            buffer.retain(|character| !character.is_control());
            Edit::Typed(buffer)
        })
    }
}

/// The arrow: two strokes, so the crate carries no icon set and the shape is the
/// style's (a native chevron and a dtr chevron are the same two lines).
fn paint_chevron(painter: &egui::Painter, centre: Pos2, colour: Color32) {
    let half_width = 4.5;
    let half_height = 2.5;
    let stroke = Stroke::new(1.5, colour);
    painter.line_segment(
        [
            Pos2::new(centre.x - half_width, centre.y - half_height),
            Pos2::new(centre.x, centre.y + half_height),
        ],
        stroke,
    );
    painter.line_segment(
        [
            Pos2::new(centre.x, centre.y + half_height),
            Pos2::new(centre.x + half_width, centre.y - half_height),
        ],
        stroke,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::Palette;
    use egui_kittest::kittest::Queryable;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn skin() -> Skin {
        Skin::dtr(Palette::dtr(
            [0x0b, 0x0f, 0x0e],
            [0xdc, 0xdf, 0xdd],
            [0xff, 0xb0, 0x00],
        ))
    }

    /// Picking a choice is `Picked`, not `Typed`: the difference is the whole
    /// reason the value is an enum — a caller loads a profile on one and edits a
    /// name on the other.
    #[test]
    fn a_choice_is_picked_and_not_typed() {
        let seen: Rc<RefCell<Option<Edit>>> = Rc::new(RefCell::new(None));
        let skin = skin();
        let seen_in = Rc::clone(&seen);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(Vec2::new(320.0, 120.0))
            .build_ui(move |ui| {
                skin.apply(ui.ctx());
                let edit = Editable::new("baud", "9600", "baud value")
                    .choices_label("baud choices")
                    .width(160.0)
                    .show(ui, &skin, &["300", "9600", "115200"]);
                if edit.is_some() {
                    *seen_in.borrow_mut() = edit;
                }
            });
        harness.run();
        harness.get_by_label("baud choices").click();
        harness.run();
        harness.get_by_label("115200").click();
        harness.run();
        assert_eq!(
            seen.borrow().clone(),
            Some(Edit::Picked("115200".to_owned()))
        );
    }

    /// Typing reports the whole new text, and reports it per edit: a half-typed
    /// rate has to stay visible in the field rather than being rejected.
    #[test]
    fn typing_reports_the_whole_text() {
        let seen: Rc<RefCell<Option<Edit>>> = Rc::new(RefCell::new(None));
        let skin = skin();
        let seen_in = Rc::clone(&seen);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(Vec2::new(320.0, 120.0))
            .build_ui(move |ui| {
                skin.apply(ui.ctx());
                let edit = Editable::new("baud", "", "baud value").width(160.0).show(
                    ui,
                    &skin,
                    &["300", "9600", "115200"],
                );
                if edit.is_some() {
                    *seen_in.borrow_mut() = edit;
                }
            });
        harness.run();
        let field = harness.get_by_label("baud value");
        field.focus();
        harness.run();
        harness.get_by_label("baud value").type_text("12");
        harness.run();
        assert_eq!(seen.borrow().clone(), Some(Edit::Typed("12".to_owned())));
    }
}
