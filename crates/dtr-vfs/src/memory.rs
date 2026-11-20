//! A filesystem in memory, for tests and for previewing what a remote backend
//! will look like before there is one.
//!
//! It can write: directories are created, entries renamed, files copied and
//! removed permanently. It has no trash (`Caps::trash` is false), so a
//! [`Removal::Trash`] is refused rather than emulated — there is nothing here a
//! desktop's trash could restore from.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context as _, Result};

use crate::{Backend, BackendId, Batches, BoxFuture, Caps, Conflict, Entry, Kind, Meta, Removal};

/// Files and directories held in maps. Reads never touch the disk, which is what
/// makes it useful: a test can assert async behaviour, cancellation and errors
/// without an `ssh` server or a temporary directory.
pub struct Memory {
    id: BackendId,
    files: Mutex<BTreeMap<PathBuf, Vec<u8>>>,
    dirs: Mutex<BTreeSet<PathBuf>>,
}

impl Memory {
    pub fn new() -> Self {
        Self {
            id: BackendId::Local,
            files: Mutex::new(BTreeMap::new()),
            dirs: Mutex::new(BTreeSet::new()),
        }
    }

    /// Put a file there, creating no directories: a listing of a path that is
    /// also a file is a test's problem, not the backend's.
    pub fn write(&self, path: impl Into<PathBuf>, bytes: impl Into<Vec<u8>>) {
        self.files
            .lock()
            .expect("memory lock")
            .insert(path.into(), bytes.into());
    }

    /// Declare an (empty) directory. A directory that holds files is implicit.
    pub fn dir(&self, path: impl Into<PathBuf>) {
        self.dirs.lock().expect("memory lock").insert(path.into());
    }

    fn has_file(&self, path: &Path) -> bool {
        self.files.lock().expect("memory lock").contains_key(path)
    }

    /// Whether `path` is a directory: declared, or implied by something stored
    /// under it. A path that holds entries is a directory even when nothing
    /// declared it, which is how `/home/lab` exists after `write` put a file in
    /// it.
    fn has_dir(&self, path: &Path) -> bool {
        if path == Path::new("/") {
            return true;
        }
        if self.dirs.lock().expect("memory lock").contains(path) {
            return true;
        }
        self.files
            .lock()
            .expect("memory lock")
            .keys()
            .any(|file| file.starts_with(path))
            || self
                .dirs
                .lock()
                .expect("memory lock")
                .iter()
                .any(|dir| dir.starts_with(path))
    }

    fn exists(&self, path: &Path) -> bool {
        self.has_file(path) || self.has_dir(path)
    }

    fn check_parent(&self, path: &Path) -> Result<()> {
        let parent = parent_of(path);
        if self.has_dir(parent) {
            Ok(())
        } else {
            anyhow::bail!("no such directory: {}", parent.display())
        }
    }

    /// Move every file and directory entry at or under `from` to the same
    /// place under `to`.
    fn rewrite(&self, from: &Path, to: &Path) {
        let mut files = self.files.lock().expect("memory lock");
        let moving: Vec<(PathBuf, Vec<u8>)> = files
            .iter()
            .filter(|(path, _)| path.starts_with(from))
            .map(|(path, bytes)| (path.clone(), bytes.clone()))
            .collect();
        for (path, bytes) in moving {
            files.remove(&path);
            files.insert(rebased(from, to, &path), bytes);
        }
        drop(files);

        let mut dirs = self.dirs.lock().expect("memory lock");
        let moving: Vec<PathBuf> = dirs
            .iter()
            .filter(|path| path.starts_with(from))
            .cloned()
            .collect();
        for path in moving {
            dirs.remove(&path);
            dirs.insert(rebased(from, to, &path));
        }
    }
}

/// `path` with its `from` prefix replaced by `to`.
fn rebased(from: &Path, to: &Path, path: &Path) -> PathBuf {
    match path.strip_prefix(from) {
        // The path is `from` itself; `join("")` would leave a trailing slash.
        Ok(relative) if relative.as_os_str().is_empty() => to.to_path_buf(),
        Ok(relative) => to.join(relative),
        // Only called on entries the caller filtered by prefix.
        Err(_) => path.to_path_buf(),
    }
}

/// The directory a path is in; `/` for a top-level name.
fn parent_of(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("/"))
}

impl Backend for Memory {
    fn id(&self) -> &BackendId {
        &self.id
    }

    fn caps(&self) -> Caps {
        Caps {
            watch: false,
            dir_size: false,
            write: true,
            // There is no desktop trash behind an in-memory map: a delete here
            // is permanent, and the UI says so.
            trash: false,
        }
    }

    fn stat<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, Result<Meta>> {
        Box::pin(async move {
            if self.dirs.lock().expect("memory lock").contains(path) {
                return Ok(Meta {
                    kind: Kind::Dir,
                    len: 0,
                    modified: None,
                });
            }
            let files = self.files.lock().expect("memory lock");
            let Some(bytes) = files.get(path) else {
                anyhow::bail!("no such file: {}", path.display());
            };
            Ok(Meta {
                kind: Kind::File,
                len: bytes.len() as u64,
                modified: None,
            })
        })
    }

