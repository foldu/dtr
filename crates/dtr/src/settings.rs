//! The preferences card: every field in the config file, editable in the window.
//!
//! It is called *preferences* because that is what it is to the person opening
//! it; its type is [`Settings`] because that is what it edits — the values the
//! config file holds, of which a serial port's defaults and the look are two
//! kinds. `[keys] preferences` and the palette's `preferences` command both open
//! this card.
//!
//! Rows are derived from [`dtr_config::Config`] rather than hand-written, so a
//! key added to the file has one obvious place to appear here. Edits are kept in
//! a draft and only reach the file (and the running terminal) on Enter, which
//! makes Esc an actual cancel rather than a rollback.
//!
//! Everything applies live, per the rule the user set: a serial setting that can
//! only take effect on reconnect reconnects, and the pane keeps its scrollback.
//!
//! The card has two **panels**, because it outgrew one column: appearance is what
//! the window and the grid look like, general is what a session does. A row
//! belongs to one panel and says so ([`Row::panel`]), which is also what decides
//! which rows a panel shows right now — the port's rate and CR translation exist
//! only while a port is focused, and the chrome's scheme and accent only while the
//! chrome is one this program paints ([`Row::shown`]).

use dtr_config::Config;

/// The baud rates worth cycling through. A terminal that offers an arbitrary
/// number would need a text editor; a serial console uses one of these.
const BAUDS: &[u32] = &[
    9_600, 19_200, 38_400, 57_600, 115_200, 230_400, 460_800, 921_600,
];

/// Which panel of the sheet is showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Panel {
    Appearance,
    General,
}

impl Panel {
    /// In the order the tab row draws them: general first, then appearance.
    pub const ALL: [Panel; 2] = [Panel::General, Panel::Appearance];

