//! A pane's `ssh` alias as a filesystem.
//!
//! One [`Session`] per alias — and, because compio's handles are thread-local,
//! one *thread* per session: the thread that opens the child is the thread that
//! speaks to it, and an operation is a message to that thread. That is also what
//! keeps the "one connection per alias" rule without a mutex crossing threads.
//!
//! Every operation — a read, a listing, or one of the write verbs — has a
//! deadline, and a timeout or a protocol error drops the child and reconnects
//! once: a session that has failed is not a session, and leaving it in place
//! blocks every pane pointed at that machine. The one answer that is not a
//! failure is [`crate::Conflict`]: it is a decision the UI acts on, so it comes
//! back without spending a reconnect.
//!
//! There is no trash over `ssh`, so [`Caps::trash`] is false and
//! [`Removal::Trash`] is refused here rather than emulated: a delete the user
//! believes is recoverable must never destroy anything.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context as _, Result};

use crate::{Backend, BackendId, Batches, BoxFuture, Caps, Conflict, Entry, Kind, Meta, Removal};

/// How long an operation may take before the connection is considered dead.
/// A hung `read` on a link that went away is otherwise forever.
const DEADLINE: Duration = Duration::from_secs(30);

/// How many entries a batch carries, matching the local backend.
const BATCH: usize = 64;

/// How a session is opened, for one alias. The real backend uses
/// [`dtr_sftp::Session::connect`]; the tests replace it with
/// [`dtr_sftp::Session::from_wire`], which is what lets the thread's whole loop
/// — dispatch, deadline, reconnect — run without a server (`docs/sftp.md` §8).
type Open = dyn Fn() -> BoxFuture<'static, Result<dtr_sftp::Session>> + Send + Sync;

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
    CreateDir {
        path: String,
        reply: async_channel::Sender<Result<()>>,
    },
    Rename {
        from: String,
        to: String,
        reply: async_channel::Sender<Result<()>>,
    },
    Remove {
        path: String,
        reply: async_channel::Sender<Result<()>>,
    },
    Copy {
        from: String,
        to: String,
        reply: async_channel::Sender<Result<u64>>,
    },
}

pub struct Ssh {
    id: BackendId,
    alias: String,
    /// The alias's thread. Started by the first operation: a backend that is
    /// built but never used costs no thread and no `ssh`.
    conn: OnceLock<Conn>,
}

impl Ssh {
    pub fn new(alias: impl Into<String>) -> Self {
        let alias = alias.into();
        Self {
            id: BackendId::Ssh(alias.clone()),
            alias,
            conn: OnceLock::new(),
        }
    }

    /// A backend whose sessions come from `open` instead of the alias's `ssh`:
    /// the test seam. The thread, the deadline, the reconnect and every verb's
    /// dispatch are the production ones — only the wire is scripted.
    #[cfg(test)]
    fn scripted(alias: impl Into<String>, open: Box<Open>) -> Self {
        let alias = alias.into();
        let conn = Conn::spawn_over(alias.clone(), Some(open));
        let cell = OnceLock::new();
        let _ = cell.set(conn);
        Self {
            id: BackendId::Ssh(alias.clone()),
            alias,
            conn: cell,
        }
    }

    fn conn(&self) -> &Conn {
        self.conn
            .get_or_init(|| Conn::spawn_over(self.alias.clone(), None))
    }
}

/// What one alias's thread needs to open a session again after a failure.
struct Dialer {
    alias: String,
    /// `None` is the alias's own `ssh`; `Some` is the test's wire.
    open: Option<Box<Open>>,
}

/// The sending half of an alias's thread.
struct Conn {
    ops: async_channel::Sender<Op>,
}

