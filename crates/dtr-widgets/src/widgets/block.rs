//! Layout: the label column, a labelled row, and the header that groups a few.
//!
//! These are not widgets in the accessibility sense — none of them senses a
//! click — but they are the reason two sheets look like one program. A row is a
//! label cell of a fixed width followed by a control, and the whole point is
//! that the control's left edge does not move when the word beside it is longer
//! than the last one.

use egui::{Sense, Stroke, Ui};

use crate::{Section, Skin};

/// The height of a section header. One number for both kinds, so changing the
/// style's section behaviour cannot move everything below it.
const SECTION_HEIGHT: f32 = 20.0;

/// The label column: `Style::label_width` wide, one row tall, dim, and cut off
/// rather than wrapped if it is too long for the column.
pub fn label_cell(ui: &mut Ui, skin: &Skin, label: &str) {
    let s = &skin.style;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(s.label_width, s.row), Sense::hover());
    ui.painter().text(
        rect.left_center(),
        egui::Align2::LEFT_CENTER,
        label,
        s.label_font.clone(),
        skin.palette.dim,
    );
}

/// One labelled row, then the gap that follows it.
///
/// The caller adds the control and keeps its value: `row` is layout only, which
/// is what lets a sheet lay out a field it owns and a widget that reports.
pub fn row(ui: &mut Ui, skin: &Skin, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        label_cell(ui, skin, label);
        add(ui);
    });
    ui.add_space(skin.style.gap);
}

/// A group header: dtr rules a dim caption, a system dialog sets it in the body
/// face with nothing after it.
///
/// `Section::Bold` is a colour, not a weight — egui's faces are one weight each,
/// and `RichText::strong` only raises the text colour, which a painted header
/// can do with the `text` slot directly.
pub fn section(ui: &mut Ui, skin: &Skin, title: &str) {
    let p = skin.palette;
    let s = &skin.style;
    let (font, colour) = match s.section {
        Section::Rule => (
            egui::FontId::new(s.label_font.size - 1.0, s.label_font.family.clone()),
            p.dim,
        ),
        // A space above, so a bold header separates from the rows before it
        // without a rule: the whitespace is the separator.
        Section::Bold => {
            ui.add_space(s.gap);
            (s.value_font.clone(), p.text)
        }
    };
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), SECTION_HEIGHT),
        Sense::hover(),
    );
    let galley = ui.painter().layout_no_wrap(title.to_owned(), font, colour);
    let origin = egui::pos2(rect.left(), rect.center().y - galley.size().y / 2.0);
    let text_width = galley.size().x;
    ui.painter().galley(origin, galley, colour);
    if s.section == Section::Rule {
        let from = rect.left() + text_width + 10.0;
        ui.painter().line_segment(
            [egui::pos2(from, rect.center().y), rect.right_center()],
            Stroke::new(s.border, p.divider),
        );
    }
}