    fn label(self) -> &'static str {
        match self {
            Panel::Appearance => "appearance",
            Panel::General => "general",
        }
    }

    /// The rows this panel shows, in the order they read.
    ///
    /// Three things decide that order: which panel the row belongs to, whether the
    /// row describes something that exists right now, and — for the chrome's
    /// scheme and accent — whether the chosen look has those of its own. A
    /// `native` window reads both from the desktop, so the rows are not there to
    /// be filled in.
    fn rows(self, port: bool, owns_colours: bool) -> Vec<Row> {
        Row::ALL
            .into_iter()
            .filter(|row| row.panel() == self)
            .filter(|row| row.shown(port, owns_colours))
            .collect()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Row {
    Widgets,
    Scheme,
    Accent,
    Theme,
    UiFont,
    FontSize,
    FontFamily,
    Baud,
    CrToCrlf,
    AutoReconnect,
    Tmux,
    Padding,
    GpuAdapter,
    GpuPower,
}

impl Row {
    const ALL: [Row; 14] = [
        Row::Widgets,
        Row::Scheme,
        Row::Accent,
        Row::Theme,
        Row::UiFont,
        Row::FontSize,
        Row::FontFamily,
        Row::Baud,
        Row::CrToCrlf,
        Row::AutoReconnect,
        Row::Tmux,
        Row::Padding,
        Row::GpuAdapter,
        Row::GpuPower,
    ];

    fn panel(self) -> Panel {
        match self {
            Row::Widgets
            | Row::Scheme
            | Row::Accent
            | Row::Theme
            | Row::UiFont
            | Row::FontSize
            | Row::FontFamily
            | Row::Padding => Panel::Appearance,
            Row::Baud
            | Row::CrToCrlf
            | Row::AutoReconnect
            | Row::Tmux
            | Row::GpuAdapter
            | Row::GpuPower => Panel::General,
        }
    }

    /// Whether the row describes something that exists right now.
    fn shown(self, port: bool, owns_colours: bool) -> bool {
        match self {
            // A rate for a port that is not open is a number belonging to nothing.
            Row::Baud | Row::CrToCrlf => port,
            // `native` and `dtr` take their scheme and accent from the desktop and
            // the terminal theme; a row for either would be a control nothing reads.
            Row::Scheme | Row::Accent => owns_colours,
            _ => true,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Row::Widgets => "widgets",
            Row::Scheme => "scheme",
            Row::Accent => "accent",
            Row::Theme => "theme",
            Row::UiFont => "font",
            Row::FontSize => "size",
            Row::FontFamily => "family",
            Row::Baud => "baud",
            Row::CrToCrlf => "cr_to_crlf",
            Row::AutoReconnect => "auto_reconnect",
            Row::Tmux => "enabled",
            Row::Padding => "padding",
            Row::GpuAdapter => "adapter",
            Row::GpuPower => "power",
        }
    }

    /// What the key does, in one line, because a key is a name and not an
    /// explanation: `serial.auto_reconnect` is a thing a person has to recognise
    /// from the file they wrote once.
    fn description(self) -> &'static str {
        match self {
            Row::Widgets => "the look the chrome is painted in",
            Row::Scheme => "light, dark, or the desktop's",
            Row::Accent => "the colour of a selection",
            Row::Theme => "the terminal's colour scheme",
            Row::UiFont => "the chrome's face; empty follows the desktop",
            Row::FontSize => "the grid's point size",
            Row::FontFamily => "the face the grid is drawn in",
            Row::Baud => "the rate the port opens at",
            Row::CrToCrlf => "a lone CR becomes a new line",
            Row::AutoReconnect => "reopen the port when it drops",
            Row::Tmux => "run ssh tabs inside tmux on the host",
            Row::Padding => "space around the grid, in points",
            Row::GpuAdapter => "the card to render with; restart to apply",
            Row::GpuPower => "low or high power; restart to apply",
        }
    }

    fn section(self) -> &'static str {
        match self {
            Row::Widgets | Row::Scheme | Row::Accent | Row::UiFont => "ui",
            Row::Theme => "",
            Row::FontSize | Row::FontFamily => "font",
            Row::Baud | Row::CrToCrlf | Row::AutoReconnect => "serial",
            Row::Tmux => "tmux",
            Row::Padding => "window",
            Row::GpuAdapter | Row::GpuPower => "gpu",
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

/// The first entry of the accent field's list: not a colour, the absence of one.
///
/// An empty field is "whatever the look would use", the same way an empty theme
/// is the built-in colours, and it has to be *offered* rather than only reached by
/// deleting text.
const DEFAULT_ACCENT: &str = "default";

/// The accents the looks ship with, for the field's list.
///
/// Asked of the presets rather than written out here, so a look that changes its
/// default changes this list too. Deduplicated in order: Adwaita's blue and
/// Windows' light-mode blue are the same colour, and a list with it twice is a
/// list that looks like a bug.
fn accent_choices() -> Vec<String> {
    let mut choices = vec![DEFAULT_ACCENT.to_owned()];
    for dark in [true, false] {
        for preset in dtr_widgets::Preset::ALL {
            let Some(accent) = preset.default_accent(dark) else {
                continue;
            };
            let hex = dtr_config::hex([accent.r(), accent.g(), accent.b()]);
            if !choices.contains(&hex) {
                choices.push(hex);
            }
        }
    }
    choices
}

/// What a mouse edit in the menu asks for. The menu renders from `&self`, so a
/// widget reports its edit rather than writing through a borrow the caller
/// cannot lend it.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// The tab row: another panel.
    Panel(Panel),
    Widgets(dtr_config::Widgets),
    Scheme(dtr_config::Scheme),
    /// The accent field's text, as typed: it may not parse yet, and a half-typed
    /// hex must stay visible rather than being replaced by the old colour.
    Accent(String),
    FontSize(f32),
    Family(Option<String>),
    Theme(Option<String>),
    /// The chrome's face; `None` follows the desktop.
    UiFont(Option<String>),
    /// The adapter's name, as typed; empty is "auto".
    GpuAdapter(String),
    GpuPower(dtr_config::GpuPower),
    AutoReconnect(bool),
    /// Keep every ssh tab inside a tmux session on the far end.
    Tmux(bool),
    Baud(u32),
    CrToCrlf(bool),
    Padding(u16),
}

/// What the card asked for.
///
/// An edit is a [`Reply::Change`] because the draft is only written when the card
/// is saved; the save and cancel buttons are the two ways out, and they are
/// buttons like every other control here — the sheet takes no keys at all.
#[derive(Clone, Debug, PartialEq)]
pub enum Reply {
    Change(Change),
    Apply,
    Cancel,
}

pub struct Settings {
    pub open: bool,
    /// Which panel is showing.
    panel: Panel,
    draft: Config,
    /// The focused port's remembered settings, when a serial session has focus.
    /// Baud and CR translation are per-device facts, so the menu edits the
    /// store's record for this port — or nothing, if there is no port.
    serial: Option<dtr_store::SerialSettings>,
    port: Option<String>,
    /// Families that passed the probe, loaded once when the menu opens.
    families: Vec<String>,
    themes: Vec<String>,
    /// The accent field's text, which is what was typed: the draft holds the
    /// parsed colour, and an empty field means "the look's own".
    accent_text: String,
    /// What the menu opened with, so Esc can restore it.
    original: Config,
}

impl Settings {
    pub fn new() -> Self {
        Self {
            open: false,
            panel: Panel::Appearance,
            draft: Config::default(),
            serial: None,
            port: None,
            families: Vec::new(),
            themes: Vec::new(),
            accent_text: String::new(),
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
    ///
    /// The sheet opens on the first tab, always: a panel that moved under the
    /// pointer or under the eye between two openings is a panel nobody learns
    /// where to find.
    pub fn show_port(
        &mut self,
        config: &Config,
        port: Option<String>,
        serial: Option<dtr_store::SerialSettings>,
    ) {
        self.port = port;
        self.serial = serial;
        self.open = true;
        self.panel = Panel::ALL[0];
        self.draft = config.clone();
        self.original = config.clone();
        // Empty means "the look's own accent", the same as an empty theme meaning
        // "the built-in colours".
        self.accent_text = self
            .draft
            .ui
            .accent
            .map(dtr_config::hex)
            .unwrap_or_default();
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
            "preferences warmed"
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

    /// Which panel is showing. The sheet draws it; a test asks it.
    #[cfg(test)]
    pub fn panel(&self) -> Panel {
        self.panel
    }

    /// Whether the chosen look has a scheme and an accent of its own.
    ///
    /// Asked of the crate that paints, so the answer to "does `windows` have an
    /// accent" lives with `windows` rather than here.
    fn owns_colours(&self) -> bool {
        crate::ui::Look::choice_of(&self.draft)
            .preset
            .owns_its_colours()
    }

    /// The rows this menu shows right now.
    pub fn rows(&self) -> Vec<Row> {
        self.panel.rows(self.serial.is_some(), self.owns_colours())
    }

    /// The rows of every panel, so the card can be as tall as the longest one and
    /// switching panels does not resize the sheet under the pointer.
    fn roomiest_rows(&self) -> usize {
        Panel::ALL
            .iter()
            .map(|panel| panel.rows(self.serial.is_some(), self.owns_colours()).len())
            .max()
            .unwrap_or(0)
    }

    /// Apply one edit from the mouse to the draft.
    pub fn apply(&mut self, change: Change) {
        match change {
            Change::Panel(panel) => self.panel = panel,
            Change::Widgets(widgets) => self.draft.ui.widgets = widgets,
            Change::Scheme(scheme) => self.draft.ui.scheme = scheme,
            Change::Accent(text) => {
                // A colour that does not parse is a colour being typed: the draft
                // keeps the last one that did, and the field says so.
                if text.trim().is_empty() {
                    self.draft.ui.accent = None;
                } else if let Some(accent) = dtr_config::parse_hex(&text) {
                    self.draft.ui.accent = Some(accent);
                }
                self.accent_text = text;
            }
            Change::FontSize(size) => self.draft.font.size = size,
            Change::Family(family) => self.draft.font.family = family,
            Change::Theme(theme) => self.draft.theme = theme,
            Change::UiFont(font) => self.draft.ui.font = font,
            Change::GpuAdapter(name) => {
                let name = name.trim();
                self.draft.gpu.adapter = (!name.is_empty()).then(|| name.to_owned());
            }
            Change::GpuPower(power) => self.draft.gpu.power = power,
            Change::AutoReconnect(on) => self.draft.serial.auto_reconnect = on,
            Change::Tmux(on) => self.draft.tmux.enabled = on,
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
            Change::Padding(padding) => self.draft.padding = padding,
        }
    }

    /// Throw the draft away and close. Esc, the cross and the cancel button are
    /// the same answer, so they are the same function.
    pub fn cancel(&mut self) {
        self.draft = self.original.clone();
        self.open = false;
    }

    /// The value column, as it should read in the menu.
    #[cfg(test)]
    fn value(&self, row: Row) -> String {
        match row {
            Row::Widgets => self.draft.ui.widgets.to_string(),
            Row::Scheme => self.draft.ui.scheme.to_string(),
            Row::Accent => match self.draft.ui.accent {
                Some(accent) => dtr_config::hex(accent),
                None => "the look's own".to_owned(),
            },
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
            Row::UiFont => self
                .draft
                .ui
                .font
                .clone()
                .unwrap_or_else(|| "system".to_owned()),
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
            Row::Tmux => on_off(self.draft.tmux.enabled),
            Row::Padding => format!("{} pt", self.draft.padding),
            Row::GpuAdapter => self
                .draft
                .gpu
                .adapter
                .clone()
                .unwrap_or_else(|| "auto".to_owned()),
            Row::GpuPower => self.draft.gpu.power.to_string(),
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
        // The chrome: which look, which scheme, and the accent as `#RRGGBB` or
        // empty for the look's own.
        writes.push((
            "tmux.enabled".to_owned(),
            self.draft.tmux.enabled.to_string(),
        ));
        writes.push(("window.padding".to_owned(), self.draft.padding.to_string()));
        writes.push((
            "ui.font".to_owned(),
            self.draft.ui.font.clone().unwrap_or_default(),
        ));
        writes.push((
            "gpu.adapter".to_owned(),
            self.draft.gpu.adapter.clone().unwrap_or_default(),
        ));
        writes.push(("gpu.power".to_owned(), self.draft.gpu.power.to_string()));
        writes.push(("ui.widgets".to_owned(), self.draft.ui.widgets.to_string()));
        writes.push(("ui.scheme".to_owned(), self.draft.ui.scheme.to_string()));
        writes.push((
            "ui.accent".to_owned(),
            self.draft
                .ui
                .accent
                .map(dtr_config::hex)
                .unwrap_or_default(),
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
    /// The height of one row: the key's line and the description's, or the
    /// control's, whichever is taller.
    ///
    /// Measured from the fonts the rows draw with rather than from their point
    /// sizes: egui's line height is its own number, and a formula that guesses it
    /// is a few points short per row, which is the scrollbar this sheet must not
    /// need.
    fn row_height(ui: &egui::Ui, style: &dtr_widgets::Style) -> f32 {
        let key = style.value_font.clone();
        let (key_line, desc_line) = ui.fonts_mut(|fonts| {
            (
                fonts.row_height(&key),
                fonts.row_height(&egui::FontId::new(
                    style.label_font.size - 1.0,
                    style.label_font.family.clone(),
                )),
            )
        });
        // Plus air: a description that sits tight under its key reads as part of
        // the next row's name, and the sheet becomes a paragraph.
        style.row.max(key_line + desc_line + 6.0)
    }

    pub fn ui(&self, ui: &mut egui::Ui, look: &crate::ui::Look) -> Option<Reply> {
        let skin = look.skin();
        let palette = skin.palette;
        let style = &skin.style;
        // The scheme the *draft* would draw in, which is what the accent field has
        // to show: the applied skin may be a different look with a different
        // scheme, and the field is about what a save would produce. Asking
        // `Choice::skin` for it keeps the desktop and theme fallbacks in one place.
        let draft_dark = crate::ui::Look::choice_of(&self.draft)
            .skin(
                dtr_widgets::Palette::dtr(look.background, look.foreground, look.accent),
                crate::system::native_skin(),
            )
            .palette
            .is_dark();
        // Header, the tab row, the longest panel's rows, the hint and the buttons.
        // A fixed height, like the keyboard map: the card must not resize itself
        // when the serial rows come and go or a look hides its scheme, or the
        // button under the pointer moves as it is pressed. The longest panel is
        // what it is sized for, so switching tabs moves nothing.
        let rows = self.roomiest_rows() as f32;
        // A row is the key and its description, stacked. The control sets the
        // floor — on Adwaita a 34-point control already holds both lines — and
        // the two lines set it on a skin with 23-point rows. Measured from the
        // fonts the rows use, not from their point sizes: egui's line height is
        // its own number, and a formula that guesses it short changes the card by
        // a few points a row, which is a scrollbar that need not be there.
        let row = Self::row_height(ui, style);
        let height = 52.0 + (style.row + style.gap) + rows * (row + style.gap) + 58.0;
        // The buttons are the card's footer, not the body's last row: the rows
        // above them scroll, and save and cancel must not scroll away with them.
        // A footer writes where its body does, which is why this is a cell.
        let answers = std::cell::RefCell::new(None::<Reply>);
        // The tab row is above the body for the same reason: the way to the other
        // panel must not scroll out of reach either.
        let switched = std::cell::RefCell::new(None::<Panel>);
        let options: Vec<(Panel, &str)> = Panel::ALL
            .iter()
            .map(|panel| (*panel, panel.label()))
            .collect();
        let out = dtr_widgets::Card::new(egui::Id::unique("dtr-preferences"), "preferences")
            .size(420.0, height)
            .close("close preferences")
            .tabs(|ui| {
                if let Some(panel) =
                    dtr_widgets::tabs(ui, &skin, "preferences panel", self.panel, &options)
                {
                    *switched.borrow_mut() = Some(panel);
                }
            })
            .footer(|ui| {
                // The two answers are buttons, and the sheet names no keys
                // because it takes none: save and cancel are the only ways out
                // besides the cross.
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if dtr_widgets::Button::new("save")
                            .primary()
                            .width(72.0)
                            .show(ui, &skin)
                        {
                            *answers.borrow_mut() = Some(Reply::Apply);
                        }
                        if dtr_widgets::Button::new("cancel")
                            .width(72.0)
                            .show(ui, &skin)
                        {
                            *answers.borrow_mut() = Some(Reply::Cancel);
                        }
                    });
                });
            })
            .show(ui, &skin, |ui, _inner| {
                ui.add_space(8.0);
                let mut reply = None;
                for row in self.rows() {
                    let section = row.section();
                    let clicked = std::cell::Cell::new(None);
                    let name = if section.is_empty() {
                        row.label().to_owned()
                    } else {
                        format!("{section}.{}", row.label())
                    };
                    let height = Self::row_height(ui, style);
                    // The row's own rect, and inside it the control first: the
                    // text then wraps in what the control left, which is the whole
                    // reason it is laid out this way round. The height is fixed so
                    // the row does not claim the rest of the card — a `with_layout`
                    // child takes every point it is offered, and the sheet drew one
                    // row and a scrollbar when it was allowed to.
                    let _ = ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width(), height),
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if let Some(edit) = self.control(ui, row, &skin, draft_dark) {
                                clicked.set(Some(edit));
                            }
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    // The key with its description under it, and the
                                    // control beside both — the shape of an Adwaita
                                    // action row. The description is dimmer than the
                                    // key: the name is what a person comes back to,
                                    // the sentence is read once. Neither line is a
                                    // hit target and neither marks a row: there is no
                                    // cursor here, because nothing here is driven by
                                    // anything but the pointer under it.
                                    ui.vertical(|ui| {
                                        ui.label(
                                            egui::RichText::new(name)
                                                .font(style.value_font.clone())
                                                .color(palette.text),
                                        );
                                        ui.label(
                                            egui::RichText::new(row.description())
                                                .font(egui::FontId::new(
                                                    style.label_font.size - 1.0,
                                                    style.label_font.family.clone(),
                                                ))
                                                .color(palette.disabled),
                                        );
                                    });
                                },
                            );
                        },
                    );
                    if reply.is_none() {
                        reply = clicked.into_inner().map(Reply::Change);
                    }
                    if matches!(row, Row::AutoReconnect) {
                        ui.add_space(2.0);
                    }
                }
                reply
            });
        if out.close {
            Some(Reply::Cancel)
        } else {
            // The footer's buttons first: they are the way out of the sheet, and a
            // tab click in the same frame is a change nobody can act on.
            answers
                .into_inner()
                .or(switched
                    .into_inner()
                    .map(|panel| Reply::Change(Change::Panel(panel))))
                .or(out.body)
        }
    }

    /// The widget on the right of a row. Every one is a mouse control; each
    /// returns the edit it made, if any.
    ///
    /// Each control names itself for accessibility. A combo box and a checkbox
    /// carry no text of their own here, so without this they are anonymous —
    /// invisible to a screen reader and impossible for the test harness to find.
    fn control(
        &self,
        ui: &mut egui::Ui,
        row: Row,
        skin: &dtr_widgets::Skin,
        draft_dark: bool,
    ) -> Option<Change> {
        let dim = skin.palette.dim;
        // `theme value`, not `theme`: the row's own label carries the bare name,
        // and two identical names make a control impossible to address.
        let name = format!("{} value", row.key());
        match row {
            Row::Widgets => {
                // A dropdown, like every other fixed list here: the five looks are
                // a closed set, and a row of chips spent the width the values
                // themselves need — on Windows' 32-point metrics it was the row
                // that made the sheet scroll.
                let current = self.draft.ui.widgets;
                let mut change = None;
                let response = egui::ComboBox::from_id_salt("ui-widgets")
                    .selected_text(egui::RichText::new(current.to_string()).size(12.0))
                    .width(140.0)
                    .show_ui(ui, |ui| {
                        for widgets in dtr_config::Widgets::ALL {
                            let label = widgets.to_string();
                            if dtr_widgets::menu_row(ui, skin, &label, widgets == current) {
                                change = Some(Change::Widgets(widgets));
                            }
                        }
                    });
                response
                    .response
                    .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, &name));
                change
            }
            Row::Scheme => {
                let current = self.draft.ui.scheme;
                let mut change = None;
                let response = egui::ComboBox::from_id_salt("ui-scheme")
                    .selected_text(egui::RichText::new(current.to_string()).size(12.0))
                    .width(140.0)
                    .show_ui(ui, |ui| {
                        for scheme in dtr_config::Scheme::ALL {
                            let label = scheme.to_string();
                            if dtr_widgets::menu_row(ui, skin, &label, scheme == current) {
                                change = Some(Change::Scheme(scheme));
                            }
                        }
                    });
                response
                    .response
                    .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, &name));
                change
            }
            Row::Accent => {
                // A hex number, which is what the config file holds; the arrow
                // offers the accent every look ships with, so the way back to a
                // default does not need a code remembered. The field shows the
                // colour a save would use, so "unset" is a colour and not a blank.
                let valid = self.accent_text.trim().is_empty()
                    || dtr_config::parse_hex(&self.accent_text).is_some();
                let shown = if self.accent_text.is_empty() {
                    self.effective_accent(draft_dark)
                        .map(dtr_config::hex)
                        .unwrap_or_default()
                } else {
                    self.accent_text.clone()
                };
                let choices = accent_choices();
                let edit = dtr_widgets::Editable::new("ui-accent", &shown, &name)
                    .choices_label("accent choices")
                    .width(140.0)
                    .valid(valid)
                    .show(ui, skin, &choices);
                match edit {
                    Some(dtr_widgets::Edit::Typed(text)) => Some(Change::Accent(text)),
                    Some(dtr_widgets::Edit::Picked(label)) => {
                        Some(Change::Accent(if label == DEFAULT_ACCENT {
                            String::new()
                        } else {
                            label
                        }))
                    }
                    None => None,
                }
            }
            Row::UiFont => {
                // "system" is the desktop's own, and the row shows that word
                // rather than the family it resolves to: a desktop that changes
                // its font must not turn a saved "follow the desktop" into a
                // pinned name.
                let current = self.draft.ui.font.clone();
                let label = current.clone().unwrap_or_else(|| "system".to_owned());
                let mut change = None;
                let response = egui::ComboBox::from_id_salt("ui-font")
                    .selected_text(egui::RichText::new(&label).size(12.0))
                    .width(190.0)
                    .show_ui(ui, |ui| {
                        if dtr_widgets::menu_row(ui, skin, "system", current.is_none()) {
                            change = Some(Change::UiFont(None));
                        }
                        for family in &self.families {
                            if dtr_widgets::menu_row(
                                ui,
                                skin,
                                family,
                                current.as_deref() == Some(family.as_str()),
                            ) {
                                change = Some(Change::UiFont(Some(family.clone())));
                            }
                        }
                    });
                response
                    .response
                    .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, &name));
                change
            }
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
            Row::Padding => {
                // Drag left/right, or click to type: the same control the point
                // size has, and the same range the file accepts, so the sheet
                // cannot write a number the parser refuses.
                let mut padding = self.draft.padding;
                let response = ui.add(
                    egui::DragValue::new(&mut padding)
                        .speed(0.5)
                        .range(dtr_config::PADDING_RANGE)
                        .suffix(" pt"),
                );
                response
                    .widget_info(|| egui::WidgetInfo::labeled(egui::Role::SpinButton, true, &name));
                response.changed().then_some(Change::Padding(padding))
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
            Row::Tmux => {
                let on = self.draft.tmux.enabled;
                dtr_widgets::check_box(ui, skin, on, &name).map(Change::Tmux)
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
            Row::GpuAdapter => {
                // The names wgpu reports, plus "auto" and the name already in the
                // file: a closed list that hid a value from another machine would
                // read as the setting having been lost.
                let current = self.draft.gpu.adapter.clone();
                let label = current.clone().unwrap_or_else(|| "auto".to_owned());
                let listed = crate::gpu::adapters();
                let mut change = None;
                let response = egui::ComboBox::from_id_salt("gpu-adapter")
                    .selected_text(egui::RichText::new(&label).size(12.0))
                    .width(190.0)
                    .show_ui(ui, |ui| {
                        if dtr_widgets::menu_row(ui, skin, "auto", current.is_none()) {
                            change = Some(Change::GpuAdapter(String::new()));
                        }
                        for name in listed {
                            if dtr_widgets::menu_row(
                                ui,
                                skin,
                                name,
                                current.as_deref() == Some(name.as_str()),
                            ) {
                                change = Some(Change::GpuAdapter(name.clone()));
                            }
                        }
                        if let Some(name) = &current
                            && !listed.iter().any(|known| known == name)
                            && dtr_widgets::menu_row(ui, skin, name, true)
                        {
                            change = Some(Change::GpuAdapter(name.clone()));
                        }
                    });
                response
                    .response
                    .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, &name));
                change
            }
            Row::GpuPower => {
                // A dropdown like the other closed lists: a segmented row of
                // three words spends the width the card's text needs, and this
                // one has to say "restart" in its description.
                let current = self.draft.gpu.power;
                let mut change = None;
                let response = egui::ComboBox::from_id_salt("gpu-power")
                    .selected_text(egui::RichText::new(current.to_string()).size(12.0))
                    .width(140.0)
                    .show_ui(ui, |ui| {
                        for power in dtr_config::GpuPower::ALL {
                            let label = power.to_string();
                            if dtr_widgets::menu_row(ui, skin, &label, power == current) {
                                change = Some(Change::GpuPower(power));
                            }
                        }
                    });
                response
                    .response
                    .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, &name));
                change
            }
        }
    }
}