impl Conn {
    /// Start the thread that owns this alias's `ssh` child. `open` replaces the
    /// alias's own `ssh` for every connection it makes: `None` is production,
    /// and the tests pass a scripted wire.
    fn spawn_over(alias: String, open: Option<Box<Open>>) -> Self {
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
                runtime.block_on(async move {
                    // One session per thread, shared by every operation: the
                    // protocol has ids, but a round trip at a time is what the
                    // callers do, and the mutex says so.
                    let session = Rc::new(async_lock::Mutex::new(None::<dtr_sftp::Session>));
                    let dialer = Rc::new(Dialer { alias, open });
                    while let Ok(op) = inbox.recv().await {
                        let session = Rc::clone(&session);
                        let dialer = Rc::clone(&dialer);
                        worker
                            .spawn(async move { serve(&dialer, &session, op).await })
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

    /// Send one message and wait for its answer. Every verb is this round trip;
    /// what differs is the [`Op`] it carries.
    async fn ask<T, M>(&self, make: M) -> Result<T>
    where
        T: Send + 'static,
        M: FnOnce(async_channel::Sender<Result<T>>) -> Op,
    {
        let (reply, answer) = async_channel::bounded(1);
        self.ops
            .send(make(reply))
            .await
            .map_err(|_| unreachable_alias())?;
        answer.recv().await.map_err(|_| unreachable_alias())?
    }

    async fn stat(&self, path: String) -> Result<dtr_sftp::Entry> {
        self.ask(|reply| Op::Stat { path, reply }).await
    }

    async fn fetch(&self, path: String, dest: PathBuf) -> Result<u64> {
        self.ask(|reply| Op::Fetch { path, dest, reply }).await
    }

    fn list(&self, path: String) -> Batches {
        let (batches, stream) = async_channel::bounded(4);
        // Unbounded, so this cannot block: `list` has to hand back its receiver
        // now, and the thread answers on it.
        let _ = self.ops.try_send(Op::List { path, batches });
        stream
    }

    async fn create_dir(&self, path: String) -> Result<()> {
        self.ask(|reply| Op::CreateDir { path, reply }).await
    }

    async fn rename(&self, from: String, to: String) -> Result<()> {
        self.ask(|reply| Op::Rename { from, to, reply }).await
    }

    async fn remove(&self, path: String) -> Result<()> {
        self.ask(|reply| Op::Remove { path, reply }).await
    }

    async fn copy(&self, from: String, to: String) -> Result<u64> {
        self.ask(|reply| Op::Copy { from, to, reply }).await
    }
}

fn unreachable_alias() -> anyhow::Error {
    anyhow::anyhow!("the sftp thread for this alias is gone")
}

/// One operation, on the alias's own thread.
async fn serve(
    dialer: &Dialer,
    session: &Rc<async_lock::Mutex<Option<dtr_sftp::Session>>>,
    op: Op,
) {
    match op {
        Op::Stat { path, reply } => {
            let result = guarded(dialer, session, move |live| {
                let path = path.clone();
                Box::pin(async move { live.stat(&path).await })
            })
            .await;
            let _ = reply.send(result).await;
        }
        Op::Fetch { path, dest, reply } => {
            let result = guarded(dialer, session, move |live| {
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
            let result = guarded(dialer, session, move |live| {
                let path = path.clone();
                Box::pin(async move { live.list(&path).await })
            })
            .await;
            match result {
                Ok(entries) => {
                    let mut batch = Vec::with_capacity(BATCH);
                    for entry in entries {
                        // `READDIR` carries the size and the time in the same
                        // packet, so a remote listing is complete as it streams:
                        // no metadata pass, and a remote sort is free.
                        batch.push(Entry {
                            // A `READDIR` carries no attributes, and the far side is
                            // a Unix host in every deployment we have: the dot rule is
                            // the best answer available, and it is the right one there.
                            hidden: entry.name.starts_with('.'),
                            name: entry.name,
                            kind: if entry.dir { Kind::Dir } else { Kind::File },
                            len: Some(entry.size),
                            modified: entry.modified.map(|secs| {
                                std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)
                            }),
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
        Op::CreateDir { path, reply } => {
            let result = guarded(dialer, session, move |live| {
                let path = path.clone();
                Box::pin(async move { create_dir(live, &path).await })
            })
            .await;
            let _ = reply.send(result).await;
        }
        Op::Rename { from, to, reply } => {
            let result = guarded(dialer, session, move |live| {
                let from = from.clone();
                let to = to.clone();
                Box::pin(async move { rename(live, &from, &to).await })
            })
            .await;
            let _ = reply.send(result).await;
        }
        Op::Remove { path, reply } => {
            let result = guarded(dialer, session, move |live| {
                let path = path.clone();
                Box::pin(async move { remove(live, &path).await })
            })
            .await;
            let _ = reply.send(result).await;
        }
        Op::Copy { from, to, reply } => {
            let result = guarded(dialer, session, move |live| {
                let from = from.clone();
                let to = to.clone();
                Box::pin(async move { copy(live, &from, &to).await })
            })
            .await;
            let _ = reply.send(result).await;
        }
    }
}

/// Run one operation with a deadline and one reconnect.
///
/// A failed or timed-out operation takes the child with it: leaving a dead
/// session in the slot would make every later call fail the same way, which
/// reads as "this host is down" long after it came back.
///
/// A [`Conflict`] is the exception: the destination being there is an answer the
/// caller asked for, so it is returned without dropping a healthy connection and
/// without asking the same question twice.
async fn guarded<T, F>(
    dialer: &Dialer,
    session: &async_lock::Mutex<Option<dtr_sftp::Session>>,
    mut op: F,
) -> Result<T>
where
    F: for<'a> FnMut(&'a mut dtr_sftp::Session) -> BoxFuture<'a, Result<T>>,
{
    for attempt in 0..2 {
        let mut slot = session.lock().await;
        if slot.is_none() {
            let connected = match &dialer.open {
                Some(open) => open().await,
                None => dtr_sftp::Session::connect(&dialer.alias).await,
            }
            .with_context(|| format!("connect to {}", dialer.alias))?;
            *slot = Some(connected);
        }
        let live = slot.as_mut().expect("a session was just put there");
        let result = compio::time::timeout(DEADLINE, op(live)).await;
        match result {
            Ok(Ok(value)) => return Ok(value),
            Ok(Err(err)) if attempt == 0 && !crate::is_conflict(&err) => {
                tracing::warn!(alias = %dialer.alias, %err, "sftp failed; reconnecting");
                if let Some(dead) = slot.take() {
                    dead.close();
                }
            }
            Ok(Err(err)) => return Err(err),
            Err(_) if attempt == 0 => {
                tracing::warn!(alias = %dialer.alias, "sftp timed out; reconnecting");
                if let Some(dead) = slot.take() {
                    dead.close();
                }
            }
            Err(_) => {
                if let Some(dead) = slot.take() {
                    dead.close();
                }
                anyhow::bail!(
                    "sftp to {} timed out after {}s",
                    dialer.alias,
                    DEADLINE.as_secs()
                );
            }
        }
    }
    anyhow::bail!("sftp to {} failed twice", dialer.alias)
}

/// `mkdir`, with the trait's conflict rule: a name that is taken is a
/// [`Conflict`], not a bare server status, so the UI can ask what to do.
async fn create_dir(live: &mut dtr_sftp::Session, path: &str) -> Result<()> {
    if let Err(err) = live.create_dir(path).await {
        // `MKDIR` on an existing name is a status error like any other; the stat
        // that follows is what turns it into the conflict the trait promises. A
        // path that is still absent keeps the server's own reason (a missing
        // parent, a permission).
        if live.stat(path).await.is_ok() {
            return Err(Conflict {
                path: PathBuf::from(path),
            }
            .into());
        }
        return Err(err.context(format!("mkdir {path}")));
    }
    Ok(())
}

/// Rename, never replacing: the plain `SSH_FXP_RENAME` (which OpenSSH refuses
/// when the destination exists) plus a preflight stat, so "the name is taken"
/// is a [`Conflict`] rather than an unexplained status.
async fn rename(live: &mut dtr_sftp::Session, from: &str, to: &str) -> Result<()> {
    if live.stat(to).await.is_ok() {
        return Err(Conflict {
            path: PathBuf::from(to),
        }
        .into());
    }
    match live.rename(from, to, false).await {
        Ok(()) => Ok(()),
        Err(err) => {
            // A destination that appeared between the preflight and the packet:
            // the server refused to replace it, and a conflict is the answer.
            if live.stat(to).await.is_ok() {
                return Err(Conflict {
                    path: PathBuf::from(to),
                }
                .into());
            }
            Err(err.context(format!("rename {from} to {to}")))
        }
    }
}

/// Delete for good: a file with `REMOVE`, an empty directory with `RMDIR`.
///
/// `stat` first because v3 has no "remove whatever is there" packet: `REMOVE`
/// on a directory is the server's refusal, which says nothing about the path.
/// A non-empty directory is refused rather than walked — a recursive delete is a
/// tree operation (and symlink care) that belongs to the transfer queue, not to
/// one verb on one path.
async fn remove(live: &mut dtr_sftp::Session, path: &str) -> Result<()> {
    let entry = live
        .stat(path)
        .await
        .with_context(|| format!("delete {path}: the path cannot be inspected"))?;
    if entry.dir {
        live.remove_dir(path)
            .await
            .with_context(|| format!("delete directory {path}"))
    } else {
        live.remove(path)
            .await
            .with_context(|| format!("delete {path}"))
    }
}

/// Stream one file from one remote path to another.
///
/// SFTP v3 has no server-to-server copy, so the bytes pass through dtr: one
/// `READ` and one `WRITE` packet per 64 KiB segment, and the whole copy holds
/// the alias's session (one operation per connection). That is the cost of not
/// inventing a second transport; the transfer queue (`docs/manager.md` §8) is
/// where a big copy becomes segments that interleave browsing and answer a
/// cancel.
///
/// The bytes go to `<to>.part` and are published with a non-replacing rename, so
/// a failed copy leaves no half file under the real name, and a destination that
/// appears mid-copy is refused rather than replaced.
async fn copy(live: &mut dtr_sftp::Session, from: &str, to: &str) -> Result<u64> {
    let source = live
        .stat(from)
        .await
        .with_context(|| format!("copy {from}"))?;
    if source.dir {
        anyhow::bail!(
            "copy {from}: copying a directory is not supported yet (it belongs to the transfer queue)"
        );
    }
    if live.stat(to).await.is_ok() {
        return Err(Conflict {
            path: PathBuf::from(to),
        }
        .into());
    }
    let temporary = temporary_name(to);
    let reading = live
        .begin_read(from)
        .await
        .with_context(|| format!("open {from} for reading"))?;
    let writing = match live.begin_upload(&temporary).await {
        Ok(writing) => writing,
        Err(err) => {
            let _ = live.finish_read(reading).await;
            return Err(err.context(format!("open {temporary} for writing")));
        }
    };
    let written = match stream(live, &reading, &writing).await {
        Ok(written) => written,
        Err(err) => {
            let _ = live.abort_upload(writing).await;
            let _ = live.finish_read(reading).await;
            let _ = live.remove(&temporary).await;
            return Err(err);
        }
    };
    if let Err(err) = live.finish_upload(writing).await {
        let _ = live.finish_read(reading).await;
        let _ = live.remove(&temporary).await;
        return Err(err.context(format!("finish writing {temporary}")));
    }
    if let Err(err) = live.finish_read(reading).await {
        let _ = live.remove(&temporary).await;
        return Err(err.context(format!("close {from}")));
    }
    match live.rename(&temporary, to, false).await {
        Ok(()) => Ok(written),
        Err(err) => {
            let _ = live.remove(&temporary).await;
            Err(err.context(format!("publish {to}")))
        }
    }
}

/// One `CHUNK` at a time from `reading` into `writing`, both already open. The
/// caller closes the handles, whatever happens.
async fn stream(
    live: &mut dtr_sftp::Session,
    reading: &dtr_sftp::Read,
    writing: &dtr_sftp::Upload,
) -> Result<u64> {
    let mut offset = 0u64;
    loop {
        let Some(bytes) = live
            .read_chunk(reading, offset)
            .await
            .with_context(|| format!("read at {offset}"))?
        else {
            break;
        };
        live.write_upload_chunk(writing, offset, &bytes)
            .await
            .with_context(|| format!("write at {offset}"))?;
        offset += bytes.len() as u64;
    }
    Ok(offset)
}

/// The name a copy writes before publishing: beside the destination, so the
/// publishing rename is on one filesystem and cannot fail with `EXDEV`.
fn temporary_name(to: &str) -> String {
    format!("{to}.part")
}

impl Backend for Ssh {
    fn id(&self) -> &BackendId {
        &self.id
    }

    fn caps(&self) -> Caps {
        // Inotify does not cross an ssh connection, and a directory has no
        // length. Saying so is what stops the UI polling for changes that can
        // never arrive. Writes are real — the codec has the packets and this
        // backend routes them through the alias's one session — but a remote
        // delete cannot be undone: there is no trash over `ssh`, so the UI hides
        // that verb and keeps the permanent delete's confirmation.
        Caps {
            watch: false,
            dir_size: false,
            write: true,
            trash: false,
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

    fn create_dir<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { self.conn().create_dir(as_str(path)).await })
    }

    fn rename<'a>(&'a self, from: &'a Path, to: &'a Path) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            // `rename x x` is a no-op by the trait's contract, and it must not
            // need a connection to say so.
            if from == to {
                return Ok(());
            }
            self.conn().rename(as_str(from), as_str(to)).await
        })
    }

    fn copy<'a>(&'a self, from: &'a Path, to: &'a Path) -> BoxFuture<'a, Result<u64>> {
        Box::pin(async move { self.conn().copy(as_str(from), as_str(to)).await })
    }

    fn move_to<'a>(&'a self, from: &'a Path, to: &'a Path) -> BoxFuture<'a, Result<()>> {
        // On one machine a move *is* a rename; the two verbs exist for the UI.
        self.rename(from, to)
    }

    fn remove<'a>(&'a self, path: &'a Path, how: Removal) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            match how {
                // Refused here, before a connection: the capability is a fact
                // about the machine, not something to find out by trying.
                Removal::Trash => Err(anyhow::anyhow!(
                    "{} has no trash: ask for a permanent delete of {}",
                    self.id(),
                    path.display()
                )),
                Removal::Permanent => self.conn().remove(as_str(path)).await,
            }
        })
    }
}

