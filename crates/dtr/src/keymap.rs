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
//! chord rather than the default's. A row is a button: pressing one starts a
//! replacement, which is the only way to rebind that does not require knowing
//! that `[keys]` exists. The replacement itself is captured by the event loop —
//! the keys must not also reach the shell — so this module draws the popup and
//! reports what was asked for, and the app does the writing.

use egui::{Align2, Sense, Ui};

use crate::keys::{Action, Group, Keymap};
use crate::ui::Look;
use dtr_widgets::{Skin, section};

const WIDTH: f32 = 900.0;
/// The card's height: every row, six headings, title and footer, which is
/// taller than a 620-point window allows by 32 points. A window with less room
/// scrolls the body, which is honest; a window with more does not grow the card,
/// because a cheat sheet is scanned in one place, not followed around.
const HEIGHT: f32 = 588.0;
const POPUP_WIDTH: f32 = 260.0;

/// The card's state: whether it is up, and whether a key is being waited for.
#[derive(Default)]
pub struct Overlay {
    pub open: bool,
    /// The action being rebound, and the last refusal, which the popup says out
    /// loud rather than leaving a keypress looking ignored.
    pub edit: Option<(Action, Option<String>)>,
}

/// What the card asked for. The drawing never changes a binding itself: a
/// replacement is a file edit, and the app owns the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The cross was pressed; Escape arrives through the keyboard instead.
    Close,
    /// A row was pressed: wait for a key for this action.
    Edit(Action),
    /// Put the built-in chord back.
    Reset(Action),
    /// Stop waiting for a key.
    Cancel,
}

/// Draw the map. `keymap` is the live one, config overrides included.
#[allow(deprecated)] // one fixed overlay: the id is a name, not a hash of state
pub fn ui(ui: &mut Ui, look: &Look, keymap: &Keymap, overlay: &Overlay) -> Option<Event> {
    let skin = look.skin();
    let palette = skin.palette;
    let style = &skin.style;

    // Two columns, so the whole map is on one screen: a cheat sheet you have to
    // scroll is one you cannot glance at. The split follows `Group::ALL`, which
    // is the order a reader expects, rather than restating it here.
    let split = Group::split();
    let columns: [&[Group]; 2] = [&Group::ALL[..split], &Group::ALL[split..]];

    let out = dtr_widgets::Card::new(egui::Id::new("dtr-keymap"), "keys")
        .size(WIDTH, HEIGHT)
        .min(360.0, 280.0)
        .body_min(320.0)
        .close("close keyboard map")
        // A hint under a scrolling list belongs to the card's footer, or it
        // scrolls away with the rows it explains.
        .footer(|ui| {
            ui.label(
                egui::RichText::new(
                    "esc closes   ·   press a row to rebind it   ·   the names are [keys] in the config",
                )
                .font(egui::FontId::new(
                    style.label_font.size - 1.0,
                    style.label_font.family.clone(),
                ))
                .color(palette.dim),
            );
        })
        .show(ui, &skin, |ui, inner| {
            ui.add_space(style.gap * 2.0);
            let mut event = None;
            // Where the row being rebound is, so the popup can sit under it.
            // Found while the rows are painted, which is the only pass there is.
            let mut anchor = None;

            // The card scrolls the body, so the map needs no `ScrollArea` of
            // its own — and `inner` is the width it has *inside* the card's bar,
            // which is why the columns no longer subtract one.
            ui.horizontal_top(|ui| {
                for (index, groups) in columns.iter().enumerate() {
                    if index > 0 {
                        ui.add_space(style.gap * 3.0);
                    }
                    ui.vertical(|ui| {
                        ui.set_width(inner / 2.0 - style.gap * 2.0);
                        // The columns do not hold chords of the same length —
                        // `ctrl-alt-shift-right` against `shift-pageup` — so
                        // each asks its own widest instead of sharing one guess
                        // that is cramped in one column and wasteful in the
                        // other.
                        let chord_width = chord_width(ui, &skin, keymap, groups);
                        for (position, group) in groups.iter().enumerate() {
                            // The gap goes before each group but the first,
                            // not after each: a trailing gap is what makes the
                            // body one line taller than the card.
                            if position > 0 {
                                ui.add_space(style.gap * 1.5);
                            }
                            section(ui, &skin, group.title());
                            ui.add_space(style.gap);
                            for action in crate::keys::ALL
                                .iter()
                                .copied()
                                .filter(|action| action.group() == *group)
                            {
                                let editing = overlay
                                    .edit
                                    .as_ref()
                                    .is_some_and(|(editing, _)| *editing == action);
                                let row =
                                    row(ui, &skin, keymap, action, editing, chord_width);
                                if editing {
                                    anchor = Some(row.rect);
                                }
                                if row.clicked && event.is_none() {
                                    event = Some(Event::Edit(action));
                                }
                            }
                        }
                    });
                }
            });

            // The popup is the second thing drawn and the last in the layer, so
            // it floats above the rows it is anchored to.
            if let Some((action, notice)) = &overlay.edit {
                let screen = ui.ctx().content_rect();
                let at = anchor.unwrap_or_else(|| ui.max_rect());
                if let Some(asked) = popup(ui, &skin, screen, at, *action, notice.as_deref()) {
                    event = Some(asked);
                }
            }
            event
        });
    if out.close {
        Some(Event::Close)
    } else {
        out.body
    }
}

