//! The serial connection sheet: every line setting a port is opened with, on
//! one surface, before the port is opened.
//!
//! The sheet owns the form, the profile list and the answers. `ui` borrows it
//! and reports what one frame did, the way [`crate::settings::Settings`] does,
//! so the caller applies the edit after the egui pass rather than lending a
//! mutable borrow into it.
//!
//! The settings are [`dtr_store::SerialSettings`] and the framing enums are
//! `serialport`'s, so the form, the store and the open call cannot drift apart.

use dtr_store::{DataBits, FlowControl, Parity, SerialSettings as Settings, StopBits};

use crate::ui::Look;

/// The rates a serial console actually uses, plus the ones a USB bridge can
/// reach. The field is typeable; this is the list behind its chevron.
pub const BAUDS: &[u32] = &[
    300, 600, 1_200, 2_400, 4_800, 9_600, 14_400, 19_200, 28_800, 38_400, 57_600, 115_200, 230_400,
    460_800, 921_600,
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub settings: Settings,
}

/// What one frame of the sheet asked for. The caller applies it after the egui
/// pass; drawing never mutates the sheet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// One control changed; this is the whole new form.
    Settings(Settings),
    /// The baud field was edited, or a default was picked. The text is kept
    /// even while it does not parse, because a half-typed rate must not erase
    /// itself.
    Baud(String),
    Name(String),
    SelectProfile(usize),
    Save,
    /// The primary action, carrying the form with the typed rate. The caller
    /// opens the port; the sheet does not know how.
    Connect(Settings),
    Cancel,
}

pub struct SerialDialog {
    port: String,
    detail: String,
    pub settings: Settings,
    pub profiles: Vec<Profile>,
    pub selected: Option<usize>,
    pub name: String,
    /// The baud field's raw text. `settings.baud` is the last value that
    /// parsed, so the header summary does not flicker while a rate is typed.
    pub baud_text: String,
}

impl SerialDialog {
    #[cfg(test)]
    pub fn new(port: impl Into<String>, detail: impl Into<String>, settings: Settings) -> Self {
        Self {
            port: port.into(),
            detail: detail.into(),
            baud_text: settings.baud.to_string(),
            settings,
            profiles: Vec::new(),
            selected: None,
            name: String::new(),
        }
    }

    /// The state a connect action starts from: the port's remembered record,
    /// the globally named profiles, and the profile that record names.
    ///
    /// The remembered values are the form, not the profile's values: a port can
    /// have been tweaked for one session and still be labelled with the profile
    /// it started from. Choosing the profile in the dropdown is what replaces
    /// the form with the profile's own settings.
    pub fn for_port(
        port: impl Into<String>,
        detail: impl Into<String>,
        remembered: Option<Settings>,
        profiles: Vec<Profile>,
    ) -> Self {
        let mut settings = remembered.clone().unwrap_or_default();
        let remembered_profile = remembered
            .as_ref()
            .and_then(|settings| settings.profile.clone());
        let selected = remembered_profile
            .as_ref()
            .and_then(|name| profiles.iter().position(|profile| &profile.name == name));
        // A remembered name that no longer has a profile is not a selection;
        // the name stays in the field so Save can recreate it.
        settings.profile = selected.map(|index| profiles[index].name.clone());
        let name = remembered_profile.unwrap_or_default();
        let mut dialog = Self {
            port: port.into(),
            detail: detail.into(),
            baud_text: settings.baud.to_string(),
            settings,
            profiles,
            selected,
            name,
        };
        if let Some(index) = selected {
            dialog.name = dialog.profiles[index].name.clone();
        }
        dialog
    }

    /// The live constructor: the port's remembered record and every named
    /// profile, both from the store.
    pub fn from_store(
        port: impl Into<String>,
        detail: impl Into<String>,
        store: Option<&dtr_store::Store>,
    ) -> Self {
        let port = port.into();
        let remembered = store.and_then(|store| match store.serial(&port) {
            Ok(settings) => settings,
            Err(err) => {
                tracing::warn!(%port, %err, "could not read the port's remembered settings");
                None
            }
        });
        let profiles = store
            .and_then(|store| match store.profiles() {
                Ok(profiles) => Some(
                    profiles
                        .into_iter()
                        .map(|(name, settings)| Profile { name, settings })
                        .collect(),
                ),
                Err(err) => {
                    tracing::warn!(%err, "could not read serial profiles");
                    None
                }
            })
            .unwrap_or_default();
        Self::for_port(port, detail, remembered, profiles)
    }

