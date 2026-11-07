//! The two presets as pictures, over the sheet they will actually be used on.
//!
//! The gallery is the serial connection sheet: every row the real one has, in
//! the same order, because a style judged on four rows is not a style judged on
//! the dialog. Every control in it is the crate's own, so this file is the one
//! place a new primitive is seen before a screen adopts it.
//!
//! Both presets render on Linux: the Windows skin is a palette and a token
//! table, not a call into Windows — the same trick `chrome::buttons_for` uses
//! for the two captions. If the Windows look could only be seen on Windows, it
//! would only be checked when somebody remembered to.

use dtr_widgets::{
    ADWAITA_ACCENT, Button, Edit, Editable, Footer, Palette, Skin, check, row, section, segmented,
};
use egui::{RichText, Stroke, vec2};
use std::cell::RefCell;
use std::rc::Rc;

/// The rates the sheet offers behind the baud chevron. The real list lives in
/// the binary; this is a sample long enough to make the dropdown worth opening.
const BAUDS: &[u32] = &[
    300, 1_200, 2_400, 4_800, 9_600, 19_200, 38_400, 57_600, 115_200, 230_400,
];

/// What the sheet holds while it is open. The widgets report, so the caller owns
/// the values — which is what lets a harness type into them, and what lets a
/// painted checkbox be pressed in a gallery as well as in the sheet.
struct State {
    profile: String,
    baud: String,
    signals: [(&'static str, bool); 2],
    terminal: [(&'static str, bool); 2],
}

impl Default for State {
    fn default() -> Self {
        Self {
            profile: "gps module".to_owned(),
            baud: "9600".to_owned(),
            signals: [("DTR", true), ("RTS", true)],
            terminal: [("CR to CRLF", true), ("auto reconnect", true)],
        }
    }
}

fn dtr_skin() -> Skin {
    Skin::dtr(Palette::dtr(
        [0x0b, 0x0f, 0x0e],
        [0xdc, 0xdf, 0xdd],
        [0xff, 0xb0, 0x00],
    ))
}

/// Two or more checks in a row, which is how the sheet asks the question.
fn checks(ui: &mut egui::Ui, skin: &Skin, items: &mut [(&str, bool)]) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 18.0;
        for (label, on) in items.iter_mut() {
            if let Some(value) = check(ui, skin, *on, label) {
                *on = value;
            }
        }
    });
}

/// One card, laid out like the real sheet.
fn gallery(ui: &mut egui::Ui, skin: &Skin, state: &mut State) {
    skin.apply(ui.ctx());
    let p = skin.palette;
    let s = &skin.style;

    // The window behind the dialog, so the card's edge is visible: the harness's
    // own panel takes `panel_fill` (the card colour) and would otherwise merge
    // with it.
    ui.painter().rect_filled(ui.max_rect(), 0.0, p.window);
    ui.add_space(8.0);

    // A fixed-width child: a card that asks for "available width" gets the
    // harness's, and a dialog's width is part of its design.
    ui.allocate_ui_with_layout(
        vec2(510.0, 0.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            egui::Frame::new()
                .fill(p.card)
                .stroke(Stroke::new(s.border, p.border))
                .corner_radius(s.card_radius)
                .inner_margin(s.card_margin)
                .show(ui, |ui| {
                    ui.set_width(470.0);

                    // Header: what the sheet is about, then the port and its summary.
                    ui.label(
                        RichText::new("serial connection")
                            .font(s.label_font.clone())
                            .color(p.dim),
                    );
                    ui.add_space(s.gap);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("/dev/ttyUSB0")
                                .font(s.title_font.clone())
                                .color(p.text),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new("9600 7E1")
                                    .font(egui::FontId::new(
                                        s.label_font.size - 1.0,
                                        s.label_font.family.clone(),
                                    ))
                                    .color(p.dim),
                            );
                        });
                    });
                    ui.label(
                        RichText::new("FTDI FT232R USB UART · 0403:6001 · A50285BI")
                            .font(egui::FontId::new(
                                s.label_font.size - 1.0,
                                s.label_font.family.clone(),
                            ))
                            .color(p.dim),
                    );
                    ui.add_space(s.gap * 3.0);

                    // Profile and name are one control: type the name, or pick a
                    // saved one. That is the whole reason the dropdown is editable,
                    // and it saves a row.
                    row(ui, skin, "profile", |ui| {
                        let width = (ui.available_width() - 62.0).max(120.0);
                        let names = ["gps module", "bench board"];
                        let edit = Editable::new("profile", &state.profile, "profile value")
                            .width(width)
                            .show(ui, skin, &names);
                        if let Some(edit) = edit {
                            state.profile = match edit {
                                Edit::Typed(text) | Edit::Picked(text) => text,
                            };
                        }
                        // A name is not a profile until it is saved under one.
                        Button::new("save").width(54.0).show(ui, skin);
                    });
                    ui.add_space(s.gap * 2.0);

