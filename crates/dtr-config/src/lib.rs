//! The config file: what dtr reads at startup, and what the UI edits in place.
//!
//! Editing is done with `toml_edit`, not by rewriting the file from a
//! `serde`-derived struct, because a config file is a document a human wrote.
//! Comments, ordering and formatting are the user's; a program that reformats
//! them on save is a program people stop editing.

use std::fmt;
use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, Table, value};

pub mod keys;

/// The most scrollback a tab may keep, in bytes, and the default.
///
/// There is no "unlimited": the VT core's own default *is* unlimited, and a
/// terminal that keeps every line a program prints is a terminal that grows
/// until the kernel kills it — measured at 50 GB of anonymous memory after a
/// benchmark flood, which took the terminal and the run with it. A ceiling is
/// therefore part of the type, not a recommendation in a comment.
pub const SCROLLBACK_LIMIT: usize = 100_000_000;

/// The range `serial.key_delay_ms` accepts, in milliseconds.
///
/// The floor is one byte period — zero would be "off", which is what the
/// runtime switch is for, not a config value — and the ceiling is the point past
/// which the delay is a stall rather than a help: 1000 ms between two bytes is
/// slower than reading them out of a paste.
pub const KEY_DELAY_MS_RANGE: std::ops::RangeInclusive<u32> = 1..=1000;

/// Everything configurable. Defaults are what a fresh install gets.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// Key bindings from `[keys]`, action name to chord text. Kept as text here
    /// and matched by the app, so the config crate needs no window library.
    pub keys: std::collections::BTreeMap<String, keys::Sequence>,
    pub serial: Serial,
    pub font: Font,
    /// Who paints the window frame. `[window] chrome`.
    pub chrome: Chrome,
    /// What the controls look like. `[ui] widgets`.
    pub ui: Ui,
    /// Name of a theme file, e.g. `"kanagawa wave"`. `None` keeps the
    /// terminal's built-in colours.
    pub theme: Option<String>,
    pub terminal: Terminal,
    /// Where a program's notification goes. `[notifications]`.
    pub notifications: Notifications,
    /// Whether an ssh session is put inside a tmux session. `[tmux]`.
    pub tmux: Tmux,
    /// How a file pane looks. `[files]`.
    pub files: Files,
}

/// How a file pane lays its listing out. `[files] view`.
///
/// The browser's own `Layout` lives in `dtr-files`, which pulls in egui; this crate
/// parses text and pulls in nothing but `toml_edit` and a key parser. So the two
/// are separate values and the window is where they meet — the same shape
/// `[ui] widgets` has with `dtr_widgets::Preset`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FilesView {
    /// A grid of icon tiles: the default, as it is in Nautilus.
    #[default]
    Grid,
    /// One row per entry, with a size column. Kept for a narrow pane.
    List,
}

impl FilesView {
    pub const ALL: [FilesView; 2] = [FilesView::Grid, FilesView::List];

    /// The word a config file and the preferences card use.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Grid => "grid",
            Self::List => "list",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "grid" => Some(Self::Grid),
            "list" => Some(Self::List),
            _ => None,
        }
    }
}

/// What a file pane orders by. `[files] sort`.
///
/// Four keys, spelled the way the sort control spells them. The browser's own
/// `Sort` is the value; this is the word in the file, and the window is where
/// they meet — the same shape [`FilesView`] has with `dtr_files::Layout`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FilesSort {
    #[default]
    Name,
    Modified,
    Size,
    /// The extension, which is what a file manager means by "type".
    Kind,
}

impl FilesSort {
    pub const ALL: [FilesSort; 4] = [
        FilesSort::Name,
        FilesSort::Modified,
        FilesSort::Size,
        FilesSort::Kind,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Modified => "modified",
            Self::Size => "size",
            Self::Kind => "type",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "name" => Some(Self::Name),
            "modified" => Some(Self::Modified),
            "size" => Some(Self::Size),
            "type" | "kind" => Some(Self::Kind),
            _ => None,
        }
    }
}

/// How a file pane looks and is ordered. `[files]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Files {
    /// Which view a pane opens in, and what `Ctrl+1`/`Ctrl+2` write back.
    pub view: FilesView,
    /// Whether dot-files are listed. Off by default, as in every file manager;
    /// `Ctrl+H` flips it and writes the answer back.
    pub hidden: bool,
    /// What the listing is ordered by.
    pub sort: FilesSort,
    /// Whether that key runs backwards. Folders stay on top either way.
    pub reverse: bool,
    /// Whether folders come before files.
    pub folders_first: bool,
}

impl Default for Files {
    fn default() -> Self {
        Self {
            view: FilesView::Grid,
            hidden: false,
            sort: FilesSort::Name,
            reverse: false,
            folders_first: true,
        }
    }
}

/// How a program's `OSC 9`/`OSC 777` reaches the person.
#[derive(Clone, Debug, PartialEq)]
pub struct Notifications {
    /// Use the desktop's own notification channel when the machine has one.
    ///
    /// On by default on every platform, because the alternative is dtr drawing
    /// its own toast and a person with notifications already configured expects
    /// the one their desktop uses. A machine with no channel — Windows today,
    /// deliberately (a toast there needs a Start Menu shortcut with an
    /// AppUserModelID), and a Linux session without a notification daemon — falls
    /// back to that toast anyway, so this key is only ever a way to *prefer*
    /// dtr's window.
    pub os: bool,
}

impl Default for Notifications {
    fn default() -> Self {
        Self { os: true }
    }
}

/// How an ssh session is made to outlive the connection that opened it.
#[derive(Clone, Debug, PartialEq)]
pub struct Tmux {
    /// Run every ssh session inside a tmux session on the far end.
    ///
    /// Off by default. It changes what the far end does with a connection and it
    /// needs tmux there, which is a surprise to spring on someone who only asked
    /// for a terminal; the switch is a row in the preferences card as well as this
    /// key. What the host has is asked either way, so turning it on where there
    /// is no tmux costs a toast and nothing else.
    pub enabled: bool,
}