    /// The populated state, for reviewing the chrome: a device with a real
    /// detail line and three profiles, one of them selected. The live path
    /// builds this from the store with [`Self::from_store`].
    #[cfg(test)]
    pub fn preview(port: &str) -> Self {
        // The detail line is sample data too; inventing an FTDI adapter for a
        // port somebody actually named would be a lie in the one part of the
        // sheet that describes the hardware.
        let detail = if port == "/dev/ttyUSB0" {
            "FTDI FT232R USB UART · 0403:6001 · A50285BI"
        } else {
            ""
        };
        let mut dialog = Self::new(port, detail, Settings::default());
        let sample = |name: &str, settings: Settings| Profile {
            name: name.to_owned(),
            settings: Settings {
                profile: Some(name.to_owned()),
                ..settings
            },
        };
        dialog.profiles = vec![
            sample("bench board", Settings::default()),
            sample(
                "esp32 console",
                Settings {
                    baud: 921_600,
                    cr_to_crlf: false,
                    ..Settings::default()
                },
            ),
            sample(
                "gps module",
                Settings {
                    baud: 9_600,
                    data_bits: DataBits::Seven,
                    parity: Parity::Even,
                    stop_bits: StopBits::One,
                    flow_control: FlowControl::None,
                    dtr: true,
                    rts: false,
                    cr_to_crlf: true,
                    auto_reconnect: true,
                    ..Settings::default()
                },
            ),
        ];
        dialog.selected = Some(2);
        dialog.name = dialog.profiles[2].name.clone();
        dialog.settings = dialog.profiles[2].settings.clone();
        dialog.baud_text = dialog.settings.baud.to_string();
        dialog
    }

    /// Apply an event. The sheet owns the profile list, so save and select are
    /// here rather than in the drawing code.
    pub fn apply(&mut self, event: Event) {
        match event {
            Event::Settings(settings) => self.settings = settings,
            Event::Baud(text) => {
                self.baud_text = text;
                if let Some(baud) = parse_baud(&self.baud_text) {
                    self.settings.baud = baud;
                }
            }
            Event::Name(name) => self.name = name,
            Event::SelectProfile(index) => self.select(index),
            Event::Save => {
                self.save();
            }
            // Connect and Cancel close the sheet; the caller decides what the
            // connection is. Applying them here is a no-op.
            Event::Connect(_) | Event::Cancel => {}
        }
    }

    pub fn select(&mut self, index: usize) {
        let Some(profile) = self.profiles.get(index) else {
            return;
        };
        self.selected = Some(index);
        self.name = profile.name.clone();
        self.settings = profile.settings.clone();
        self.settings.profile = Some(profile.name.clone());
        self.baud_text = self.settings.baud.to_string();
    }

    /// Save the form under the name in the field. The same name updates in
    /// place rather than growing a second copy; a new name appends and becomes
    /// selected. Returns the saved profile so the caller can write it to the
    /// store.
    pub fn save(&mut self) -> Option<(String, Settings)> {
        let name = self.name.trim().to_owned();
        if name.is_empty() {
            return None;
        }
        self.settings.profile = Some(name.clone());
        match self
            .profiles
            .iter()
            .position(|profile| profile.name == name)
        {
            Some(index) => {
                self.profiles[index].settings = self.settings.clone();
                self.selected = Some(index);
            }
            None => {
                self.profiles.push(Profile {
                    name: name.clone(),
                    settings: self.settings.clone(),
                });
                self.selected = Some(self.profiles.len() - 1);
            }
        }
        self.name = name.clone();
        Some((name, self.settings.clone()))
    }

    pub fn baud(&self) -> Option<u32> {
        parse_baud(&self.baud_text)
    }

    pub fn port(&self) -> &str {
        &self.port
    }

    /// Draw the sheet and report what one frame asked for.
    #[allow(deprecated)] // one fixed overlay: the id is a name, not a hash of state
    pub fn ui(&self, ui: &mut egui::Ui, look: &Look) -> Option<Event> {
        // Idempotent, and here rather than only at startup so every caller —
        // the window, a capture, a kittest harness — gets a working loader.
        crate::ui::install_image_loaders(ui.ctx());
        let screen = ui.max_rect();
        let width = (WIDTH.min(screen.width() - 32.0)).max(360.0);
        let height = (HEIGHT.min(screen.height() - 32.0)).max(320.0);
        let rect = egui::Rect::from_center_size(screen.center(), egui::vec2(width, height));
        let id = egui::Id::new("dtr-serial-dialog");
        crate::ui::prime_area(ui.ctx(), id, egui::Order::Foreground, rect, true);

        // The terminal is not the subject while this is up. Same weight as the
        // settings menu: enough to read the sheet against, not so much that the
        // window looks disabled.
        ui.painter()
            .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(150));