                    // The line: every framing parameter the port is opened with.
                    section(ui, skin, "line");
                    ui.add_space(s.gap);
                    row(ui, skin, "baud", |ui| {
                        let choices: Vec<String> = BAUDS.iter().map(u32::to_string).collect();
                        let edit = Editable::new("baud", &state.baud, "baud value")
                            .width(150.0)
                            // A half-typed rate stays in the field and says so with
                            // the border, rather than being reverted under the cursor.
                            .valid(state.baud.parse::<u32>().is_ok())
                            .show(ui, skin, &choices);
                        if let Some(edit) = edit {
                            state.baud = match edit {
                                Edit::Typed(text) | Edit::Picked(text) => text,
                            };
                        }
                    });
                    row(ui, skin, "data bits", |ui| {
                        segmented(
                            ui,
                            skin,
                            "data bits",
                            7u8,
                            &[(5, "5"), (6, "6"), (7, "7"), (8, "8")],
                        );
                    });
                    row(ui, skin, "parity", |ui| {
                        segmented(
                            ui,
                            skin,
                            "parity",
                            1u8,
                            &[(0, "none"), (1, "even"), (2, "odd")],
                        );
                    });
                    row(ui, skin, "stop bits", |ui| {
                        segmented(ui, skin, "stop bits", 1u8, &[(1, "1"), (2, "2")]);
                    });
                    row(ui, skin, "flow control", |ui| {
                        segmented(
                            ui,
                            skin,
                            "flow control",
                            0u8,
                            &[(0, "none"), (1, "software"), (2, "hardware")],
                        );
                    });
                    ui.add_space(s.gap);

                    // What the port is opened with, before a byte moves.
                    section(ui, skin, "signals at open");
                    ui.add_space(s.gap);
                    checks(ui, skin, &mut state.signals);
                    ui.add_space(s.gap);

                    // What the terminal does to the session afterwards.
                    section(ui, skin, "terminal");
                    ui.add_space(s.gap);
                    checks(ui, skin, &mut state.terminal);
                    ui.add_space(s.gap * 2.0);

                    // Footer: the keys, and the two answers.
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("enter connect   esc cancel")
                                .font(egui::FontId::new(
                                    s.label_font.size - 1.5,
                                    s.label_font.family.clone(),
                                ))
                                .color(p.dim),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let primary = Button::new("connect").primary().width(88.0);
                            let secondary = Button::new("cancel").width(76.0);
                            // `right_to_left` lays out from the corner inwards, so the
                            // first one added is the rightmost.
                            match s.footer {
                                Footer::PrimaryLast => {
                                    primary.show(ui, skin);
                                    ui.add_space(s.gap);
                                    secondary.show(ui, skin);
                                }
                                Footer::PrimaryFirst => {
                                    secondary.show(ui, skin);
                                    ui.add_space(s.gap);
                                    primary.show(ui, skin);
                                }
                            }
                        });
                    });
                });
        },
    );
}

fn harness(skin: Skin) -> egui_kittest::Harness<'static> {
    let state = Rc::new(RefCell::new(State::default()));
    egui_kittest::Harness::builder()
        .with_size(vec2(580.0, 760.0))
        .build_ui(move |ui| {
            let mut state = state.borrow_mut();
            gallery(ui, &skin, &mut state);
        })
}

#[test]
fn the_dtr_skin_renders() {
    let mut harness = harness(dtr_skin());
    harness.run();
    harness.snapshot("widgets_dtr");
}

/// Windows in dark mode: the palette the OS actually uses, not a dark grey
/// invented for the occasion.
/// The whole point of the editable dropdown is the list under it, and a
/// snapshot of a closed one says nothing about it.
#[test]
fn the_dropdown_opens_under_its_field() {
    use egui_kittest::kittest::Queryable;
    let mut harness = harness(dtr_skin());
    harness.run();
    harness.get_by_label("baud value choices").click();
    harness.run();
    harness.snapshot("widgets_dtr_open");
}

/// A hand-painted hover fill is invisible to every test where the pointer is
/// somewhere else, which is how three controls in this tree ended up with no
/// hover state worth having. One snapshot with the pointer on a chip is the
/// cheapest way to see that the state exists and is the palette's.
#[test]
fn a_chip_lights_up_under_the_pointer() {
    use egui_kittest::kittest::Queryable;
    let mut harness = harness(dtr_skin());
    harness.run();
    harness.get_by_label("parity odd").hover();
    harness.run();
    harness.snapshot("widgets_dtr_hover");
}

#[test]
fn the_windows_dark_skin_renders() {
    let mut harness = harness(Skin::windows_dark());
    harness.run();
    harness.snapshot("widgets_windows_dark");
}

/// And in light mode, which is where a style that quietly kept egui's dark
/// defaults shows up immediately.
#[test]
fn the_windows_light_skin_renders() {
    let mut harness = harness(Skin::windows_light());
    harness.run();
    harness.snapshot("widgets_windows_light");
}

/// GNOME in dark mode: libadwaita's own surfaces, over the same sheet. Rendered on
/// Linux like the Windows skins are, because a look that can only be seen on its
/// own desktop is a look nobody checks.
#[test]
fn the_adwaita_dark_skin_renders() {
    let mut harness = harness(Skin::adwaita(true, ADWAITA_ACCENT));
    harness.run();
    harness.snapshot("widgets_adwaita_dark");
}

/// And in light mode, where a style that quietly kept the dark derived slots —
/// the border and the dim text are mixed from `currentColor`, not listed — shows
/// up immediately.
#[test]
fn the_adwaita_light_skin_renders() {
    let mut harness = harness(Skin::adwaita(false, ADWAITA_ACCENT));
    harness.run();
    harness.snapshot("widgets_adwaita_light");
}
