//! The config file: what dtr reads at startup, and what the UI edits in place.
//!
//! Editing is done with `toml_edit`, not by rewriting the file from a
//! `serde`-derived struct, because a config file is a document a human wrote.
//! Comments, ordering and formatting are the user's; a program that reformats
//! them on save is a program people stop editing.

use std::fmt;
use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, Table, value};

/// Everything configurable. Defaults are what a fresh install gets.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub serial: Serial,
    pub font: Font,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Serial {
    /// Baud rate for ports that were never opened before.
    pub baud: u32,
    /// Map a lone CR to CRLF on receive. Serial consoles send bare CR.
    pub cr_to_crlf: bool,
    /// Follow a device across a replug.
    pub auto_reconnect: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Font {
    /// Grid font size in points.
    pub size: f32,
    /// A font file to load instead of the embedded default. `.ttf`/`.otf`; the
    /// first face in the file is used.
    pub path: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            serial: Serial {
                baud: 115_200,
                cr_to_crlf: true,
                auto_reconnect: true,
            },
            font: Font {
                size: 14.0,
                path: None,
            },
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
            if let Some(baud) = serial.get("baud") {
                config.serial.baud = baud
                    .as_integer()
                    .and_then(|value| u32::try_from(value).ok())
                    .ok_or_else(|| Error::Value("serial.baud must be a positive integer".into()))?;
            }
            if let Some(map) = serial.get("cr_to_crlf") {
                config.serial.cr_to_crlf = map
                    .as_bool()
                    .ok_or_else(|| Error::Value("serial.cr_to_crlf must be true or false".into()))?;
            }
            if let Some(reconnect) = serial.get("auto_reconnect") {
                config.serial.auto_reconnect = reconnect.as_bool().ok_or_else(|| {
                    Error::Value("serial.auto_reconnect must be true or false".into())
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
    let (section, name) = key
        .split_once('.')
        .ok_or_else(|| Error::Value(format!("{key:?} is not section.key")))?;

    let item: Item = match raw {
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

    if document.get(section).is_none() {
        let mut table = Table::new();
        table.insert(name, item);
        document.insert(section, Item::Table(table));
    } else {
        let table = document[section]
            .as_table_mut()
            .ok_or_else(|| Error::Value(format!("{section} is not a table")))?;
        match table.get_mut(name) {
            // Replacing the value in place keeps the key and the comments
            // written above it; only the value's own text changes.
            Some(existing) => *existing = item,
            None => {
                table.insert(name, item);
            }
        }
    }

    Ok(document.to_string())
}

/// The shipped default file, written when the user asks for one.
pub const TEMPLATE: &str = r#"# dtr configuration.
#
# Every value here is optional; the defaults are shown. Edit this file by hand
# or with `dtr config set <section.key> <value>`, which keeps your comments.

[serial]
# Baud rate for ports that have never been opened before.
baud = 115200
# Serial consoles send a bare CR for a newline; map it to CRLF.
cr_to_crlf = true
# Follow the device if it is unplugged and replugged under a new name.
auto_reconnect = true

[font]
# Grid font size in points.
size = 14
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
        assert_eq!(config.serial.baud, 115_200);
        assert_eq!(config.font.size, 14.0);
    }

    #[test]
    fn values_are_read() {
        let config = Config::parse(
            r#"
[serial]
baud = 9600
cr_to_crlf = false

[font]
size = 18.5
path = "/tmp/mono.ttf"
"#,
        )
        .unwrap();
        assert_eq!(config.serial.baud, 9600);
        assert!(!config.serial.cr_to_crlf);
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
    fn a_wrong_type_names_the_key() {
        let error = Config::parse("[serial]\nbaud = \"fast\"\n").unwrap_err().to_string();
        assert!(error.contains("serial.baud"), "{error}");
    }

    #[test]
    fn editing_keeps_comments_and_formatting() {
        let before = "# my settings\n[font]\n# size in points\nsize = 14\n\n[serial]\nbaud = 115200\n";
        let after = set_in_text(before, "font.size", "16").unwrap();
        assert!(after.contains("# my settings"));
        assert!(after.contains("# size in points"));
        assert!(after.contains("size = 16"));
        assert!(after.contains("baud = 115200"));
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
    fn a_key_without_a_section_is_an_error() {
        assert!(set_in_text("", "baud", "9600").is_err());
    }
}
