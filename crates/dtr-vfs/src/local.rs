//! This machine's filesystem.

use std::path::Path;

use anyhow::{Context as _, Result};
use tokio::fs;

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

fn kind_of(meta: &std::fs::Metadata) -> Kind {
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
        // notify can watch it; a directory has no length of its own.
        Caps {
            watch: true,
            dir_size: false,
        }
    }

    fn stat<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, Result<Meta>> {
        Box::pin(async move {
            let meta = fs::metadata(path)
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
            let copied = fs::copy(path, dest)
                .await
                .with_context(|| format!("copy {} to {}", path.display(), dest.display()))?;
            Ok(copied)
        })
    }

    fn list(&self, path: &Path) -> Batches {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let path = path.to_path_buf();
        tokio::spawn(async move {
            let mut dir = match fs::read_dir(&path).await {
                Ok(dir) => dir,
                Err(err) => {
                    let _ = tx
                        .send(Err(
                            anyhow::Error::new(err).context(format!("list {}", path.display()))
                        ))
                        .await;
                    return;
                }
            };
            let mut batch = Vec::with_capacity(BATCH);
            loop {
                let entry = match dir.next_entry().await {
                    Ok(Some(entry)) => entry,
                    Ok(None) => break,
                    Err(err) => {
                        let _ = tx
                            .send(Err(
                                anyhow::Error::new(err).context(format!("list {}", path.display()))
                            ))
                            .await;
                        return;
                    }
                };
                let Ok(meta) = entry.metadata().await else {
                    // One unreadable entry does not end the listing; it is
                    // shown as other with no size, which is the truth.
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
                if batch.len() >= BATCH && tx.send(Ok(std::mem::take(&mut batch))).await.is_err() {
                    // The reader went away: stop walking, that is the cancel.
                    return;
                }
            }
            if !batch.is_empty() {
                let _ = tx.send(Ok(batch)).await;
            }
        });
        rx
    }
}
