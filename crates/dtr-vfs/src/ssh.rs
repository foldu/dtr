//! A pane's `ssh` alias as a filesystem.
//!
//! One [`Session`] per alias — and, because compio's handles are thread-local,
//! one *thread* per session: the thread that opens the child is the thread that
//! speaks to it, and an operation is a message to that thread. That is also what
//! keeps the "one connection per alias" rule without a mutex crossing threads.
//!
//! Every operation has a deadline, and a timeout or a protocol error drops the
//! child and reconnects once — a session that has failed is not a session, and
//! leaving it in place blocks every pane pointed at that machine.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use anyhow::{Context as _, Result};

use crate::{Backend, BackendId, Batches, BoxFuture, Caps, Entry, Kind, Meta};

/// How long an operation may take before the connection is considered dead.
/// A hung `read` on a link that went away is otherwise forever.
const DEADLINE: Duration = Duration::from_secs(30);

/// How many entries a batch carries, matching the local backend.
const BATCH: usize = 64;

/// One message to an alias's thread.
enum Op {
    Stat {
        path: String,
        reply: async_channel::Sender<Result<dtr_sftp::Entry>>,
    },
    Fetch {
        path: String,
        dest: PathBuf,
        reply: async_channel::Sender<Result<u64>>,
    },
    List {
        path: String,
        batches: async_channel::Sender<Result<Vec<Entry>>>,
    },
}

pub struct Ssh {
    id: BackendId,
    alias: String,
    /// The alias's thread. Started by the first operation: a backend that is
    /// built but never used costs no thread and no `ssh`.
    conn: std::sync::OnceLock<Conn>,
}

impl Ssh {
    pub fn new(alias: impl Into<String>) -> Self {
        let alias = alias.into();
        Self {
            id: BackendId::Ssh(alias.clone()),
            alias,
            conn: std::sync::OnceLock::new(),
        }
    }

    fn conn(&self) -> &Conn {
        self.conn.get_or_init(|| Conn::spawn(self.alias.clone()))
    }
}

/// The sending half of an alias's thread.
struct Conn {
    ops: async_channel::Sender<Op>,
}

impl Conn {
    /// Start the thread that owns this alias's `ssh` child.
    fn spawn(alias: String) -> Self {
        let (ops, inbox) = async_channel::unbounded::<Op>();
        let named = alias.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("dtr-sftp-{named}"))
            .spawn(move || {
                let runtime = match compio::runtime::Runtime::new() {
                    Ok(runtime) => runtime,
                    Err(err) => {
                        tracing::error!(%err, %alias, "no sftp runtime");
                        return;
                    }
                };
                let worker = runtime.clone();
                let inner = alias.clone();
                runtime.block_on(async move {
                    // One session per thread, shared by every operation: the
                    // protocol has ids, but a round trip at a time is what the
                    // callers do, and the mutex says so.
                    let session = Rc::new(async_lock::Mutex::new(None::<dtr_sftp::Session>));
                    while let Ok(op) = inbox.recv().await {
                        let session = Rc::clone(&session);
                        let alias = inner.clone();
                        worker
                            .spawn(async move { serve(&alias, &session, op).await })
                            .detach();
                    }
                });
            });
        if let Err(err) = spawned {
            // The channel stays open but has no receiver, so every operation
            // fails with "the thread is gone" rather than hanging.
            tracing::error!(%err, %named, "could not start the sftp thread");
        }
        Self { ops }
    }

    async fn stat(&self, path: String) -> Result<dtr_sftp::Entry> {
        let (reply, answer) = async_channel::bounded(1);
        self.ops
            .send(Op::Stat { path, reply })
            .await
            .map_err(|_| unreachable_alias())?;
        answer.recv().await.map_err(|_| unreachable_alias())?
    }

    async fn fetch(&self, path: String, dest: PathBuf) -> Result<u64> {
        let (reply, answer) = async_channel::bounded(1);
        self.ops
            .send(Op::Fetch { path, dest, reply })
            .await
            .map_err(|_| unreachable_alias())?;
        answer.recv().await.map_err(|_| unreachable_alias())?
    }

    fn list(&self, path: String) -> Batches {
        let (batches, stream) = async_channel::bounded(4);
        // Unbounded, so this cannot block: `list` has to hand back its receiver
        // now, and the thread answers on it.
        let _ = self.ops.try_send(Op::List { path, batches });
        stream
    }
}

fn unreachable_alias() -> anyhow::Error {
    anyhow::anyhow!("the sftp thread for this alias is gone")
}

/// One operation, on the alias's own thread.
async fn serve(alias: &str, session: &Rc<async_lock::Mutex<Option<dtr_sftp::Session>>>, op: Op) {
    match op {
        Op::Stat { path, reply } => {
            let result = guarded(alias, session, move |live| {
                let path = path.clone();
                Box::pin(async move { live.stat(&path).await })
            })
            .await;
            let _ = reply.send(result).await;
        }
        Op::Fetch { path, dest, reply } => {
            let result = guarded(alias, session, move |live| {
                let path = path.clone();
                let dest = dest.clone();
                Box::pin(async move { live.download(&path, &dest).await })
            })
            .await;
            let _ = reply.send(result).await;
        }
        Op::List { path, batches } => {
            // The protocol hands back a whole `read_dir` at once, so this is
            // one round trip delivered in batches: the stream the UI consumes
            // has the shape a paged implementation will have.
            let result = guarded(alias, session, move |live| {
                let path = path.clone();
                Box::pin(async move { live.list(&path).await })
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
                            && batches.send(Ok(std::mem::take(&mut batch))).await.is_err()
                        {
                            return;
                        }
                    }
                    if !batch.is_empty() {
                        let _ = batches.send(Ok(batch)).await;
                    }
                }
                Err(err) => {
                    let _ = batches.send(Err(err)).await;
                }
            }
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
    session: &async_lock::Mutex<Option<dtr_sftp::Session>>,
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
        let result = compio::time::timeout(DEADLINE, op(live)).await;
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
    anyhow::bail!("sftp to {alias} failed twice")
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
            let entry = self.conn().stat(as_str(path)).await?;
            Ok(Meta {
                kind: if entry.dir { Kind::Dir } else { Kind::File },
                len: entry.size,
                modified: entry.modified.and_then(|seconds| {
                    std::time::UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
                }),
            })
        })
    }

    fn fetch<'a>(&'a self, path: &'a Path, dest: &'a Path) -> BoxFuture<'a, Result<u64>> {
        Box::pin(async move { self.conn().fetch(as_str(path), dest.to_path_buf()).await })
    }

    fn list(&self, path: &Path) -> Batches {
        self.conn().list(as_str(path))
    }
}

/// A path as the protocol takes it: UTF-8, lossily, because a pane printed it.
fn as_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