impl Settings {
    /// The accent this sheet would draw with right now: the draft's if it has
    /// one, else the chosen look's own.
    ///
    /// Asked so the field can *show* the answer. An empty box beside `ui.accent`
    /// reads as a missing value rather than as "the look's own", and the difference
    /// between Windows' `#005FB8` and egui's `#009BFF` is exactly what a person
    /// opening that row wants to see before deciding to change it. `dark` is the
    /// scheme the draft resolves to — the caller asks `Choice::skin` for it, so the
    /// desktop and theme fallbacks are the same ones the window uses.
    fn effective_accent(&self, dark: bool) -> Option<[u8; 3]> {
        let choice = crate::ui::Look::choice_of(&self.draft);
        if let Some(accent) = choice.accent {
            return Some([accent.r(), accent.g(), accent.b()]);
        }
        choice
            .preset
            .default_accent(dark)
            .map(|accent| [accent.r(), accent.g(), accent.b()])
    }

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
    fn cancelling_puts_the_draft_back() {
        let mut settings = Settings::new();
        settings.show(&Config::default());
        settings.apply(Change::Panel(Panel::Appearance));
        let before = settings.draft().font.size;
        settings.apply(Change::FontSize(before + 2.0));
        assert!(settings.draft().font.size > before);

        settings.cancel();
        assert_eq!(settings.draft().font.size, before);
        assert!(!settings.open, "cancelling closes the sheet");
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
        settings.apply(Change::Baud(230_400));
        assert_eq!(settings.serial().unwrap().baud, 230_400);
        settings.apply(Change::Baud(115_200));
        assert_eq!(settings.serial().unwrap().baud, 115_200);
    }

