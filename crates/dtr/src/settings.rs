//! The settings menu: every field in the config file, editable in the window.
//!
//! Rows are derived from [`dtr_config::Config`] rather than hand-written, so a
//! key added to the file has one obvious place to appear here. Edits are kept in
//! a draft and only reach the file (and the running terminal) on Enter, which
//! makes Esc an actual cancel rather than a rollback.
//!
//! Everything applies live, per the rule the user set: a serial setting that can
//! only take effect on reconnect reconnects, and the pane keeps its scrollback.

use dtr_config::Config;

/// The baud rates worth cycling through. A terminal that offers an arbitrary
/// number would need a text editor; a serial console uses one of these.
const BAUDS: &[u32] = &[
    9_600, 19_200, 38_400, 57_600, 115_200, 230_400, 460_800, 921_600,
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Row {
    FontSize,
    FontFamily,
    Theme,
    Baud,
    CrToCrlf,
    AutoReconnect,
}

impl Row {
    /// Rows that describe the terminal itself, always shown.
    const FILE: [Row; 4] = [
        Row::FontSize,
        Row::FontFamily,
        Row::Theme,
        Row::AutoReconnect,
    ];
    /// Rows that describe one port, shown only while a serial session has focus:
    /// a rate for a port that is not open is a number belonging to nothing.
    const PORT: [Row; 2] = [Row::Baud, Row::CrToCrlf];

    fn label(self) -> &'static str {
        match self {
            Row::FontSize => "size",
            Row::FontFamily => "family",
            Row::Theme => "theme",
            Row::Baud => "baud",
            Row::CrToCrlf => "cr_to_crlf",
            Row::AutoReconnect => "auto_reconnect",
        }
    }

    fn section(self) -> &'static str {
        match self {
            Row::FontSize | Row::FontFamily => "font",
            Row::Theme => "",
            Row::Baud | Row::CrToCrlf | Row::AutoReconnect => "serial",
        }
    }

    /// The key as it appears in the config file, e.g. `serial.baud`. Also the
    /// accessible name of the row's control, which is why it is not test-only.
    fn key(self) -> String {
        match self.section() {
            "" => self.label().to_owned(),
            section => format!("{section}.{}", self.label()),
        }
    }
}

/// What a mouse edit in the menu asks for. The menu renders from `&self`, so a
/// widget reports its edit rather than writing through a borrow the caller
/// cannot lend it.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    Select(usize),
    FontSize(f32),
    Family(Option<String>),
    Theme(Option<String>),
    AutoReconnect(bool),
    Baud(u32),
    CrToCrlf(bool),
}

/// What a key did to the menu.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Effect {
    /// Nothing outside the menu needs to know.
    Changed,
    /// Enter: write the file and apply.
    Apply,
    /// Esc: throw the draft away.
    Cancel,
}

pub struct Settings {
    pub open: bool,
    row: usize,
    draft: Config,
    /// The focused port's remembered settings, when a serial session has focus.
    /// Baud and CR translation are per-device facts, so the menu edits the
    /// store's record for this port — or nothing, if there is no port.
    serial: Option<dtr_store::SerialSettings>,
    port: Option<String>,
    /// Families that passed the probe, loaded once when the menu opens.
    families: Vec<String>,
    themes: Vec<String>,
    /// What the menu opened with, so Esc can restore it.
    original: Config,
}

impl Settings {
    pub fn new() -> Self {
        Self {
            open: false,
            row: 0,
            draft: Config::default(),
            serial: None,
            port: None,
            families: Vec::new(),
            themes: Vec::new(),
            original: Config::default(),
        }
    }

    /// Open on `config`. The font list is probed here rather than at startup:
    /// most runs never open this menu, and probing parses every installed face.
    pub fn show(&mut self, config: &Config) {
        self.show_port(config, None, None);
    }