        let baud_ok = self.baud().is_some();
        let mut event = None;
        egui::Area::new(id)
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .constrain(false)
            // A fade would make the sheet invisible on the first frame, and a
            // capture has exactly one.
            .fade_in(false)
            .show(ui.ctx(), |ui| {
                ui.set_min_size(rect.size());
                egui::Frame::new()
                    .fill(Look::colour(look.surface))
                    .stroke(egui::Stroke::new(
                        1.0,
                        mix(look.surface, look.foreground, 0.14),
                    ))
                    .corner_radius(10)
                    .inner_margin(egui::Margin::symmetric(20, 18))
                    .show(ui, |ui| {
                        ui.set_width((width - 40.0).max(320.0));
                        self.header(ui, look, &mut event);
                        ui.add_space(16.0);
                        self.profiles_ui(ui, look, baud_ok, &mut event);
                        ui.add_space(14.0);
                        self.line_ui(ui, look, &mut event);
                        ui.add_space(8.0);
                        self.signals_ui(ui, look, &mut event);
                        ui.add_space(8.0);
                        self.terminal_ui(ui, look, &mut event);
                        ui.add_space(14.0);
                        self.footer_ui(ui, look, baud_ok, &mut event);
                    });
            });
        event
    }

    fn header(&self, ui: &mut egui::Ui, look: &Look, event: &mut Option<Event>) {
        let foreground = Look::colour(look.foreground);
        let dim = Look::colour(look.dim);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("serial connection")
                    .monospace()
                    .size(12.0)
                    .color(dim),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let close = close_cross(ui, look);
                if close {
                    *event = Some(Event::Cancel);
                }
            });
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(&self.port)
                    .monospace()
                    .size(18.0)
                    .strong()
                    .color(foreground),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(self.settings.summary())
                        .monospace()
                        .size(11.0)
                        .color(dim),
                );
            });
        });
        if !self.detail.is_empty() {
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(&self.detail)
                    .monospace()
                    .size(11.0)
                    .color(dim),
            );
        }
    }

    fn profiles_ui(
        &self,
        ui: &mut egui::Ui,
        look: &Look,
        baud_ok: bool,
        event: &mut Option<Event>,
    ) {
        let save_width = 58.0;
        let gap = 8.0;
        ui.horizontal(|ui| {
            label_cell(ui, "profile", look);
            let combo_width = ui.available_width().max(140.0);
            if let Some(index) = profile_combo(ui, &self.profiles, self.selected, combo_width, look)
            {
                *event = Some(Event::SelectProfile(index));
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            label_cell(ui, "name", look);
            let mut name = self.name.clone();
            let width = (ui.available_width() - save_width - gap).max(120.0);
            ui.scope(|ui| {
                widget_style(ui, look);
                let response = ui.add(
                    egui::TextEdit::singleline(&mut name)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(width)
                        .hint_text("profile name"),
                );
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::Role::TextInput, true, "profile name")
                });
                if response.changed() {
                    *event = Some(Event::Name(name));
                }
            });
            // Save belongs with the name it writes, not with the list it
            // selects from. An unparseable rate is not a profile worth saving.
            let save = button(
                ui,
                "save",
                false,
                save_width,
                23.0,
                baud_ok && !self.name.trim().is_empty(),
                look,
            );
            if clicked(ui, &save) {
                *event = Some(Event::Save);
            }
        });
    }

    fn line_ui(&self, ui: &mut egui::Ui, look: &Look, event: &mut Option<Event>) {
        section(ui, "line", look);
        ui.add_space(2.0);
        row(ui, "baud", look, |ui| {
            if let Some(asked) = baud_field(ui, &self.baud_text, look) {
                *event = Some(asked);
            }
        });
        row(ui, "data bits", look, |ui| {
            if let Some(data_bits) =
                segmented(ui, self.settings.data_bits, &DATA_BITS, "data bits", look)
            {
                *event = Some(Event::Settings(Settings {
                    data_bits,
                    ..self.settings.clone()
                }));
            }
        });
        row(ui, "parity", look, |ui| {
            if let Some(parity) = segmented(ui, self.settings.parity, &PARITIES, "parity", look) {
                *event = Some(Event::Settings(Settings {
                    parity,
                    ..self.settings.clone()
                }));
            }
        });
        row(ui, "stop bits", look, |ui| {
            if let Some(stop_bits) =
                segmented(ui, self.settings.stop_bits, &STOP_BITS, "stop bits", look)
            {
                *event = Some(Event::Settings(Settings {
                    stop_bits,
                    ..self.settings.clone()
                }));
            }
        });
        row(ui, "flow control", look, |ui| {
            if let Some(flow_control) =
                segmented(ui, self.settings.flow_control, &FLOWS, "flow control", look)
            {
                *event = Some(Event::Settings(Settings {
                    flow_control,
                    ..self.settings.clone()
                }));
            }
        });
    }

    fn signals_ui(&self, ui: &mut egui::Ui, look: &Look, event: &mut Option<Event>) {
        section(ui, "signals at open", look);
        ui.add_space(2.0);
        let mut settings = self.settings.clone();
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            changed |= check(ui, &mut settings.dtr, "DTR", look);
            changed |= check(ui, &mut settings.rts, "RTS", look);
        });
        if changed {
            *event = Some(Event::Settings(settings));
        }
    }

    fn terminal_ui(&self, ui: &mut egui::Ui, look: &Look, event: &mut Option<Event>) {
        section(ui, "terminal", look);
        ui.add_space(2.0);
        let mut settings = self.settings.clone();
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            changed |= check(ui, &mut settings.cr_to_crlf, "CR to CRLF", look);
            changed |= check(ui, &mut settings.auto_reconnect, "auto reconnect", look);
        });
        if changed {
            *event = Some(Event::Settings(settings));
        }
    }

    fn footer_ui(&self, ui: &mut egui::Ui, look: &Look, baud_ok: bool, event: &mut Option<Event>) {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("enter connect   esc cancel")
                    .monospace()
                    .size(10.0)
                    .color(Look::colour(look.dim)),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let connect = button(ui, "connect", true, 86.0, 26.0, baud_ok, look);
                if baud_ok && clicked(ui, &connect) {
                    let mut settings = self.settings.clone();
                    if let Some(baud) = self.baud() {
                        settings.baud = baud;
                    }
                    *event = Some(Event::Connect(settings));
                }
                ui.add_space(6.0);
                let cancel = button(ui, "cancel", false, 70.0, 26.0, true, look);
                if clicked(ui, &cancel) {
                    *event = Some(Event::Cancel);
                }
            });
        });
    }
}