    #[test]
    fn without_a_port_the_serial_rows_are_inert_and_say_so() {
        let mut settings = Settings::new();
        settings.show(&Config::default());
        settings.apply(Change::Baud(230_400));
        assert!(settings.serial_write().is_none());
        assert_eq!(settings.value(Row::Baud), "no port");
    }

    /// Opening the menu takes the list the probe found; it does not walk the
    /// machine's fonts itself.
    ///
    /// `show` runs in the click that opens the card, and the probe is a
    /// fontconfig walk that parses a face per family — 300 ms of frozen window,
    /// which is what "the preferences take a while to open" was. `main` warms it and
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
        settings.draft.ui.widgets = dtr_config::Widgets::Adwaita;
        settings.draft.ui.scheme = dtr_config::Scheme::Light;
        settings.draft.ui.accent = Some([0xED, 0x5B, 0x00]);
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
        assert_eq!(parsed.ui.widgets, dtr_config::Widgets::Adwaita);
        assert_eq!(parsed.ui.scheme, dtr_config::Scheme::Light);
        assert_eq!(parsed.ui.accent, Some([0xED, 0x5B, 0x00]));
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
        assert!(keys.contains(&"tmux.enabled"));
        assert!(keys.contains(&"ui.widgets"));
        assert!(keys.contains(&"ui.scheme"));
        assert!(keys.contains(&"ui.accent"));
        // Every row that lives in the file must have a writer; the serial rate
        // rows live in the store and are covered by `serial_write`.
        for row in Row::ALL {
            if matches!(row, Row::Baud | Row::CrToCrlf) {
                continue;
            }
            assert!(
                keys.contains(&row.key().as_str()),
                "{} is not written",
                row.key()
            );
        }
    }

    /// A panel is a filter over one list of rows, so every row is in exactly one:
    /// a row in neither is a setting nobody can reach, and a row in both is a row
    /// drawn twice under two names.
    #[test]
    fn every_row_belongs_to_exactly_one_panel() {
        for row in Row::ALL {
            let panels: Vec<Panel> = Panel::ALL
                .into_iter()
                .filter(|panel| panel.rows(true, true).contains(&row))
                .collect();
            assert_eq!(panels, vec![row.panel()], "{} is misplaced", row.key());
        }
    }

    /// The sheet opens on the first tab, always: general, whichever session is
    /// focused, so where the rows are never moves between openings.
    #[test]
    fn the_sheet_opens_on_the_first_tab() {
        let mut settings = Settings::new();
        settings.show(&Config::default());
        assert_eq!(settings.panel(), Panel::General);
        assert_eq!(
            settings.rows(),
            vec![
                Row::AutoReconnect,
                Row::Tmux,
                Row::GpuAdapter,
                Row::GpuPower
            ],
            "a shell has no port to describe"
        );

        settings.show_port(
            &Config::default(),
            Some("/dev/ttyUSB0".to_owned()),
            Some(dtr_store::SerialSettings::default()),
        );
        assert_eq!(settings.panel(), Panel::General);
        assert_eq!(
            settings.rows(),
            vec![
                Row::Baud,
                Row::CrToCrlf,
                Row::AutoReconnect,
                Row::Tmux,
                Row::GpuAdapter,
                Row::GpuPower
            ]
        );

        // Appearance is where the window is, and it never has a port row.
        settings.apply(Change::Panel(Panel::Appearance));
        assert_eq!(
            settings.rows(),
            vec![
                Row::Widgets,
                Row::Theme,
                Row::UiFont,
                Row::FontSize,
                Row::FontFamily,
                Row::Padding
            ],
            "a native shell has no scheme or accent of its own"
        );
    }

    /// The tmux row is the file's key and the settings' own switch: the pointer
    /// writes the draft and a save writes `tmux.enabled`.
    #[test]
    fn the_tmux_toggle_is_one_row_and_one_key() {
        let mut settings = Settings::new();
        settings.show(&Config::default());
        assert!(settings.rows().contains(&Row::Tmux));
        assert!(
            !settings.draft.tmux.enabled,
            "the sheet opens on what the file says"
        );

        settings.apply(Change::Tmux(true));
        assert!(settings.draft.tmux.enabled);
        assert!(
            settings
                .writes()
                .contains(&("tmux.enabled".to_owned(), "true".to_owned())),
            "the row and the file are one key"
        );
    }

    /// The padding row is the file's key and the draft's number: one value, and
    /// the control cannot write one outside the range the parser accepts.
    #[test]
    fn the_padding_row_is_one_key() {
        let mut settings = Settings::new();
        settings.show(&Config::default());
        assert_eq!(settings.draft.padding, dtr_config::PADDING_DEFAULT);
        settings.apply(Change::Padding(12));
        assert_eq!(settings.draft.padding, 12);
        assert!(
            settings
                .writes()
                .contains(&("window.padding".to_owned(), "12".to_owned())),
            "the row and the file are one key"
        );
    }

    /// The chrome's face is one key: a family in the file, or an empty string
    /// meaning "follow the desktop".
    #[test]
    fn the_chrome_font_is_one_key() {
        let mut settings = Settings::new();
        settings.show(&Config::default());
        assert!(settings.draft.ui.font.is_none());
        settings.apply(Change::UiFont(Some("Geist".to_owned())));
        assert_eq!(settings.draft.ui.font.as_deref(), Some("Geist"));
        assert!(
            settings
                .writes()
                .contains(&("ui.font".to_owned(), "Geist".to_owned())),
            "the row and the file are one key"
        );
        settings.apply(Change::UiFont(None));
        assert!(
            settings
                .writes()
                .contains(&("ui.font".to_owned(), String::new())),
            "system is written as empty, not as a pinned family"
        );
    }

    /// The GPU rows are two keys, and an empty adapter is written back as empty
    /// so "let wgpu choose" survives a save.
    #[test]
    fn the_gpu_rows_are_one_key_each() {
        let mut settings = Settings::new();
        settings.show(&Config::default());
        assert_eq!(settings.rows().last(), Some(&Row::GpuPower));

        settings.apply(Change::GpuAdapter(" nvidia ".to_owned()));
        assert_eq!(settings.draft.gpu.adapter.as_deref(), Some("nvidia"));
        settings.apply(Change::GpuPower(dtr_config::GpuPower::Low));
        assert!(
            settings
                .writes()
                .contains(&("gpu.adapter".to_owned(), "nvidia".to_owned()))
        );
        assert!(
            settings
                .writes()
                .contains(&("gpu.power".to_owned(), "low".to_owned()))
        );

        settings.apply(Change::GpuAdapter(String::new()));
        assert!(
            settings
                .writes()
                .contains(&("gpu.adapter".to_owned(), String::new()))
        );
    }

    /// The port's rows are the port's panel, whatever else is showing: a baud rate
    /// beside the font size was the one column this split exists to end.
    #[test]
    fn the_port_rows_are_on_the_general_panel() {
        let mut settings = Settings::new();
        settings.show_port(
            &Config::default(),
            Some("/dev/ttyUSB0".to_owned()),
            Some(dtr_store::SerialSettings::default()),
        );
        assert!(!settings.rows().contains(&Row::FontSize));
        settings.apply(Change::Panel(Panel::Appearance));
        assert!(!settings.rows().contains(&Row::Baud));
    }

    /// The scheme and the accent exist only for a look that owns them.
    #[test]
    fn the_look_rows_follow_the_look_that_owns_them() {
        let mut settings = Settings::new();
        settings.show(&Config::default());
        settings.apply(Change::Panel(Panel::Appearance));
        settings.apply(Change::Widgets(dtr_config::Widgets::Adwaita));
        assert_eq!(
            settings.rows(),
            vec![
                Row::Widgets,
                Row::Scheme,
                Row::Accent,
                Row::Theme,
                Row::UiFont,
                Row::FontSize,
                Row::FontFamily,
                Row::Padding
            ]
        );
        // Ask for the look that has no accent.
        settings.apply(Change::Widgets(dtr_config::Widgets::Native));
        assert!(!settings.rows().contains(&Row::Accent));
    }

    /// An accent is typed as a hex number, and a half-typed one must not be
    /// mistaken for a colour: the field keeps what was typed and the draft keeps
    /// the last thing that parsed.
    #[test]
    fn a_half_typed_accent_is_not_a_colour() {
        let mut settings = Settings::new();
        settings.show(&Config::default());
        settings.apply(Change::Widgets(dtr_config::Widgets::Adwaita));
        settings.apply(Change::Accent("#ED5B".to_owned()));
        assert_eq!(settings.draft().ui.accent, None, "half a hex is not a hex");
        assert_eq!(settings.accent_text, "#ED5B");
        settings.apply(Change::Accent("#ED5B00".to_owned()));
        assert_eq!(settings.draft().ui.accent, Some([0xED, 0x5B, 0x00]));
        // Empty means the look's own, which is how the list's first entry reads.
        settings.apply(Change::Accent(String::new()));
        assert_eq!(settings.draft().ui.accent, None);
        assert_eq!(settings.value(Row::Accent), "the look's own");
    }

    /// The draft a save writes is the skin the window draws: menu, config file
    /// and skin are one chain, and a test that stopped at the file would not
    /// notice the chain being cut in the middle — which is what a look that saved
    /// correctly and never applied would look like.
    #[test]
    fn the_draft_a_save_writes_is_the_skin_the_window_draws() {
        let mut settings = Settings::new();
        settings.show(&Config::default());
        settings.apply(Change::Panel(Panel::Appearance));
        settings.apply(Change::Widgets(dtr_config::Widgets::Adwaita));
        settings.apply(Change::Scheme(dtr_config::Scheme::Light));
        settings.apply(Change::Accent("#ED5B00".to_owned()));

        // `apply_settings` writes this draft to the file and hands it to
        // `Look::choice_of`; both halves are asked here.
        let choice = crate::ui::Look::choice_of(settings.draft());
        assert_eq!(choice.preset, dtr_widgets::Preset::Adwaita);
        assert_eq!(choice.scheme, dtr_widgets::Scheme::Light);
        assert_eq!(
            choice.accent,
            Some(egui::Color32::from_rgb(0xED, 0x5B, 0x00))
        );

        let skin = choice.skin(
            dtr_widgets::Palette::dtr([0x0b, 0x0f, 0x0e], [0xdc, 0xdf, 0xdd], [0xff, 0xb0, 0x00]),
            None,
        );
        assert_eq!(skin.origin, dtr_widgets::Origin::Adwaita);
        assert!(!skin.palette.is_dark(), "the scheme row said light");
        assert_eq!(
            skin.palette.accent,
            egui::Color32::from_rgb(0xED, 0x5B, 0x00)
        );
    }

    /// The accent field shows the colour a save would use, so an unset accent is
    /// not a blank box: Windows' own is `#005FB8` in the light and `#60CDFF` in
    /// the dark, and which one is only knowable once the scheme has been resolved.
    #[test]
    fn the_accent_field_shows_the_colour_a_save_would_use() {
        let mut settings = Settings::new();
        settings.show(&Config::default());
        settings.apply(Change::Panel(Panel::Appearance));
        // `native` has no accent of its own — the desktop's is not ours to show.
        assert_eq!(settings.effective_accent(true), None);

        settings.apply(Change::Widgets(dtr_config::Widgets::Adwaita));
        assert_eq!(settings.effective_accent(true), Some([0x35, 0x84, 0xE4]));

        settings.apply(Change::Widgets(dtr_config::Widgets::Windows));
        settings.apply(Change::Scheme(dtr_config::Scheme::Light));
        assert_eq!(settings.effective_accent(false), Some([0x00, 0x5F, 0xB8]));
        settings.apply(Change::Scheme(dtr_config::Scheme::Dark));
        assert_eq!(settings.effective_accent(true), Some([0x60, 0xCD, 0xFF]));

        // A chosen accent beats the look's own, which is what the field then shows.
        settings.apply(Change::Accent("#ED5B00".to_owned()));
        assert_eq!(settings.effective_accent(true), Some([0xED, 0x5B, 0x00]));
    }

    /// The accent list is the looks' own defaults, because a way back to a default
    /// that has to be remembered is not a way back.
    #[test]
    fn the_accent_list_offers_the_looks_own_accents() {
        let choices = accent_choices();
        assert_eq!(choices.first().map(String::as_str), Some(DEFAULT_ACCENT));
        assert!(choices.contains(&dtr_config::hex([0x35, 0x84, 0xE4])));
        let mut unique = choices.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), choices.len(), "a default is listed twice");
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
                Row::AutoReconnect,
                Row::Tmux,
                Row::GpuAdapter,
                Row::GpuPower
            ]
        );
        settings.apply(Change::Panel(Panel::Appearance));
        assert_eq!(
            settings.rows(),
            vec![
                Row::Widgets,
                Row::Theme,
                Row::UiFont,
                Row::FontSize,
                Row::FontFamily,
                Row::Padding
            ]
        );
    }

    /// The theme is a combo box: click it, click a theme, get the edit. This is
    /// the mouse path the menu is meant to be driven by.
    #[test]
    fn picking_a_theme_from_the_combo_box_reports_it() {
        let mut settings = Settings::new();
        settings.show(&Config::default());
        settings.apply(Change::Panel(Panel::Appearance));
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
            matches!(*change.borrow(), Some(Reply::Change(Change::Theme(_)))),
            "clicking a theme must report the choice"
        );
    }

    /// The card's three ways out are buttons, and each reports the same answer
    /// the keyboard gives: save applies, cancel and the cross throw the draft
    /// away.
    #[test]
    fn the_cross_and_the_footer_buttons_report_their_answers() {
        let mut settings = Settings::new();
        settings.show(&Config::default());
        let look = crate::ui::Look::default();
        let answers = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = answers.clone();
        let mut harness = egui_kittest::Harness::new_ui(move |ui| {
            if let Some(reply) = settings.ui(ui, &look) {
                sink.borrow_mut().push(reply);
            }
        });
        harness.run();

        harness.get_by_label("save").click();
        harness.run();
        assert_eq!(answers.borrow().last(), Some(&Reply::Apply));

        harness.get_by_label("cancel").click();
        harness.run();
        assert_eq!(answers.borrow().last(), Some(&Reply::Cancel));

        harness.get_by_label("close preferences").click();
        harness.run();
        assert_eq!(answers.borrow().last(), Some(&Reply::Cancel));
    }

    /// The rows are drawn and the menu renders: a snapshot catches a row that
    /// moves, a label that disappears, or the highlight landing on the wrong row.
    /// This is the sheet as it opens, on the first tab.
    #[test]
    fn the_menu_renders() {
        let mut settings = Settings::new();
        settings.show(&Config::default());
        let look = crate::ui::Look::default();
        let mut harness = Harness::new_ui(|ui| {
            settings.ui(ui, &look);
        });
        harness.run();
        harness.snapshot("preferences_menu");
    }

    /// The appearance panel, with a look that owns its colours: two more rows and
    /// two more kinds of control — a segmented scheme and an accent field.
    #[test]
    fn the_appearance_panel_renders_its_rows() {
        let mut settings = Settings::new();
        settings.show(&Config::default());
        settings.apply(Change::Panel(Panel::Appearance));
        settings.apply(Change::Widgets(dtr_config::Widgets::Adwaita));
        settings.apply(Change::Scheme(dtr_config::Scheme::Light));
        settings.apply(Change::Accent("#ED5B00".to_owned()));
        let look = crate::ui::Look::default();
        let mut harness = Harness::new_ui(|ui| {
            settings.ui(ui, &look);
        });
        harness.run();
        // The rows exist, which is what the snapshot is for; the controls are
        // addressed by name, which is what a screen reader sees.
        harness.get_by_label("ui.accent value");
        harness.snapshot("preferences_appearance");
    }

    /// A port's panel on its own, with the port rows: the sheet a person with a
    /// serial board open lands on.
    #[test]
    fn the_general_panel_renders_its_port_rows() {
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
        let look = crate::ui::Look::default();
        let mut harness = Harness::new_ui(|ui| {
            settings.ui(ui, &look);
        });
        harness.run();
        harness.get_by_label("serial.baud value");
        harness.snapshot("preferences_general");
    }

    /// The tabs are tabs: a press on the other one asks for its panel, and the
    /// current one is marked selected for a screen reader.
    #[test]
    fn pressing_a_tab_asks_for_that_panel() {
        let mut settings = Settings::new();
        settings.show(&Config::default());
        let look = crate::ui::Look::default();
        let asked = std::rc::Rc::new(std::cell::RefCell::new(None));
        let sink = asked.clone();
        let mut harness = egui_kittest::Harness::new_ui(move |ui| {
            if let Some(reply) = settings.ui(ui, &look) {
                *sink.borrow_mut() = Some(reply);
            }
        });
        harness.run();
        harness.get_by_label("preferences panel appearance").click();
        harness.run();
        assert_eq!(
            *asked.borrow(),
            Some(Reply::Change(Change::Panel(Panel::Appearance)))
        );
    }

    /// The look is a dropdown of four, and picking one reports it.
    ///
    /// The same two presses the theme row takes: one opens the list, one picks.
    #[test]
    fn picking_a_look_reports_it() {
        let mut settings = Settings::new();
        settings.show(&Config::default());
        settings.apply(Change::Panel(Panel::Appearance));
        let look = crate::ui::Look::default();
        let asked = std::rc::Rc::new(std::cell::RefCell::new(None));
        let sink = asked.clone();
        let mut harness = egui_kittest::Harness::new_ui(move |ui| {
            if let Some(reply) = settings.ui(ui, &look) {
                *sink.borrow_mut() = Some(reply);
            }
        });
        harness.run();
        harness.get_by_label("ui.widgets value").click();
        harness.run();
        harness.get_by_label("windows").click();
        harness.run();
        assert_eq!(
            *asked.borrow(),
            Some(Reply::Change(Change::Widgets(dtr_config::Widgets::Windows)))
        );
    }

    /// A row is the window's control, not a question to be submitted: the draft
    /// is on the window before Save is pressed, and Cancel puts the file's back.
    ///
    /// The loop here is `App`'s — a report is applied to the draft and the look
    /// is re-derived from it — and the assertion is the pixels, because "the
    /// window reloads" is a claim about the picture, not about a field.
    ///
    /// The change is made through the model rather than by clicking, on purpose:
    /// a click leaves the pointer over the row it pressed and the focus on it,
    /// and a picture that differs by a hover fill would prove nothing. That a
    /// click *reaches* the model is `picking_a_look_reports_it`'s job.
    #[test]
    fn an_edit_repaints_the_sheet_before_it_is_saved() {
        // `adwaita` to begin with, so the edit is a change of *look* rather than
        // of one accent: the sheet is drawn in the skin, and the skin is what a
        // look changes.
        let mut config = Config::default();
        config.ui.widgets = dtr_config::Widgets::Adwaita;
        let mut settings = Settings::new();
        settings.show(&config);
        settings.apply(Change::Panel(Panel::Appearance));

        let state = std::rc::Rc::new(std::cell::RefCell::new((
            settings,
            crate::ui::Look::from_config(&config, None),
        )));
        let drawing = state.clone();
        let mut harness = egui_kittest::Harness::new_ui(move |ui| {
            let (settings, look) = &mut *drawing.borrow_mut();
            let reply = settings.ui(ui, look);
            match reply {
                // Save writes the file; the window already has the draft, which
                // is the point of previewing.
                Some(Reply::Apply) => {}
                Some(Reply::Change(change)) => {
                    settings.apply(change);
                    *look = crate::ui::Look::from_config(settings.draft(), None);
                }
                // `cancel` restores the draft; the loop re-derives the window
                // from it, which is how a preview is taken back.
                Some(Reply::Cancel) => {
                    settings.cancel();
                    *look = crate::ui::Look::from_config(settings.draft(), None);
                }
                None => {}
            }
        });
        harness.run();
        let adwaita = harness.render().expect("the harness renders offscreen");

        {
            let (settings, look) = &mut *state.borrow_mut();
            settings.apply(Change::Widgets(dtr_config::Widgets::Windows));
            *look = crate::ui::Look::from_config(settings.draft(), None);
        }
        harness.run();
        let windows = harness.render().expect("the harness renders offscreen");
        assert!(
            adwaita.as_raw() != windows.as_raw(),
            "the look changed and the sheet drew the same pixels"
        );
        assert_eq!(state.borrow().1.choice.preset, dtr_widgets::Preset::Windows);

        // Cancel is a revert: the file's look is back on the window, and the
        // picture with it. The bytes are not compared to the first render,
        // because a harness that has run two dozen frames of one skin and then
        // another keeps a little of each in egui's own style state — what the
        // revert has to show is that the *preview* is gone, which is the skin
        // and the pixels both.
        {
            let (settings, look) = &mut *state.borrow_mut();
            settings.cancel();
            *look = crate::ui::Look::from_config(settings.draft(), None);
        }
        harness.run();
        assert_eq!(state.borrow().1.choice.preset, dtr_widgets::Preset::Adwaita);
        let reverted = harness.render().expect("the harness renders offscreen");
        assert!(
            windows.as_raw() != reverted.as_raw(),
            "cancel left the previewed look on the window"
        );
    }
}
