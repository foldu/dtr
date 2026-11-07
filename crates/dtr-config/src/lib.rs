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
}

/// Facts about the terminal itself, as opposed to what it is showing.
#[derive(Clone, Debug, PartialEq)]
pub struct Terminal {
    /// Bytes of scrollback a tab keeps before the oldest pages are dropped.
    ///
    /// Clamped to `[0, SCROLLBACK_LIMIT]` when the file is read: a value above
    /// the ceiling is a mistake, not an instruction, and the error says so.
    pub scrollback_limit: usize,
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
}

#[derive(Clone, Debug, PartialEq)]
pub struct Font {
    /// Grid font size in points.
    pub size: f32,
    /// An installed family to use, e.g. `"JetBrains Mono"`. `None` picks the
    /// first usable family the probe likes.
    pub family: Option<String>,
    /// A font file to load instead of any installed family. `.ttf`/`.otf`; the
    /// first face in the file is used. Hand-edited only: the settings menu
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
/// `Native` is the platform's own look — the system palette and Windows 11's
/// metrics — and `Dtr` is the terminal's: monospace, tighter, accent-lit. On a
/// machine with no system palette `native` resolves to the dtr skin, so this is a
/// preference, not a guarantee.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Widgets {
    Dtr,
    Native,
}

impl Default for Widgets {
    fn default() -> Self {
        Self::Native
    }
}

impl fmt::Display for Widgets {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Dtr => "dtr",
            Self::Native => "native",
        })
    }
}

/// The controls' look. `[ui]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Ui {
    pub widgets: Widgets,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            widgets: Widgets::default(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            keys: std::collections::BTreeMap::new(),
            serial: Serial {
                auto_reconnect: true,
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
        }
    }
}

impl Default for Terminal {
    fn default() -> Self {
        Self {
            scrollback_limit: SCROLLBACK_LIMIT,
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
                    "dtr" => Widgets::Dtr,
                    "native" => Widgets::Native,
                    other => {
                        return Err(Error::Value(format!(
                            "ui.widgets is {other:?}; it must be \"dtr\" or \"native\""
                        )));
                    }
                };
            }
        }

        if let Some(theme) = document.get("theme") {
            let name = theme
                .as_str()
                .ok_or_else(|| Error::Value("theme must be a string".into()))?;
            // The settings menu writes an empty string to mean "no theme";
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
                    // An empty string is how the settings menu would write
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
pub const TEMPLATE: &str = r#"# dtr configuration.
#
# Every value here is optional; the defaults are shown. Edit this file by hand
# or with `dtr config set <section.key> <value>`, which keeps your comments.

[serial]
# Follow the device if it is unplugged and replugged under a new name.
auto_reconnect = true
#
# Baud rate and CR translation are not here: they describe the board on the
# other end of the cable, so dtr remembers them per port and the settings menu
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
# system palette, Windows 11's metrics — and is the default; "dtr" is the
# terminal's: monospace labels, tighter rows, the theme's accent. On a machine
# with no system palette (Linux) "native" is the dtr style anyway.
widgets = "native"

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
"#;

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
    /// through the settings menu's writer as well as a hand-written file.
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
        assert_eq!(
            Config::parse("[ui]\nwidgets = \"dtr\"\n")
                .unwrap()
                .ui
                .widgets,
            super::Widgets::Dtr
        );
    }

    #[test]
    fn a_wrong_widget_style_names_the_key_and_the_values() {
        let error = Config::parse("[ui]\nwidgets = \"fancy\"\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("ui.widgets"), "{error}");
        assert!(error.contains("native"), "{error}");
        assert!(error.contains("dtr"), "{error}");
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