impl Default for Tmux {
    fn default() -> Self {
        Self { enabled: false }
    }
}

/// Facts about the terminal itself, as opposed to what it is showing.
#[derive(Clone, Debug, PartialEq)]
pub struct Terminal {
    /// Bytes of scrollback a tab keeps before the oldest pages are dropped.
    ///
    /// Clamped to `[0, SCROLLBACK_LIMIT]` when the file is read: a value above
    /// the ceiling is a mistake, not an instruction, and the error says so.
    pub scrollback_limit: usize,
    /// Whether a program may read the clipboard with `OSC 52 ; c ; ?`.
    ///
    /// Off by default. A read is a real capability — a program on the far end of
    /// an ssh session could ask for whatever was copied last — so it is opt-in,
    /// and a request that is not allowed is *refused* rather than ignored, which
    /// is a difference the program can see. (Ghostty's `clipboard-read` defaults
    /// to asking; dtr has no prompt yet, so the honest default is "no".)
    pub clipboard_read: bool,
    /// Whether a program may resize the window with `CSI 8 ; rows ; cols t`.
    ///
    /// Off by default, and deliberately so: a program that can resize the window
    /// is a program that can resize the window *while somebody is typing*, and
    /// Ghostty's own escape hatch is opt-in for the same reason.
    pub window_resize: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Serial {
    /// Follow a device across a replug.
    ///
    /// This is the one serial setting that is a *policy of the terminal* rather
    /// than a fact about a device: it says whether dtr watches for the port
    /// coming back. Baud and CR translation describe the board on the other end
    /// of the cable, so they live in the store as per-port memory (see
    /// `dtr-store`), not here.
    pub auto_reconnect: bool,
    /// How long to leave between two bytes typed on a serial line, in
    /// milliseconds, while delayed input is switched on.
    ///
    /// A bootloader's line reader often has no flow control and a buffer small
    /// enough to drop the second byte of a fast escape sequence, so a person
    /// typing at it gets `hepl` for `help`. Spacing the bytes is the only fix
    /// that does not need the board's cooperation. This is the *period*; the
    /// mode itself is a runtime switch (`ctrl-alt-d`, or the palette's `lag`)
    /// because it is a property of the board in front of you, not of the
    /// terminal.
    ///
    /// 1–1000 ms. The default, 1 ms, is enough for the U-Boot configuration this
    /// was written for, and small enough not to be felt on a board that needs
    /// nothing.
    pub key_delay_ms: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Font {
    /// Grid font size in points.
    pub size: f32,
    /// An installed family to use, e.g. `"JetBrains Mono"`. `None` picks the
    /// first usable family the probe likes.
    pub family: Option<String>,
    /// A font file to load instead of any installed family. `.ttf`/`.otf`; the
    /// first face in the file is used. Hand-edited only: the preferences card
    /// offers families, because a path you cannot verify is a broken terminal.
    pub path: Option<PathBuf>,
    /// Families to try, in order, when the grid face has no glyph for a
    /// character — before dtr's own list and before the walk over everything
    /// installed.
    ///
    /// This is the answer to "which font should draw CJK on *this* machine":
    /// dtr's list is a good guess (Sarasa, then Noto), and the platform's
    /// answer is a different guess, but only the person looking at the screen
    /// knows which of the six installed Han faces they want.
    pub fallback: Vec<String>,
}

/// Who paints the window's frame.
///
/// `Client` means dtr's own caption: the tab strip is the title bar, with the
/// window buttons drawn on it. `Native` is the old layout — the window manager's
/// decorations above a tab strip — which is also the escape hatch for a
/// compositor or a tiling WM that does the chrome better.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chrome {
    Client,
    Native,
}

impl Default for Chrome {
    fn default() -> Self {
        Self::Client
    }
}

impl fmt::Display for Chrome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Client => "client",
            Self::Native => "native",
        })
    }
}

/// What the chrome's controls are drawn as.
///
/// `Native` is the platform's own look — the system palette, its scheme and the
/// user's accent — and falls back to the terminal's own chrome where a machine
/// cannot say. `Adwaita`, `Windows` and `Egui` are looks the program carries with
/// it, so a person on Linux can have Windows' chrome and a person on Windows can
/// have GNOME's; they are the three with a scheme and an accent of their own,
/// because `native` reads both from the desktop.
///
/// `"dtr"` is accepted as an old spelling of `"egui"`: the fourth look is what this
/// program's chrome used to be called, and a config file written before the menu
/// existed keeps working.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Widgets {
    Native,
    Adwaita,
    Windows,
    Egui,
}

impl Default for Widgets {
    fn default() -> Self {
        Self::Native
    }
}

impl fmt::Display for Widgets {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Native => "native",
            Self::Adwaita => "adwaita",
            Self::Windows => "windows",
            Self::Egui => "egui",
        })
    }
}

impl Widgets {
    pub const ALL: [Widgets; 4] = [
        Widgets::Native,
        Widgets::Adwaita,
        Widgets::Windows,
        Widgets::Egui,
    ];
}

/// Which scheme the chrome draws in.
///
/// `System` is the desktop's answer, and the default: a chrome that follows the
/// desktop is the one that matches every other window at sunset. `Light` and
/// `Dark` are an instruction, for someone who would rather the chrome stayed put.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scheme {
    #[default]
    System,
    Light,
    Dark,
}

impl fmt::Display for Scheme {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        })
    }
}

impl Scheme {
    /// Every scheme the parser accepts, for a menu that offers them.
    pub const ALL: [Scheme; 3] = [Scheme::System, Scheme::Light, Scheme::Dark];
}

/// The controls' look. `[ui]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Ui {
    pub widgets: Widgets,
    /// The scheme, for the looks that own one. `native` and `dtr` ignore it.
    pub scheme: Scheme,
    /// The accent as `#RRGGBB`, for the looks that own one. `None` keeps the
    /// preset's own default, so a window that never set one still follows a
    /// future default rather than the value of the day it was first saved.
    pub accent: Option<[u8; 3]>,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            widgets: Widgets::default(),
            scheme: Scheme::default(),
            accent: None,
        }
    }
}

