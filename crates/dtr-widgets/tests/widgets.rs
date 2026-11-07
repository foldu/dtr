//! The two presets as pictures, over the sheet they will actually be used on.
//!
//! The gallery is the serial connection sheet: every row the real one has, in
//! the same order, because a style judged on four rows is not a style judged on
//! the dialog. The widgets are egui's *stock* ones for now — this is a test of
//! `Skin::egui_style`, the derivation that used to leave egui's own grey next to
//! a hand-painted card; the painted primitives arrive next and replace them
//! without changing the shape of the sheet.
//!
//! Both presets render on Linux: the Windows skin is a palette and a token
//! table, not a call into Windows — the same trick `chrome::buttons_for` uses
//! for the two captions. If the Windows look could only be seen on Windows, it
//! would only be checked when somebody remembered to.

use dtr_widgets::{Footer, Palette, Section, Skin};
use egui::{RichText, Stroke, vec2};

fn dtr_skin() -> Skin {
    Skin::dtr(Palette::dtr(
        [0x0b, 0x0f, 0x0e],
        [0xdc, 0xdf, 0xdd],
        [0xff, 0xb0, 0x00],
    ))
}

fn label_cell(ui: &mut egui::Ui, skin: &Skin, label: &str) {
    let (rect, _) = ui.allocate_exact_size(
        vec2(skin.style.label_width, skin.style.row),
        egui::Sense::hover(),
    );
    ui.painter().text(
        rect.left_center(),
        egui::Align2::LEFT_CENTER,
        label,
        skin.style.label_font.clone(),
        skin.palette.dim,
    );
}

fn row(ui: &mut egui::Ui, skin: &Skin, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        label_cell(ui, skin, label);
        add(ui);
    });
    ui.add_space(skin.style.gap);
}

/// A group header: dtr rules a dim caption, native sets a bold title.
fn section(ui: &mut egui::Ui, skin: &Skin, title: &str) {
    let p = skin.palette;
    let s = &skin.style;
    match s.section {
        Section::Rule => {
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), 20.0), egui::Sense::hover());
            let galley = ui.painter().layout_no_wrap(
                title.to_owned(),
                egui::FontId::new(s.label_font.size - 1.0, s.label_font.family.clone()),
                p.dim,
            );
            let rule_from = rect.left() + galley.size().x + 10.0;
            let centre = rect.center().y - galley.size().y / 2.0;
            ui.painter()
                .galley(egui::pos2(rect.left(), centre), galley, p.dim);
            ui.painter().line_segment(
                [egui::pos2(rule_from, rect.center().y), rect.right_center()],
                Stroke::new(1.0, p.divider),
            );
        }
        Section::Bold => {
            ui.add_space(s.gap);
            ui.label(
                RichText::new(title)
                    .font(s.value_font.clone())
                    .strong()
                    .color(p.text),
            );
        }
    }
}

/// A segmented choice. Stock egui's `selectable_label` is the closest thing to a
/// chip until the painted one lands; the shape of the row is what is being
/// compared here, not the chip's corners.
fn segmented<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    skin: &Skin,
    value: T,
    options: &[(T, &'static str)],
) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (option, label) in options {
            let selected = *option == value;
            let text = RichText::new(*label).font(skin.style.value_font.clone());
            let _ = ui.selectable_label(selected, text);
        }
    });
}

fn checks(ui: &mut egui::Ui, skin: &Skin, items: &[(&str, bool)]) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 18.0;
        for (label, on) in items {
            let mut on = *on;
            let _ = ui.checkbox(
                &mut on,
                RichText::new(*label).font(skin.style.value_font.clone()),
            );
        }
    });
}

