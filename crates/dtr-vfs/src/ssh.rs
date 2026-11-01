//! A pane's `ssh` alias as a filesystem.
//!
//! One [`Session`] per alias, behind a lock: two tabs on `jupyter` must share
//! one `ssh -s jupyter sftp` child, or the server's `MaxSessions` refuses the
//! second. Every operation has a deadline, and a timeout or a protocol error
//! drops the child and reconnects once — a session that has failed is not a
//! session, and leaving it in place blocks every pane pointed at that machine.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use tokio::sync::Mutex;

use crate::{Backend, BackendId, Batches, BoxFuture, Caps, Entry, Kind, Meta};

/// How long an operation may take before the connection is considered dead.
/// A hung `read` on a link that went away is otherwise forever.
const DEADLINE: Duration = Duration::from_secs(30);

/// How many entries a batch carries, matching the local backend.
const BATCH: usize = 64;

/// One alias's connection, shared by every operation on it.
type Slot = Arc<Mutex<Option<dtr_sftp::Session>>>;

pub struct Ssh {
    id: BackendId,
    alias: String,
    /// Shared rather than owned, because a listing runs in its own task and
    /// still has to use *this* connection — a fresh one per `read_dir` would
    /// spawn an `ssh` per directory, which is the thing pooling avoids.
    session: Slot,
}

impl Ssh {
    pub fn new(alias: impl Into<String>) -> Self {
        let alias = alias.into();
        Self {
            id: BackendId::Ssh(alias.clone()),
            alias,
            session: Arc::new(Mutex::new(None)),
        }
    }
}

/// Run one operation with a deadline and one reconnect.
///
/// A failed or timed-out operation takes the child with it: leaving a dead
/// session in the slot would make every later call fail the same way, which
/// reads as "this host is down" long after it came back.
async fn guarded<T, F>(
    alias: &str,
    session: &Mutex<Option<dtr_sftp::Session>>,
    mut op: F,
) -> Result<T>
where
    F: for<'a> FnMut(&'a mut dtr_sftp::Session) -> BoxFuture<'a, Result<T>>,
{
    for attempt in 0..2 {
        let mut slot = session.lock().await;
        if slot.is_none() {
            let connected = dtr_sftp::Session::connect(alias)
                .await
                .with_context(|| format!("connect to {alias}"))?;
            *slot = Some(connected);
        }
        let live = slot.as_mut().expect("a session was just put there");
        let result = tokio::time::timeout(DEADLINE, op(live)).await;
        match result {
            Ok(Ok(value)) => return Ok(value),
            Ok(Err(err)) if attempt == 0 => {
                tracing::warn!(%alias, %err, "sftp failed; reconnecting");
                if let Some(dead) = slot.take() {
                    dead.close();
                }
            }
            Ok(Err(err)) => return Err(err),
            Err(_) if attempt == 0 => {
                tracing::warn!(%alias, "sftp timed out; reconnecting");
                if let Some(dead) = slot.take() {
                    dead.close();
                }
            }
            Err(_) => {
                if let Some(dead) = slot.take() {
                    dead.close();
                }
                anyhow::bail!("sftp to {alias} timed out after {}s", DEADLINE.as_secs());
            }
        }
    }
    unreachable!("the loop returns on the last attempt")
}

fn as_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

impl Backend for Ssh {
    fn id(&self) -> &BackendId {
        &self.id
    }

    fn caps(&self) -> Caps {
        // Inotify does not cross an ssh connection, and a directory has no
        // length. Saying so is what stops the UI polling for changes that can
        // never arrive.
        Caps {
            watch: false,
            dir_size: false,
        }
    }

    fn stat<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, Result<Meta>> {
        Box::pin(async move {
            let remote = as_str(path);
            let entry = guarded(&self.alias, &self.session, move |live| {
                let remote = remote.clone();
                Box::pin(async move { live.stat(&remote).await })
            })
            .await?;
            Ok(Meta {
                kind: if entry.dir { Kind::Dir } else { Kind::File },
                len: entry.size,
                modified: entry.modified.and_then(|seconds| {
                    std::time::UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
                }),
            })
        })
    }

    fn list(&self, path: &Path) -> Batches {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let remote = as_str(path);
        let alias = self.alias.clone();
        let session = Arc::clone(&self.session);
        tokio::spawn(async move {
            // The protocol hands back a whole `read_dir` at once, so this is
            // one round trip delivered in batches: the stream the UI consumes
            // has the shape a paged implementation will have.
            let result = guarded(&alias, &session, move |live| {
                let remote = remote.clone();
                Box::pin(async move { live.list(&remote).await })
            })
            .await;
            match result {
                Ok(entries) => {
                    let mut batch = Vec::with_capacity(BATCH);
                    for entry in entries {
                        batch.push(Entry {
                            name: entry.name,
                            kind: if entry.dir { Kind::Dir } else { Kind::File },
                            len: entry.size,
                        });
                        if batch.len() >= BATCH
                            && tx.send(Ok(std::mem::take(&mut batch))).await.is_err()
                        {
                            return;
                        }
                    }
                    if !batch.is_empty() {
                        let _ = tx.send(Ok(batch)).await;
                    }
                }
                Err(err) => {
                    let _ = tx.send(Err(err)).await;
                }
            }
        });
        rx
    }

    fn fetch<'a>(&'a self, path: &'a Path, dest: &'a Path) -> BoxFuture<'a, Result<u64>> {
        Box::pin(async move {
            let remote = as_str(path);
            let local = dest.to_path_buf();
            guarded(&self.alias, &self.session, move |live| {
                let remote = remote.clone();
                let local = local.clone();
                Box::pin(async move { live.download(&remote, &local).await })
            })
            .await
        })
    }
}
