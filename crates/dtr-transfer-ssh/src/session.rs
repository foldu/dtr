//! One session per alias, one packet at a time.
//!
//! The queue's driver already runs inside a compio runtime on one thread
//! (`docs/compio.md` §10.3), and compio's handles are thread-local, so this
//! module starts no thread and owns no mailbox: it hands the driver futures
//! that borrow a plain [`RefCell`] slot. That is safe for the same reason the
//! queue's scheduler exists — a job holds every machine it touches, so two ops
//! never use one alias at once (`crates/dtr-transfer`'s per-machine
//! serialisation) — and it keeps each session on the thread that created it.
//!
//! Two rules differ between a whole-session verb and a segment:
//!
//! - **A verb gets [`DEADLINE`] and one reconnect.** A `stat`, a rename, a
//!   delete: a failed session is not a session, and leaving a dead child in the
//!   slot makes every later call fail the same way — which reads as "this host
//!   is down" long after it came back (`crates/dtr-vfs/src/ssh.rs`'s rule).
//! - **A segment is not retried.** A reconnect loses the open `SSH_FXP_OPEN`
//!   handle, so a failed segment fails the file; the queue's retry reopens the
//!   part from byte zero, which is the queue's contract, not this module's.
//!
//! The slot is only borrowed for the one packet: between steps the session is
//! idle and a `stat` for the conflict probe, a `discard`, or another job's
//! machine can use it. That is the ownership measurement 5 of
//! `docs/manager.md` §8 asks for, and it is why an op holds a [`Slot`] and a
//! handle rather than a `Session`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use dtr_sftp::{CHUNK, Session};
use dtr_transfer::BoxFuture;

use crate::Dialer;

/// How long one request may take before the connection is considered dead. A
/// hung `read` on a link that went away is otherwise forever, and the SFTP
/// codec's own per-answer bound is the same 30 s (`dtr_sftp::ANSWER` is private,
/// so the figure is restated here, as `dtr-vfs` restates it).
pub(crate) const DEADLINE: Duration = Duration::from_secs(30);

/// One alias's session, empty after a failure or an [`abort`](crate::SshTransport).
pub(crate) type Slot = Rc<RefCell<Option<Session>>>;

/// The alias sessions a transport keeps, and the way it opens one.
pub(crate) struct Sessions {
    slots: HashMap<String, Slot>,
    /// `None` is the host's own `ssh`; `Some` is the test's wire. The seam is
    /// the same one `crates/dtr-vfs/src/ssh.rs` uses, and it is why dispatch,
    /// the deadline and the reconnect can be tested without a server.
    dialer: Option<Dialer>,
}

impl Sessions {
    pub(crate) fn new(dialer: Option<Dialer>) -> Self {
        Self {
            slots: HashMap::new(),
            dialer,
        }
    }

    fn slot(&mut self, alias: &str) -> Slot {
        self.slots
            .entry(alias.to_owned())
            .or_insert_with(|| Rc::new(RefCell::new(None)))
            .clone()
    }

    /// The alias's session, opened if this is the first request since the last
    /// failure.
    ///
    /// A connect has the same deadline as a request: an `ssh` that never
    /// answers is a host that is down, not a driver that waits for ever.
    pub(crate) async fn connect(&mut self, alias: &str) -> Result<Slot> {
        let slot = self.slot(alias);
        if slot.borrow().is_some() {
            return Ok(slot);
        }
        let session = match compio::time::timeout(DEADLINE, self.dial(alias)).await {
            Ok(connected) => connected.with_context(|| format!("connect to {alias}"))?,
            Err(_) => bail!(
                "sftp to {alias} did not connect within {}s",
                DEADLINE.as_secs()
            ),
        };
        *slot.borrow_mut() = Some(session);
        Ok(slot)
    }

    async fn dial(&self, alias: &str) -> Result<Session> {
        match &self.dialer {
            Some(dialer) => dialer(alias).await,
            None => Session::connect(alias).await,
        }
    }

