//! The two presets as pictures.
//!
//! A style is a claim about how a window looks, and the only honest test of a
//! claim like that is a rendering. Both presets render on Linux — the Windows
//! skin is a palette and a token table, not a call into Windows — which is the
//! same trick `chrome::buttons_for` uses for the two captions: if the Windows
//! look could only be seen on Windows, it would only be checked when somebody
//! remembered to.
//!
//! The gallery is deliberately made of egui's *stock* widgets. It is testing the
//! derivation in `Skin::egui_style`, not the painted primitives (which arrive
//! with the crate's own `widgets` module and get a gallery of their own).

use dtr_widgets::{Palette, Skin};
use egui::{RichText, vec2};

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

/// A card with one of everything the two surfaces actually use.
fn gallery(ui: &mut egui::Ui, skin: &Skin) {
    skin.apply(ui.ctx());
    let p = skin.palette;
    let s = &skin.style;

    // The window behind the dialog, so the card's edge is visible: the harness's
    // own panel takes `panel_fill` (the card colour) and would otherwise merge
    // with it.
    ui.painter().rect_filled(ui.max_rect(), 0.0, p.window);
    ui.add_space(8.0);
    egui::Frame::new()
        .fill(p.card)
        .stroke(egui::Stroke::new(s.border, p.border))
        .corner_radius(s.card_radius)
        .inner_margin(s.card_margin)
        .show(ui, |ui| {
            ui.set_min_width(440.0);
            ui.label(
                RichText::new("serial connection")
                    .font(s.label_font.clone())
                    .color(p.dim),
            );
            ui.add_space(s.gap);
            ui.label(
                RichText::new("/dev/ttyUSB0")
                    .font(s.title_font.clone())
                    .color(p.text),
            );
            ui.add_space(s.gap * 2.0);

            row(ui, skin, "profile", |ui| {
                let mut picked = String::from("gps module");
                let response = egui::ComboBox::from_id_salt("profile")
                    .selected_text(RichText::new(picked.clone()).font(s.value_font.clone()))
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut picked, "gps module".to_owned(), "gps module");
                        ui.selectable_value(&mut picked, "bench board".to_owned(), "bench board");
                    });
                // Stock widgets carry their own names only when they have a
                // label; a combo whose label is the row beside it has none.
                response.response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "profile value")
                });
            });

            row(ui, skin, "baud", |ui| {
                let mut baud = 9_600;
                ui.add(egui::DragValue::new(&mut baud).suffix(" baud"));
            });

            row(ui, skin, "name", |ui| {
                let mut name = String::from("gps module");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut name)
                        .font(egui::TextStyle::Body)
                        .desired_width(220.0),
                );
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::Role::TextInput, true, "name value")
                });
            });

            row(ui, skin, "reconnect", |ui| {
                let mut on = true;
                let response = ui.checkbox(&mut on, "");
                response.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::Role::CheckBox,
                        true,
                        on,
                        "auto reconnect value",
                    )
                });
            });

            ui.add_space(s.gap);
            ui.separator();
            ui.add_space(s.gap);

            // One row, right-aligned, and the style says which button the
            // corner belongs to: Windows puts the primary first, dtr last.
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), s.row),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
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
                        dtr_widgets::Footer::PrimaryLast => {
                            let _ = ui.add(primary);
                            ui.add_space(s.gap);
                            let _ = ui.add(secondary);
                        }
                        dtr_widgets::Footer::PrimaryFirst => {
                            let _ = ui.add(secondary);
                            ui.add_space(s.gap);
                            let _ = ui.add(primary);
                        }
                    }
                },
            );
        });
}

fn harness(skin: Skin) -> egui_kittest::Harness<'static> {
    egui_kittest::Harness::builder()
        .with_size(vec2(540.0, 470.0))
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