/// A colour as `#RRGGBB`, which is what the config file holds and the settings
/// menu shows.
pub fn hex(colour: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", colour[0], colour[1], colour[2])
}

/// Parse `#RRGGBB` or `RRGGBB`, the two spellings a person types.
///
/// A three-digit `#RGB` is not accepted: the value is written back by the settings
/// menu, and a short form that came back long is a line the user did not write.
pub fn parse_hex(text: &str) -> Option<[u8; 3]> {
    let digits = text.trim().strip_prefix('#').unwrap_or(text.trim());
    if digits.len() != 6 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?])
}

impl Default for Config {
    fn default() -> Self {
        Self {
            keys: std::collections::BTreeMap::new(),
            serial: Serial {
                auto_reconnect: true,
                key_delay_ms: 1,
            },
            font: Font {
                size: 14.0,
                family: None,
                path: None,
                fallback: Vec::new(),
            },
            chrome: Chrome::default(),
            ui: Ui::default(),
            theme: None,
            terminal: Terminal::default(),
            notifications: Notifications::default(),
            files: Files::default(),
            tmux: Tmux::default(),
        }
    }
}

impl Default for Terminal {
    fn default() -> Self {
        Self {
            scrollback_limit: SCROLLBACK_LIMIT,
            clipboard_read: false,
            window_resize: false,
        }
    }
}

impl Default for Serial {
    fn default() -> Self {
        Config::default().serial
    }
}

impl Default for Font {
    fn default() -> Self {
        Config::default().font
    }
}

/// `~/.config/dtr/config.toml` (or the platform's equivalent).
pub fn default_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_default()
        .join("dtr")
        .join("config.toml")
}

