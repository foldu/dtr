//! This machine's filesystem.

use std::path::Path;

use anyhow::{Context as _, Result};

use crate::{Backend, BackendId, Batches, BoxFuture, Caps, Entry, Kind, Meta};

/// How many entries a batch holds. Small enough that a 100 000-entry directory
/// starts showing rows at once, large enough that the channel is not the cost.
const BATCH: usize = 64;

/// The filesystem dtr itself is running on.
pub struct Local {
    id: BackendId,
}

impl Default for Local {
    fn default() -> Self {
        Self::new()
    }
}

impl Local {
    pub fn new() -> Self {
        Self {
            id: BackendId::Local,
        }
    }
}

/// The two answers a filesystem's metadata agrees on, whichever crate asked:
/// `compio::fs::Metadata` is its own type, not a `std::fs::Metadata`.
trait KindOf {
    fn is_dir(&self) -> bool;
    fn is_file(&self) -> bool;
}

impl KindOf for std::fs::Metadata {
    fn is_dir(&self) -> bool {
        std::fs::Metadata::is_dir(self)
    }
    fn is_file(&self) -> bool {
        std::fs::Metadata::is_file(self)
    }
}

impl KindOf for compio::fs::Metadata {
    fn is_dir(&self) -> bool {
        compio::fs::Metadata::is_dir(self)
    }
    fn is_file(&self) -> bool {
        compio::fs::Metadata::is_file(self)
    }
}

fn kind_of(meta: &(impl KindOf + ?Sized)) -> Kind {
    if meta.is_dir() {
        Kind::Dir
    } else if meta.is_file() {
        Kind::File
    } else {
        Kind::Other
    }
}

impl Backend for Local {
    fn id(&self) -> &BackendId {
        &self.id
    }

    fn caps(&self) -> Caps {
        // A local filesystem can be watched; a directory has no length of its
        // own.
        Caps {
            watch: true,
            dir_size: false,
        }
    }

    fn stat<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, Result<Meta>> {
        Box::pin(async move {
            let meta = compio::fs::metadata(path)
                .await
                .with_context(|| format!("stat {}", path.display()))?;
            Ok(Meta {
                kind: kind_of(&meta),
                len: meta.len(),
                modified: meta.modified().ok(),
            })
        })
    }

    fn fetch<'a>(&'a self, path: &'a Path, dest: &'a Path) -> BoxFuture<'a, Result<u64>> {
        Box::pin(async move {
            // compio-fs has no `copy`, and a file copy is a blocking syscall
            // wherever it runs: the blocking pool is where it belongs.
            let from = path.to_path_buf();
            let to = dest.to_path_buf();
            let described = format!("copy {} to {}", path.display(), dest.display());
            compio::runtime::spawn_blocking(move || std::fs::copy(&from, &to))
                .await
                .map_err(|err| anyhow::anyhow!("{err}"))?
                .with_context(|| described)
        })
    }

    fn list(&self, path: &Path) -> Batches {
        let (tx, rx) = async_channel::bounded(4);
        let dir = path.to_path_buf();
        // A directory walk is blocking syscalls all the way down, and it must
        // not hold the thread that draws: the batch channel is how it talks to
        // whoever is showing the rows.
        compio::runtime::spawn_blocking(move || walk(&dir, &tx)).detach();
        rx
    }
}

/// Read one directory, a batch at a time, into `tx`.
///
/// A reader that goes away is the cancel: `send_blocking` refuses a closed
/// channel, and blocking on a full one is backpressure rather than a lost
/// batch. One unreadable entry does not end the listing — it is shown as other
/// with no size, which is the truth.
fn walk(path: &Path, tx: &async_channel::Sender<Result<Vec<Entry>>>) {
    let failed = |err: std::io::Error| {
        let _ = tx.send_blocking(Err(
            anyhow::Error::new(err).context(format!("list {}", path.display()))
        ));
    };
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(err) => return failed(err),
    };
    let mut batch = Vec::with_capacity(BATCH);
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => return failed(err),
        };
        let Ok(meta) = entry.metadata() else {
            batch.push(Entry {
                name: entry.file_name().to_string_lossy().into_owned(),
                kind: Kind::Other,
                len: 0,
            });
            continue;
        };
        batch.push(Entry {
            name: entry.file_name().to_string_lossy().into_owned(),
            kind: kind_of(&meta),
            len: meta.len(),
        });
        if batch.len() >= BATCH && tx.send_blocking(Ok(std::mem::take(&mut batch))).is_err() {
            return;
        }
    }
    if !batch.is_empty() {
        let _ = tx.send_blocking(Ok(batch));
    }
}