    fn fetch<'a>(&'a self, path: &'a Path, dest: &'a Path) -> BoxFuture<'a, Result<u64>> {
        Box::pin(async move {
            let bytes = {
                let files = self.files.lock().expect("memory lock");
                files
                    .get(path)
                    .with_context(|| format!("fetch {}", path.display()))?
                    .clone()
            };
            let written = bytes.len() as u64;
            compio::fs::write(dest, bytes)
                .await
                .0
                .with_context(|| format!("write {}", dest.display()))?;
            Ok(written)
        })
    }

    fn list(&self, path: &Path) -> Batches {
        let (tx, rx) = async_channel::bounded(4);
        let mut entries: Vec<Entry> = Vec::new();
        let mut seen = BTreeSet::new();

        let direct = |child: &Path| child.parent() == Some(path) && child != path;
        for (name, bytes) in self.files.lock().expect("memory lock").iter() {
            if direct(name) {
                if let Some(file_name) = name.file_name() {
                    seen.insert(file_name.to_owned());
                    entries.push(Entry {
                        // In-memory paths have no machine behind them, so the Unix
                        // spelling is the only rule there is to apply.
                        hidden: file_name.to_string_lossy().starts_with('.'),
                        name: file_name.to_string_lossy().into_owned(),
                        kind: Kind::File,
                        len: Some(bytes.len() as u64),
                        // An in-memory file has no time, and `None` says so
                        // rather than claiming 1970.
                        modified: None,
                    });
                }
            }
        }
        for dir in self.dirs.lock().expect("memory lock").iter() {
            if direct(dir) {
                if let Some(file_name) = dir.file_name() {
                    if seen.insert(file_name.to_owned()) {
                        entries.push(Entry {
                            hidden: file_name.to_string_lossy().starts_with('.'),
                            name: file_name.to_string_lossy().into_owned(),
                            kind: Kind::Dir,
                            len: None,
                            modified: None,
                        });
                    }
                }
            }
        }

        // One batch, then the stream ends: an in-memory listing is already
        // whole, and pretending otherwise would test nothing real.
        compio::runtime::spawn(async move {
            if !entries.is_empty() {
                let _ = tx.send(Ok(entries)).await;
            }
        })
        .detach();
        rx
    }

    fn create_dir<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if self.exists(path) {
                return Err(Conflict {
                    path: path.to_path_buf(),
                }
                .into());
            }
            self.check_parent(path)?;
            self.dirs
                .lock()
                .expect("memory lock")
                .insert(path.to_path_buf());
            Ok(())
        })
    }

    fn rename<'a>(&'a self, from: &'a Path, to: &'a Path) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if from == to {
                return Ok(());
            }
            if !self.exists(from) {
                anyhow::bail!("no such file: {}", from.display());
            }
            if self.exists(to) {
                return Err(Conflict {
                    path: to.to_path_buf(),
                }
                .into());
            }
            if to.starts_with(from) {
                anyhow::bail!(
                    "cannot move {} into itself ({})",
                    from.display(),
                    to.display()
                );
            }
            self.check_parent(to)?;
            self.rewrite(from, to);
            Ok(())
        })
    }

    fn copy<'a>(&'a self, from: &'a Path, to: &'a Path) -> BoxFuture<'a, Result<u64>> {
        Box::pin(async move {
            if self.exists(to) {
                return Err(Conflict {
                    path: to.to_path_buf(),
                }
                .into());
            }
            let bytes = self.files.lock().expect("memory lock").get(from).cloned();
            let Some(bytes) = bytes else {
                if self.has_dir(from) {
                    anyhow::bail!(
                        "copy {}: copying a directory is not supported yet (it belongs to the transfer queue)",
                        from.display()
                    );
                }
                anyhow::bail!("no such file: {}", from.display());
            };
            self.check_parent(to)?;
            let written = bytes.len() as u64;
            self.files
                .lock()
                .expect("memory lock")
                .insert(to.to_path_buf(), bytes);
            Ok(written)
        })
    }

    fn move_to<'a>(&'a self, from: &'a Path, to: &'a Path) -> BoxFuture<'a, Result<()>> {
        // On one machine a move *is* a rename; the two verbs exist for the UI.
        self.rename(from, to)
    }

    fn remove<'a>(&'a self, path: &'a Path, how: Removal) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            match how {
                Removal::Trash => anyhow::bail!(
                    "the in-memory backend has no trash: ask for a permanent delete of {}",
                    path.display()
                ),
                Removal::Permanent => {
                    if self.has_file(path) {
                        self.files.lock().expect("memory lock").remove(path);
                        return Ok(());
                    }
                    if self.has_dir(path) {
                        let mut files = self.files.lock().expect("memory lock");
                        files.retain(|file, _| !file.starts_with(path));
                        drop(files);
                        let mut dirs = self.dirs.lock().expect("memory lock");
                        dirs.retain(|dir| !dir.starts_with(path));
                        return Ok(());
                    }
                    anyhow::bail!("no such file: {}", path.display());
                }
            }
        })
    }
}