/// One card, laid out like the real sheet.
fn gallery(ui: &mut egui::Ui, skin: &Skin) {
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

                    // Profiles: pick one, or name the current form.
                    row(ui, skin, "profile", |ui| {
                        let mut picked = String::from("gps module");
                        let response = egui::ComboBox::from_id_salt("profile")
                            .selected_text(RichText::new(picked.clone()).font(s.value_font.clone()))
                            .width(300.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut picked,
                                    "gps module".to_owned(),
                                    "gps module",
                                );
                                ui.selectable_value(
                                    &mut picked,
                                    "bench board".to_owned(),
                                    "bench board",
                                );
                            });
                        response.response.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "profile value")
                        });
                    });
                    ui.horizontal(|ui| {
                        label_cell(ui, skin, "name");
                        let mut name = String::from("gps module");
                        let response = ui.add(
                            egui::TextEdit::singleline(&mut name)
                                .font(egui::TextStyle::Body)
                                .desired_width(300.0 - 62.0),
                        );
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::Role::TextInput, true, "name value")
                        });
                        let save =
                            egui::Button::new(RichText::new("save").font(s.value_font.clone()))
                                .corner_radius(s.control_radius)
                                .min_size(vec2(54.0, s.row));
                        let _ = ui.add(save);
                    });
                    ui.add_space(s.gap * 2.0);

                    // The line: every framing parameter the port is opened with.
                    section(ui, skin, "line");
                    ui.add_space(s.gap);
                    row(ui, skin, "baud", |ui| {
                        let mut baud = 9_600;
                        let response = ui.add(egui::DragValue::new(&mut baud).suffix(" baud"));
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::Role::SpinButton, true, "baud value")
                        });
                    });
                    row(ui, skin, "data bits", |ui| {
                        segmented(ui, skin, 7u8, &[(5, "5"), (6, "6"), (7, "7"), (8, "8")]);
                    });
                    row(ui, skin, "parity", |ui| {
                        segmented(ui, skin, 1u8, &[(0, "none"), (1, "even"), (2, "odd")]);
                    });
                    row(ui, skin, "stop bits", |ui| {
                        segmented(ui, skin, 1u8, &[(1, "1"), (2, "2")]);
                    });
                    row(ui, skin, "flow control", |ui| {
                        segmented(
                            ui,
                            skin,
                            0u8,
                            &[(0, "none"), (1, "software"), (2, "hardware")],
                        );
                    });
                    ui.add_space(s.gap);

                    // What the port is opened with, before a byte moves.
                    section(ui, skin, "signals at open");
                    ui.add_space(s.gap);
                    checks(ui, skin, &[("DTR", true), ("RTS", true)]);
                    ui.add_space(s.gap);

                    // What the terminal does to the session afterwards.
                    section(ui, skin, "terminal");
                    ui.add_space(s.gap);
                    checks(ui, skin, &[("CR to CRLF", true), ("auto reconnect", true)]);
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
                            let primary = egui::Button::new(
                                RichText::new("connect")
                                    .font(s.value_font.clone())
                                    .color(p.on_accent),
                            )
                            .fill(p.accent)
                            .corner_radius(s.control_radius)
                            .min_size(vec2(88.0, s.row));
                            let secondary = egui::Button::new(
                                RichText::new("cancel")
                                    .font(s.value_font.clone())
                                    .color(p.text),
                            )
                            .corner_radius(s.control_radius)
                            .min_size(vec2(76.0, s.row));
                            // `right_to_left` lays out from the corner inwards, so the
                            // first one added is the rightmost.
                            match s.footer {
                                Footer::PrimaryLast => {
                                    let _ = ui.add(primary);
                                    ui.add_space(s.gap);
                                    let _ = ui.add(secondary);
                                }
                                Footer::PrimaryFirst => {
                                    let _ = ui.add(secondary);
                                    ui.add_space(s.gap);
                                    let _ = ui.add(primary);
                                }
                            }
                        });
                    });
                });
        },
    );
}

fn harness(skin: Skin) -> egui_kittest::Harness<'static> {
    egui_kittest::Harness::builder()
        .with_size(vec2(580.0, 820.0))
        .build_ui(move |ui| gallery(ui, &skin))
}

#[test]
fn the_dtr_skin_renders() {
    let mut harness = harness(dtr_skin());
    harness.run();
    harness.snapshot("widgets_dtr");
}

/// Windows in dark mode: the palette the OS actually uses, not a dark grey
/// invented for the occasion.
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