fn parse_baud(text: &str) -> Option<u32> {
    text.parse::<u32>().ok().filter(|baud| *baud > 0)
}

const DATA_BITS: [(DataBits, &str); 4] = [
    (DataBits::Five, "5"),
    (DataBits::Six, "6"),
    (DataBits::Seven, "7"),
    (DataBits::Eight, "8"),
];

const PARITIES: [(Parity, &str); 3] = [
    (Parity::None, "none"),
    (Parity::Odd, "odd"),
    (Parity::Even, "even"),
];

const STOP_BITS: [(StopBits, &str); 2] = [(StopBits::One, "1"), (StopBits::Two, "2")];

const FLOWS: [(FlowControl, &str); 3] = [
    (FlowControl::None, "none"),
    (FlowControl::Software, "software"),
    (FlowControl::Hardware, "hardware"),
];

/* ------------------------------------------------------------------------- *
 * Chrome
 * ------------------------------------------------------------------------- */

/// Wide enough for the four controls that need it and no wider: a sheet that
/// covers half the window hides the session it is about to join.
const WIDTH: f32 = 560.0;
/// The primed height: the content it is measured to hold. Priming with the
/// right number is what centers the card instead of letting the first frame
/// grow it downward, and it is what keeps a one-frame capture from showing a
/// sheet that is still measuring itself.
const HEIGHT: f32 = 530.0;
const LABEL_WIDTH: f32 = 82.0;
const ROW_HEIGHT: f32 = 23.0;
const ROW_GAP: f32 = 5.0;

fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> egui::Color32 {
    let blend = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    egui::Color32::from_rgb(blend(a[0], b[0]), blend(a[1], b[1]), blend(a[2], b[2]))
}

fn clicked(ui: &egui::Ui, response: &egui::Response) -> bool {
    // A focused click-sensing widget is activated by Enter too, and Enter is
    // the sheet's own key. Only the pointer is a click here.
    response.clicked() && ui.input(|input| input.pointer.any_click())
}

fn label_cell(ui: &mut egui::Ui, text: &str, look: &Look) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(LABEL_WIDTH, ROW_HEIGHT), egui::Sense::hover());
    let colour = Look::colour(look.dim);
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_owned(), egui::FontId::monospace(11.0), colour);
    ui.painter().galley(
        egui::pos2(rect.left(), rect.center().y - galley.size().y / 2.0),
        galley,
        colour,
    );
}

fn row(ui: &mut egui::Ui, label: &str, look: &Look, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        label_cell(ui, label, look);
        add(ui);
    });
    ui.add_space(ROW_GAP);
}

fn section(ui: &mut egui::Ui, title: &str, look: &Look) {
    let dim = Look::colour(look.dim);
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 20.0), egui::Sense::hover());
    let galley = ui
        .painter()
        .layout_no_wrap(title.to_owned(), egui::FontId::monospace(10.5), dim);
    let label_width = galley.size().x;
    ui.painter().galley(
        egui::pos2(rect.left(), rect.center().y - galley.size().y / 2.0),
        galley,
        dim,
    );
    let left = rect.left() + label_width + 10.0;
    ui.painter().line_segment(
        [egui::pos2(left, rect.center().y), rect.right_center()],
        egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(dim.r(), dim.g(), dim.b(), 46),
        ),
    );
}

/// A segmented choice: one rounded chip per option, the selected one carrying
/// the accent. The chips are painted rather than built from `Button`s so the
/// selected state can be a fill plus a border instead of the widget theme's
/// idea of a pressed button.
fn segmented<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    value: T,
    options: &[(T, &'static str)],
    group: &str,
    look: &Look,
) -> Option<T> {
    let mut picked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (option, label) in options {
            let selected = *option == value;
            let (rect, response) = ui.allocate_exact_size(
                egui::vec2(chip_width(ui, label), ROW_HEIGHT),
                egui::Sense::click(),
            );
            let hovered = response.hovered();
            let fill = if selected {
                mix(look.surface, look.accent, 0.16)
            } else if hovered {
                mix(look.surface, look.foreground, 0.12)
            } else {
                mix(look.surface, look.foreground, 0.055)
            };
            let border = if selected {
                egui::Stroke::new(
                    1.0,
                    egui::Color32::from_rgba_unmultiplied(
                        look.accent[0],
                        look.accent[1],
                        look.accent[2],
                        170,
                    ),
                )
            } else if hovered {
                egui::Stroke::new(1.0, mix(look.surface, look.foreground, 0.22))
            } else {
                egui::Stroke::NONE
            };
            let colour = if selected || hovered {
                Look::colour(look.foreground)
            } else {
                Look::colour(look.dim)
            };
            let galley = ui.painter().layout_no_wrap(
                (*label).to_owned(),
                egui::FontId::monospace(11.5),
                colour,
            );
            let painter = ui.painter();
            painter.rect_filled(rect, 4.0, fill);
            if border != egui::Stroke::NONE {
                painter.rect_stroke(rect, 4.0, border, egui::StrokeKind::Inside);
            }
            painter.galley(
                egui::pos2(
                    rect.center().x - galley.size().x / 2.0,
                    rect.center().y - galley.size().y / 2.0,
                ),
                galley,
                colour,
            );
            let name = format!("{group} {label}");
            response.widget_info(|| {
                egui::WidgetInfo::selected(egui::Role::RadioButton, true, selected, &name)
            });
            if response.has_focus() {
                response.surrender_focus();
            }
            if clicked(ui, &response) {
                picked = Some(*option);
            }
        }
    });
    picked
}

