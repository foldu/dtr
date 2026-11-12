//! A row in a menu: a full-width target, an optional chord at its right, and a
//! tick on the entry that is already the value.
//!
//! egui's own menu rows are `SelectableLabel`s, which paint the *text selection*
//! colour — the same blue a dragged-over word gets, in a list where nothing is
//! being selected. A menu row is a control: the pointer's row is `control_hover`,
//! and the row a value came from is marked with a tick rather than a fill,
//! because a menu where the chosen entry looks like a pressed button is louder
//! than the choice it is trying to show.

use egui::{Sense, Ui};

use crate::skin::Skin;

use super::check::tick;

/// Draw one menu row and report whether the pointer chose it.
pub fn menu_row(ui: &mut Ui, skin: &Skin, label: &str, selected: bool) -> bool {
    row(ui, skin, label, selected, None)
}

/// The same row with the chord that does it without the menu, at its right.
///
/// libadwaita's `popover.menu accelerator` is the item's own font at
/// `--dim-opacity`, and that is why the chord is dim and right-aligned rather
/// than a second accent: it is the same sentence as the label, said more
/// quietly.
pub fn menu_row_chord(ui: &mut Ui, skin: &Skin, label: &str, chord: &str) -> bool {
    row(ui, skin, label, false, Some(chord))
}

fn row(ui: &mut Ui, skin: &Skin, label: &str, selected: bool, chord: Option<&str>) -> bool {
    let palette = skin.palette;
    let style = &skin.style;
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_owned(), style.value_font.clone(), palette.text);
    let chord = chord.map(|chord| {
        ui.painter()
            .layout_no_wrap(chord.to_owned(), style.label_font.clone(), palette.dim)
    });
    // What the row needs before the menu gives it a width: the label, the chord,
    // and the mark a chosen value carries.
    let tick_side = (style.row * 0.5).min(14.0);
    let mark = if selected { tick_side + style.gap } else { 0.0 };
    let mut needed = galley.size().x + style.padding.x * 2.0 + mark;
    if let Some(chord) = &chord {
        needed += chord.size().x + style.gap * 2.0;
    }
    let width = ui.available_width().max(needed);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, style.row), Sense::click());

    let painter = ui.painter();
    // Only the pointer's row is filled: a menu's own background is the frame's,
    // and painting every row with the card colour bands the list instead.
    if response.hovered() {
        painter.rect_filled(rect, style.control_radius, palette.control_hover);
    }
    // The label starts at the row's own padding. It used to start a column
    // further in, a tick gutter reserved on every row of every menu for a mark
    // that the app menu and the new-tab menu never draw — which is what made a
    // short menu read as a column of indented words.
    painter.galley(
        egui::pos2(
            rect.left() + style.padding.x,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        palette.text,
    );
    if let Some(chord) = chord {
        painter.galley(
            egui::pos2(
                rect.right() - style.padding.x - mark - chord.size().x,
                rect.center().y - chord.size().y / 2.0,
            ),
            chord,
            palette.dim,
        );
    }
    if selected {
        // On the right, where a dropdown puts its check, rather than in a column
        // on the left: `popover.menu check.right` is libadwaita's own spelling
        // for the list under a field, and a left tick pushed every label in to
        // make room for a mark only one row had.
        let centre = egui::pos2(
            rect.right() - style.padding.x - tick_side / 2.0,
            rect.center().y,
        );
        tick(
            painter,
            egui::Rect::from_center_size(centre, egui::vec2(tick_side, tick_side)),
            palette.accent,
        );
    }

    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected, label));
    if response.has_focus() {
        response.surrender_focus();
    }
    super::pointer_only(response).clicked()
}

/// A rule between groups in a menu.
///
/// Not `ui.separator()`: egui's takes the whole width it is offered, which is
/// the popup's area rather than the row's, so the line crossed the menu's
/// rounded corners. Here the rule is as wide as a row, which is what
/// libadwaita's `popover.menu separator` is — the content's width, inside the
/// popover's own inset — and the air around it is [`Style::gap`], the same gap
/// that separates two rows.
pub fn menu_separator(ui: &mut Ui, skin: &Skin) {
    let style = &skin.style;
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), style.gap), Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        egui::Stroke::new(style.border, skin.palette.border),
    );
}