impl Config {
    /// Read a config file. A missing file is not an error — it is the state
    /// every install starts in.
    pub fn load(path: &Path) -> Result<Self, Error> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(Error::Io(err.to_string())),
        };
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self, Error> {
        let document: DocumentMut = text
            .parse()
            .map_err(|err| Error::Syntax(format!("{err}")))?;
        let mut config = Self::default();

        if let Some(serial) = document.get("serial").and_then(Item::as_table) {
            // `serial.baud` and `serial.cr_to_crlf` used to live here. They are
            // remembered per port now, no released version ever wrote them, so
            // there is nothing to migrate and nothing worth a warning. A file
            // that still has them reads the rest and carries on.
            if let Some(reconnect) = serial.get("auto_reconnect") {
                config.serial.auto_reconnect = reconnect.as_bool().ok_or_else(|| {
                    Error::Value("serial.auto_reconnect must be true or false".into())
                })?;
            }
            if let Some(delay) = serial.get("key_delay_ms") {
                let value = delay.as_integer().ok_or_else(|| {
                    Error::Value(
                        "serial.key_delay_ms must be a whole number of milliseconds".into(),
                    )
                })?;
                let (low, high) = (
                    i64::from(*KEY_DELAY_MS_RANGE.start()),
                    i64::from(*KEY_DELAY_MS_RANGE.end()),
                );
                if !(low..=high).contains(&value) {
                    return Err(Error::Value(format!(
                        "serial.key_delay_ms {value} is outside {low} to {high} milliseconds"
                    )));
                }
                config.serial.key_delay_ms = value as u32;
            }
        }

        if let Some(table) = document.get("keys").and_then(Item::as_table) {
            for (action, item) in table.iter() {
                // `new-tab = false` turns a default binding off, which is how a
                // key you want for the shell gets handed back to the shell.
                if item.as_bool() == Some(false) {
                    config
                        .keys
                        .insert(action.to_owned(), keys::Sequence::disabled());
                    continue;
                }
                let text = item.as_str().ok_or_else(|| {
                    Error::Value(format!("keys.{action} must be a string or false"))
                })?;
                let sequence = keys::parse_sequence(text)
                    .map_err(|err| Error::Value(format!("keys.{action}: {}", err.reason)))?;
                config.keys.insert(action.to_owned(), sequence);
            }
        }

        if let Some(window) = document.get("window").and_then(Item::as_table) {
            if let Some(chrome) = window.get("chrome") {
                let text = chrome
                    .as_str()
                    .ok_or_else(|| Error::Value("window.chrome must be a string".into()))?;
                config.chrome = match text {
                    "client" => Chrome::Client,
                    "native" => Chrome::Native,
                    other => {
                        return Err(Error::Value(format!(
                            "window.chrome is {other:?}; it must be \"client\" or \"native\""
                        )));
                    }
                };
            }
        }

        if let Some(ui) = document.get("ui").and_then(Item::as_table) {
            if let Some(widgets) = ui.get("widgets") {
                let text = widgets
                    .as_str()
                    .ok_or_else(|| Error::Value("ui.widgets must be a string".into()))?;
                config.ui.widgets = match text {
                    "native" => Widgets::Native,
                    "adwaita" => Widgets::Adwaita,
                    "windows" => Widgets::Windows,
                    // The fourth look's old name: one thing, two spellings.
                    "egui" | "dtr" => Widgets::Egui,
                    other => {
                        return Err(Error::Value(format!(
                            "ui.widgets is {other:?}; it must be one of \
                             \"native\", \"adwaita\", \"windows\" or \"egui\""
                        )));
                    }
                };
            }
            if let Some(scheme) = ui.get("scheme") {
                let text = scheme
                    .as_str()
                    .ok_or_else(|| Error::Value("ui.scheme must be a string".into()))?;
                config.ui.scheme = match text {
                    "system" | "" => Scheme::System,
                    "light" => Scheme::Light,
                    "dark" => Scheme::Dark,
                    other => {
                        return Err(Error::Value(format!(
                            "ui.scheme is {other:?}; it must be \"system\", \"light\" or \"dark\""
                        )));
                    }
                };
            }
            if let Some(accent) = ui.get("accent") {
                let text = accent
                    .as_str()
                    .ok_or_else(|| Error::Value("ui.accent must be a string".into()))?;
                // The preferences card writes an empty string to mean "the preset's
                // own", the same way it writes "no theme".
                config.ui.accent = if text.is_empty() {
                    None
                } else {
                    Some(parse_hex(text).ok_or_else(|| {
                        Error::Value(format!(
                            "ui.accent is {text:?}; it must be a colour like \"#3584E4\""
                        ))
                    })?)
                };
            }
        }

        if let Some(theme) = document.get("theme") {
            let name = theme
                .as_str()
                .ok_or_else(|| Error::Value("theme must be a string".into()))?;
            // The preferences card writes an empty string to mean "no theme";
            // reading it back must not produce a theme called "".
            config.theme = (!name.is_empty()).then(|| name.to_owned());
        }

        if let Some(terminal) = document.get("terminal").and_then(Item::as_table) {
            if let Some(limit) = terminal.get("scrollback_limit") {
                let value = limit.as_integer().ok_or_else(|| {
                    Error::Value("terminal.scrollback_limit must be a number of bytes".into())
                })?;
                if value < 0 || value as usize > SCROLLBACK_LIMIT {
                    return Err(Error::Value(format!(
                        "terminal.scrollback_limit {value} is outside 0 to {SCROLLBACK_LIMIT} bytes; \
                         dtr does not offer unlimited scrollback"
                    )));
                }
                config.terminal.scrollback_limit = value as usize;
            }
            if let Some(read) = terminal.get("clipboard_read") {
                config.terminal.clipboard_read = read.as_bool().ok_or_else(|| {
                    Error::Value("terminal.clipboard_read must be true or false".into())
                })?;
            }
            if let Some(resize) = terminal.get("window_resize") {
                config.terminal.window_resize = resize.as_bool().ok_or_else(|| {
                    Error::Value("terminal.window_resize must be true or false".into())
                })?;
            }
        }

        if let Some(section) = document.get("notifications").and_then(Item::as_table) {
            if let Some(os) = section.get("os") {
                config.notifications.os = os
                    .as_bool()
                    .ok_or_else(|| Error::Value("notifications.os must be true or false".into()))?;
            }
        }

        if let Some(section) = document.get("tmux").and_then(Item::as_table) {
            if let Some(enabled) = section.get("enabled") {
                config.tmux.enabled = enabled
                    .as_bool()
                    .ok_or_else(|| Error::Value("tmux.enabled must be true or false".into()))?;
            }
        }

        if let Some(section) = document.get("files").and_then(Item::as_table) {
            if let Some(hidden) = section.get("hidden") {
                config.files.hidden = hidden
                    .as_bool()
                    .ok_or_else(|| Error::Value("files.hidden must be true or false".into()))?;
            }
            if let Some(sort) = section.get("sort") {
                let text = sort
                    .as_str()
                    .ok_or_else(|| Error::Value("files.sort must be a string".into()))?;
                config.files.sort = FilesSort::parse(text).ok_or_else(|| {
                    Error::Value(format!(
                        "files.sort is {text:?}; it must be \"name\", \"modified\", \"size\" or \"type\""
                    ))
                })?;
            }
            if let Some(reverse) = section.get("reverse") {
                config.files.reverse = reverse
                    .as_bool()
                    .ok_or_else(|| Error::Value("files.reverse must be true or false".into()))?;
            }
            if let Some(first) = section.get("folders-first") {
                config.files.folders_first = first.as_bool().ok_or_else(|| {
                    Error::Value("files.folders-first must be true or false".into())
                })?;
            }
            if let Some(view) = section.get("view") {
                let text = view
                    .as_str()
                    .ok_or_else(|| Error::Value("files.view must be a string".into()))?;
                config.files.view = FilesView::parse(text).ok_or_else(|| {
                    Error::Value(format!(
                        "files.view is {text:?}; it must be \"grid\" or \"list\""
                    ))
                })?;
            }
        }

        if let Some(font) = document.get("font").and_then(Item::as_table) {
            if let Some(size) = font.get("size") {
                let size = size
                    .as_float()
                    .or_else(|| size.as_integer().map(|value| value as f64))
                    .ok_or_else(|| Error::Value("font.size must be a number".into()))?;
                if !(6.0..=48.0).contains(&size) {
                    return Err(Error::Value(format!(
                        "font.size {size} is outside the sane range 6–48"
                    )));
                }
                config.font.size = size as f32;
            }
            if let Some(family) = font.get("family") {
                let text = family
                    .as_str()
                    .ok_or_else(|| Error::Value("font.family must be a string".into()))?;
                config.font.family = (!text.is_empty()).then(|| text.to_owned());
            }
            if let Some(path) = font.get("path") {
                let text = path
                    .as_str()
                    .ok_or_else(|| Error::Value("font.path must be a string".into()))?;
                config.font.path = Some(PathBuf::from(text));
            }
            if let Some(fallback) = font.get("fallback") {
                let list = fallback.as_array().ok_or_else(|| {
                    Error::Value("font.fallback must be an array of family names".into())
                })?;
                let mut names = Vec::with_capacity(list.len());
                for item in list {
                    let name = item.as_str().ok_or_else(|| {
                        Error::Value("font.fallback must be an array of family names".into())
                    })?;
                    // An empty string is how the preferences card would write
                    // "none"; a family called "" matches nothing, so drop it
                    // rather than carry a lookup that can never succeed.
                    if !name.trim().is_empty() {
                        names.push(name.trim().to_owned());
                    }
                }
                config.font.fallback = names;
            }
        }

        Ok(config)
    }
}

/// Set one value in the document at `path`, keeping every comment and every
/// other line exactly as it was.
///
/// New sections are appended rather than inserted, so an edit does not move the
/// user's existing text.
pub fn set_value(path: &Path, key: &str, raw: &str) -> Result<(), Error> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(Error::Io(err.to_string())),
    };
    let updated = set_in_text(&text, key, raw)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| Error::Io(err.to_string()))?;
    }
    std::fs::write(path, updated).map_err(|err| Error::Io(err.to_string()))
}