    /// Open on `config`, editing `port`'s remembered settings as well when a
    /// serial session has focus.
    pub fn show_port(
        &mut self,
        config: &Config,
        port: Option<String>,
        serial: Option<dtr_store::SerialSettings>,
    ) {
        self.port = port;
        self.serial = serial;
        self.open = true;
        self.row = 0;
        self.draft = config.clone();
        self.original = config.clone();
        // Neither list is worth a stall on the click that opened this menu. The
        // family probe is a fontconfig walk (warmed at startup, and started here
        // for a run that got here first) and the theme list a scan of six
        // directories, which is cheap but not free. `refresh_families` fills the
        // families in on the next frame if the probe has not landed yet.
        let started = std::time::Instant::now();
        crate::fonts::warm();
        if self.families.is_empty() {
            self.families = crate::fonts::ready_families().unwrap_or_default();
        }
        if self.themes.is_empty() {
            self.themes = crate::theme::names();
        }
        tracing::debug!(
            elapsed_us = started.elapsed().as_micros() as u64,
            families = self.families.len(),
            themes = self.themes.len(),
            "settings populated"
        );
    }

    /// Take the family list once the startup probe has finished.
    ///
    /// `show` does not wait for the probe — the menu appears in a frame, and the
    /// one thing a click must not do is freeze — so a menu opened in the first
    /// moments of a run shows `default` alone until this fills the list in.
    pub fn refresh_families(&mut self) {
        if self.families.is_empty()
            && let Some(families) = crate::fonts::ready_families()
        {
            self.families = families;
        }
    }

    pub fn draft(&self) -> &Config {
        &self.draft
    }

    /// The rows this menu shows right now.
    pub fn rows(&self) -> Vec<Row> {
        let mut rows = Row::FILE.to_vec();
        if self.serial.is_some() {
            // Between the terminal settings and the reconnect policy, which is
            // where they read as a group.
            for (offset, row) in Row::PORT.iter().enumerate() {
                rows.insert(3 + offset, *row);
            }
        }
        rows
    }

    /// Apply one edit from the mouse to the draft.
    pub fn apply(&mut self, change: Change) {
        match change {
            Change::Select(row) => self.row = row,
            Change::FontSize(size) => self.draft.font.size = size,
            Change::Family(family) => self.draft.font.family = family,
            Change::Theme(theme) => self.draft.theme = theme,
            Change::AutoReconnect(on) => self.draft.serial.auto_reconnect = on,
            Change::Baud(baud) => {
                if let Some(serial) = self.serial.as_mut() {
                    serial.baud = baud;
                }
            }
            Change::CrToCrlf(on) => {
                if let Some(serial) = self.serial.as_mut() {
                    serial.cr_to_crlf = on;
                }
            }
        }
    }

    pub fn current_row(&self) -> Row {
        let rows = self.rows();
        rows[self.row.min(rows.len() - 1)]
    }

    pub fn handle(&mut self, key: &winit::keyboard::Key) -> Option<Effect> {
        use winit::keyboard::Key;
        match key {
            Key::Named(winit::keyboard::NamedKey::Escape) => {
                self.draft = self.original.clone();
                self.open = false;
                return Some(Effect::Cancel);
            }
            Key::Named(winit::keyboard::NamedKey::Enter) => return Some(Effect::Apply),
            Key::Named(winit::keyboard::NamedKey::ArrowDown)
            | Key::Named(winit::keyboard::NamedKey::Tab) => {
                let count = self.rows().len();
                self.row = (self.row + 1) % count;
                return Some(Effect::Changed);
            }
            Key::Named(winit::keyboard::NamedKey::ArrowUp)
            | Key::Named(winit::keyboard::NamedKey::Backspace) => {
                let count = self.rows().len();
                self.row = (self.row + count - 1) % count;
                return Some(Effect::Changed);
            }
            Key::Named(winit::keyboard::NamedKey::ArrowRight) => {
                self.nudge(1);
                return Some(Effect::Changed);
            }
            Key::Named(winit::keyboard::NamedKey::ArrowLeft) => {
                self.nudge(-1);
                return Some(Effect::Changed);
            }
            Key::Character(text) if text == " " || text == "j" || text == "k" => {
                let step = if text == "k" { -1 } else { 1 };
                self.nudge(step);
                return Some(Effect::Changed);
            }
            _ => {}
        }
        None
    }

