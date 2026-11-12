//! Watching the places a terminal's destinations come from.
//!
//! Nothing here uses a kernel watcher. Two things could, and one used to:
//!
//! * **Serial devices** are polled through `serialport::available_ports` every
//!   couple of seconds (`App::request_scan`). udev would be faster on Linux and
//!   would not exist on Windows; `serialport` already knows how to enumerate COM
//!   ports there, so polling keeps one behaviour instead of two.
//! * **The config file** is stat-ed on a 250 ms deadline (`App::about_to_wait`):
//!   modification time and size, or "not there". `notify` — inotify on Linux,
//!   `ReadDirectoryChangesW` on Windows — was doing that with two watches, an
//!   access-event filter and a thread of its own, for a file an editor writes
//!   once. `docs/compio.md` §5 is the measurement; the price is that a saved
//!   file is re-read at most one poll later, never that a change is missed.
//!
//! What is left is the part that was never the watcher's: an editor writes in
//! bursts, so `Settle` turns one save into one reload.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

/// The config file as it was last seen on disk.
///
/// Compared with the *previous look* rather than with an event, so a change
/// cannot be lost by arriving between two polls: the poll sees the final state.
/// The one thing this cannot see is a file that changes and changes back,
/// modification time and length included, between two looks — which no writer
/// does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stamp {
    /// Not there: the file has not been created yet, or has been removed, or
    /// its directory does not exist.
    Absent,
    Present {
        len: u64,
        /// Some filesystems do not record one; the length then stands alone,
        /// which is still right for a config an editor rewrites.
        modified: Option<SystemTime>,
    },
}

impl Stamp {
    fn of(path: &std::path::Path) -> Self {
        match std::fs::metadata(path) {
            Ok(meta) => Self::Present {
                len: meta.len(),
                modified: meta.modified().ok(),
            },
            Err(_) => Self::Absent,
        }
    }
}

/// The config file, stat-ed by the app on a deadline instead of watched.
///
/// A file an editor saves is written by a rename-over as often as in place, and
/// both are a change to the same path: the stamp is taken from the path, not
/// from an open descriptor, so neither needs a special case.
#[derive(Debug)]
pub struct ConfigWatch {
    path: PathBuf,
    /// The last answer. `None` until the first look, so starting up does not
    /// look like a change to the file the app was already told to read.
    last: Option<Stamp>,
}

impl ConfigWatch {
    /// How often the app looks. Short enough that a save feels immediate, long
    /// enough that an idle window does four `stat`s a second and nothing else.
    pub const POLL: Duration = Duration::from_millis(250);

    /// Watch `path`, which need not exist yet: a config file gets to appear.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            last: None,
        }
    }

    /// Whether the file differs from the last look.
    ///
    /// The first call only seeds the stamp and answers `false`; every answer is
    /// relative to the look before it, so a caller that polls on a deadline does
    /// not need to know when the file was created.
    pub fn changed(&mut self) -> bool {
        let now = Stamp::of(&self.path);
        let changed = self.last.is_some_and(|last| last != now);
        self.last = Some(now);
        changed
    }
}

/// A burst of file writes collapsed into one reload.
///
/// One save is several events — a truncate, a write, sometimes a rename — and
/// re-reading the file per event would rebuild the window's look and its keys
/// three times for one edit. So a change only starts a clock, and the clock
/// restarts while the file keeps changing.
#[derive(Debug, Default, Clone, Copy)]
pub struct Settle {
    at: Option<std::time::Instant>,
}

impl Settle {
    /// How long the file has to be quiet before it is read.
    pub const DELAY: Duration = Duration::from_millis(150);

    /// Note a change at `now`, pushing the deadline out.
    pub fn note(&mut self, now: std::time::Instant) {
        self.at = Some(now + Self::DELAY);
    }

    /// When the quiet window ends, if one is open.
    pub fn due(&self) -> Option<std::time::Instant> {
        self.at
    }

    /// Whether the window has closed: true once, and only after `DELAY`.
    pub fn take(&mut self, now: std::time::Instant) -> bool {
        match self.at {
            Some(at) if now >= at => {
                self.at = None;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn scratch() -> tempfile::TempDir {
        tempfile::tempdir().expect("a scratch directory")
    }

    #[test]
    fn a_config_that_does_not_exist_yet_still_gets_to_appear() {
        let dir = scratch();
        let path = dir.path().join("config");
        let mut watch = ConfigWatch::new(&path);
        assert!(
            !watch.changed(),
            "the first look seeds the stamp: a file that was always missing is not a change"
        );
        std::fs::write(&path, "theme = \"Kanagawa Wave\"\n").unwrap();
        assert!(watch.changed(), "a config that appeared is a change");
        assert!(!watch.changed(), "and it is one change, not one per look");
    }

    #[test]
    fn a_write_is_a_change_and_the_look_after_it_is_not() {
        let dir = scratch();
        let path = dir.path().join("config");
        std::fs::write(&path, "theme = \"a\"\n").unwrap();
        let mut watch = ConfigWatch::new(&path);
        assert!(!watch.changed());
        // A different length, so the answer does not depend on the filesystem
        // having a fine-grained modification time.
        std::fs::write(&path, "theme = \"another one\"\n").unwrap();
        assert!(watch.changed());
        assert!(!watch.changed());
    }

    #[test]
    fn an_editor_that_renames_a_temporary_over_the_file_is_seen() {
        let dir = scratch();
        let path = dir.path().join("config");
        std::fs::write(&path, "theme = \"a\"\n").unwrap();
        let mut watch = ConfigWatch::new(&path);
        assert!(!watch.changed());
        let temporary = dir.path().join("config.tmp");
        std::fs::write(&temporary, "theme = \"written elsewhere\"\n").unwrap();
        std::fs::rename(&temporary, &path).unwrap();
        assert!(
            watch.changed(),
            "the stamp is the path's, so the new inode is a change"
        );
    }

    /// One save is several events, and a reload is not free: the window's look,
    /// the keys and the terminals' theme all move. So the clock restarts while
    /// the file is still being written, and fires once, 150 ms after the last
    /// change.
    #[test]
    fn a_burst_of_edits_becomes_one_reload() {
        let start = Instant::now();
        let mut settle = Settle::default();
        assert_eq!(settle.due(), None, "an untouched file has no clock");
        assert!(!settle.take(start));

        settle.note(start);
        settle.note(start + Duration::from_millis(100));
        assert_eq!(
            settle.due(),
            Some(start + Duration::from_millis(100) + Settle::DELAY),
            "the last change sets the deadline, not the first"
        );
        assert!(
            !settle.take(start + Duration::from_millis(200)),
            "half the window is not a quiet file"
        );
        assert!(settle.take(start + Duration::from_millis(250)));
        assert!(
            !settle.take(start + Duration::from_millis(260)),
            "one reload per burst, not one per frame"
        );
    }
}