/// How wide one column's chord column has to be: the widest chord in the groups
/// it holds, measured in the font the card draws it with.
///
/// Measured rather than a constant because the answer belongs to the bindings —
/// a rebinding to a longer chord, or the native style's larger value font, moves
/// it — and because a chord that runs into its label is a chord nobody can read.
fn chord_width(ui: &Ui, skin: &Skin, keymap: &Keymap, groups: &[Group]) -> f32 {
    let font = skin.style.value_font.clone();
    let mut widest: f32 = 0.0;
    for action in crate::keys::ALL
        .iter()
        .copied()
        .filter(|action| groups.contains(&action.group()))
    {
        for chord in keymap.bound(action) {
            let width = ui
                .painter()
                .layout_no_wrap(chord, font.clone(), skin.palette.text)
                .size()
                .x;
            widest = widest.max(width);
        }
    }
    // A gap, not a padding: the label starts where the chord ends plus a space.
    widest + 14.0
}

/// One binding row: the chord, what it does, the `[keys]` name, and a target.
///
/// It is a button, not a line of text, because rebinding has to start somewhere
/// a person can find. The painter draws and the response decides; the node says
/// `press a key` was the whole point.
fn row(
    ui: &mut Ui,
    skin: &Skin,
    keymap: &Keymap,
    action: Action,
    editing: bool,
    chord_width: f32,
) -> Row {
    let palette = skin.palette;
    let style = &skin.style;
    let chords = keymap.bound(action);
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), style.row - 3.0),
        Sense::click(),
    );
    if editing || response.hovered() {
        ui.painter().rect_filled(
            rect.expand2(egui::vec2(2.0, 0.0)),
            style.chip_radius,
            palette.control_active,
        );
    }
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
    ui.painter().text(
        egui::pos2(rect.left() + chord_width, rect.center().y),
        Align2::LEFT_CENTER,
        action.label(),
        egui::FontId::new(style.label_font.size, style.label_font.family.clone()),
        if chords.is_empty() {
            palette.disabled
        } else {
            palette.dim
        },
    );
    ui.painter().text(
        rect.right_center(),
        Align2::RIGHT_CENTER,
        action.name(),
        egui::FontId::monospace(style.label_font.size - 1.5),
        palette.disabled,
    );
    let name = format!("{} binding", action.label());
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, editing, &name));
    if response.has_focus() {
        response.surrender_focus();
    }
    Row {
        rect,
        // Enter belongs to the shell, so only the pointer starts a rebind.
        clicked: response.clicked() && ui.input(|input| input.pointer.any_click()),
    }
}

struct Row {
    rect: egui::Rect,
    clicked: bool,
}