    /// Move the selected row by one step: +1 forward, -1 back.
    fn nudge(&mut self, step: i32) {
        match self.current_row() {
            Row::FontSize => {
                let next = (self.draft.font.size + step as f32).clamp(6.0, 48.0);
                self.draft.font.size = next;
            }
            Row::FontFamily => self.cycle_family(step),
            Row::Theme => self.cycle_theme(step),
            Row::Baud => {
                let Some(serial) = self.serial.as_mut() else {
                    return;
                };
                let index = BAUDS
                    .iter()
                    .position(|rate| *rate >= serial.baud)
                    .unwrap_or(0) as i32;
                let index = (index + step).clamp(0, BAUDS.len() as i32 - 1) as usize;
                serial.baud = BAUDS[index];
            }
            Row::CrToCrlf => {
                let Some(serial) = self.serial.as_mut() else {
                    return;
                };
                serial.cr_to_crlf = !serial.cr_to_crlf;
            }
            Row::AutoReconnect => {
                self.draft.serial.auto_reconnect = !self.draft.serial.auto_reconnect;
            }
        }
    }

    fn cycle_family(&mut self, step: i32) {
        if self.families.is_empty() {
            return;
        }
        let current = self.draft.font.family.clone().unwrap_or_default();
        let index = self
            .families
            .iter()
            .position(|name| name == &current)
            .map(|index| index as i32)
            .unwrap_or(-step);
        let next = (index + step).rem_euclid(self.families.len() as i32) as usize;
        self.draft.font.family = Some(self.families[next].clone());
    }

    fn cycle_theme(&mut self, step: i32) {
        // One extra entry, before the files, for "no theme".
        let count = self.themes.len() as i32 + 1;
        let current = self.draft.theme.clone();
        let index = match current {
            None => 0,
            Some(name) => self
                .themes
                .iter()
                .position(|theme| theme == &name)
                .map_or(0, |index| index as i32 + 1),
        };
        let next = (index + step).rem_euclid(count);
        self.draft.theme = if next == 0 {
            None
        } else {
            self.themes.get(next as usize - 1).cloned()
        };
    }

    /// The value column, as it should read in the menu.
    #[cfg(test)]
    fn value(&self, row: Row) -> String {
        match row {
            Row::FontSize => format!("{:.0} pt", self.draft.font.size),
            Row::FontFamily => self
                .draft
                .font
                .family
                .clone()
                .or_else(|| crate::fonts::default_family(&self.families))
                .unwrap_or_else(|| "—".to_owned()),
            Row::Theme => self
                .draft
                .theme
                .clone()
                .unwrap_or_else(|| "built-in".to_owned()),
            // No port, no value: the row says so rather than showing a rate that
            // belongs to nothing.
            Row::Baud => match &self.serial {
                Some(serial) => serial.baud.to_string(),
                None => "no port".to_owned(),
            },
            Row::CrToCrlf => match &self.serial {
                Some(serial) => on_off(serial.cr_to_crlf),
                None => "no port".to_owned(),
            },
            Row::AutoReconnect => on_off(self.draft.serial.auto_reconnect),
        }
    }

    /// The lines to write to the config file, as `(key, raw value)`.
    pub fn writes(&self) -> Vec<(String, String)> {
        let mut writes = vec![
            (
                "font.size".to_owned(),
                format!("{:.0}", self.draft.font.size),
            ),
            (
                "serial.auto_reconnect".to_owned(),
                self.draft.serial.auto_reconnect.to_string(),
            ),
        ];
        // An unset family or theme is written as the empty string, which the
        // parser reads back as "none" — better than deleting the user's line.
        writes.push((
            "font.family".to_owned(),
            self.draft.font.family.clone().unwrap_or_default(),
        ));
        writes.push((
            "theme".to_owned(),
            self.draft.theme.clone().unwrap_or_default(),
        ));
        writes
    }
}