    /// One whole-session verb, with the deadline and one reconnect on a fresh
    /// child.
    pub(crate) async fn verb<T, F>(&mut self, alias: &str, doing: &str, mut f: F) -> Result<T>
    where
        F: for<'a> FnMut(&'a mut Session) -> BoxFuture<'a, Result<T>>,
    {
        for attempt in 0..2 {
            let slot = self.connect(alias).await?;
            let outcome = {
                let mut borrowed = slot.borrow_mut();
                let session = borrowed.as_mut().context("the session was not stored")?;
                match compio::time::timeout(DEADLINE, f(session)).await {
                    Ok(result) => result,
                    Err(_) => Err(anyhow::anyhow!("no answer in {}s", DEADLINE.as_secs())),
                }
            };
            match outcome {
                Ok(value) => return Ok(value),
                Err(error) if attempt == 0 => {
                    tracing::warn!(
                        alias = %alias,
                        %error,
                        operation = %doing,
                        "sftp failed; reconnecting"
                    );
                    burn(&slot);
                }
                Err(error) => {
                    burn(&slot);
                    return Err(error).with_context(|| format!("{doing} on {alias}"));
                }
            }
        }
        unreachable!("two attempts either return or reconnect once")
    }
}

/// Release a session without awaiting.
///
/// Killing the child closes every handle the server had for it, and no
/// destructor can await a `CLOSE`. [`Transport::open`](dtr_transfer::Transport::open)'s
/// handles are the only thing an op owns, and `Op::abort` is synchronous by the
/// trait's contract, so this is the honest release: the next verb — the queue's
/// `discard`, or a retry's `open` — reconnects (`docs/transfers.md` §9).
pub(crate) fn burn(slot: &Slot) {
    if let Some(session) = slot.borrow_mut().take() {
        session.close();
    }
}

/// One packet, with the deadline. A failure releases the session, because a
/// protocol error and a dead pipe are not distinguishable from this side.
pub(crate) async fn call<T, F>(slot: &Slot, doing: &str, f: F) -> Result<T>
where
    F: for<'a> FnOnce(&'a mut Session) -> BoxFuture<'a, Result<T>>,
{
    let outcome = {
        let mut borrowed = slot.borrow_mut();
        match borrowed.as_mut() {
            Some(session) => match compio::time::timeout(DEADLINE, f(session)).await {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!("no answer in {}s", DEADLINE.as_secs())),
            },
            None => Err(anyhow::anyhow!("the session for this alias is closed")),
        }
    };
    if outcome.is_err() {
        burn(slot);
    }
    outcome.map_err(|error| error.context(doing.to_owned()))
}

/// `SSH_FXP_OPEN` for reading: the segmented read's first packet.
pub(crate) async fn begin_read(slot: &Slot, path: &str) -> Result<dtr_sftp::Read> {
    let path = path.to_owned();
    call(slot, &format!("open {path} for reading"), move |session| {
        let path = path.clone();
        Box::pin(async move { session.begin_read(&path).await })
    })
    .await
}

/// `SSH_FXP_OPEN` for writing: the segmented upload's first packet.
pub(crate) async fn begin_upload(slot: &Slot, path: &str) -> Result<dtr_sftp::Upload> {
    let path = path.to_owned();
    call(slot, &format!("open {path} for writing"), move |session| {
        let path = path.clone();
        Box::pin(async move { session.begin_upload(&path).await })
    })
    .await
}

/// `SSH_FXP_CLOSE` on a read handle.
pub(crate) async fn finish_read(slot: &Slot, read: dtr_sftp::Read) -> Result<()> {
    call(slot, "close the read", move |session| {
        Box::pin(async move { session.finish_read(read).await })
    })
    .await
}

/// `SSH_FXP_CLOSE` on an upload handle.
pub(crate) async fn finish_upload(slot: &Slot, upload: dtr_sftp::Upload) -> Result<()> {
    call(slot, "close the upload", move |session| {
        Box::pin(async move { session.finish_upload(upload).await })
    })
    .await
}

/// An open remote file being read, at most [`CHUNK`] bytes per call.
///
/// The handle is the codec's, the slot is the alias's, and neither is a
/// session: a step borrows the slot for one packet and gives it back, which is
/// what lets the queue's next `stat` or another job's segment have the alias
/// between two segments.
pub(crate) struct RemoteRead {
    slot: Slot,
    handle: Option<dtr_sftp::Read>,
    offset: u64,
}

impl RemoteRead {
    pub(crate) fn new(slot: Slot, handle: dtr_sftp::Read) -> Self {
        Self {
            slot,
            handle: Some(handle),
            offset: 0,
        }
    }

