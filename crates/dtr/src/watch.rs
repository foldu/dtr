//! Watching the places a terminal's destinations come from.
//!
//! Two sources, two mechanisms, and the split is on purpose:
//!
//! * **Files** (`~/.ssh/config`) are watched with `notify`, which is inotify on
//!   Linux and `ReadDirectoryChangesW` on Windows — the same code either way.
//! * **Serial devices** are *polled* through `serialport::available_ports`, not
//!   watched through `/dev`. udev would be faster on Linux and would not exist
//!   on Windows; `serialport` already knows how to enumerate COM ports there,
//!   so polling keeps one behaviour instead of two.
//!
//! The watcher itself is dumb: it reports "something changed", and the app
//! decides what to re-read and how often. inotify during a USB enumeration
//! delivers a burst per device, so a burst is coalesced into one refresh.

use std::path::PathBuf;

/// Where destinations come from.
pub fn paths() -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from("/dev")];
    if let Some(home) = dirs::home_dir() {
        let ssh = home.join(".ssh");
        // The file specifically when it exists, its directory otherwise — a
        // config that does not exist yet still gets to appear.
        if ssh.join("config").exists() {
            paths.push(ssh.join("config"));
        } else {
            paths.push(ssh);
        }
    }
    paths
}

/// Holds a live `notify` watcher. Dropping it stops the watching.
pub struct Watcher {
    /// Held, not used: dropping the watcher is what stops the watching.
    #[allow(dead_code)]
    inner: notify::RecommendedWatcher,
}

impl Watcher {
    /// Watch every destination source, calling `notify` on each change.
    ///
    /// `notify` is called from the watcher's own thread, so it must do
    /// something thread-safe — the app hands it an `EventLoopProxy`.
    pub fn spawn(mut notify: impl FnMut() + Send + 'static) -> Option<Self> {
        use notify::Watcher as _;

        let mut inner = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            match event {
                Ok(event) => {
                    // Access events fire on read; only real content changes and
                    // name changes matter here.
                    if event.kind.is_access() {
                        return;
                    }
                    notify();
                }
                Err(err) => tracing::debug!(%err, "watch error"),
            }
        })
        .map_err(|err| tracing::warn!(%err, "could not start the watcher"))
        .ok()?;

        for path in paths() {
            if let Err(err) = inner.watch(&path, notify::RecursiveMode::NonRecursive) {
                tracing::debug!(path = %path.display(), %err, "not watched");
            }
        }
        Some(Self { inner })
    }
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Watcher")
    }
}

/// The config file, watched so an edit anywhere else is picked up here.
///
/// Its own watcher rather than a third entry in `paths()`, because a config
/// edit is not a destination change: reloading one rebuilds the window's look
/// and its keys, and coupling that to the serial poll would make every udev
/// burst re-read the file.
pub struct ConfigWatcher {
    #[allow(dead_code)]
    inner: notify::RecommendedWatcher,
}

impl ConfigWatcher {
    /// Watch the *directory* holding `path`, so a file that does not exist yet,
    /// or one an editor writes by renaming a temporary over it, is still seen.
    /// Only events that name `path` are reported.
    pub fn spawn(
        path: &std::path::Path,
        mut notify: impl FnMut() + Send + 'static,
    ) -> Option<Self> {
        use notify::Watcher as _;

        let watched = path.to_owned();
        let mut inner =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| match event {
                Ok(event) => {
                    if event.kind.is_access() {
                        return;
                    }
                    if !event.paths.iter().any(|changed| changed == &watched) {
                        return;
                    }
                    notify();
                }
                Err(err) => tracing::debug!(%err, "watch error"),
            })
            .map_err(|err| tracing::warn!(%err, "could not start the config watcher"))
            .ok()?;

        let directory = path.parent().unwrap_or_else(|| std::path::Path::new("."));
        if let Err(err) = inner.watch(directory, notify::RecursiveMode::NonRecursive) {
            // The directory may not exist on a first run; `--config-init` makes
            // it, and the file being absent is not a failure to start.
            tracing::debug!(path = %directory.display(), %err, "config directory not watched");
            return None;
        }
        Some(Self { inner })
    }
}

impl std::fmt::Debug for ConfigWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConfigWatcher")
    }
}

/// A burst of file events collapsed into one reload.
///
/// One save is several events — a truncate, a write, sometimes a rename — and
/// re-reading the file per event would rebuild the window's look and its keys
/// three times for one edit. So an event only starts a clock, and the clock
/// restarts while events keep arriving.
#[derive(Debug, Default, Clone, Copy)]
pub struct Settle {
    at: Option<std::time::Instant>,
}

impl Settle {
    /// How long the file has to be quiet before it is read.
    pub const DELAY: std::time::Duration = std::time::Duration::from_millis(150);

    /// Note an event at `now`, pushing the deadline out.
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

    #[test]
    fn the_ssh_config_is_watched_even_before_it_exists() {
        let paths = paths();
        assert!(
            paths
                .iter()
                .any(|path| path == std::path::Path::new("/dev"))
        );
        assert!(
            paths
                .iter()
                .any(|path| path.ends_with(".ssh") || path.ends_with(".ssh/config")),
            "the ssh config, or its directory, is watched"
        );
    }

    /// One save is several events, and a reload is not free: the window's look,
    /// the keys and the terminals' theme all move. So the clock restarts while
    /// the file is still being written, and fires once, 150 ms after the last
    /// event.
    #[test]
    fn a_burst_of_edits_becomes_one_reload() {
        use std::time::{Duration, Instant};
        let start = Instant::now();
        let mut settle = Settle::default();
        assert_eq!(settle.due(), None, "an untouched file has no clock");
        assert!(!settle.take(start));

        settle.note(start);
        settle.note(start + Duration::from_millis(100));
        assert_eq!(
            settle.due(),
            Some(start + Duration::from_millis(100) + Settle::DELAY),
            "the last event sets the deadline, not the first"
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
