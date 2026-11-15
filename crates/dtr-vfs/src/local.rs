//! This machine's filesystem.

use std::path::Path;

use anyhow::{Context as _, Result};

use crate::{Backend, BackendId, Batches, BoxFuture, Caps, Entry, Facts, Kind, Meta, Refinements};

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
        // A directory walk is blocking syscalls all the way down, so it gets a
        // thread of its own — and a plain `std` one rather than a runtime's pool,
        // because a listing has to be startable from a thread that has no runtime:
        // a re-read triggered by a keystroke is one, and the picker's window is
        // another. Dropping the receiver is the cancel.
        let reporting = tx.clone();
        if let Err(err) = std::thread::Builder::new()
            .name("dtr-list".into())
            .spawn(move || walk(&dir, &tx))
        {
            let _ = reporting.send_blocking(Err(anyhow::anyhow!(
                "could not start a listing thread: {err}"
            )));
        }
        rx
    }

    fn facts(&self, dir: &Path, names: Vec<String>) -> Refinements {
        let (tx, rx) = async_channel::bounded(4);
        let dir = dir.to_path_buf();
        let reporting = tx.clone();
        if let Err(err) = std::thread::Builder::new()
            .name("dtr-facts".into())
            .spawn(move || stat_all(&dir, names, &tx))
        {
            let _ = reporting.send_blocking(Err(anyhow::anyhow!(
                "could not start a metadata thread: {err}"
            )));
        }
        rx
    }
}

/// Read one directory, a batch at a time, into `tx`.
///
/// A reader that goes away is the cancel: `send_blocking` refuses a closed
/// channel, and blocking on a full one is backpressure rather than a lost
/// batch. One unreadable entry does not end the listing — it is shown as other
/// with no size, which is the truth.
///
/// **No `stat` happens here.** A directory entry carries the name and, where the
/// filesystem provides it, the `d_type` — so a name sort costs one `readdir` and
/// a sort, and the size and the date are [`Local::facts`]'s business. Where
/// `d_type` is `DT_UNKNOWN` (network filesystems, mostly) std does the `lstat`
/// itself, so this is a saving on ext4/xfs/btrfs and never worse elsewhere. On
/// Windows the walk is [`dtr_shell::Dir`], which gets the same things from
/// `FindFirstFileW` — including whether the entry is hidden and how big it is,
/// which `read_dir` on that platform keeps to itself.
fn walk(path: &Path, tx: &async_channel::Sender<Result<Vec<Entry>>>) {
    let failed = |err: std::io::Error| {
        let _ = tx.send_blocking(Err(
            anyhow::Error::new(err).context(format!("list {}", path.display()))
        ));
    };
    let found = match entries(path) {
        Ok(found) => found,
        Err(err) => return failed(err),
    };
    let mut batch = Vec::with_capacity(BATCH);
    for entry in found {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => return failed(err),
        };
        batch.push(entry);
        if batch.len() >= BATCH && tx.send_blocking(Ok(std::mem::take(&mut batch))).is_err() {
            return;
        }
    }
    if !batch.is_empty() {
        let _ = tx.send_blocking(Ok(batch));
    }
}

/// Every name in `path`, with everything the walk could say for free — including
/// whether this machine calls it hidden, which is the platform's rule and not the
/// view's.
#[cfg(not(windows))]
fn entries(path: &Path) -> std::io::Result<impl Iterator<Item = std::io::Result<Entry>>> {
    Ok(std::fs::read_dir(path)?.map(|entry| {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        // The Unix rule, and the only place it is written down: a name that starts
        // with a dot is the filesystem's way of saying hidden.
        let hidden = name.starts_with('.');
        Ok(Entry {
            name,
            kind: kind_of_entry(&entry),
            hidden,
            len: None,
            modified: None,
        })
    }))
}

/// The Windows walk, whose attributes arrive with the name — and with the size and
/// the write time, which is why a listing there needs no metadata pass at all.
#[cfg(windows)]
fn entries(path: &Path) -> std::io::Result<impl Iterator<Item = std::io::Result<Entry>>> {
    Ok(dtr_shell::Dir::open(path)?.map(|found| {
        let found = found?;
        Ok(Entry {
            name: found.name,
            kind: if found.is_dir { Kind::Dir } else { Kind::File },
            hidden: found.hidden,
            len: found.len,
            modified: found.modified,
        })
    }))
}

/// What a directory entry says it is, without a `stat` where the filesystem
/// answers that question in the entry itself.
///
/// A symlink is the one case that needs one: `d_type` says "symlink", not what it
/// points at, and a link to a directory that arrived as a file would **change
/// groups** when the metadata pass caught up — a row jumping from the files to the
/// folders while somebody is reading it. Links are rare, and the kind a row is
/// given here is the kind it keeps.
#[cfg(not(windows))]
fn kind_of_entry(entry: &std::fs::DirEntry) -> Kind {
    let Ok(file_type) = entry.file_type() else {
        return Kind::Other;
    };
    if file_type.is_dir() {
        return Kind::Dir;
    }
    if file_type.is_file() {
        return Kind::File;
    }
    if file_type.is_symlink() {
        // Follows, because that is what a file manager shows: a link to a
        // directory behaves as a directory.
        return match std::fs::metadata(entry.path()) {
            Ok(meta) => kind_of(&meta),
            Err(_) => Kind::Other,
        };
    }
    Kind::Other
}

/// Stat `names` in `dir`, a batch at a time.
///
/// The order is the caller's, so the rows on screen can be asked for first and the
/// rest followed up. A name that cannot be stat'ed still gets a batch, with both
/// facts unknown: dropping it would leave a row that never stops saying "reading".
fn stat_all(dir: &Path, names: Vec<String>, tx: &async_channel::Sender<Result<Vec<Facts>>>) {
    let mut batch = Vec::with_capacity(BATCH);
    for name in names {
        let meta = std::fs::metadata(dir.join(&name)).ok();
        batch.push(Facts {
            len: meta.as_ref().map(std::fs::Metadata::len),
            modified: meta.as_ref().and_then(|meta| meta.modified().ok()),
            name,
        });
        if batch.len() >= BATCH && tx.send_blocking(Ok(std::mem::take(&mut batch))).is_err() {
            return;
        }
    }
    if !batch.is_empty() {
        let _ = tx.send_blocking(Ok(batch));
    }
}