/// Remove a key, so whatever the program's own default is applies again.
///
/// This is what "reset" means for a key binding: the default lives in the
/// binary, so the way to restore it is to stop overriding it. Deleting the line
/// is also the only honest answer when the config never had one — writing the
/// default's own text back would freeze a future default at today's chord.
pub fn remove_value(path: &Path, key: &str) -> Result<(), Error> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(Error::Io(err.to_string())),
    };
    let updated = remove_in_text(&text, key)?;
    std::fs::write(path, updated).map_err(|err| Error::Io(err.to_string()))
}

/// The pure half of [`remove_value`].
pub fn remove_in_text(text: &str, key: &str) -> Result<String, Error> {
    let mut document: DocumentMut = text
        .parse()
        .map_err(|err| Error::Syntax(format!("{err}")))?;
    match key.split_once('.') {
        None => {
            document.remove(key);
        }
        Some((section, name)) => {
            if let Some(table) = document.get_mut(section).and_then(Item::as_table_mut) {
                table.remove(name);
            }
        }
    }
    Ok(document.to_string())
}

/// The pure half of [`set_value`], so it can be tested without a file.
pub fn set_in_text(text: &str, key: &str, raw: &str) -> Result<String, Error> {
    let mut document: DocumentMut = text
        .parse()
        .map_err(|err| Error::Syntax(format!("{err}")))?;

    let mut item: Item = match raw {
        "true" => value(true),
        "false" => value(false),
        other => match other.parse::<i64>() {
            Ok(number) => value(number),
            Err(_) => match other.parse::<f64>() {
                Ok(number) => value(number),
                Err(_) => value(other),
            },
        },
    };

    match key.split_once('.') {
        // A bare key is top level, like `theme = "kanagawa wave"`.
        None => match document.get_mut(key) {
            Some(existing) => {
                keep_decor(existing, &mut item);
                *existing = item;
            }
            None => {
                document.insert(key, item);
            }
        },
        Some((section, name)) => {
            if document.get(section).is_none() {
                let mut table = Table::new();
                table.insert(name, item);
                document.insert(section, Item::Table(table));
            } else {
                let table = document[section]
                    .as_table_mut()
                    .ok_or_else(|| Error::Value(format!("{section} is not a table")))?;
                match table.get_mut(name) {
                    // Replacing the value in place keeps the key and the
                    // comments written above it; only the value's text changes.
                    Some(existing) => {
                        keep_decor(existing, &mut item);
                        *existing = item;
                    }
                    None => {
                        table.insert(name, item);
                    }
                }
            }
        }
    }

    Ok(document.to_string())
}

/// Carry the old value's decor onto the new one.
///
/// `toml_edit` stores the whitespace and the trailing comment as *decor on the
/// value*, so replacing the value wholesale is what silently deletes
/// `baud = 9600 # an old board` down to `baud = 115200`.
fn keep_decor(existing: &Item, item: &mut Item) {
    let (Some(old), Some(new)) = (existing.as_value(), item.as_value_mut()) else {
        return;
    };
    *new.decor_mut() = old.decor().clone();
}

/// The shipped default file, written when the user asks for one.
///
/// `r##` rather than `r#`: the accent's own example is a quoted `#RRGGBB`, and a
/// `"#` inside an `r#"` string ends it.
pub const TEMPLATE: &str = r##"# dtr configuration.
#
# Every value here is optional; the defaults are shown. Edit this file by hand
# or with `dtr config set <section.key> <value>`, which keeps your comments.

[serial]
# Follow the device if it is unplugged and replugged under a new name.
auto_reconnect = true
#
# Milliseconds between two bytes typed on a serial line while delayed input is
# on. A board whose line reader has no flow control drops fast input; spacing the
# bytes is what makes typing land. The mode is off until `ctrl-alt-d` or the
# palette's `lag` turns it on, and the status bar then shows a red DELAY chip.
# 1 to 1000.
key_delay_ms = 1
#
# Baud rate and CR translation are not here: they describe the board on the
# other end of the cable, so dtr remembers them per port and the preferences card
# edits them while a port is open.

# Name of a theme file to apply; see `dtr --themes`.
# theme = "kanagawa wave"

[window]
# Who paints the window's frame. "client" puts the tab strip in the title bar
# and draws the window buttons there; "native" leaves both to the window
# manager, with the tabs underneath.
chrome = "client"

[ui]
# What the chrome's controls look like. "native" is the platform's own — the
# system palette, its scheme, the user's accent — and is the default; on a machine
# with no system palette it is the terminal's own chrome instead. "adwaita",
# "windows" and "egui" are looks dtr carries with it, so a person on Linux can
# have Windows' chrome and a person on Windows can have GNOME's. "dtr" is an old
# spelling of "egui" and still reads as it.
widgets = "native"
# The scheme those three draw in: "system" (the default) follows the desktop,
# "light" and "dark" are an instruction. "native" ignores it — its scheme is the
# desktop's.
scheme = "system"
# The accent those three draw with, as #RRGGBB. Leave it empty (or out) to keep
# the look's own: Adwaita's blue, Windows' accent, egui's link colour.
# accent = "#3584E4"

[files]
# How a file pane lays its listing out: "grid" is a wall of icon tiles, as in
# Nautilus and every other file manager, and "list" is one row per entry with a
# size column — the escape hatch for a pane too narrow for tiles. Ctrl+1 and
# Ctrl+2 switch between them and write the choice back here.
view = "grid"
# Whether dot-files are listed. Ctrl+H flips it and remembers the answer.
hidden = false
# What the listing is ordered by: "name", "modified", "size" or "type". A
# modified or size sort reads every entry's metadata, which is the one order
# that costs more than the directory read; the pane says so while it works.
sort = "name"
# Whether that key runs backwards, and whether folders stay above files (they
# do by default, and a reverse sort does not move them).
reverse = false
folders-first = true

