//! A row of tabs: one label per panel, the current one marked by an accent rule
//! under it.
//!
//! Not a [`crate::segmented`], although the two look alike from three feet away.
//! A segmented control *sets a value* — the baud rate, the CR translation, which
//! look the window wears — and its chips are filled with the accent when chosen.
//! A tab row *changes which panel is on screen*, so the label stays text (nothing
//! is being chosen in it), the rule is an underline rather than a fill, and the
//! accessibility role is `Tab` with the current one selected. A screen reader
//! that hears "radio button, selected, settings appearance" has been told the
//! wrong thing about what pressing it does.
//!
//! The rule is painted *inside* the row, so a tab row is exactly `Style::row`
//! tall and a card can reserve it with the same arithmetic as any other row.

use egui::{Sense, Stroke, Ui, Vec2};

use crate::skin::Skin;

/// Draw the tabs and report the one the pointer chose, if any.
///
/// `group` names the row for accessibility (`settings appearance`, `settings
/// general`): a bare label is not unique in a card that also has a value with the
/// same word in it.
pub fn tabs<T: Copy + PartialEq>(
    ui: &mut Ui,
    skin: &Skin,
    group: &str,
    value: T,
    options: &[(T, &str)],
) -> Option<T> {
    let palette = skin.palette;
    let style = &skin.style;
    let mut picked = None;
    // Left to right, for the reason [`crate::segmented`] gives: the order is the
    // caller's, and a parent that pins this row to the right edge would otherwise
    // mirror it.
    ui.allocate_ui_with_layout(
        Vec2::new(ui.available_width(), style.row),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            // One row, and the rule is drawn inside it: a tab row that was two
            // points taller than a row would put every control below it off the
            // grid the card laid out.
            let (_, rect) = ui.allocate_space(Vec2::new(ui.available_width(), style.row));
            let mut x = rect.left();
            for (option, label) in options {
                let selected = *option == value;
                let galley = ui.painter().layout_no_wrap(
                    (*label).to_owned(),
                    style.value_font.clone(),
                    palette.text,
                );
                let width = galley.size().x + style.padding.x * 2.0;
                let tab = egui::Rect::from_min_size(
                    egui::pos2(x, rect.top()),
                    Vec2::new(width, style.row),
                );
                x += width + style.gap;

                let response =
                    ui.interact(tab, ui.make_persistent_id((group, *label)), Sense::click());
                let hovered = response.hovered();
                let colour = match (selected, hovered) {
                    (true, _) => palette.text,
                    (false, true) => palette.text,
                    (false, false) => palette.dim,
                };
                ui.painter().galley(
                    egui::pos2(
                        tab.center().x - galley.size().x / 2.0,
                        tab.center().y - galley.size().y / 2.0,
                    ),
                    galley,
                    colour,
                );
                if selected {
                    // Inside the row, on its last two points, so the row's height
                    // is the style's and not the style's plus a rule.
                    let y = tab.bottom() - 1.0;
                    ui.painter().line_segment(
                        [
                            egui::pos2(tab.left() + style.padding.x / 2.0, y),
                            egui::pos2(tab.right() - style.padding.x / 2.0, y),
                        ],
                        Stroke::new(2.0, palette.accent),
                    );
                }

                let name = format!("{group} {label}");
                response.widget_info(|| {
                    egui::WidgetInfo::selected(egui::Role::Tab, true, selected, &name)
                });
                // `WidgetInfo::selected` reaches accesskit as *toggled*, and a tab
                // is not switched on: a screen reader reads a tab's state off
                // `Selected` and would otherwise announce nothing at all.
                ui.ctx().accesskit_node_builder(response.id, |node| {
                    node.set_selected(selected);
                });
                if response.has_focus() {
                    response.surrender_focus();
                }
                // Enter belongs to whatever is behind the card, not to this row.
                if super::pointer_only(response).clicked() && !selected {
                    picked = Some(*option);
                }
            }
        },
    );
    picked
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

    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Panel {
        Appearance,
        General,
    }

    const OPTIONS: [(Panel, &str); 2] = [
        (Panel::Appearance, "appearance"),
        (Panel::General, "general"),
    ];

    /// A press on a tab that is not the current one reports it, and a press on
    /// the current one reports nothing — re-selecting what is already shown is
    /// not an edit.
    #[test]
    fn a_press_reports_the_other_tab_alone() {
        let skin = skin();
        let seen: Rc<RefCell<Vec<Panel>>> = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&seen);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(Vec2::new(360.0, 80.0))
            .build_ui(move |ui| {
                skin.apply(ui.ctx());
                if let Some(panel) = tabs(ui, &skin, "settings", Panel::Appearance, &OPTIONS) {
                    sink.borrow_mut().push(panel);
                }
            });
        harness.run();
        harness.get_by_label("settings general").click();
        harness.run();
        assert_eq!(seen.borrow().as_slice(), &[Panel::General]);

        harness.get_by_label("settings appearance").click();
        harness.run();
        assert_eq!(
            seen.borrow().as_slice(),
            &[Panel::General],
            "the current tab is not an edit"
        );
    }

    /// The row is one style row tall, rule included, and the current tab says so
    /// to a screen reader as a *tab*, not as a radio button.
    #[test]
    fn the_current_tab_is_a_selected_tab() {
        let skin = skin();
        let row = skin.style.row;
        let mut harness = egui_kittest::Harness::builder()
            .with_size(Vec2::new(360.0, 80.0))
            .build_ui(move |ui| {
                skin.apply(ui.ctx());
                tabs(ui, &skin, "settings", Panel::General, &OPTIONS);
            });
        harness.run();
        let current = harness.get_by_label("settings general");
        assert_eq!(current.accesskit_node().role(), egui::accesskit::Role::Tab);
        assert_eq!(current.accesskit_node().is_selected(), Some(true));
        let other = harness.get_by_label("settings appearance");
        assert_eq!(other.accesskit_node().is_selected(), Some(false));
        // One row, not one row and a rule: `Style::row` is the whole height.
        assert!(current.rect().height() <= row + 0.5);
        harness.snapshot("tabs");
    }
}
