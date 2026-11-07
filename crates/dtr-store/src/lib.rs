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

pub use serialport::{DataBits, FlowControl, Parity, StopBits};

const SETTINGS: TableDefinition<&str, &str> = TableDefinition::new("settings");
const META: TableDefinition<&str, &str> = TableDefinition::new("meta");
const LOGBOOK: TableDefinition<u64, &str> = TableDefinition::new("logbook");
/// Named profiles live under their own key, so a profile name never has to be
/// escaped into the settings key space.
const PROFILES: TableDefinition<&str, &str> = TableDefinition::new("serial_profiles");

/// Unit separator: it cannot appear in a device path, unlike `/` or `:`.
const SEP: char = '\u{1f}';
const LAST_PORT: &str = "last_port";

/// Everything one port is opened with, and the profile it came from.
///
/// The framing types are `serialport`'s own — they are also what `tokio-serial`
/// re-exports, so there is one vocabulary from the form to the open call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialSettings {
    pub baud: u32,
    pub data_bits: DataBits,
    pub parity: Parity,
    pub stop_bits: StopBits,
    pub flow_control: FlowControl,
    /// Line levels to apply as soon as the port is open.
    pub dtr: bool,
    pub rts: bool,
    pub cr_to_crlf: bool,
    pub auto_reconnect: bool,
    /// The named profile this port was last connected with, when one was
    /// selected. The settings above are still the source of truth for the
    /// connection: a profile can have been edited for one session.
    pub profile: Option<String>,
}

impl Default for SerialSettings {
    fn default() -> Self {
        Self {
            baud: 115_200,
            data_bits: DataBits::Eight,
            parity: Parity::None,
            stop_bits: StopBits::One,
            flow_control: FlowControl::None,
            dtr: true,
            rts: true,
            cr_to_crlf: true,
            auto_reconnect: true,
            profile: None,
        }
    }
}

impl SerialSettings {
    fn key(port: &str, field: &str) -> String {
        format!("serial{SEP}{port}{SEP}{field}")
    }

    /// `8N1`: the shorthand the connection sheet is a picture of.
    pub fn line_code(&self) -> String {
        let parity = match self.parity {
            Parity::None => 'N',
            Parity::Odd => 'O',
            Parity::Even => 'E',
        };
        format!(
            "{}{}{}",
            u8::from(self.data_bits),
            parity,
            u8::from(self.stop_bits)
        )
    }

    /// `115200 8N1`.
    pub fn summary(&self) -> String {
        format!("{} {}", self.baud, self.line_code())
    }
}

fn parity_name(parity: Parity) -> &'static str {
    match parity {
        Parity::None => "none",
        Parity::Odd => "odd",
        Parity::Even => "even",
    }
}

fn parse_parity(text: &str) -> Option<Parity> {
    match text {
        "none" => Some(Parity::None),
        "odd" => Some(Parity::Odd),
        "even" => Some(Parity::Even),
        _ => None,
    }
}

fn flow_name(flow: FlowControl) -> &'static str {
    match flow {
        FlowControl::None => "none",
        FlowControl::Software => "software",
        FlowControl::Hardware => "hardware",
    }
}

fn parse_flow(text: &str) -> Option<FlowControl> {
    match text {
        "none" => Some(FlowControl::None),
        "software" => Some(FlowControl::Software),
        "hardware" => Some(FlowControl::Hardware),
        _ => None,
    }
}

fn encode_settings(settings: &SerialSettings) -> String {
    let mut out = String::new();
    let mut field = |name: &str, value: String| {
        out.push_str(name);
        out.push('=');
        out.push_str(&value);
        out.push('\n');
    };
    field("baud", settings.baud.to_string());
    field("data_bits", u8::from(settings.data_bits).to_string());
    field("parity", parity_name(settings.parity).to_owned());
    field("stop_bits", u8::from(settings.stop_bits).to_string());
    field("flow_control", flow_name(settings.flow_control).to_owned());
    field("dtr", settings.dtr.to_string());
    field("rts", settings.rts.to_string());
    field("cr_to_crlf", settings.cr_to_crlf.to_string());
    field("auto_reconnect", settings.auto_reconnect.to_string());
    out
}

