//! A filesystem in memory, for tests and for previewing what a remote backend
//! will look like before there is one.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context as _, Result};
use tokio::io::AsyncRead;

use crate::{Backend, BackendId, Batches, BoxFuture, Caps, Entry, Kind, Meta};

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
}

impl Backend for Memory {
    fn id(&self) -> &BackendId {
        &self.id
    }

    fn caps(&self) -> Caps {
        Caps {
            watch: false,
            dir_size: false,
        }
    }

    fn stat<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, Result<Meta>> {
        Box::pin(async move {
            if self
                .dirs
                .lock()
                .expect("memory lock")
                .contains(path)
            {
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

    fn open<'a>(
        &'a self,
        path: &'a Path,
    ) -> BoxFuture<'a, Result<Box<dyn AsyncRead + Send + Unpin>>> {
        Box::pin(async move {
            let files = self.files.lock().expect("memory lock");
            let bytes = files
                .get(path)
                .with_context(|| format!("open {}", path.display()))?
                .clone();
            Ok(Box::new(std::io::Cursor::new(bytes)) as Box<dyn AsyncRead + Send + Unpin>)
        })
    }

    fn list(&self, path: &Path) -> Batches {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let mut entries: Vec<Entry> = Vec::new();
        let mut seen = BTreeSet::new();

        let direct = |child: &Path| child.parent() == Some(path) && child != path;
        for (name, bytes) in self.files.lock().expect("memory lock").iter() {
            if direct(name) {
                if let Some(file_name) = name.file_name() {
                    seen.insert(file_name.to_owned());
                    entries.push(Entry {
                        name: file_name.to_string_lossy().into_owned(),
                        kind: Kind::File,
                        len: bytes.len() as u64,
                    });
                }
            }
        }
        for dir in self.dirs.lock().expect("memory lock").iter() {
            if direct(dir) {
                if let Some(file_name) = dir.file_name() {
                    if seen.insert(file_name.to_owned()) {
                        entries.push(Entry {
                            name: file_name.to_string_lossy().into_owned(),
                            kind: Kind::Dir,
                            len: 0,
                        });
                    }
                }
            }
        }

        // One batch, then the stream ends: an in-memory listing is already
        // whole, and pretending otherwise would test nothing real.
        tokio::spawn(async move {
            if !entries.is_empty() {
                let _ = tx.send(Ok(entries)).await;
            }
        });
        rx
    }
}