fn chip_width(ui: &egui::Ui, label: &str) -> f32 {
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        egui::FontId::monospace(11.5),
        egui::Color32::WHITE,
    );
    galley.size().x + 22.0
}

fn check(ui: &mut egui::Ui, value: &mut bool, label: &str, look: &Look) -> bool {
    let foreground = Look::colour(look.foreground);
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_owned(), egui::FontId::monospace(11.5), foreground);
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(14.0 + 7.0 + galley.size().x, 22.0),
        egui::Sense::click(),
    );
    let box_rect = egui::Rect::from_min_size(
        egui::pos2(rect.left(), rect.center().y - 7.0),
        egui::vec2(14.0, 14.0),
    );
    let hovered = response.hovered();
    let fill = if *value {
        Look::colour(look.accent)
    } else {
        mix(look.surface, look.foreground, 0.05)
    };
    let border = if *value {
        Look::colour(look.accent)
    } else if hovered {
        mix(look.surface, look.foreground, 0.30)
    } else {
        mix(look.surface, look.foreground, 0.16)
    };
    let painter = ui.painter();
    painter.rect_filled(box_rect, 3.0, fill);
    painter.rect_stroke(
        box_rect,
        3.0,
        egui::Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
    let text_colour = if hovered {
        foreground
    } else {
        mix(look.foreground, look.dim, 0.35)
    };
    painter.galley(
        egui::pos2(
            box_rect.right() + 7.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        text_colour,
    );
    if *value {
        // Lucide's check, embedded and tinted to the surface: a white mask is
        // the only kind `Image::tint` can colour.
        egui::Image::new(egui::include_image!("../assets/icons/check.svg"))
            .fit_to_exact_size(egui::vec2(10.0, 10.0))
            .tint(Look::colour(look.surface))
            .paint_at(
                ui,
                egui::Rect::from_center_size(box_rect.center(), egui::vec2(10.0, 10.0)),
            );
    }
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, *value, label));
    if response.has_focus() {
        response.surrender_focus();
    }
    if clicked(ui, &response) {
        *value = !*value;
        true
    } else {
        false
    }
}

fn close_cross(ui: &mut egui::Ui, look: &Look) -> bool {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::click());
    let hovered = response.hovered();
    if hovered {
        ui.painter()
            .rect_filled(rect, 4.0, mix(look.surface, look.foreground, 0.10));
    }
    egui::Image::new(egui::include_image!("../assets/icons/x.svg"))
        .fit_to_exact_size(egui::vec2(12.0, 12.0))
        .tint(if hovered {
            Look::colour(look.foreground)
        } else {
            Look::colour(look.dim)
        })
        .paint_at(
            ui,
            egui::Rect::from_center_size(rect.center(), egui::vec2(12.0, 12.0)),
        );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Button, true, "close serial connection")
    });
    if response.has_focus() {
        response.surrender_focus();
    }
    clicked(ui, &response)
}