    /// The next at most [`CHUNK`] bytes, or `None` at end of file.
    ///
    /// `None` closes the handle: a finished read leaves no server-side handle
    /// behind, so the alias's session is clean for the publish or the next job.
    pub(crate) async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>> {
        let Some(handle) = self.handle.as_ref() else {
            return Ok(None);
        };
        let offset = self.offset;
        let outcome = {
            let mut borrowed = self.slot.borrow_mut();
            match borrowed.as_mut() {
                Some(session) => {
                    match compio::time::timeout(DEADLINE, session.read_chunk(handle, offset)).await
                    {
                        Ok(result) => result,
                        Err(_) => Err(anyhow::anyhow!("no answer in {}s", DEADLINE.as_secs())),
                    }
                }
                None => Err(anyhow::anyhow!("the session for this alias is closed")),
            }
        };
        let bytes = match outcome {
            Ok(bytes) => bytes,
            Err(error) => {
                burn(&self.slot);
                return Err(error).with_context(|| format!("read at {offset}"));
            }
        };
        match bytes {
            Some(bytes) => {
                self.offset += bytes.len() as u64;
                Ok(Some(bytes))
            }
            None => {
                // End of file. A close that fails after every byte arrived does
                // not make the copy wrong, so it is not an error — the codec's
                // own `download` ignores it the same way.
                let _ = self.close().await;
                Ok(None)
            }
        }
    }

    /// Close the handle early: a cancel or a failure that still has a session.
    pub(crate) async fn close(&mut self) -> Result<()> {
        match self.handle.take() {
            Some(handle) => finish_read(&self.slot, handle).await,
            None => Ok(()),
        }
    }
}

impl Drop for RemoteRead {
    fn drop(&mut self) {
        // An op that failed or was cancelled never gets to await a CLOSE, and a
        // killed child closes the server's handles. Leaving the session in place
        // with a handle the server still owns would spend it on the next verb.
        if self.handle.is_some() {
            burn(&self.slot);
        }
    }
}

/// One open remote part being written, at most [`CHUNK`] bytes per packet.
pub(crate) struct RemoteWrite {
    slot: Slot,
    handle: Option<dtr_sftp::Upload>,
    offset: u64,
}

impl RemoteWrite {
    pub(crate) fn new(slot: Slot, handle: dtr_sftp::Upload) -> Self {
        Self {
            slot,
            handle: Some(handle),
            offset: 0,
        }
    }

    /// Append `data`. One segment may be larger than the codec's packet bound
    /// when the queue was built with a bigger buffer, so this splits into
    /// [`CHUNK`]-sized packets — each bounded, each a turn for another caller.
    pub(crate) async fn write(&mut self, data: &[u8]) -> Result<()> {
        for piece in data.chunks(CHUNK) {
            let offset = self.offset;
            let Some(handle) = self.handle.as_ref() else {
                bail!("writing to a part that was already closed");
            };
            let outcome = {
                let mut borrowed = self.slot.borrow_mut();
                match borrowed.as_mut() {
                    Some(session) => {
                        match compio::time::timeout(
                            DEADLINE,
                            session.write_upload_chunk(handle, offset, piece),
                        )
                        .await
                        {
                            Ok(result) => result,
                            Err(_) => Err(anyhow::anyhow!("no answer in {}s", DEADLINE.as_secs())),
                        }
                    }
                    None => Err(anyhow::anyhow!("the session for this alias is closed")),
                }
            };
            if let Err(error) = outcome {
                burn(&self.slot);
                return Err(error).with_context(|| format!("write at {offset}"));
            }
            self.offset += piece.len() as u64;
        }
        Ok(())
    }

    /// `SSH_FXP_CLOSE`: the server considers the part complete.
    pub(crate) async fn close(&mut self) -> Result<()> {
        match self.handle.take() {
            Some(handle) => finish_upload(&self.slot, handle).await,
            None => Ok(()),
        }
    }

    /// Release the part without a `CLOSE`. Synchronous, because
    /// [`Writer::abort`](dtr_transfer::Writer::abort) is.
    pub(crate) fn abort(&mut self) {
        self.handle = None;
        burn(&self.slot);
    }
}
