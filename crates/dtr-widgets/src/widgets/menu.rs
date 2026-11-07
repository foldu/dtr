//! A row in a menu: a full-width target, with a tick beside the entry that is
//! already the value.
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

/// The column a tick sits in. Reserved on every row so the labels line up,
/// whether or not this menu has a chosen entry at all.
const TICK_GUTTER: f32 = 14.0;

/// Draw one menu row and report whether the pointer chose it.
pub fn menu_row(ui: &mut Ui, skin: &Skin, label: &str, selected: bool) -> bool {
    let palette = skin.palette;
    let style = &skin.style;
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_owned(), style.value_font.clone(), palette.text);
    // A row is as wide as the menu, not as wide as its word: the gap on the right
    // of a short entry is what makes a list look like a list.
    let width = ui
        .available_width()
        .max(galley.size().x + style.padding.x * 2.0 + TICK_GUTTER);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, style.row), Sense::click());

    let painter = ui.painter();
    // Only the pointer's row is filled: a menu's own background is the frame's,
    // and painting every row with the card colour bands the list instead.
    if response.hovered() {
        painter.rect_filled(rect, style.control_radius, palette.control_hover);
    }
    if selected {
        let side = (style.row * 0.5).min(14.0);
        let centre = egui::pos2(rect.left() + style.padding.x + side / 2.0, rect.center().y);
        tick(
            painter,
            egui::Rect::from_center_size(centre, egui::vec2(side, side)),
            palette.accent,
        );
    }
    painter.galley(
        egui::pos2(
            rect.left() + style.padding.x + TICK_GUTTER,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        palette.text,
    );

    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected, label));
    if response.has_focus() {
        response.surrender_focus();
    }
    super::pointer_only(response).clicked()
}