fn button(
    ui: &mut egui::Ui,
    label: &str,
    primary: bool,
    width: f32,
    height: f32,
    enabled: bool,
    look: &Look,
) -> egui::Response {
    let foreground = Look::colour(look.foreground);
    let text_colour = if primary {
        Look::colour(look.background)
    } else if enabled {
        foreground
    } else {
        // A disabled save is still readable: "there is nothing to save yet" is
        // information, not a decoration.
        mix(look.dim, look.surface, 0.35)
    };
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        egui::FontId::monospace(if primary { 12.0 } else { 11.5 }),
        text_colour,
    );
    let size = egui::vec2(width.max(galley.size().x + 22.0), height);
    let (rect, response) = ui.allocate_exact_size(
        size,
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    let hovered = enabled && response.hovered();
    let painter = ui.painter();
    if primary {
        let fill = if hovered {
            mix(look.accent, look.foreground, 0.10)
        } else {
            Look::colour(look.accent)
        };
        painter.rect_filled(rect, 5.0, fill);
    } else {
        // A quiet button still has to look like a control. A bare word next to
        // a filled one reads as a label, and "save" especially has to be a
        // thing you can press.
        let fill = if !enabled {
            mix(look.surface, look.foreground, 0.03)
        } else if hovered {
            mix(look.surface, look.foreground, 0.13)
        } else {
            mix(look.surface, look.foreground, 0.06)
        };
        painter.rect_filled(rect, 5.0, fill);
        painter.rect_stroke(
            rect,
            5.0,
            egui::Stroke::new(
                1.0,
                mix(
                    look.surface,
                    look.foreground,
                    if enabled { 0.16 } else { 0.09 },
                ),
            ),
            egui::StrokeKind::Inside,
        );
    }
    painter.galley(
        egui::pos2(
            rect.center().x - galley.size().x / 2.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        text_colour,
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
    if response.has_focus() {
        response.surrender_focus();
    }
    response
}

fn chevron(ui: &egui::Ui, rect: egui::Rect, visuals: &egui::style::WidgetVisuals, _open: bool) {
    egui::Image::new(egui::include_image!("../assets/icons/chevron-down.svg"))
        .fit_to_exact_size(egui::vec2(11.0, 11.0))
        .tint(visuals.fg_stroke.color)
        .paint_at(
            ui,
            egui::Rect::from_center_size(rect.center(), egui::vec2(11.0, 11.0)),
        );
}

/// The combo boxes and the name field are egui widgets, so they take their
/// colours from a style rather than from our painter. This is that style: the
/// same surface/foreground palette the hand-painted controls use, with the
/// accent only where a choice is made.
fn widget_style(ui: &mut egui::Ui, look: &Look) {
    let mut style = ui.style().as_ref().clone();
    let raised = mix(look.surface, look.foreground, 0.07);
    let raised_hover = mix(look.surface, look.foreground, 0.13);
    let border = mix(look.surface, look.foreground, 0.16);
    let accent = Look::colour(look.accent);
    let background = Look::colour(look.background);
    let foreground = Look::colour(look.foreground);
    let dim = Look::colour(look.dim);

    let widgets = &mut style.visuals.widgets;
    for widget in [
        &mut widgets.inactive,
        &mut widgets.hovered,
        &mut widgets.active,
        &mut widgets.open,
    ] {
        widget.bg_stroke = egui::Stroke::new(1.0, border);
        widget.corner_radius = egui::CornerRadius::same(4);
    }
    widgets.inactive.bg_fill = raised;
    widgets.inactive.weak_bg_fill = raised;
    widgets.inactive.fg_stroke = egui::Stroke::new(1.0, dim);
    widgets.hovered.bg_fill = raised_hover;
    widgets.hovered.weak_bg_fill = raised_hover;
    widgets.hovered.bg_stroke = egui::Stroke::new(1.0, mix(look.surface, look.foreground, 0.28));
    widgets.hovered.fg_stroke = egui::Stroke::new(1.0, foreground);
    widgets.active.bg_fill = mix(look.surface, look.accent, 0.16);
    widgets.active.weak_bg_fill = mix(look.surface, look.accent, 0.16);
    widgets.active.fg_stroke = egui::Stroke::new(1.0, foreground);
    widgets.open.bg_fill = mix(look.surface, look.foreground, 0.10);
    widgets.open.weak_bg_fill = mix(look.surface, look.foreground, 0.10);
    widgets.open.fg_stroke = egui::Stroke::new(1.0, foreground);

    style.visuals.extreme_bg_color = background;
    style.visuals.selection.bg_fill = accent;
    style.visuals.selection.stroke = egui::Stroke::new(1.0, background);
    style.visuals.override_text_color = Some(foreground);
    style.visuals.text_cursor.stroke.color = accent;
    style.spacing.interact_size.y = ROW_HEIGHT;
    style.spacing.button_padding = egui::vec2(8.0, 3.0);
    ui.set_style(style);
}

/// A typed baud rate with the standard rates behind its chevron.
///
/// A combo box cannot be typed into, and a drag value cannot offer the list;
/// this is one control that does both. The text is kept even while it does not
/// parse, so a half-typed rate is visible rather than silently reverted.
fn baud_field(ui: &mut egui::Ui, text: &str, look: &Look) -> Option<Event> {
    let mut picked = None;
    ui.scope(|ui| {
        widget_style(ui, look);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(150.0, ROW_HEIGHT), egui::Sense::hover());
        let arrow_width = 22.0;
        let arrow_rect =
            egui::Rect::from_min_max(egui::pos2(rect.right() - arrow_width, rect.top()), rect.max);
        let text_rect = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 1.0, rect.top() + 1.0),
            egui::pos2(arrow_rect.left() - 2.0, rect.bottom() - 1.0),
        );
        let valid = parse_baud(text).is_some();
        let hovered = ui.rect_contains_pointer(rect);
        let fill = if hovered {
            mix(look.surface, look.foreground, 0.10)
        } else {
            mix(look.surface, look.foreground, 0.07)
        };
        let border = if !valid {
            Look::colour(look.accent)
        } else if hovered {
            mix(look.surface, look.foreground, 0.28)
        } else {
            mix(look.surface, look.foreground, 0.16)
        };
        let painter = ui.painter();
        painter.rect_filled(rect, 4.0, fill);
        painter.rect_stroke(
            rect,
            4.0,
            egui::Stroke::new(1.0, border),
            egui::StrokeKind::Inside,
        );

        let mut buffer = text.to_owned();
        let response = ui.put(
            text_rect.shrink2(egui::vec2(7.0, 2.0)),
            egui::TextEdit::singleline(&mut buffer)
                .frame(egui::Frame::NONE)
                .font(egui::TextStyle::Monospace)
                .desired_width((text_rect.width() - 14.0).max(40.0)),
        );
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "baud value"));
        if response.changed() {
            buffer.retain(|character| character.is_ascii_digit());
            if buffer.len() > 9 {
                buffer.truncate(9);
            }
            picked = Some(Event::Baud(buffer));
        }

        let arrow = ui.interact(
            arrow_rect,
            egui::Id::unique("dtr-serial-baud-defaults"),
            egui::Sense::click(),
        );
        if arrow.hovered() {
            ui.painter().rect_filled(
                arrow_rect.shrink(2.0),
                3.0,
                mix(look.surface, look.foreground, 0.12),
            );
        }
        egui::Image::new(egui::include_image!("../assets/icons/chevron-down.svg"))
            .fit_to_exact_size(egui::vec2(11.0, 11.0))
            .tint(if arrow.hovered() {
                Look::colour(look.foreground)
            } else {
                Look::colour(look.dim)
            })
            .paint_at(
                ui,
                egui::Rect::from_center_size(arrow_rect.center(), egui::vec2(11.0, 11.0)),
            );
        arrow
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "baud defaults"));
        if arrow.has_focus() {
            arrow.surrender_focus();
        }
        egui::Popup::menu(&arrow).show(|ui| {
            ui.set_min_width(120.0);
            for &baud in BAUDS {
                if ui.button(baud.to_string()).clicked() {
                    picked = Some(Event::Baud(baud.to_string()));
                    ui.close();
                }
            }
        });
    });
    picked
}

