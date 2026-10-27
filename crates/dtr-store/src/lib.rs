//! On-disk state for dtr.
//!
//! Two kinds of state live here, and they are different in nature. `settings`
//! remembers what a port needs, so a device does not have to be re-specified on
//! every run; `logbook` (M1) will record what happened. Both go in one redb
//! file because a session is a row plus the settings that produced it.
//!
//! The settings table is a plain key/value map with namespaced keys, so adding
//! a field never needs a migration — the absence of a key is the default.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};

const SETTINGS: TableDefinition<&str, &str> = TableDefinition::new("settings");
const META: TableDefinition<&str, &str> = TableDefinition::new("meta");
const LOGBOOK: TableDefinition<u64, &str> = TableDefinition::new("logbook");

/// Unit separator: it cannot appear in a device path, unlike `/` or `:`.
const SEP: char = '\u{1f}';
const LAST_PORT: &str = "last_port";

/// What we remember about one port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SerialSettings {
    pub baud: u32,
    pub cr_to_crlf: bool,
}

impl Default for SerialSettings {
    fn default() -> Self {
        Self {
            baud: 115_200,
            cr_to_crlf: true,
        }
    }
}

impl SerialSettings {
    fn key(port: &str, field: &str) -> String {
        format!("serial{SEP}{port}{SEP}{field}")
    }
}

/// The database. Opening it always creates the file and its parent directory.
pub struct Store {
    db: Database,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let db = Database::create(path)
            .with_context(|| format!("opening {}", path.display()))?;

        // Materialise both tables so a read on a fresh file does not error.
        let write = db.begin_write()?;
        {
            write.open_table(SETTINGS)?;
            write.open_table(META)?;
            write.open_table(LOGBOOK)?;
        }
        write.commit()?;

        Ok(Self { db })
    }

    /// Where the database lives when the user says nothing.
    pub fn default_path() -> Option<PathBuf> {
        dirs::state_dir()
            .or_else(dirs::data_local_dir)
            .map(|dir| dir.join("dtr").join("dtr.redb"))
    }

    /// The remembered settings for a port, if it has ever been used.
    pub fn serial(&self, port: &str) -> Result<Option<SerialSettings>> {
        let read = self.db.begin_read()?;
        let table = read.open_table(SETTINGS)?;

        let baud = match table.get(SerialSettings::key(port, "baud").as_str())? {
            Some(value) => value.value().parse().ok(),
            None => None,
        };
        let Some(baud) = baud else {
            return Ok(None);
        };

        let cr_to_crlf = table
            .get(SerialSettings::key(port, "cr_to_crlf").as_str())?
            .map(|value| value.value() != "false")
            .unwrap_or(true);

        Ok(Some(SerialSettings { baud, cr_to_crlf }))
    }

    pub fn remember_serial(&self, port: &str, settings: &SerialSettings) -> Result<()> {
        let write = self.db.begin_write()?;
        {
            let mut table = write.open_table(SETTINGS)?;
            table.insert(
                SerialSettings::key(port, "baud").as_str(),
                settings.baud.to_string().as_str(),
            )?;
            table.insert(
                SerialSettings::key(port, "cr_to_crlf").as_str(),
                if settings.cr_to_crlf { "true" } else { "false" },
            )?;
        }
        write.commit()?;
        Ok(())
    }

    pub fn last_port(&self) -> Result<Option<String>> {
        let read = self.db.begin_read()?;
        let table = read.open_table(META)?;
        Ok(table
            .get(LAST_PORT)?
            .map(|value| value.value().to_owned()))
    }

    pub fn set_last_port(&self, port: &str) -> Result<()> {
        let write = self.db.begin_write()?;
        {
            let mut table = write.open_table(META)?;
            table.insert(LAST_PORT, port)?;
        }
        write.commit()?;
        Ok(())
    }
}

