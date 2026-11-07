//! The keyboard map: every binding, on one card, for somebody who installed
//! this five minutes ago.
//!
//! It exists because a terminal's bindings are invisible by nature — a key that
//! does nothing is indistinguishable from a key nobody told you about — and the
//! palette's footer only has room for four of them. This is the list: what is
//! bound, what it does, and, in the same place, what the config has switched
//! off, because "unbound" and "silently does nothing" are the same silence at
//! the keyboard.
//!
//! The chords come from the live [`Keymap`], so a rebound key shows its real
//! chord rather than the default's. Nothing here is interactive: it is a
//! reference card, and the rows are text. The chord is painted and the action is
//! a label, so a screen reader (and the test harness) can read what each row
//! *does* even though the key itself is decoration for the eye.

use egui::{Align2, Sense, Ui};

use crate::keys::{Action, Group, Keymap};
use crate::ui::Look;
use dtr_widgets::{Skin, section};

/// How wide the chord column is. Wide enough for `ctrl-shift-pageup`, the
/// longest default, with room to spare — a chord that wraps is a chord nobody
/// can read.
const CHORD_WIDTH: f32 = 118.0;
const WIDTH: f32 = 780.0;
/// The card's height in the dtr style: its twenty-two rows, five headings, title
/// and footer, rounded up. The native style scrolls inside it.
const HEIGHT: f32 = 480.0;

/// What the card asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The cross was pressed; Escape arrives through the keyboard instead.
    Close,
}

/// Draw the map. `keymap` is the live one, config overrides included.
#[allow(deprecated)] // one fixed overlay: the id is a name, not a hash of state
pub fn ui(ui: &mut Ui, look: &Look, keymap: &Keymap) -> Option<Event> {
    let skin = look.skin();
    let palette = skin.palette;
    let style = &skin.style;
    let screen = ui.max_rect();

    let width = WIDTH.min(screen.width() - 32.0).max(360.0);
    // The card is a fixed box with a scrolling body, like the sheet: the dtr
    // style fits its twenty-two rows in 520 points and the native style does not
    // fit them in a 620-point window at all, and a card that scrolls is honest
    // where a card that clips is a lie.
    let height = HEIGHT.min(screen.height() - 32.0).max(280.0);
    let rect = egui::Rect::from_center_size(screen.center(), egui::vec2(width, height));
    // Two columns, so the whole map is on one screen: a cheat sheet you have to
    // scroll is one you cannot glance at. The split follows `Group::ALL`, which
    // is the order a reader expects, rather than restating it here.
    let columns: [&[Group]; 2] = [&Group::ALL[..3], &Group::ALL[3..]];
    let id = egui::Id::new("dtr-keymap");
    crate::ui::prime_area(ui.ctx(), id, egui::Order::Foreground, rect, true);

    // The terminal is not the subject while this is up. The same scrim as the
    // serial sheet: enough to read the card against, not so much that the window
    // looks disabled.
    ui.painter().rect_filled(screen, 0.0, palette.scrim);

    let mut event = None;
    egui::Area::new(id)
        .order(egui::Order::Foreground)
        .fixed_pos(rect.min)
        .constrain(false)
        // A fade would make the card invisible on the first frame, and a capture
        // has exactly one.
        .fade_in(false)
        .show(ui.ctx(), |ui| {
            // egui's own labels are still egui's, so the card hands them the
            // skin's style.
            skin.apply(ui.ctx());
            ui.set_min_size(rect.size());
            egui::Frame::new()
                .fill(palette.card)
                .stroke(egui::Stroke::new(style.border, palette.border))
                .corner_radius(style.card_radius)
                .inner_margin(style.card_margin)
                .show(ui, |ui| {
                    let inner = (width - style.card_margin.sum().x).max(320.0);
                    ui.set_width(inner);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("keys")
                                .font(style.title_font.clone())
                                .color(palette.text),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if dtr_widgets::cross(ui, &skin, "close keyboard map", 24.0).clicked()
                            {
                                event = Some(Event::Close);
                            }
                        });
                    });
                    ui.add_space(style.gap * 2.0);

                    // Two columns, so the whole map fits a dtr-sized card at one
                    // glance; the native style's 32-point rows need the scroll
                    // that follows.
                    egui::ScrollArea::vertical()
                        .id_salt(egui::Id::unique("dtr-keymap-columns"))
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                        ui.horizontal_top(|ui| {
                        for (index, groups) in columns.iter().enumerate() {
                            if index > 0 {
                                ui.add_space(style.gap * 3.0);
                            }
                            ui.vertical(|ui| {
                                ui.set_width(inner / 2.0 - style.gap * 2.0);
                                for group in groups.iter() {
                                    section(ui, &skin, group.title());
                                    ui.add_space(style.gap);
                                    for action in crate::keys::ALL
                                        .iter()
                                        .copied()
                                        .filter(|action| action.group() == *group)
                                    {
                                        row(ui, &skin, keymap, action);
                                    }
                                    ui.add_space(style.gap * 1.5);
                                }
                            });
                        }
                    });
                    });

                    ui.add_space(style.gap);
                    ui.label(
                        egui::RichText::new(
                            "esc closes   ·   rebind in [keys] in the config, or `keys` in the palette",
                        )
                        .font(egui::FontId::new(
                            style.label_font.size - 1.0,
                            style.label_font.family.clone(),
                        ))
                        .color(palette.dim),
                    );
                });
        });
    event
}

