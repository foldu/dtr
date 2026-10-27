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
use redb::{Database, ReadableDatabase, TableDefinition};

const SETTINGS: TableDefinition<&str, &str> = TableDefinition::new("settings");
const META: TableDefinition<&str, &str> = TableDefinition::new("meta");

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
