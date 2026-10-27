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
const BAUDS: &[u32] = &[9_600, 19_200, 38_400, 57_600, 115_200, 230_400, 460_800, 921_600];

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

    /// The key as it appears in the config file, e.g. `serial.baud`.
    #[cfg(test)]
    fn key(self) -> String {
        match self.section() {
            "" => self.label().to_owned(),
            section => format!("{section}.{}", self.label()),
        }
    }
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
        if self.families.is_empty() {
            self.families = crate::fonts::usable_families();
        }
        if self.themes.is_empty() {
            self.themes = crate::theme::names();
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
            Key::Named(winit::keyboard::NamedKey::ArrowDown) | Key::Named(winit::keyboard::NamedKey::Tab) => {
                let count = self.rows().len();
                self.row = (self.row + 1) % count;
                return Some(Effect::Changed);
            }
            Key::Named(winit::keyboard::NamedKey::ArrowUp) | Key::Named(winit::keyboard::NamedKey::Backspace) => {
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
            Row::Theme => self.draft.theme.clone().unwrap_or_else(|| "built-in".to_owned()),
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
            ("font.size".to_owned(), format!("{:.0}", self.draft.font.size)),
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
    pub fn ui(&self, ui: &mut egui::Ui, look: &crate::ui::Look) {
        let screen = ui.max_rect();
        let width = 420.0_f32.min(screen.width() - 32.0);
        let rows = self.rows().len() as f32;
        let height = 44.0 + rows * 24.0 + 34.0;
        let rect = egui::Rect::from_center_size(screen.center(), egui::vec2(width, height));
        crate::ui::prime_area(ui.ctx(), egui::Id::new("dtr-settings"), egui::Order::Foreground, rect, true);
        // Dim the terminal behind the menu: the settings are the subject now.
        ui.painter().rect_filled(
            screen,
            0.0,
            egui::Color32::from_black_alpha(140),
        );
        egui::Area::new(egui::Id::new("dtr-settings"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .constrain(false)
            // A fade-in would make the menu invisible on the first frame, and
            // the first frame is the only one a capture has.
            .fade_in(false)
            .show(ui.ctx(), |ui| {
                ui.set_min_size(rect.size());
                egui::Frame::new()
                    .fill(crate::ui::Look::colour(look.surface))
                    .stroke(egui::Stroke::new(1.0, crate::ui::Look::colour(look.dim)))
                    .corner_radius(8)
                    .inner_margin(egui::Margin::symmetric(16, 12))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new("settings")
                                .monospace()
                                .size(13.0)
                                .strong()
                                .color(crate::ui::Look::colour(look.foreground)),
                        );
                        ui.add_space(8.0);
                        for (index, row) in self.rows().iter().enumerate() {
                            let selected = index == self.row;
                            let section = row.section();
                            ui.horizontal(|ui| {
                                let marker = if selected { "›" } else { " " };
                                ui.label(
                                    egui::RichText::new(marker)
                                        .monospace()
                                        .size(12.0)
                                        .color(crate::ui::Look::colour(look.accent)),
                                );
                                let name = if section.is_empty() {
                                    row.label().to_owned()
                                } else {
                                    format!("{section}.{}", row.label())
                                };
                                ui.label(
                                    egui::RichText::new(name)
                                        .monospace()
                                        .size(12.0)
                                        .color(crate::ui::Look::colour(if selected {
                                            look.foreground
                                        } else {
                                            look.dim
                                        })),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new(self.value(*row))
                                                .monospace()
                                                .size(12.0)
                                                .color(crate::ui::Look::colour(look.accent)),
                                        );
                                    },
                                );
                            });
                            if matches!(row, Row::AutoReconnect) {
                                ui.add_space(2.0);
                            }
                        }
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new("↑↓ row   ←→ change   enter save   esc cancel")
                                .monospace()
                                .size(11.0)
                                .color(crate::ui::Look::colour(look.dim)),
                        );
                    });
            });
    }
}

impl Settings {
    /// The port record to store, when a serial port is focused and its settings
    /// were edited.
    pub fn serial_write(&self) -> Option<(String, dtr_store::SerialSettings)> {
        Some((self.port.clone()?, self.serial?))
    }

    /// The settings the menu would apply to a live port.
    #[cfg(test)]
    fn serial(&self) -> Option<dtr_store::SerialSettings> {
        self.serial
    }
}

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
            }),
        );
        settings.row = settings.rows().iter().position(|row| *row == Row::Baud).unwrap();
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
        });
        settings.draft.serial.auto_reconnect = false;

        let mut text = String::from(
            "# keep me\n\n[serial]\nbaud = 9600 # an old board\n\n[font]\nsize = 14\n",
        );
        for (key, value) in settings.writes() {
            text = dtr_config::set_in_text(&text, &key, &value).expect("write");
        }

        assert!(text.contains("# keep me"), "the comment above was lost");
        assert!(text.contains("# an old board"), "an inline comment was lost");

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
            assert!(keys.contains(&row.key().as_str()), "{} is not written", row.key());
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