fn decode_settings(text: &str) -> SerialSettings {
    let mut settings = SerialSettings::default();
    for line in text.lines() {
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        match name {
            "baud" => settings.baud = value.parse().unwrap_or(settings.baud),
            "data_bits" => {
                settings.data_bits = value
                    .parse::<u8>()
                    .ok()
                    .and_then(|bits| DataBits::try_from(bits).ok())
                    .unwrap_or(settings.data_bits);
            }
            "parity" => settings.parity = parse_parity(value).unwrap_or(settings.parity),
            "stop_bits" => {
                settings.stop_bits = value
                    .parse::<u8>()
                    .ok()
                    .and_then(|bits| StopBits::try_from(bits).ok())
                    .unwrap_or(settings.stop_bits);
            }
            "flow_control" => {
                settings.flow_control = parse_flow(value).unwrap_or(settings.flow_control);
            }
            "dtr" => settings.dtr = value != "false",
            "rts" => settings.rts = value != "false",
            "cr_to_crlf" => settings.cr_to_crlf = value != "false",
            "auto_reconnect" => settings.auto_reconnect = value != "false",
            _ => {}
        }
    }
    settings
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
        let db = Database::create(path).with_context(|| format!("opening {}", path.display()))?;

        // Materialise every table so a read on a fresh file does not error.
        let write = db.begin_write()?;
        {
            write.open_table(SETTINGS)?;
            write.open_table(META)?;
            write.open_table(LOGBOOK)?;
            write.open_table(PROFILES)?;
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
    ///
    /// A record written before framing was remembered has no `data_bits` (and
    /// friends); the missing keys are the defaults, which is why adding a field
    /// needs no migration.
    pub fn serial(&self, port: &str) -> Result<Option<SerialSettings>> {
        let read = self.db.begin_read()?;
        let table = read.open_table(SETTINGS)?;
        let get = |field: &str| -> Result<Option<String>> {
            Ok(table
                .get(SerialSettings::key(port, field).as_str())?
                .map(|value| value.value().to_owned()))
        };

        let Some(baud) = get("baud")?.and_then(|value| value.parse().ok()) else {
            return Ok(None);
        };

        let data_bits = get("data_bits")?
            .and_then(|value| value.parse::<u8>().ok())
            .and_then(|value| DataBits::try_from(value).ok())
            .unwrap_or(DataBits::Eight);
        let parity = get("parity")?
            .and_then(|value| parse_parity(&value))
            .unwrap_or(Parity::None);
        let stop_bits = get("stop_bits")?
            .and_then(|value| value.parse::<u8>().ok())
            .and_then(|value| StopBits::try_from(value).ok())
            .unwrap_or(StopBits::One);
        let flow_control = get("flow_control")?
            .and_then(|value| parse_flow(&value))
            .unwrap_or(FlowControl::None);
        let profile = get("profile")?.filter(|name| !name.is_empty());

        Ok(Some(SerialSettings {
            baud,
            data_bits,
            parity,
            stop_bits,
            flow_control,
            dtr: get("dtr")?.map(|value| value != "false").unwrap_or(true),
            rts: get("rts")?.map(|value| value != "false").unwrap_or(true),
            cr_to_crlf: get("cr_to_crlf")?
                .map(|value| value != "false")
                .unwrap_or(true),
            auto_reconnect: get("auto_reconnect")?
                .map(|value| value != "false")
                .unwrap_or(true),
            profile,
        }))
    }

    pub fn remember_serial(&self, port: &str, settings: &SerialSettings) -> Result<()> {
        let write = self.db.begin_write()?;
        {
            let mut table = write.open_table(SETTINGS)?;
            let mut put = |field: &str, value: String| -> Result<()> {
                table.insert(SerialSettings::key(port, field).as_str(), value.as_str())?;
                Ok(())
            };
            put("baud", settings.baud.to_string())?;
            put("data_bits", u8::from(settings.data_bits).to_string())?;
            put("parity", parity_name(settings.parity).to_owned())?;
            put("stop_bits", u8::from(settings.stop_bits).to_string())?;
            put("flow_control", flow_name(settings.flow_control).to_owned())?;
            put("dtr", settings.dtr.to_string())?;
            put("rts", settings.rts.to_string())?;
            put("cr_to_crlf", settings.cr_to_crlf.to_string())?;
            put("auto_reconnect", settings.auto_reconnect.to_string())?;
            put("profile", settings.profile.clone().unwrap_or_default())?;
        }
        write.commit()?;
        Ok(())
    }

    /// Every named profile, in name order (redb iterates keys in order).
    pub fn profiles(&self) -> Result<Vec<(String, SerialSettings)>> {
        let read = self.db.begin_read()?;
        let table = read.open_table(PROFILES)?;
        let mut out = Vec::new();
        for entry in table.iter()? {
            let (name, value) = entry?;
            let name = name.value().to_owned();
            let mut settings = decode_settings(value.value());
            settings.profile = Some(name.clone());
            out.push((name, settings));
        }
        Ok(out)
    }

    /// Write a profile under `name`, replacing one with the same name. A blank
    /// name is refused by the form; here it is a no-op rather than an error so
    /// a stray click cannot fail a connection.
    pub fn save_profile(&self, name: &str, settings: &SerialSettings) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Ok(());
        }
        let mut stored = settings.clone();
        stored.profile = Some(name.to_owned());
        let encoded = encode_settings(&stored);
        let write = self.db.begin_write()?;
        {
            let mut table = write.open_table(PROFILES)?;
            table.insert(name, encoded.as_str())?;
        }
        write.commit()?;
        Ok(())
    }

    pub fn last_port(&self) -> Result<Option<String>> {
        let read = self.db.begin_read()?;
        let table = read.open_table(META)?;
        Ok(table.get(LAST_PORT)?.map(|value| value.value().to_owned()))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_without_framing_reads_as_8n1() {
        let settings = decode_settings("baud=4800\ncr_to_crlf=false\n");
        assert_eq!(settings.baud, 4800);
        assert!(!settings.cr_to_crlf);
        assert_eq!(settings.data_bits, DataBits::Eight);
        assert_eq!(settings.parity, Parity::None);
        assert_eq!(settings.stop_bits, StopBits::One);
        assert_eq!(settings.flow_control, FlowControl::None);
        assert!(settings.dtr);
        assert!(settings.rts);
        assert!(settings.auto_reconnect);
        assert_eq!(settings.profile, None);
    }

    #[test]
    fn the_line_code_is_the_shorthand() {
        let settings = SerialSettings {
            data_bits: DataBits::Seven,
            parity: Parity::Even,
            stop_bits: StopBits::Two,
            ..SerialSettings::default()
        };
        assert_eq!(settings.line_code(), "7E2");
        assert_eq!(settings.summary(), "115200 7E2");
    }
}
