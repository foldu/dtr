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

/// Everything configurable. Defaults are what a fresh install gets.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// Key bindings from `[keys]`, action name to chord text. Kept as text here
    /// and matched by the app, so the config crate needs no window library.
    pub keys: std::collections::BTreeMap<String, keys::Sequence>,
    pub serial: Serial,
    pub font: Font,
    /// Name of a theme file, e.g. `"kanagawa wave"`. `None` keeps the
    /// terminal's built-in colours.
    pub theme: Option<String>,
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
            },
            theme: None,
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
        let document: DocumentMut = text.parse().map_err(|err| Error::Syntax(format!("{err}")))?;
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
                    config.keys.insert(action.to_owned(), keys::Sequence::disabled());
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

        if let Some(theme) = document.get("theme") {
            let name = theme
                .as_str()
                .ok_or_else(|| Error::Value("theme must be a string".into()))?;
            // The settings menu writes an empty string to mean "no theme";
            // reading it back must not produce a theme called "".
            config.theme = (!name.is_empty()).then(|| name.to_owned());
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

/// The pure half of [`set_value`], so it can be tested without a file.
pub fn set_in_text(text: &str, key: &str, raw: &str) -> Result<String, Error> {
    let mut document: DocumentMut = text.parse().map_err(|err| Error::Syntax(format!("{err}")))?;

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
            Error::Syntax(message) => write!(formatter, "the config file is not valid TOML: {message}"),
            Error::Value(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::{Config, set_in_text};

    #[test]
    fn a_missing_file_is_the_defaults() {
        let config = Config::load(std::path::Path::new("/nonexistent/dtr/config.toml")).unwrap();
        assert!(config.serial.auto_reconnect);
        assert_eq!(config.font.size, 14.0);
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
        assert_eq!(config.font.path.as_deref(), Some(std::path::Path::new("/tmp/mono.ttf")));
    }

    #[test]
    fn an_integer_size_is_accepted_as_a_float() {
        let config = Config::parse("[font]\nsize = 16\n").unwrap();
        assert_eq!(config.font.size, 16.0);
    }

    #[test]
    fn a_nonsense_size_is_refused_with_the_reason() {
        let error = Config::parse("[font]\nsize = 400\n").unwrap_err().to_string();
        assert!(error.contains("6–48"), "{error}");
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
        let error = Config::parse("[serial]\nauto_reconnect = \"soon\"\n").unwrap_err().to_string();
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
        let after = set_in_text("# my config\ntheme = \"old\"\n\n[font]\nsize = 14\n", "theme", "kanagawa wave").unwrap();
        assert!(after.contains("# my config"), "{after}");
        assert!(after.contains("theme = \"kanagawa wave\""), "{after}");
        assert!(after.contains("size = 14"), "{after}");
    }

    #[test]
    fn a_top_level_key_is_created_when_missing() {
        let after = set_in_text("[font]\nsize = 14\n", "theme", "kanagawa wave").unwrap();
        assert!(after.contains("theme = \"kanagawa wave\""), "{after}");
        assert!(after.contains("size = 14"), "{after}");
    }
}