fn profile_combo(
    ui: &mut egui::Ui,
    profiles: &[Profile],
    selected: Option<usize>,
    width: f32,
    look: &Look,
) -> Option<usize> {
    let current = selected
        .and_then(|index| profiles.get(index))
        .map(|profile| profile.name.as_str())
        .unwrap_or("no profile");
    let mut picked = None;
    ui.scope(|ui| {
        widget_style(ui, look);
        let response = egui::ComboBox::from_id_salt("serial-profile")
            .selected_text(egui::RichText::new(current).monospace().size(11.5))
            .width(width)
            .icon(chevron)
            .show_ui(ui, |ui| {
                ui.set_min_width(width);
                if profiles.is_empty() {
                    ui.label(
                        egui::RichText::new("no profiles yet")
                            .monospace()
                            .size(11.0)
                            .color(Look::colour(look.dim)),
                    );
                }
                for (index, profile) in profiles.iter().enumerate() {
                    if ui
                        .selectable_label(selected == Some(index), &profile.name)
                        .clicked()
                    {
                        picked = Some(index);
                    }
                }
            });
        response
            .response
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "profile value"));
    });
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_default_is_8n1_at_115200() {
        let settings = Settings::default();
        assert_eq!(settings.summary(), "115200 8N1");
    }

    #[test]
    fn the_line_code_spells_parity_and_stop_bits() {
        let settings = Settings {
            baud: 9_600,
            data_bits: DataBits::Seven,
            parity: Parity::Even,
            stop_bits: StopBits::Two,
            ..Settings::default()
        };
        assert_eq!(settings.summary(), "9600 7E2");
    }

    #[test]
    fn saving_the_same_name_updates_rather_than_duplicates() {
        let mut dialog = SerialDialog::new("/dev/ttyUSB0", "", Settings::default());
        dialog.name = "bench".to_owned();
        dialog.settings.baud = 9_600;
        dialog.save();
        assert_eq!(dialog.profiles.len(), 1);
        assert_eq!(dialog.profiles[0].settings.baud, 9_600);

        dialog.settings.baud = 57_600;
        dialog.save();
        assert_eq!(dialog.profiles.len(), 1);
        assert_eq!(dialog.profiles[0].settings.baud, 57_600);
        assert_eq!(dialog.selected, Some(0));
        assert_eq!(dialog.settings.profile.as_deref(), Some("bench"));
    }

    #[test]
    fn an_empty_name_saves_nothing() {
        let mut dialog = SerialDialog::new("/dev/ttyUSB0", "", Settings::default());
        dialog.name = "   ".to_owned();
        dialog.save();
        assert!(dialog.profiles.is_empty());
        assert_eq!(dialog.selected, None);
    }

    #[test]
    fn selecting_a_profile_replaces_the_whole_form() {
        let mut dialog = SerialDialog::preview("/dev/ttyUSB0");
        dialog.select(0);
        assert_eq!(dialog.settings, dialog.profiles[0].settings);
        assert_eq!(dialog.name, "bench board");
        assert_eq!(dialog.selected, Some(0));
    }

    #[test]
    fn for_port_prefills_the_remembered_values_and_profile() {
        let profiles = vec![Profile {
            name: "gps".to_owned(),
            settings: Settings {
                baud: 9_600,
                parity: Parity::Even,
                profile: Some("gps".to_owned()),
                ..Settings::default()
            },
        }];
        let remembered = Settings {
            baud: 57_600,
            data_bits: DataBits::Seven,
            parity: Parity::Even,
            profile: Some("gps".to_owned()),
            ..Settings::default()
        };
        let dialog = SerialDialog::for_port("/dev/ttyUSB0", "", Some(remembered.clone()), profiles);
        assert_eq!(dialog.settings, remembered);
        assert_eq!(dialog.selected, Some(0));
        assert_eq!(dialog.name, "gps");
        assert_eq!(dialog.baud_text, "57600");
    }

    #[test]
    fn from_store_prefills_the_remembered_form_and_profile() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let store = dtr_store::Store::open(&dir.path().join("dtr.redb")).expect("open");
        let settings = Settings {
            baud: 57_600,
            parity: Parity::Even,
            profile: Some("gps".to_owned()),
            ..Settings::default()
        };
        store
            .remember_serial("/dev/ttyUSB0", &settings)
            .expect("remember");
        store.save_profile("gps", &settings).expect("profile");

        let dialog = SerialDialog::from_store("/dev/ttyUSB0", "FTDI", Some(&store));
        assert_eq!(dialog.settings, settings);
        assert_eq!(dialog.selected, Some(0));
        assert_eq!(dialog.name, "gps");
        assert_eq!(dialog.baud_text, "57600");
    }

    #[test]
    fn a_remembered_profile_that_is_gone_is_not_selected() {
        let remembered = Settings {
            profile: Some("missing".to_owned()),
            ..Settings::default()
        };
        let dialog = SerialDialog::for_port("/dev/ttyUSB0", "", Some(remembered), Vec::new());
        assert_eq!(dialog.selected, None);
        assert_eq!(dialog.settings.profile, None);
        assert_eq!(dialog.name, "missing");
    }

    #[test]
    fn a_partial_baud_stays_in_the_field_and_keeps_the_last_valid_rate() {
        let mut dialog = SerialDialog::new("/dev/ttyUSB0", "", Settings::default());
        dialog.apply(Event::Baud("9".to_owned()));
        assert_eq!(dialog.baud_text, "9");
        assert_eq!(dialog.settings.baud, 9);
        dialog.apply(Event::Baud(String::new()));
        assert_eq!(dialog.settings.baud, 9);
        assert_eq!(dialog.baud(), None);
    }
}

