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
}
