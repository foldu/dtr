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
use std::time::{Duration, Instant};

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

/// Swallows a burst of change events into one refresh.
///
/// A USB enumeration produces dozens of events in a few milliseconds; each one
/// would mean re-reading `/dev` and the ssh config. This lets the first through
/// and then ignores the rest for `window`.
#[derive(Debug)]
pub struct Coalesce {
    window: Duration,
    last: Option<Instant>,
}

impl Coalesce {
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            last: None,
        }
    }

    /// Whether this change should cause a refresh now.
    pub fn ready(&mut self, now: Instant) -> bool {
        match self.last {
            Some(last) if now.duration_since(last) < self.window => false,
            _ => {
                self.last = Some(now);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_of_changes_refreshes_once() {
        let mut coalesce = Coalesce::new(Duration::from_millis(250));
        let now = Instant::now();
        assert!(coalesce.ready(now), "the first change refreshes");
        assert!(!coalesce.ready(now + Duration::from_millis(10)));
        assert!(!coalesce.ready(now + Duration::from_millis(200)));
        assert!(
            coalesce.ready(now + Duration::from_millis(300)),
            "the window has passed, so the next change refreshes"
        );
    }

    #[test]
    fn the_ssh_config_is_watched_even_before_it_exists() {
        let paths = paths();
        assert!(paths.iter().any(|path| path == std::path::Path::new("/dev")));
        assert!(
            paths
                .iter()
                .any(|path| path.ends_with(".ssh") || path.ends_with(".ssh/config")),
            "the ssh config, or its directory, is watched"
        );
    }
}