/// One binding: the chord, then what it does.
fn row(ui: &mut Ui, skin: &Skin, keymap: &Keymap, action: Action) {
    let palette = skin.palette;
    let style = &skin.style;
    let chords = keymap.bound(action);
    ui.horizontal(|ui| {
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(CHORD_WIDTH, style.row - 3.0), Sense::hover());
        let chord = match chords.first() {
            Some(chord) => chord.as_str(),
            // Said out loud rather than left blank: a switched-off binding is a
            // decision, and a reader looking for that key deserves to see it.
            None => "unbound",
        };
        ui.painter().text(
            rect.left_center(),
            Align2::LEFT_CENTER,
            chord,
            style.value_font.clone(),
            if chords.is_empty() {
                palette.disabled
            } else {
                palette.text
            },
        );
        ui.add(
            egui::Label::new(
                egui::RichText::new(action.label())
                    .font(egui::FontId::new(
                        style.label_font.size,
                        style.label_font.family.clone(),
                    ))
                    .color(if chords.is_empty() {
                        palette.disabled
                    } else {
                        palette.dim
                    }),
            )
            .truncate(),
        );
        // The name the binding is written under in `[keys]`, in the monospace a
        // config file is read in: the card is where a person finds out *what to
        // write*, not only what to press.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(action.name())
                        .font(egui::FontId::monospace(style.label_font.size - 1.5))
                        .color(palette.disabled),
                )
                .truncate(),
            );
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::Keymap;
    use std::collections::BTreeMap;

    fn harness(keymap: Keymap) -> egui_kittest::Harness<'static> {
        let look = Look::default();
        egui_kittest::Harness::builder()
            .with_size(egui::vec2(900.0, 620.0))
            .build_ui(move |ui| {
                let _ = super::ui(ui, &look, &keymap);
            })
    }

    /// The default chord parses. It is written `ctrl-?`, which only works if the
    /// config parser accepts punctuation as a key *and* the resolver retries a
    /// shifted character without its shift — a default that silently fails to
    /// parse leaves the action with no binding at all, and nothing says so.
    #[test]
    fn the_default_chord_is_bound() {
        let keymap = Keymap::new(&BTreeMap::new());
        assert_eq!(keymap.bound(Action::Keymap), vec!["ctrl-?".to_owned()]);
    }

    /// The card is the list, so it has to have every action on it: an action a
    /// person cannot find here is an action they will not know exists.
    #[test]
    fn every_action_is_on_the_card() {
        use egui_kittest::kittest::Queryable;
        let keymap = Keymap::new(&BTreeMap::new());
        let mut harness = harness(keymap);
        harness.run_steps(4);
        for action in crate::keys::ALL {
            let mut rows = harness.get_all_by_label(action.label());
            assert!(
                rows.next().is_some(),
                "{} is not on the card",
                action.name()
            );
        }
    }

    /// A rebound key shows its own chord, not the default: the map is read after
    /// somebody edits `[keys]`, so it has to describe *their* keyboard.
    #[test]
    fn a_rebound_chord_is_the_one_shown() {
        use egui_kittest::kittest::Queryable;
        let mut overrides = BTreeMap::new();
        overrides.insert(
            "palette".to_owned(),
            dtr_config::keys::parse_sequence("ctrl-k").expect("chord"),
        );
        let mut harness = harness(Keymap::new(&overrides));
        harness.run_steps(4);
        harness.get_by_label("command palette").scroll_to_me();
        harness.snapshot("keymap");
    }
}