[terminal]
# Bytes of scrollback a tab keeps before the oldest pages are dropped. There is
# no unlimited setting: a program that prints without end would grow the process
# until the kernel killed it. 100 MB is the ceiling, and the default; lower it
# if you would rather not hold that much.
scrollback_limit = 100000000

[font]
# Grid font size in points.
size = 14
# Key bindings. Action names are listed in `dtr --keys`; a chord is
# `ctrl-shift-t`, a named key is `tab`, `escape`, `pageup`, and several keys in
# a row are separated by spaces (`ctrl-k ctrl-s`). Shifted punctuation is the
# character it produces: ctrl+| is ctrl and shift and backslash, so write `|`.
# Setting an action to `false` hands its chord back to the shell.
#
# [keys]
# "new-tab" = "ctrl-shift-t"
# "palette" = "ctrl-shift-p"
# "split-horizontal" = "ctrl-|"

# Installed family to use; `dtr --fonts` lists the ones that work here.
# family = "JetBrains Mono"
# Path to a .ttf/.otf to use instead of the embedded font.
# path = "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf"
"##;

#[derive(Debug)]
pub enum Error {
    Io(String),
    Syntax(String),
    Value(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(message) => write!(formatter, "{message}"),
            Error::Syntax(message) => {
                write!(formatter, "the config file is not valid TOML: {message}")
            }
            Error::Value(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::{Config, SCROLLBACK_LIMIT, set_in_text};

    #[test]
    fn a_missing_file_is_the_defaults() {
        let config = Config::load(std::path::Path::new("/nonexistent/dtr/config.toml")).unwrap();
        assert!(config.serial.auto_reconnect);
        assert_eq!(config.font.size, 14.0);
    }

    /// One millisecond is the default because it is the U-Boot configuration the
    /// mode was written for: enough for a board that drops back-to-back bytes,
    /// and not felt on one that does not.
    #[test]
    fn a_key_delay_defaults_to_one_millisecond_and_is_configured_in_that_range() {
        assert_eq!(Config::default().serial.key_delay_ms, 1);
        assert_eq!(Config::parse("").unwrap().serial.key_delay_ms, 1);
        let config = Config::parse("[serial]\nkey_delay_ms = 25\n").unwrap();
        assert_eq!(config.serial.key_delay_ms, 25);
        // The shipped template is the file `--write-config` writes, so it has to
        // be a config the parser accepts.
        let shipped = Config::parse(super::TEMPLATE).expect("the template parses");
        assert_eq!(shipped.serial.key_delay_ms, 1);
    }

    /// The bounds are in the message, because "outside the range" without the
    /// range is a person guessing at the number.
    #[test]
    fn a_key_delay_outside_one_to_a_thousand_is_refused() {
        for text in [
            "[serial]\nkey_delay_ms = 0\n",
            "[serial]\nkey_delay_ms = 1001\n",
            "[serial]\nkey_delay_ms = -5\n",
        ] {
            let error = Config::parse(text).unwrap_err().to_string();
            assert!(error.contains("serial.key_delay_ms"), "{error}");
            assert!(error.contains("1 to 1000"), "{error}");
        }
        let error = Config::parse("[serial]\nkey_delay_ms = \"slow\"\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("whole number"), "{error}");
    }

    /// Scrollback is bounded by default, and there is no spelling that makes it
    /// unbounded: the VT core's unlimited default is what grew a dtr to 50 GB
    /// and got it OOM-killed.
    #[test]
    fn scrollback_is_always_bounded() {
        let default = Config::parse("").unwrap();
        assert_eq!(default.terminal.scrollback_limit, SCROLLBACK_LIMIT);

        let lower = Config::parse("[terminal]\nscrollback_limit = 4194304\n").unwrap();
        assert_eq!(lower.terminal.scrollback_limit, 4 * 1024 * 1024);

        // 0 is "no scrollback", which is a limit, not the absence of one.
        let none = Config::parse("[terminal]\nscrollback_limit = 0\n").unwrap();
        assert_eq!(none.terminal.scrollback_limit, 0);

        for too_much in [
            format!("[terminal]\nscrollback_limit = {}\n", SCROLLBACK_LIMIT + 1),
            "[terminal]\nscrollback_limit = -1\n".to_owned(),
        ] {
            let error = Config::parse(&too_much).expect_err("the ceiling should refuse this");
            assert!(
                format!("{error}").contains("unlimited"),
                "the message should say why: {error}"
            );
        }
    }

    /// The view is a word in the file and a value in the browser, and a typo is
    /// an error rather than a silent grid: a setting that quietly ignores what it
    /// was asked for is the failure mode `[ui] widgets` already refuses.
    #[test]
    fn the_file_view_is_grid_unless_the_file_says_list() {
        use super::FilesView;
        assert_eq!(
            Config::parse("").unwrap().files.view,
            FilesView::Grid,
            "the grid is the default, as it is in Nautilus"
        );
        assert_eq!(
            Config::parse("[files]\nview = \"list\"\n")
                .unwrap()
                .files
                .view,
            FilesView::List
        );
        // The same word in any case, because it is a name rather than a literal.
        assert_eq!(
            Config::parse("[files]\nview = \"LIST\"\n")
                .unwrap()
                .files
                .view,
            FilesView::List
        );
        let error = Config::parse("[files]\nview = \"tiles\"\n").expect_err("not a view");
        assert!(format!("{error}").contains("files.view"), "{error}");
        assert!(format!("{error}").contains("grid"), "{error}");

        // And the word written back is the one that reads.
        assert_eq!(FilesView::Grid.as_str(), "grid");
        assert_eq!(FilesView::parse("list"), Some(FilesView::List));

        // Hidden files are off unless the file says otherwise, and a string is
        // not a bool.
        assert!(!Config::parse("").unwrap().files.hidden);
        assert!(
            Config::parse("[files]\nhidden = true\n")
                .unwrap()
                .files
                .hidden
        );
        let error =
            Config::parse("[files]\nhidden = \"yes\"\n").expect_err("a string is not a bool");
        assert!(format!("{error}").contains("files.hidden"), "{error}");
    }

    #[test]
    fn notifications_reach_the_desktop_by_default() {
        // The desktop's channel is the one a person with notifications already
        // configured expects; dtr's toast is the fallback, not the preference.
        assert!(Config::parse("").unwrap().notifications.os);
        assert!(
            !Config::parse("[notifications]\nos = false\n")
                .unwrap()
                .notifications
                .os
        );
        let error =
            Config::parse("[notifications]\nos = \"maybe\"\n").expect_err("a string is not a bool");
        assert!(format!("{error}").contains("notifications.os"), "{error}");
    }

    /// Keeping a session on the far end is asked for, not assumed: it changes
    /// what the other machine does with a connection and it needs tmux there.
    #[test]
    fn tmux_is_a_choice_and_it_starts_off() {
        assert!(!Config::parse("").unwrap().tmux.enabled);
        assert!(
            Config::parse("[tmux]\nenabled = true\n")
                .unwrap()
                .tmux
                .enabled
        );
        let error =
            Config::parse("[tmux]\nenabled = \"maybe\"\n").expect_err("a string is not a bool");
        assert!(format!("{error}").contains("tmux.enabled"), "{error}");
    }

    #[test]
    fn a_clipboard_read_is_off_unless_the_file_says_so() {
        // A read is a capability, so the default cannot be "yes".
        assert!(!Config::parse("").unwrap().terminal.clipboard_read);
        assert!(
            Config::parse("[terminal]\nclipboard_read = true\n")
                .unwrap()
                .terminal
                .clipboard_read
        );
        let error = Config::parse("[terminal]\nclipboard_read = \"always\"\n")
            .expect_err("a string is not a bool");
        assert!(format!("{error}").contains("clipboard_read"), "{error}");
        // A program resizing the window is off for the same kind of reason.
        assert!(!Config::parse("").unwrap().terminal.window_resize);
        assert!(
            Config::parse("[terminal]\nwindow_resize = true\n")
                .unwrap()
                .terminal
                .window_resize
        );
    }

    #[test]
    fn values_are_read() {
        let config = Config::parse(
            r#"
[serial]
auto_reconnect = false

[font]
size = 18.5
path = "/tmp/mono.ttf"
"#,
        )
        .unwrap();
        assert!(!config.serial.auto_reconnect);
        assert_eq!(config.font.size, 18.5);
        assert_eq!(
            config.font.path.as_deref(),
            Some(std::path::Path::new("/tmp/mono.ttf"))
        );
    }

    /// `[font] fallback` is the only way to say "draw CJK with *this* face"
    /// without replacing the grid face, so it has to survive a round trip
    /// through the preferences card's writer as well as a hand-written file.
    #[test]
    fn the_fallback_list_is_read_in_order() {
        let config = Config::parse(
            r#"
[font]
fallback = ["Noto Sans CJK SC", "0xProto Nerd Font Mono"]
"#,
        )
        .unwrap();
        assert_eq!(
            config.font.fallback,
            ["Noto Sans CJK SC", "0xProto Nerd Font Mono"]
        );
        // An empty string is how "no family" would be written, and a family
        // called "" matches nothing: drop it rather than carry the lookup.
        let config = Config::parse("[font]\nfallback = [\"\", \" Hack \"]\n").unwrap();
        assert_eq!(config.font.fallback, ["Hack"]);
    }

    #[test]
    fn a_fallback_that_is_not_a_list_says_so() {
        let error = Config::parse("[font]\nfallback = \"Noto Sans CJK SC\"\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("font.fallback"), "{error}");
    }

    #[test]
    fn an_integer_size_is_accepted_as_a_float() {
        let config = Config::parse("[font]\nsize = 16\n").unwrap();
        assert_eq!(config.font.size, 16.0);
    }

    #[test]
    fn a_nonsense_size_is_refused_with_the_reason() {
        let error = Config::parse("[font]\nsize = 400\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("6–48"), "{error}");
    }

    #[test]
    fn the_window_frame_is_the_terminal_by_default() {
        assert_eq!(Config::default().chrome, super::Chrome::Client);
        assert_eq!(
            Config::parse("[window]\nchrome = \"native\"\n")
                .unwrap()
                .chrome,
            super::Chrome::Native
        );
    }

    #[test]
    fn a_wrong_chrome_names_the_key_and_the_values() {
        let error = Config::parse("[window]\nchrome = \"fancy\"\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("window.chrome"), "{error}");
        assert!(error.contains("client"), "{error}");
    }

    #[test]
    fn the_controls_are_native_unless_asked_otherwise() {
        assert_eq!(Config::default().ui.widgets, super::Widgets::Native);
        assert_eq!(Config::default().ui.scheme, super::Scheme::System);
        assert_eq!(Config::default().ui.accent, None);
        for widgets in super::Widgets::ALL {
            let text = format!("[ui]\nwidgets = \"{widgets}\"\n");
            assert_eq!(
                Config::parse(&text).unwrap().ui.widgets,
                widgets,
                "{widgets} did not survive the round trip"
            );
        }
        // The fourth look's old name is the same look, not a fifth one.
        assert_eq!(
            Config::parse("[ui]\nwidgets = \"dtr\"\n")
                .unwrap()
                .ui
                .widgets,
            super::Widgets::Egui
        );
    }

    /// A wrong value names the key and every value it could have been: a file the
    /// terminal refused is a file a person has to fix, and "invalid" is not a fix.
    #[test]
    fn a_wrong_widget_style_names_the_key_and_the_values() {
        let error = Config::parse("[ui]\nwidgets = \"fancy\"\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("ui.widgets"), "{error}");
        for name in ["native", "adwaita", "windows", "egui"] {
            assert!(error.contains(name), "{name} missing from {error}");
        }
    }

    /// The scheme is one of three answers, and an absent key is "the desktop's" —
    /// the one value a file expresses by leaving the line out, because that is
    /// what it already means.
    #[test]
    fn the_scheme_is_system_light_or_dark() {
        for (text, expected) in [
            ("system", super::Scheme::System),
            ("light", super::Scheme::Light),
            ("dark", super::Scheme::Dark),
            ("", super::Scheme::System),
        ] {
            let file = format!("[ui]\nscheme = \"{text}\"\n");
            assert_eq!(Config::parse(&file).unwrap().ui.scheme, expected);
        }
        // The menu offers `Scheme::ALL`, so a scheme the parser takes and the list
        // does not is a value nobody can pick — the same round trip the looks get.
        for scheme in super::Scheme::ALL {
            let text = format!("[ui]\nscheme = \"{scheme}\"\n");
            assert_eq!(Config::parse(&text).unwrap().ui.scheme, scheme);
        }
        let error = Config::parse("[ui]\nscheme = \"sepia\"\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("ui.scheme"), "{error}");
        assert!(error.contains("dark"), "{error}");
    }

    /// An accent is `#RRGGBB`, written either way, and an empty string is "the
    /// look's own" — the same spelling the theme key uses for "none".
    #[test]
    fn an_accent_is_a_hex_colour_or_nothing() {
        for text in ["#3584E4", "3584e4", " #3584E4 "] {
            let file = format!("[ui]\naccent = \"{text}\"\n");
            assert_eq!(
                Config::parse(&file).unwrap().ui.accent,
                Some([0x35, 0x84, 0xE4]),
                "{text:?} did not parse"
            );
        }
        assert_eq!(
            Config::parse("[ui]\naccent = \"\"\n").unwrap().ui.accent,
            None
        );
        for bad in ["#358", "blue", "#3584E", "#GGGGGG"] {
            let file = format!("[ui]\naccent = \"{bad}\"\n");
            let error = Config::parse(&file).unwrap_err().to_string();
            assert!(error.contains("ui.accent"), "{bad:?}: {error}");
        }
        assert_eq!(
            super::hex([0x35, 0x84, 0xE4]),
            "#3584E4",
            "the writer's spelling must be the reader's"
        );
    }

    #[test]
    fn a_moved_key_is_ignored_rather_than_refused() {
        // Nobody shipped with these keys, so a file that has them is either
        // hand-written from the old template or not ours at all: read the rest
        // and carry on.
        let config = Config::parse("[serial]\nbaud = 9600\ncr_to_crlf = false\n").unwrap();
        assert!(config.serial.auto_reconnect);
    }

    #[test]
    fn a_wrong_type_names_the_key() {
        let error = Config::parse("[serial]\nauto_reconnect = \"soon\"\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("serial.auto_reconnect"), "{error}");
    }

    #[test]
    fn key_bindings_are_parsed_and_bad_ones_name_the_action() {
        let config = Config::parse("[keys]\nnew-tab = \"ctrl-shift-t\"\n").unwrap();
        assert_eq!(
            config.keys.get("new-tab").map(|s| s.to_string()),
            Some("ctrl-shift-t".to_owned())
        );

        let config = Config::parse("[keys]\nnew-tab = false\n").unwrap();
        assert!(config.keys.get("new-tab").unwrap().is_disabled());

        let error = Config::parse("[keys]\nnew-tab = \"hyper-t\"\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("keys.new-tab"), "{error}");
        assert!(error.contains("not a modifier"), "{error}");
    }

    #[test]
    fn editing_keeps_comments_and_formatting() {
        let before = "# my settings\n[font]\n# size in points\nsize = 14\n\n[serial]\nauto_reconnect = true\n";
        let after = set_in_text(before, "font.size", "16").unwrap();
        assert!(after.contains("# my settings"));
        assert!(after.contains("# size in points"));
        assert!(after.contains("size = 16"));
        assert!(after.contains("auto_reconnect = true"));
    }

    #[test]
    fn editing_adds_a_missing_section_without_disturbing_the_rest() {
        let after = set_in_text("[font]\nsize = 14\n", "serial.auto_reconnect", "false").unwrap();
        assert!(after.contains("size = 14"));
        assert!(after.contains("[serial]"));
        assert!(after.contains("auto_reconnect = false"));
    }

    #[test]
    fn editing_an_empty_file_writes_a_section() {
        let after = set_in_text("", "serial.baud", "9600").unwrap();
        assert!(after.contains("[serial]"));
        assert!(after.contains("baud = 9600"));
    }

    #[test]
    fn a_top_level_key_is_set_in_place() {
        let after = set_in_text(
            "# my config\ntheme = \"old\"\n\n[font]\nsize = 14\n",
            "theme",
            "kanagawa wave",
        )
        .unwrap();
        assert!(after.contains("# my config"), "{after}");
        assert!(after.contains("theme = \"kanagawa wave\""), "{after}");
        assert!(after.contains("size = 14"), "{after}");
    }

    #[test]
    fn removing_a_key_leaves_the_rest_alone() {
        use super::remove_in_text;
        let before =
            "# mine\n[keys]\nnew-tab = \"ctrl-x\"\ncopy = \"ctrl-shift-c\"\n\n[font]\nsize = 14\n";
        let after = remove_in_text(before, "keys.new-tab").unwrap();
        assert!(!after.contains("ctrl-x"), "{after}");
        assert!(after.contains("copy = \"ctrl-shift-c\""), "{after}");
        assert!(after.contains("# mine"), "{after}");
        assert!(after.contains("size = 14"), "{after}");
    }

    #[test]
    fn removing_a_key_that_is_not_there_changes_nothing() {
        use super::remove_in_text;
        let before = "[font]\nsize = 14\n";
        assert_eq!(remove_in_text(before, "keys.new-tab").unwrap(), before);
        assert_eq!(
            remove_in_text(before, "no-such-section.key").unwrap(),
            before
        );
    }

    #[test]
    fn a_top_level_key_is_created_when_missing() {
        let after = set_in_text("[font]\nsize = 14\n", "theme", "kanagawa wave").unwrap();
        assert!(after.contains("theme = \"kanagawa wave\""), "{after}");
        assert!(after.contains("size = 14"), "{after}");
    }
}