/// A path as the protocol takes it: UTF-8, lossily, because a pane printed it.
fn as_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// The backend's tests run the real per-alias thread over a scripted wire.
///
/// `dtr_sftp::Session::from_wire` is the seam: the `open` hook replaces
/// `Session::connect`, so dispatch, the deadline, the reconnect and the verbs'
/// decisions are exercised without an ssh server. What cannot be exercised this
/// way is the real protocol against OpenSSH (a server's status codes and its
/// `posix-rename` behaviour) and the 30 s timeout itself, which would make a
/// test wait half a minute; both are recorded as gaps in `docs/sftp.md` §8.
#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use dtr_sftp::Wire;

    use super::*;

    /// One scripted connection's answers, and what the client wrote on every
    /// connection.
    #[derive(Default)]
    struct Script {
        /// The answer streams left, one per connection attempt.
        incoming: Mutex<VecDeque<Vec<u8>>>,
        /// Every frame the client wrote, in order, across connections.
        written: Mutex<Vec<Vec<u8>>>,
        /// How many sessions were opened: the retry rule is a count.
        connections: Mutex<usize>,
    }

    /// A wire over one scripted answer stream.
    struct ScriptedWire {
        incoming: Vec<u8>,
        at: usize,
        script: Arc<Script>,
    }

    impl Wire for ScriptedWire {
        fn write_all<'a>(&'a mut self, bytes: Vec<u8>) -> BoxFuture<'a, Result<()>> {
            Box::pin(async move {
                self.script.written.lock().expect("script lock").push(bytes);
                Ok(())
            })
        }

        fn read_exact<'a>(&'a mut self, len: usize) -> BoxFuture<'a, Result<Vec<u8>>> {
            Box::pin(async move {
                let end = self
                    .at
                    .checked_add(len)
                    .context("the script ran out of answers")?;
                if end > self.incoming.len() {
                    anyhow::bail!("the script ran out of answers");
                }
                let bytes = self.incoming[self.at..end].to_vec();
                self.at = end;
                Ok(bytes)
            })
        }
    }

    fn frame(packet: &[u8]) -> Vec<u8> {
        let mut out = (packet.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(packet);
        out
    }

    fn put_string(bytes: &[u8]) -> Vec<u8> {
        let mut out = (bytes.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(bytes);
        out
    }

    fn version() -> Vec<u8> {
        let mut packet = vec![2u8];
        packet.extend_from_slice(&3u32.to_be_bytes());
        frame(&packet)
    }

    fn answer(id: u32, kind: u8, payload: &[u8]) -> Vec<u8> {
        let mut packet = vec![kind];
        packet.extend_from_slice(&id.to_be_bytes());
        packet.extend_from_slice(payload);
        frame(&packet)
    }

    fn status_payload(code: u32, message: &str) -> Vec<u8> {
        let mut out = code.to_be_bytes().to_vec();
        out.extend_from_slice(&put_string(message.as_bytes()));
        // Language tag, which v3 sends and dtr never reads.
        out.extend_from_slice(&put_string(b""));
        out
    }

    fn ok(id: u32) -> Vec<u8> {
        answer(id, 101, &status_payload(0, ""))
    }

    fn eof(id: u32) -> Vec<u8> {
        answer(id, 101, &status_payload(1, "eof"))
    }

    fn not_found(id: u32) -> Vec<u8> {
        answer(id, 101, &status_payload(2, "no such file"))
    }

    /// `SSH_FXP_ATTRS` with size and permissions, the way OpenSSH sends them.
    fn attrs(id: u32, dir: bool, size: u64) -> Vec<u8> {
        let mode: u32 = if dir { 0o040755 } else { 0o100644 };
        let mut payload = (1u32 | 4).to_be_bytes().to_vec(); // size | permissions
        payload.extend_from_slice(&size.to_be_bytes());
        payload.extend_from_slice(&mode.to_be_bytes());
        answer(id, 105, &payload)
    }

    fn handle(id: u32, name: &str) -> Vec<u8> {
        answer(id, 102, &put_string(name.as_bytes()))
    }

    fn data(id: u32, bytes: &[u8]) -> Vec<u8> {
        answer(id, 103, &put_string(bytes))
    }

    /// One connection: the handshake, then the answers in order.
    fn connection(answers: &[Vec<u8>]) -> Vec<u8> {
        let mut incoming = version();
        for reply in answers {
            incoming.extend_from_slice(reply);
        }
        incoming
    }

    /// A backend over the scripted connections, plus the script it records to.
    fn scripted(scripts: Vec<Vec<u8>>) -> (Ssh, Arc<Script>) {
        let script = Arc::new(Script {
            incoming: Mutex::new(scripts.into()),
            written: Mutex::new(Vec::new()),
            connections: Mutex::new(0),
        });
        let lines = Arc::clone(&script);
        let open: Box<Open> = Box::new(move || {
            *lines.connections.lock().expect("script lock") += 1;
            let next = lines.incoming.lock().expect("script lock").pop_front();
            let script = Arc::clone(&lines);
            Box::pin(async move {
                let Some(incoming) = next else {
                    anyhow::bail!("the test scripted no more connections");
                };
                dtr_sftp::Session::from_wire(Box::new(ScriptedWire {
                    incoming,
                    at: 0,
                    script,
                }))
                .await
            })
        });
        (Ssh::scripted("fake-host", open), script)
    }

    /// Every request the client sent, handshakes excluded, one connection after
    /// another.
    fn requests(script: &Arc<Script>) -> Vec<Vec<u8>> {
        script
            .written
            .lock()
            .expect("script lock")
            .iter()
            .map(|frame| frame[4..].to_vec())
            .filter(|packet| packet[0] != 1)
            .collect()
    }

    /// How many sessions the backend opened: the retry rule in a number.
    fn connections(script: &Arc<Script>) -> usize {
        *script.connections.lock().expect("script lock")
    }

    /// The consecutive string fields at the front of a payload.
    fn strings(mut bytes: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        while bytes.len() >= 4 {
            let len = u32::from_be_bytes(bytes[..4].try_into().expect("four bytes")) as usize;
            if 4 + len > bytes.len() {
                break;
            }
            out.push(String::from_utf8_lossy(&bytes[4..4 + len]).into_owned());
            bytes = &bytes[4 + len..];
        }
        out
    }

    fn kinds(packets: &[Vec<u8>]) -> Vec<u8> {
        packets.iter().map(|packet| packet[0]).collect()
    }

    // SSH_FXP packet types, as raw numbers so the assertions read the wire.
    const OPEN: u8 = 3;
    const CLOSE: u8 = 4;
    const READ: u8 = 5;
    const WRITE: u8 = 6;
    const REMOVE: u8 = 13;
    const MKDIR: u8 = 14;
    const RMDIR: u8 = 15;
    const STAT: u8 = 17;
    const RENAME: u8 = 18;

    #[test]
    fn the_ssh_backend_writes_and_has_no_trash() {
        assert_eq!(
            Ssh::new("jupyter").caps(),
            Caps {
                watch: false,
                dir_size: false,
                write: true,
                trash: false,
            },
            "the codec has the write packets, and ssh has no desktop trash"
        );
    }

    /// A refused trash must not even open a connection: the capability is a fact
    /// about the machine, and the answer is the permanent delete's confirmation.
    #[compio::test]
    async fn a_remote_trash_is_refused_without_asking_the_host() {
        let (ssh, script) = scripted(Vec::new());
        let err = ssh
            .remove(Path::new("/srv/notes.txt"), Removal::Trash)
            .await
            .expect_err("ssh has no trash");
        assert!(err.to_string().contains("no trash"), "{err}");
        assert!(
            err.to_string().contains("permanent delete"),
            "the refusal names the only delete there is: {err}"
        );
        assert_eq!(connections(&script), 0, "the host was never contacted");
        assert!(requests(&script).is_empty());
    }

    #[compio::test]
    async fn create_dir_sends_one_mkdir_and_reports_an_existing_name_as_a_conflict() {
        // One connection for both verbs: the session is kept, so the ids keep
        // counting across operations.
        let (ssh, script) = scripted(vec![connection(&[
            ok(1),
            answer(2, 101, &status_payload(4, "directory exists")),
            attrs(3, true, 0),
        ])]);
        ssh.create_dir(Path::new("/srv/new")).await.expect("mkdir");
        let first = requests(&script);
        assert_eq!(kinds(&first), [MKDIR]);
        assert_eq!(
            strings(&first[0][5..]).first().map(String::as_str),
            Some("/srv/new")
        );

        let err = ssh
            .create_dir(Path::new("/srv/new"))
            .await
            .expect_err("the name is taken");
        assert!(
            crate::is_conflict(&err),
            "an existing name is a conflict, not a server status: {err:#}"
        );
        let second: Vec<Vec<u8>> = requests(&script).into_iter().skip(1).collect();
        assert_eq!(
            kinds(&second),
            [MKDIR, STAT],
            "the failed mkdir is explained by one stat"
        );
        assert_eq!(
            connections(&script),
            1,
            "a conflict is an answer, not a dead session: no reconnect"
        );
    }

    #[compio::test]
    async fn rename_never_replaces_and_a_taken_name_is_a_conflict() {
        let (ssh, script) = scripted(vec![connection(&[not_found(1), ok(2), attrs(3, false, 0)])]);
        ssh.rename(Path::new("/srv/a.txt"), Path::new("/srv/b.txt"))
            .await
            .expect("rename");
        let first = requests(&script);
        assert_eq!(kinds(&first), [STAT, RENAME], "preflight, then the move");
        assert_eq!(
            strings(&first[1][5..]),
            ["/srv/a.txt", "/srv/b.txt"],
            "the plain packet carries both paths"
        );

        let err = ssh
            .rename(Path::new("/srv/a.txt"), Path::new("/srv/b.txt"))
            .await
            .expect_err("the destination is there");
        assert!(crate::is_conflict(&err), "{err:#}");
        let second: Vec<Vec<u8>> = requests(&script).into_iter().skip(2).collect();
        assert_eq!(kinds(&second), [STAT], "nothing is renamed on a conflict");
        assert_eq!(connections(&script), 1, "no reconnect for a conflict");
    }

    /// A move on one machine is a rename: the same packet, and the same rule.
    #[compio::test]
    async fn move_to_another_directory_is_the_rename_packet() {
        let (ssh, script) = scripted(vec![connection(&[not_found(1), ok(2)])]);
        ssh.move_to(Path::new("/srv/a.txt"), Path::new("/srv/sub/a.txt"))
            .await
            .expect("move");
        let packets = requests(&script);
        assert_eq!(kinds(&packets), [STAT, RENAME]);
        assert_eq!(strings(&packets[1][5..]), ["/srv/a.txt", "/srv/sub/a.txt"]);
    }

    /// The trait's `rename x x` no-op must not need a host: no script, no
    /// connection, and no packet.
    #[compio::test]
    async fn renaming_a_path_to_itself_is_a_no_op() {
        let (ssh, script) = scripted(Vec::new());
        ssh.rename(Path::new("/srv/a.txt"), Path::new("/srv/a.txt"))
            .await
            .expect("a no-op");
        assert_eq!(connections(&script), 0);
    }

    #[compio::test]
    async fn remove_deletes_a_file_with_remove_and_a_directory_with_rmdir() {
        let (ssh, script) = scripted(vec![connection(&[
            attrs(1, false, 5),
            ok(2),
            attrs(3, true, 0),
            ok(4),
        ])]);
        ssh.remove(Path::new("/srv/a.txt"), Removal::Permanent)
            .await
            .expect("delete a file");
        let first = requests(&script);
        assert_eq!(kinds(&first), [STAT, REMOVE]);

        ssh.remove(Path::new("/srv/sub"), Removal::Permanent)
            .await
            .expect("delete a directory");
        let second: Vec<Vec<u8>> = requests(&script).into_iter().skip(2).collect();
        assert_eq!(
            kinds(&second),
            [STAT, RMDIR],
            "a directory is RMDIR, not a refused REMOVE"
        );
        assert_eq!(connections(&script), 1, "both verbs share the one session");
    }

    /// The copy decision in one test: stat both ends, read here, write there,
    /// publish with a non-replacing rename.
    #[compio::test]
    async fn copy_streams_through_dtr_and_publishes_with_a_rename() {
        let (ssh, script) = scripted(vec![connection(&[
            attrs(1, false, 5),    // stat(from)
            not_found(2),          // stat(to)
            handle(3, "src"),      // begin_read
            handle(4, "dst.part"), // begin_upload
            data(5, b"hello"),     // READ at 0
            ok(6),                 // WRITE at 0
            eof(7),                // READ at 5: the end
            ok(8),                 // finish_upload (CLOSE)
            ok(9),                 // finish_read (CLOSE)
            ok(10),                // rename(part, to)
        ])]);
        let written = ssh
            .copy(Path::new("/srv/a.txt"), Path::new("/srv/b.txt"))
            .await
            .expect("copy");
        assert_eq!(written, 5, "the byte count is what was streamed");

        let packets = requests(&script);
        assert_eq!(
            kinds(&packets),
            [
                STAT, STAT, OPEN, OPEN, READ, WRITE, READ, CLOSE, CLOSE, RENAME
            ],
            "both ends are statted, the bytes stream, then the publish"
        );
        assert_eq!(
            strings(&packets[9][5..]),
            ["/srv/b.txt.part", "/srv/b.txt"],
            "the copy lands under a .part name and is published by rename"
        );
        assert_eq!(connections(&script), 1, "a copy is one operation");
    }

    #[compio::test]
    async fn a_copy_onto_an_existing_name_is_a_conflict_and_writes_nothing() {
        let (ssh, script) = scripted(vec![connection(&[attrs(1, false, 5), attrs(2, false, 7)])]);
        let err = ssh
            .copy(Path::new("/srv/a.txt"), Path::new("/srv/b.txt"))
            .await
            .expect_err("the destination is there");
        assert_eq!(connections(&script), 1);
        assert!(crate::is_conflict(&err), "{err:#}");
        let packets = requests(&script);
        assert_eq!(
            kinds(&packets),
            [STAT, STAT],
            "nothing is opened or written on a conflict"
        );
    }

    /// A tree copy is the transfer queue's job, and a copy must say so rather
    /// than half-copying a directory.
    #[compio::test]
    async fn a_copy_of_a_directory_is_refused_with_a_reason() {
        // A refusal is not a conflict, so it takes the shared retry rule too:
        // the session is replaced and the verb is asked once more on a fresh one.
        let (ssh, script) = scripted(vec![
            connection(&[attrs(1, true, 0)]),
            connection(&[attrs(1, true, 0)]),
        ]);
        let err = ssh
            .copy(Path::new("/srv/tree"), Path::new("/srv/copy"))
            .await
            .expect_err("a directory is not a file");
        assert!(err.to_string().contains("transfer queue"), "{err:#}");
        assert_eq!(
            kinds(&requests(&script)),
            [STAT, STAT],
            "one stat per attempt, and nothing opened"
        );
        assert_eq!(connections(&script), 2, "the shared retry rule, once");
    }

    /// One reconnect is the rule: a protocol failure drops the session and the
    /// verb is asked once more on a fresh one.
    #[compio::test]
    async fn a_failed_operation_reconnects_once_and_the_verb_succeeds() {
        let (ssh, script) = scripted(vec![
            // First connection: the preflight says the name is free, the rename
            // is refused, and the explain-away stat still says it is free.
            connection(&[
                not_found(1),
                answer(2, 101, &status_payload(4, "connection lost")),
                not_found(3),
            ]),
            // Second connection: the same verb, now answered.
            connection(&[not_found(1), ok(2)]),
        ]);
        ssh.rename(Path::new("/srv/a.txt"), Path::new("/srv/b.txt"))
            .await
            .expect("the second attempt succeeds");
        assert_eq!(connections(&script), 2, "exactly one reconnect");
    }

    #[compio::test]
    async fn a_path_is_utf8_lossily_and_the_packet_carries_it() {
        // The pane printed the name, so it may not be UTF-8: the protocol takes
        // a string, and the conversion is lossy rather than a refusal.
        let (ssh, script) = scripted(vec![connection(&[not_found(1), ok(2)])]);
        ssh.rename(Path::new("/srv/a.txt"), Path::new("/srv/sub dir/ü.txt"))
            .await
            .expect("rename");
        let packets = requests(&script);
        assert_eq!(
            strings(&packets[1][5..]),
            ["/srv/a.txt", "/srv/sub dir/ü.txt"]
        );
    }
}
