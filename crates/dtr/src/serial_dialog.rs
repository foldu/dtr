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
use dtr_widgets::{Button, Edit, Editable, Footer, Skin, check, cross, row, section, segmented};

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
        let skin = look.skin();
        let p = skin.palette;
        let s = &skin.style;
        let screen = ui.max_rect();
        let width = (WIDTH.min(screen.width() - 32.0)).max(360.0);
        let height = (HEIGHT.min(screen.height() - 32.0)).max(320.0);
        let rect = egui::Rect::from_center_size(screen.center(), egui::vec2(width, height));
        let id = egui::Id::new("dtr-serial-dialog");
        crate::ui::prime_area(ui.ctx(), id, egui::Order::Foreground, rect, true);

        // The terminal is not the subject while this is up. Same weight as the
        // settings menu: enough to read the sheet against, not so much that the
        // window looks disabled.
        ui.painter().rect_filled(screen, 0.0, p.scrim);

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
                // The sheet is the one place stock egui widgets still appear (the
                // text field inside `Editable`), so it hands them the skin's style
                // rather than inheriting whatever the frame was using.
                skin.apply(ui.ctx());
                ui.set_min_size(rect.size());
                egui::Frame::new()
                    .fill(p.card)
                    .stroke(egui::Stroke::new(s.border, p.border))
                    .corner_radius(s.card_radius)
                    .inner_margin(s.card_margin)
                    .show(ui, |ui| {
                        ui.set_width((width - s.card_margin.sum().x).max(320.0));
                        self.header(ui, &skin, &mut event);
                        ui.add_space(s.gap * 3.0);
                        self.profiles_ui(ui, &skin, baud_ok, &mut event);
                        ui.add_space(s.gap * 2.0);
                        self.line_ui(ui, &skin, &mut event);
                        ui.add_space(s.gap);
                        self.signals_ui(ui, &skin, &mut event);
                        ui.add_space(s.gap);
                        self.terminal_ui(ui, &skin, &mut event);
                        ui.add_space(s.gap * 3.0);
                        self.footer_ui(ui, &skin, baud_ok, &mut event);
                    });
            });
        event
    }

    fn header(&self, ui: &mut egui::Ui, skin: &Skin, event: &mut Option<Event>) {
        let p = skin.palette;
        let s = &skin.style;
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("serial connection")
                    .font(s.label_font.clone())
                    .color(p.dim),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if cross(ui, skin, "close serial connection", 24.0) {
                    *event = Some(Event::Cancel);
                }
            });
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(&self.port)
                    .font(s.title_font.clone())
                    .color(p.text),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(self.settings.summary())
                        .font(egui::FontId::new(
                            s.label_font.size - 1.0,
                            s.label_font.family.clone(),
                        ))
                        .color(p.dim),
                );
            });
        });
        if !self.detail.is_empty() {
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(&self.detail)
                    .font(egui::FontId::new(
                        s.label_font.size - 1.0,
                        s.label_font.family.clone(),
                    ))
                    .color(p.dim),
            );
        }
    }

    /// The profile and its name are one control: type a name, or pick a saved
    /// one. They were two rows — a combo box and a field — because a combo box
    /// cannot be typed into, and picking a saved profile is precisely what
    /// replaces the name.
    fn profiles_ui(
        &self,
        ui: &mut egui::Ui,
        skin: &Skin,
        baud_ok: bool,
        event: &mut Option<Event>,
    ) {
        let save_width = 58.0;
        let names: Vec<&str> = self.profiles.iter().map(|p| p.name.as_str()).collect();
        row(ui, skin, "profile", |ui| {
            let width = (ui.available_width() - save_width - skin.style.gap).max(140.0);
            let edit = Editable::new("profile", &self.name, "profile value")
                .width(width)
                .show(ui, skin, &names);
            match edit {
                // A pick loads that profile; a typed character is the name being
                // written, and a name that happens to equal a saved one's must not
                // silently load it.
                Some(Edit::Picked(name)) => {
                    if let Some(index) = self.profiles.iter().position(|p| p.name == name) {
                        *event = Some(Event::SelectProfile(index));
                    }
                }
                Some(Edit::Typed(text)) => *event = Some(Event::Name(text)),
                None => {}
            }
            // Save belongs with the name it writes, not with the list it selects
            // from. An unparseable rate is not a profile worth saving.
            if Button::new("save")
                .width(save_width)
                .enabled(baud_ok && !self.name.trim().is_empty())
                .show(ui, skin)
            {
                *event = Some(Event::Save);
            }
        });
    }

    fn line_ui(&self, ui: &mut egui::Ui, skin: &Skin, event: &mut Option<Event>) {
        section(ui, skin, "line");
        ui.add_space(skin.style.gap);
        row(ui, skin, "baud", |ui| {
            let choices: Vec<String> = BAUDS.iter().map(u32::to_string).collect();
            // A half-typed rate stays in the field and says so with the border,
            // rather than being reverted under the cursor.
            let edit = Editable::new("baud", &self.baud_text, "baud value")
                .width(150.0)
                .valid(self.baud().is_some())
                .show(ui, skin, &choices);
            if let Some(edit) = edit {
                *event = Some(Event::Baud(match edit {
                    Edit::Typed(text) | Edit::Picked(text) => text,
                }));
            }
        });
        row(ui, skin, "data bits", |ui| {
            if let Some(data_bits) =
                segmented(ui, skin, "data bits", self.settings.data_bits, &DATA_BITS)
            {
                *event = Some(Event::Settings(Settings {
                    data_bits,
                    ..self.settings.clone()
                }));
            }
        });
        row(ui, skin, "parity", |ui| {
            if let Some(parity) = segmented(ui, skin, "parity", self.settings.parity, &PARITIES) {
                *event = Some(Event::Settings(Settings {
                    parity,
                    ..self.settings.clone()
                }));
            }
        });
        row(ui, skin, "stop bits", |ui| {
            if let Some(stop_bits) =
                segmented(ui, skin, "stop bits", self.settings.stop_bits, &STOP_BITS)
            {
                *event = Some(Event::Settings(Settings {
                    stop_bits,
                    ..self.settings.clone()
                }));
            }
        });
        row(ui, skin, "flow control", |ui| {
            if let Some(flow_control) =
                segmented(ui, skin, "flow control", self.settings.flow_control, &FLOWS)
            {
                *event = Some(Event::Settings(Settings {
                    flow_control,
                    ..self.settings.clone()
                }));
            }
        });
    }

    fn signals_ui(&self, ui: &mut egui::Ui, skin: &Skin, event: &mut Option<Event>) {
        section(ui, skin, "signals at open");
        ui.add_space(skin.style.gap);
        let mut settings = self.settings.clone();
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            if let Some(on) = check(ui, skin, settings.dtr, "DTR") {
                settings.dtr = on;
                changed = true;
            }
            if let Some(on) = check(ui, skin, settings.rts, "RTS") {
                settings.rts = on;
                changed = true;
            }
        });
        if changed {
            *event = Some(Event::Settings(settings));
        }
    }

    fn terminal_ui(&self, ui: &mut egui::Ui, skin: &Skin, event: &mut Option<Event>) {
        section(ui, skin, "terminal");
        ui.add_space(skin.style.gap);
        let mut settings = self.settings.clone();
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            if let Some(on) = check(ui, skin, settings.cr_to_crlf, "CR to CRLF") {
                settings.cr_to_crlf = on;
                changed = true;
            }
            if let Some(on) = check(ui, skin, settings.auto_reconnect, "auto reconnect") {
                settings.auto_reconnect = on;
                changed = true;
            }
        });
        if changed {
            *event = Some(Event::Settings(settings));
        }
    }

    fn footer_ui(&self, ui: &mut egui::Ui, skin: &Skin, baud_ok: bool, event: &mut Option<Event>) {
        let p = skin.palette;
        let s = &skin.style;
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("enter connect   esc cancel")
                    .font(egui::FontId::new(
                        s.label_font.size - 1.5,
                        s.label_font.family.clone(),
                    ))
                    .color(p.dim),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // `right_to_left` lays out from the corner inwards, so the first
                // one added is the rightmost: which is the primary's side is the
                // style's answer, not this function's.
                let primary = Button::new("connect")
                    .primary()
                    .width(86.0)
                    .enabled(baud_ok);
                let secondary = Button::new("cancel").width(70.0);
                let (connect, cancel) = match s.footer {
                    Footer::PrimaryLast => {
                        let connect = primary.show(ui, skin);
                        ui.add_space(s.gap);
                        (connect, secondary.show(ui, skin))
                    }
                    Footer::PrimaryFirst => {
                        let cancel = secondary.show(ui, skin);
                        ui.add_space(s.gap);
                        (primary.show(ui, skin), cancel)
                    }
                };
                if cancel {
                    *event = Some(Event::Cancel);
                } else if connect && baud_ok {
                    let mut settings = self.settings.clone();
                    if let Some(baud) = self.baud() {
                        settings.baud = baud;
                    }
                    *event = Some(Event::Connect(settings));
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
const HEIGHT: f32 = 480.0;

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
        harness.get_by_label("profile value choices").click();
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
        harness.get_by_label("baud value choices").click();
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