#[cfg(test)]
mod kittest_tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    fn harness(
        dialog: SerialDialog,
    ) -> (
        Harness<'static>,
        std::rc::Rc<std::cell::RefCell<Option<Event>>>,
    ) {
        let look = Look::default();
        let seen = std::rc::Rc::new(std::cell::RefCell::new(None));
        let sink = seen.clone();
        let harness = Harness::new_ui(move |ui| {
            if let Some(event) = dialog.ui(ui, &look) {
                *sink.borrow_mut() = Some(event);
            }
        });
        // The sheet draws Lucide icons; without the loader egui paints its
        // missing-image glyph, which its own fonts cannot render either.
        crate::ui::install_image_loaders(&harness.ctx);
        (harness, seen)
    }

    #[test]
    fn picking_a_profile_reports_it() {
        let (mut harness, seen) = harness(SerialDialog::preview("/dev/ttyUSB0"));
        harness.run();
        harness.get_by_label("profile value").click();
        harness.run();
        harness.get_by_label("bench board").click();
        harness.run();
        assert_eq!(*seen.borrow(), Some(Event::SelectProfile(0)));
    }

    #[test]
    fn a_segment_reports_the_whole_new_form() {
        let (mut harness, seen) = harness(SerialDialog::preview("/dev/ttyUSB0"));
        harness.run();
        harness.get_by_label("parity odd").click();
        harness.run();
        match seen.borrow().clone() {
            Some(Event::Settings(settings)) => assert_eq!(settings.parity, Parity::Odd),
            other => panic!("expected a settings edit, got {other:?}"),
        }
    }

    #[test]
    fn a_signal_is_a_checkbox_with_a_name() {
        let (mut harness, seen) =
            harness(SerialDialog::new("/dev/ttyUSB0", "", Settings::default()));
        harness.run();
        harness.get_by_label("DTR").click();
        harness.run();
        match seen.borrow().clone() {
            Some(Event::Settings(settings)) => assert!(!settings.dtr),
            other => panic!("expected the DTR toggle, got {other:?}"),
        }
    }

    #[test]
    fn a_default_baud_can_be_picked_from_the_chevron() {
        let (mut harness, seen) =
            harness(SerialDialog::new("/dev/ttyUSB0", "", Settings::default()));
        harness.run();
        harness.get_by_label("baud defaults").click();
        harness.run();
        harness.get_by_label("9600").click();
        harness.run();
        assert_eq!(*seen.borrow(), Some(Event::Baud("9600".to_owned())));
    }

    #[test]
    fn connect_carries_the_typed_rate() {
        let mut dialog = SerialDialog::new("/dev/ttyUSB0", "", Settings::default());
        dialog.apply(Event::Baud("9600".to_owned()));
        let (mut harness, seen) = harness(dialog);
        harness.run();
        harness.get_by_label("connect").click();
        harness.run();
        match seen.borrow().clone() {
            Some(Event::Connect(settings)) => assert_eq!(settings.baud, 9_600),
            other => panic!("expected connect, got {other:?}"),
        }
    }

    /// The sheet with a populated profile list, one profile selected and a
    /// non-default line: the state that shows the chrome doing its job.
    #[test]
    fn the_sheet_renders() {
        let dialog = SerialDialog::preview("/dev/ttyUSB0");
        let look = Look::default();
        let mut harness = Harness::new_ui(move |ui| {
            dialog.ui(ui, &look);
        });
        crate::ui::install_image_loaders(&harness.ctx);
        harness.run();
        harness.snapshot("serial_dialog");
    }
}