impl Settings {
    /// Draw the menu. Called from the same `run_ui` closure as the panes, as an
    /// overlay beside the palette — `egui::Window` renders nothing there.
    #[allow(deprecated)] // one fixed overlay: the id is a name, not a hash of state
    /// Draw the menu. Every control is a mouse control: a row is a click target
    /// for selection, and the values are a drag number, two combo boxes and two
    /// toggles. The keyboard still works, but nothing here needs it.
    pub fn ui(&self, ui: &mut egui::Ui, look: &crate::ui::Look) -> Option<Change> {
        let skin = look.skin();
        let palette = skin.palette;
        let style = &skin.style;
        let mut change = None;
        let screen = ui.max_rect();
        let width = 420.0_f32.min(screen.width() - 32.0);
        let rows = self.rows().len() as f32;
        let height = 44.0 + rows * (style.row + 1.0) + 34.0;
        let rect = egui::Rect::from_center_size(screen.center(), egui::vec2(width, height));
        crate::ui::prime_area(
            ui.ctx(),
            egui::Id::new("dtr-settings"),
            egui::Order::Foreground,
            rect,
            true,
        );
        // Dim the terminal behind the menu: the settings are the subject now.
        ui.painter().rect_filled(screen, 0.0, palette.scrim);
        egui::Area::new(egui::Id::new("dtr-settings"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .constrain(false)
            // A fade-in would make the menu invisible on the first frame, and
            // the first frame is the only one a capture has.
            .fade_in(false)
            .show(ui.ctx(), |ui| {
                // The drag number and the combo boxes are still egui's own, so
                // the menu hands them the skin's style rather than inheriting the
                // frame's.
                skin.apply(ui.ctx());
                ui.set_min_size(rect.size());
                egui::Frame::new()
                    .fill(palette.card)
                    .stroke(egui::Stroke::new(style.border, palette.border))
                    .corner_radius(style.card_radius)
                    .inner_margin(style.card_margin)
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new("settings")
                                .font(style.title_font.clone())
                                .color(palette.text),
                        );
                        ui.add_space(8.0);
                        for (index, row) in self.rows().iter().enumerate() {
                            let selected = index == self.row;
                            let section = row.section();
                            let clicked = std::cell::Cell::new(None);
                            ui.horizontal(|ui| {
                                let name = if section.is_empty() {
                                    row.label().to_owned()
                                } else {
                                    format!("{section}.{}", row.label())
                                };
                                // The label is the selection target, so clicking
                                // a row does what the arrow keys do.
                                let label = ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(name)
                                            .font(style.value_font.clone())
                                            .color(if selected {
                                                palette.text
                                            } else {
                                                palette.dim
                                            }),
                                    )
                                    .sense(egui::Sense::click()),
                                );
                                if label.clicked() {
                                    clicked.set(Some(Change::Select(index)));
                                }
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if let Some(edit) = self.control(ui, *row, &skin) {
                                            clicked.set(Some(edit));
                                        }
                                    },
                                );
                            });
                            if change.is_none() {
                                change = clicked.into_inner();
                            }
                            if matches!(row, Row::AutoReconnect) {
                                ui.add_space(2.0);
                            }
                        }
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new("click to change   enter save   esc cancel")
                                .font(egui::FontId::new(
                                    style.label_font.size - 1.0,
                                    style.label_font.family.clone(),
                                ))
                                .color(palette.dim),
                        );
                    });
            });
        change
    }

    /// The widget on the right of a row. Every one is a mouse control; each
    /// returns the edit it made, if any.
    ///
    /// Each control names itself for accessibility. A combo box and a checkbox
    /// carry no text of their own here, so without this they are anonymous —
    /// invisible to a screen reader and impossible for the test harness to find.
    fn control(&self, ui: &mut egui::Ui, row: Row, skin: &dtr_widgets::Skin) -> Option<Change> {
        let dim = skin.palette.dim;
        // `theme value`, not `theme`: the row's own label carries the bare name,
        // and two identical names make a control impossible to address.
        let name = format!("{} value", row.key());
        match row {
            Row::FontSize => {
                // Drag left/right, or click to type. Both are the mouse.
                let mut size = self.draft.font.size;
                let response = ui.add(
                    egui::DragValue::new(&mut size)
                        .speed(0.25)
                        .range(6.0..=48.0)
                        .fixed_decimals(0)
                        .suffix(" pt"),
                );
                response
                    .widget_info(|| egui::WidgetInfo::labeled(egui::Role::SpinButton, true, &name));
                response.changed().then_some(Change::FontSize(size))
            }
            Row::FontFamily => {
                // The list `show` found, not a fresh probe: a fontconfig walk in
                // the drawing code is a walk per frame.
                let families = &self.families;
                let current = self
                    .draft
                    .font
                    .family
                    .clone()
                    .unwrap_or_else(|| "default".to_owned());
                let mut change = None;
                let response = egui::ComboBox::from_id_salt("font-family")
                    .selected_text(egui::RichText::new(&current).monospace().size(12.0))
                    .width(190.0)
                    .show_ui(ui, |ui| {
                        if dtr_widgets::menu_row(
                            ui,
                            skin,
                            "default",
                            self.draft.font.family.is_none(),
                        ) {
                            change = Some(Change::Family(None));
                        }
                        for family in families {
                            let picked = self.draft.font.family.as_deref() == Some(family.as_str());
                            if dtr_widgets::menu_row(ui, skin, family.as_str(), picked) {
                                change = Some(Change::Family(Some(family.clone())));
                            }
                        }
                    });
                response
                    .response
                    .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, &name));
                change
            }
            Row::Theme => {
                // Same rule: the scan belongs to `show`, not to a frame.
                let names = &self.themes;
                let current = self
                    .draft
                    .theme
                    .clone()
                    .unwrap_or_else(|| "built-in".to_owned());
                let mut change = None;
                let response = egui::ComboBox::from_id_salt("theme")
                    .selected_text(egui::RichText::new(&current).monospace().size(12.0))
                    .width(190.0)
                    .show_ui(ui, |ui| {
                        if dtr_widgets::menu_row(ui, skin, "built-in", self.draft.theme.is_none()) {
                            change = Some(Change::Theme(None));
                        }
                        for name in names {
                            let picked = self.draft.theme.as_deref() == Some(name.as_str());
                            if dtr_widgets::menu_row(ui, skin, name.as_str(), picked) {
                                change = Some(Change::Theme(Some(name.clone())));
                            }
                        }
                    });
                response
                    .response
                    .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, &name));
                change
            }
            Row::AutoReconnect => {
                let on = self.draft.serial.auto_reconnect;
                dtr_widgets::check_box(ui, skin, on, &name).map(Change::AutoReconnect)
            }
            Row::Baud => {
                let Some(serial) = self.serial.as_ref() else {
                    ui.label(
                        egui::RichText::new("no port")
                            .monospace()
                            .size(12.0)
                            .color(dim),
                    );
                    return None;
                };
                let mut change = None;
                let response = egui::ComboBox::from_id_salt("baud")
                    .selected_text(
                        egui::RichText::new(serial.baud.to_string())
                            .monospace()
                            .size(12.0),
                    )
                    .width(110.0)
                    .show_ui(ui, |ui| {
                        for &baud in BAUDS {
                            let label = baud.to_string();
                            if dtr_widgets::menu_row(ui, skin, &label, serial.baud == baud) {
                                change = Some(Change::Baud(baud));
                            }
                        }
                    });
                response
                    .response
                    .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, &name));
                change
            }
            Row::CrToCrlf => {
                let Some(serial) = self.serial.as_ref() else {
                    ui.label(
                        egui::RichText::new("no port")
                            .monospace()
                            .size(12.0)
                            .color(dim),
                    );
                    return None;
                };
                dtr_widgets::check_box(ui, skin, serial.cr_to_crlf, &name).map(Change::CrToCrlf)
            }
        }
    }
}