/// The "press a key" card, under the row it is for.
///
/// It is an `Area` — a layer of its own — because the body it is drawn from
/// scrolls, and a child `Ui` inherits the scroll viewport's clip rect, so the
/// card would be cut off at the sheet's edge exactly when the row is near it.
/// It used to be a child `Ui` because an `Area` fades in, and a frame that is
/// still fading lets the rows underneath show through it; `fade_in(false)` is
/// the answer to that, and the same reason the sheet itself has it.
#[allow(deprecated)] // one fixed popup: the id is a name, not a hash of state
fn popup(
    ui: &mut Ui,
    skin: &Skin,
    screen: egui::Rect,
    at: egui::Rect,
    action: Action,
    notice: Option<&str>,
) -> Option<Event> {
    let palette = skin.palette;
    let style = &skin.style;
    let mut event = None;
    // Under the row, and inside the window: a row near the bottom would otherwise
    // put the card off the screen, which is where the answer is.
    let size = egui::vec2(POPUP_WIDTH, 96.0);
    let origin = egui::pos2(
        at.left()
            .min(screen.right() - size.x - 8.0)
            .max(screen.left() + 8.0),
        (at.bottom() + 4.0)
            .min(screen.bottom() - size.y - 8.0)
            .max(screen.top() + 8.0),
    );
    egui::Area::new(egui::Id::new("dtr-keymap-rebind"))
        .order(egui::Order::Foreground)
        .fixed_pos(origin)
        .constrain(false)
        .movable(false)
        .fade_in(false)
        .show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(palette.card)
                .stroke(egui::Stroke::new(style.border, palette.border_strong))
                .corner_radius(style.control_radius)
                .shadow(style.shadow.clone().unwrap_or_default())
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.set_width(size.x);
                    ui.label(
                        egui::RichText::new("press a key")
                            .font(style.value_font.clone())
                            .color(palette.text),
                    );
                    ui.label(
                        egui::RichText::new(format!("for {}", action.label()))
                            .font(egui::FontId::new(
                                style.label_font.size - 0.5,
                                style.label_font.family.clone(),
                            ))
                            .color(palette.dim),
                    );
                    if let Some(notice) = notice {
                        ui.label(
                            egui::RichText::new(notice)
                                .font(egui::FontId::new(
                                    style.label_font.size - 0.5,
                                    style.label_font.family.clone(),
                                ))
                                .color(palette.danger),
                        );
                    }
                    ui.add_space(style.gap);
                    ui.horizontal(|ui| {
                        if dtr_widgets::Button::new("reset").width(64.0).show(ui, skin) {
                            event = Some(Event::Reset(action));
                        }
                        if dtr_widgets::Button::new("cancel")
                            .width(64.0)
                            .show(ui, skin)
                        {
                            event = Some(Event::Cancel);
                        }
                    });
                });
        });
    event
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// The answers the card gave, which the app would apply.
    type Answers = std::rc::Rc<std::cell::RefCell<Vec<Event>>>;

    /// The card, and somewhere for its answers to land.
    fn harness(keymap: Keymap) -> (egui_kittest::Harness<'static>, Answers) {
        let look = Look::default();
        let mut overlay = Overlay {
            open: true,
            edit: None,
        };
        let answers: Answers = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let log = answers.clone();
        // The answers are what the app applies; here they keep the card's own
        // state in step, which is what a second frame reads.
        let harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(900.0, 620.0))
            .build_ui(move |ui| {
                if let Some(event) = super::ui(ui, &look, &keymap, &overlay) {
                    log.borrow_mut().push(event);
                    match event {
                        Event::Edit(action) => overlay.edit = Some((action, None)),
                        Event::Cancel | Event::Close => overlay.edit = None,
                        Event::Reset(_) => overlay.edit = None,
                    }
                }
            });
        (harness, answers)
    }

    /// The card is the list, so it has to have every action on it: an action a
    /// person cannot find here is an action they will not know exists.
    #[test]
    fn every_action_is_on_the_card() {
        use egui_kittest::kittest::Queryable;
        let (mut harness, _) = harness(Keymap::new(&BTreeMap::new()));
        harness.run_steps(3);
        for action in crate::keys::ALL {
            let label = format!("{} binding", action.label());
            let mut rows = harness.get_all_by_label(&label);
            assert!(rows.next().is_some(), "{label} is not on the card");
        }
    }

    /// `ctrl-?` is the default chord, and it only works if the parser accepts
    /// punctuation and the resolver retries a shifted character without its
    /// shift — a default that fails to parse leaves the action unbound in
    /// silence.
    #[test]
    fn the_default_chord_is_bound() {
        let keymap = Keymap::new(&BTreeMap::new());
        assert_eq!(keymap.bound(Action::Keymap), vec!["ctrl-?".to_owned()]);
    }

    /// A rebound key shows its own chord, and pressing a row asks to replace it.
    #[test]
    fn a_row_asks_to_be_rebound() {
        use egui_kittest::kittest::Queryable;
        let mut overrides = BTreeMap::new();
        overrides.insert(
            "palette".to_owned(),
            dtr_config::keys::parse_sequence("ctrl-k").expect("chord"),
        );
        let (mut harness, answers) = harness(Keymap::new(&overrides));
        harness.run_steps(3);
        harness.get_by_label("command palette binding").click();
        harness.run_steps(3);
        // The popup is the whole of the promise: the row must ask, and the card
        // must say what it is waiting for.
        harness.get_by_label("press a key");
        harness.snapshot("keymap_rebind");

        // And reset is a click that reaches the app as an answer, not a painted
        // word: `Reset` is what deletes the override.
        harness.get_by_label("reset").click();
        harness.run_steps(2);
        assert!(
            answers.borrow().contains(&Event::Reset(Action::Palette)),
            "reset was not reported: {:?}",
            answers.borrow()
        );
    }
}