/// One session, as the logbook remembers it.
///
/// Timestamps are milliseconds since the Unix epoch, UTC. Storing an instant
/// rather than a formatted date is deliberate: formatting is a display concern
/// and the timezone is not the database's business.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionRecord {
    pub id: u64,
    /// What the window was attached to: a port, a shell, or a demo label.
    pub label: String,
    pub port: Option<String>,
    pub baud: Option<u32>,
    pub imap: Option<String>,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    /// How it ended: `closed`, `screenshot`, or `error: …`.
    pub exit: Option<String>,
    pub note: Option<String>,
}

impl SessionRecord {
    pub fn duration_ms(&self) -> Option<u64> {
        self.ended_at
            .map(|ended| ended.saturating_sub(self.started_at))
    }
}

/// Values are escaped so a note may contain anything, newlines included.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn encode(record: &SessionRecord) -> String {
    let mut out = String::new();
    let mut field = |key: &str, value: &str| {
        out.push_str(key);
        out.push('=');
        out.push_str(&escape(value));
        out.push('\n');
    };

    field("label", &record.label);
    if let Some(port) = &record.port {
        field("port", port);
    }
    if let Some(baud) = record.baud {
        field("baud", &baud.to_string());
    }
    if let Some(imap) = &record.imap {
        field("imap", imap);
    }
    field("started", &record.started_at.to_string());
    if let Some(ended) = record.ended_at {
        field("ended", &ended.to_string());
    }
    if let Some(exit) = &record.exit {
        field("exit", exit);
    }
    if let Some(note) = &record.note {
        field("note", note);
    }
    out
}

fn decode(id: u64, text: &str) -> SessionRecord {
    let mut record = SessionRecord {
        id,
        ..SessionRecord::default()
    };
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = unescape(value);
        match key {
            "label" => record.label = value,
            "port" => record.port = Some(value),
            "baud" => record.baud = value.parse().ok(),
            "imap" => record.imap = Some(value),
            "started" => record.started_at = value.parse().unwrap_or_default(),
            "ended" => record.ended_at = value.parse().ok(),
            "exit" => record.exit = Some(value),
            "note" => record.note = Some(value),
            _ => {}
        }
    }
    record
}

/// Milliseconds since the Unix epoch. Zero if the clock is before it, which is
/// a broken clock rather than a reason to fail.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

impl Store {
    /// Record the start of a session and return its id.
    ///
    /// The id is the start instant, bumped if that millisecond is taken: it
    /// sorts naturally and needs no counter to keep in sync.
    pub fn begin_session(&self, record: &SessionRecord) -> Result<u64> {
        let mut id = record.started_at.max(now_ms());
        let write = self.db.begin_write()?;
        {
            let mut table = write.open_table(LOGBOOK)?;
            while table.get(id)?.is_some() {
                id += 1;
            }
            let stored = SessionRecord {
                id,
                ..record.clone()
            };
            table.insert(id, encode(&stored).as_str())?;
        }
        write.commit()?;
        Ok(id)
    }

    pub fn end_session(&self, id: u64, ended_at: u64, exit: &str) -> Result<()> {
        let write = self.db.begin_write()?;
        {
            let mut table = write.open_table(LOGBOOK)?;
            let existing = table.get(id)?.map(|value| value.value().to_owned());
            let Some(existing) = existing else {
                return Ok(());
            };
            let mut record = decode(id, &existing);
            record.ended_at = Some(ended_at);
            record.exit = Some(exit.to_owned());
            table.insert(id, encode(&record).as_str())?;
        }
        write.commit()?;
        Ok(())
    }

    /// Every session, oldest first.
    pub fn sessions(&self) -> Result<Vec<SessionRecord>> {
        let read = self.db.begin_read()?;
        let table = read.open_table(LOGBOOK)?;
        let mut out = Vec::new();
        for entry in table.iter()? {
            let (key, value) = entry?;
            out.push(decode(key.value(), value.value()));
        }
        Ok(out)
    }
}