impl Settings {
    /// The port record to store, when a serial port is focused and its settings
    /// were edited.
    pub fn serial_write(&self) -> Option<(String, dtr_store::SerialSettings)> {
        Some((self.port.clone()?, self.serial.clone()?))
    }

    /// The settings the menu would apply to a live port.
    #[cfg(test)]
    fn serial(&self) -> Option<dtr_store::SerialSettings> {
        self.serial.clone()
    }
}

#[cfg(test)]
fn on_off(value: bool) -> String {
    if value { "on" } else { "off" }.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_applies_and_escape_restores() {
        let mut settings = Settings::new();
        settings.show(&Config::default());
        let before = settings.draft().font.size;
        settings.row = 0;
        assert!(settings.nudge_test(1));
        assert!(settings.draft().font.size > before);

        assert_eq!(
            settings.handle(&winit::keyboard::Key::Named(
                winit::keyboard::NamedKey::Escape
            )),
            Some(Effect::Cancel)
        );
        assert_eq!(settings.draft().font.size, before);
    }

    #[test]
    fn baud_cycles_through_the_standard_rates_on_the_port_record() {
        let mut settings = Settings::new();
        settings.show_port(
            &Config::default(),
            Some("/dev/ttyUSB0".to_owned()),
            Some(dtr_store::SerialSettings {
                baud: 115_200,
                cr_to_crlf: true,
                ..dtr_store::SerialSettings::default()
            }),
        );
        settings.row = settings
            .rows()
            .iter()
            .position(|row| *row == Row::Baud)
            .unwrap();
        settings.nudge(1);
        assert_eq!(settings.serial().unwrap().baud, 230_400);
        settings.nudge(-1);
        assert_eq!(settings.serial().unwrap().baud, 115_200);
    }

    #[test]
    fn without_a_port_the_serial_rows_are_inert_and_say_so() {
        let mut settings = Settings::new();
        settings.show(&Config::default());
        settings.row = 0;
        settings.nudge(1);
        assert!(settings.serial_write().is_none());
        assert_eq!(settings.value(Row::Baud), "no port");
    }

    /// Opening the menu takes the list the probe found; it does not walk the
    /// machine's fonts itself.
    ///
    /// `show` runs in the click that opens the menu, and the probe is a
    /// fontconfig walk that parses a face per family — 300 ms of frozen window,
    /// which is what "the settings take a while to open" was. `main` warms it and
    /// `refresh_families` fills the list in when a run got here first.
    #[test]
    fn the_menu_takes_the_family_list_the_probe_found() {
        // Deterministic: make the probe's answer exist before the menu asks.
        let families = crate::fonts::usable_families();
        let mut settings = Settings::new();
        settings.show(&Config::default());
        assert_eq!(settings.families, families);
        assert!(
            !settings.themes.is_empty(),
            "the compiled-in theme is always in the list"
        );
    }

    #[test]
    fn saving_round_trips_through_the_real_writer() {
        // The whole menu is only as good as this: every field written through
        // `toml_edit`, reparsed, and compared with the draft — with a comment
        // and an unrelated key surviving the trip.
        let mut settings = Settings::new();
        settings.families = vec!["DejaVu Sans Mono".to_owned(), "Hack".to_owned()];
        settings.themes = vec!["kanagawa wave".to_owned()];
        settings.show(&Config::default());
        settings.draft.font.size = 17.0;
        settings.draft.font.family = Some("Hack".to_owned());
        settings.draft.theme = Some("kanagawa wave".to_owned());
        settings.serial = Some(dtr_store::SerialSettings {
            baud: 230_400,
            cr_to_crlf: false,
            ..dtr_store::SerialSettings::default()
        });
        settings.draft.serial.auto_reconnect = false;

        let mut text = String::from(
            "# keep me\n\n[serial]\nbaud = 9600 # an old board\n\n[font]\nsize = 14\n",
        );
        for (key, value) in settings.writes() {
            text = dtr_config::set_in_text(&text, &key, &value).expect("write");
        }

        assert!(text.contains("# keep me"), "the comment above was lost");
        assert!(
            text.contains("# an old board"),
            "an inline comment was lost"
        );

        let parsed = Config::parse(&text).expect("reparse");
        assert_eq!(parsed.font.size, 17.0);
        assert_eq!(parsed.font.family.as_deref(), Some("Hack"));
        assert_eq!(parsed.theme.as_deref(), Some("kanagawa wave"));
        assert!(!parsed.serial.auto_reconnect);
    }

    #[test]
    fn an_empty_theme_writes_none_not_an_empty_name() {
        let settings = Settings::new();
        let text = String::new();
        let mut text = text;
        for (key, value) in settings.writes() {
            text = dtr_config::set_in_text(&text, &key, &value).expect("write");
        }
        let parsed = Config::parse(&text).expect("reparse");
        assert_eq!(parsed.theme, None);
        assert_eq!(parsed.font.family, None);
    }

    #[test]
    fn every_row_writes_a_key() {
        let settings = Settings::new();
        let writes = settings.writes();
        let keys: Vec<&str> = writes.iter().map(|(key, _)| key.as_str()).collect();
        assert!(keys.contains(&"font.size"));
        assert!(keys.contains(&"font.family"));
        assert!(keys.contains(&"theme"));
        assert!(keys.contains(&"serial.auto_reconnect"));
        // Every row that lives in the file must have a writer; the serial rate
        // rows live in the store and are covered by `serial_write`.
        for row in Row::FILE {
            assert!(
                keys.contains(&row.key().as_str()),
                "{} is not written",
                row.key()
            );
        }
    }

    impl Settings {
        /// `nudge` returns nothing; the test only needs to know it ran.
        fn nudge_test(&mut self, step: i32) -> bool {
            self.nudge(step);
            true
        }
    }
}

#[cfg(test)]
mod kittest_tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    /// Opening the menu on a shell shows the terminal's own settings and nothing
    /// about a port, because there is no port to describe.
    #[test]
    fn a_shell_menu_has_no_port_rows() {
        let mut settings = Settings::new();
        settings.show(&Config::default());
        assert_eq!(
            settings.rows(),
            vec![
                Row::FontSize,
                Row::FontFamily,
                Row::Theme,
                Row::AutoReconnect
            ]
        );
    }

    /// Escape leaves without writing anything, which is the whole point of a
    /// draft: a menu you cannot back out of is worse than no menu.
    #[test]
    fn escape_cancels_without_writing() {
        let mut settings = Settings::new();
        settings.show(&Config::default());
        settings.handle(&winit::keyboard::Key::Named(
            winit::keyboard::NamedKey::ArrowRight,
        ));
        assert!(!settings.writes().is_empty(), "the change is in the draft");
        assert_eq!(
            settings.handle(&winit::keyboard::Key::Named(
                winit::keyboard::NamedKey::Escape
            )),
            Some(Effect::Cancel)
        );
    }

    /// The theme is a combo box: click it, click a theme, get the edit. This is
    /// the mouse path the menu is meant to be driven by.
    #[test]
    fn picking_a_theme_from_the_combo_box_reports_it() {
        let mut settings = Settings::new();
        settings.show(&Config::default());
        let look = crate::ui::Look::default();
        let change = std::rc::Rc::new(std::cell::RefCell::new(None));
        let sink = change.clone();
        let mut harness = egui_kittest::Harness::new_ui(move |ui| {
            if let Some(edit) = settings.ui(ui, &look) {
                *sink.borrow_mut() = Some(edit);
            }
        });
        harness.run();
        // The combo box is found by the name it declares, then the list opens.
        harness.get_by_label("theme value").click();
        harness.run();
        // The built-in entry is always in the list, whatever themes are
        // installed on the machine running the test.
        harness.get_by_label("built-in").click();
        harness.run();
        assert!(
            matches!(*change.borrow(), Some(Change::Theme(_))),
            "clicking a theme must report the choice"
        );
    }

    /// The rows are drawn and the menu renders: a snapshot catches a row that
    /// moves, a label that disappears, or the highlight landing on the wrong row.
    #[test]
    fn the_menu_renders() {
        let mut settings = Settings::new();
        settings.show(&Config::default());
        settings.handle(&winit::keyboard::Key::Named(
            winit::keyboard::NamedKey::ArrowDown,
        ));
        let look = crate::ui::Look::default();
        let mut harness = Harness::new_ui(|ui| {
            settings.ui(ui, &look);
        });
        harness.run();
        harness.snapshot("settings_menu");
    }
}
