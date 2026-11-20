//! The ssh half of the transfer queue: one [`Transport`] for every job shape.
//!
//! `docs/manager.md` §8 decides that a remote copy is a queue of bounded
//! segments, not a mount; `crates/dtr-transfer` is that queue's core, and its
//! [`Transport`] is the seam an app fills in. This crate is the F5 adapter that
//! fills it: local↔local work is delegated to
//! [`LocalTransport`](dtr_transfer::LocalTransport), anything touching an `ssh`
//! alias goes over `dtr-sftp`'s segmented primitives, and **one adapter serves
//! every job shape**, so the wiring holds one `Transport` beside one `Queue`.
//!
//! # Routing
//!
//! The two [`Location`]s decide; [`OpenRequest::direction`]
//! (`dtr_transfer::OpenRequest`) is the card's word, not a routing hint.
//!
//! | source | part | operation |
//! | --- | --- | --- |
//! | local | local | [`LocalTransport`](dtr_transfer::LocalTransport)'s op |
//! | local | an alias | segmented upload: a `File`, then one `WRITE` per segment |
//! | an alias | local | segmented download: one `READ` per segment, a `File` |
//! | one alias | another (or the same) | `StreamOp` over a segmented read and a segmented write |
//!
//! # One session per alias, released between steps
//!
//! The queue's driver already runs on a compio runtime on one thread
//! (`docs/compio.md` §10.3), and compio's handles are thread-local, so this
//! crate **starts no thread**: [`SshTransport`] is created on the thread that
//! will poll its futures, and its ops share a per-alias
//! `Rc<RefCell<Option<Session>>>` slot. A segment borrows the slot for exactly
//! the packets it needs — one per machine — and returns it, which is what lets a
//! listing or another job's machine use the alias between segments, and what
//! makes a cancel land between
//! two of them (`docs/manager.md` §8, `docs/transfers.md` §3.1). The queue's
//! per-machine serialisation is what makes the plain `RefCell` safe: a job holds
//! every machine it touches, so two ops never use one alias at once.
//!
//! Every request has a 30 s deadline. A whole-session verb (`stat`, `publish`,
//! `discard`, `remove`, `preflight`) is retried once on a fresh child, because a
//! session that failed is not a session. A segment is **not** retried: a
//! reconnect loses the open handle, so a failed segment fails the file and the
//! queue's retry restarts it from byte zero.
//!
//! # What the adapter owns, and what it must not
//!
//! The `.part` name, the publish ordering, the conflict questions, progress,
//! cancellation and the per-machine scheduling are the queue's
//! (`docs/transfers.md` §9); this crate supplies only the I/O. In particular
//! [`Op::abort`](dtr_transfer::Op::abort) is synchronous: it releases the alias
//! by killing its child, because no destructor can await a `CLOSE`, and the
//! queue's following `discard` removes the part. It never publishes.
//!
//! [`Location`]: dtr_vfs::Location

use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use dtr_sftp::{STATVFS, Session};
use dtr_transfer::{BoxFuture, LocalTransport, Op, OpenRequest, StreamOp, Transport};
use dtr_vfs::{BackendId, Kind, Location, Meta};

mod ops;
mod session;

use ops::{LocalReader, LocalWriter, RemoteReader, RemoteWriter};
use session::{RemoteRead, RemoteWrite, Sessions, burn};

/// How an alias's session is opened. Production is
/// [`dtr_sftp::Session::connect`]; a test passes a scripted wire, because a
/// scripted server is the only way to exercise this without an `ssh` host
/// (`docs/sftp.md` §8 records the same gap for the backend).
pub type Dialer = Rc<dyn Fn(&str) -> BoxFuture<'static, Result<Session>>>;

/// The transfer queue's remote transport: every job shape through one adapter.
///
/// Create it on the worker thread that owns the runtime and the [`Queue`], and
/// keep one for the queue's life: the sessions in it are thread-local children
/// of the process (`docs/compio.md` §10.3, trap 4).
///
/// [`Queue`]: dtr_transfer::Queue
pub struct SshTransport {
    /// Local pairs are the queue's own reference implementation; this adapter
    /// composes it rather than growing a second local copy
    /// (`docs/transfers.md` §4).
    local: Box<dyn Transport>,
    sessions: Sessions,
}

impl SshTransport {
    /// A transport over the host's own `ssh` and the real local filesystem.
    pub fn new() -> Self {
        Self::with_parts(Box::new(LocalTransport::new()), None)
    }

    /// The test seam: any local transport, any way to open a session.
    ///
    /// Not part of the wiring's contract — the app calls [`SshTransport::new`]
    /// — but it is public because the scripted-wire tests are an integration
    /// test, and a fake local transport is how "local↔local never leaves this
    /// adapter" is asserted without real files.
    #[doc(hidden)]
    pub fn with_parts(local: Box<dyn Transport>, dialer: Option<Dialer>) -> Self {
        Self {
            local,
            sessions: Sessions::new(dialer),
        }
    }

    /// Compose an upload: a local `File` as the reader and the destination
    /// alias's segmented upload as the writer.
    async fn open_upload(&mut self, request: OpenRequest, alias: &str) -> Result<Box<dyn Op>> {
        let slot = self.sessions.connect(alias).await?;
        let part = as_str(&request.part.path);
        let upload = match session::begin_upload(&slot, &part).await {
            Ok(upload) => upload,
            Err(error) => return Err(error.context(format!("create {}", request.part.describe()))),
        };
        let file = match std::fs::File::open(&request.source.path) {
            Ok(file) => file,
            Err(error) => {
                // The part exists on the server now; release the handle rather
                // than leaving an upload open for the next verb to trip over.
                burn(&slot);
                return Err(error).with_context(|| format!("open {}", request.source.describe()));
            }
        };
        Ok(Box::new(StreamOp::new(
            Box::new(LocalReader::new(file)),
            Box::new(RemoteWriter::new(RemoteWrite::new(slot, upload))),
        )))
    }

    /// Compose a download: the source alias's segmented read and a local
    /// `File` for the part.
    async fn open_download(&mut self, request: OpenRequest, alias: &str) -> Result<Box<dyn Op>> {
        let slot = self.sessions.connect(alias).await?;
        let source = as_str(&request.source.path);
        let read = match session::begin_read(&slot, &source).await {
            Ok(read) => read,
            Err(error) => return Err(error.context(format!("open {}", request.source.describe()))),
        };
        let file = match std::fs::File::create(&request.part.path) {
            Ok(file) => file,
            Err(error) => {
                burn(&slot);
                return Err(error).with_context(|| format!("create {}", request.part.describe()));
            }
        };
        Ok(Box::new(StreamOp::new(
            Box::new(RemoteReader::new(RemoteRead::new(slot, read))),
            Box::new(LocalWriter::new(file)),
        )))
    }

    /// Compose a stream between two aliases (which may be the same one: a
    /// same-host copy is two handles on one session).
    async fn open_between(
        &mut self,
        request: OpenRequest,
        from: &str,
        to: &str,
    ) -> Result<Box<dyn Op>> {
        let source_slot = self.sessions.connect(from).await?;
        let source = as_str(&request.source.path);
        let read = match session::begin_read(&source_slot, &source).await {
            Ok(read) => read,
            Err(error) => {
                return Err(error.context(format!("open {}", request.source.describe())));
            }
        };
        let destination_slot = self.sessions.connect(to).await?;
        let part = as_str(&request.part.path);
        let upload = match session::begin_upload(&destination_slot, &part).await {
            Ok(upload) => upload,
            Err(error) => {
                // Close the read rather than leave it open on a session that is
                // otherwise healthy.
                let _ = session::finish_read(&source_slot, read).await;
                return Err(error.context(format!("create {}", request.part.describe())));
            }
        };
        Ok(Box::new(StreamOp::new(
            Box::new(RemoteReader::new(RemoteRead::new(source_slot, read))),
            Box::new(RemoteWriter::new(RemoteWrite::new(
                destination_slot,
                upload,
            ))),
        )))
    }
}

impl Default for SshTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for SshTransport {
    fn stat<'a>(&'a mut self, at: &'a Location) -> BoxFuture<'a, Result<Option<Meta>>> {
        Box::pin(async move {
            match &at.backend {
                BackendId::Local => self.local.stat(at).await,
                BackendId::Ssh(alias) => {
                    let path = as_str(&at.path);
                    let entry = self
                        .sessions
                        .verb(alias, &format!("stat {}", at.describe()), |session| {
                            let path = path.clone();
                            Box::pin(async move {
                                match session.stat(&path).await {
                                    Ok(entry) => Ok(Some(entry)),
                                    // "Not there" is an answer, not a failure: the
                                    // queue asks "does the destination exist?"
                                    // with this call and a reconnect must not be
                                    // spent on the normal answer.
                                    Err(error) if is_not_found(&error) => Ok(None),
                                    Err(error) => Err(error),
                                }
                            })
                        })
                        .await?;
                    Ok(entry.map(meta_of))
                }
            }
        })
    }

    fn open<'a>(&'a mut self, request: OpenRequest) -> BoxFuture<'a, Result<Box<dyn Op>>> {
        Box::pin(async move {
            let source = request.source.backend.clone();
            let part = request.part.backend.clone();
            match (source, part) {
                (BackendId::Local, BackendId::Local) => self.local.open(request).await,
                (BackendId::Local, BackendId::Ssh(alias)) => {
                    self.open_upload(request, &alias).await
                }
                (BackendId::Ssh(alias), BackendId::Local) => {
                    self.open_download(request, &alias).await
                }
                (BackendId::Ssh(from), BackendId::Ssh(to)) => {
                    self.open_between(request, &from, &to).await
                }
            }
        })
    }

    fn publish<'a>(
        &'a mut self,
        part: &'a Location,
        destination: &'a Location,
        overwrite: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if part.backend != destination.backend {
                bail!(
                    "the part {} and the name {} are on different machines",
                    part.describe(),
                    destination.describe()
                );
            }
            match &part.backend {
                BackendId::Local => self.local.publish(part, destination, overwrite).await,
                BackendId::Ssh(alias) => {
                    // The codec chooses the packet: `posix-rename@openssh.com`
                    // (the atomic replace) only when the user answered "replace",
                    // and plain `SSH_FXP_RENAME` otherwise, which fails if the
                    // name appeared while the bytes were moving
                    // (`docs/sftp.md` §7).
                    let from = as_str(&part.path);
                    let to = as_str(&destination.path);
                    self.sessions
                        .verb(
                            alias,
                            &format!("rename {} to {}", part.describe(), destination.describe()),
                            |session| {
                                let from = from.clone();
                                let to = to.clone();
                                Box::pin(async move { session.rename(&from, &to, overwrite).await })
                            },
                        )
                        .await
                }
            }
        })
    }

    fn discard<'a>(&'a mut self, part: &'a Location) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            match &part.backend {
                BackendId::Local => self.local.discard(part).await,
                BackendId::Ssh(alias) => {
                    let path = as_str(&part.path);
                    self.sessions
                        .verb(alias, &format!("remove {}", part.describe()), |session| {
                            let path = path.clone();
                            Box::pin(async move {
                                match session.remove(&path).await {
                                    Ok(()) => Ok(()),
                                    // The part is already gone — a cancel after a
                                    // failure that left none, or a retry's cleanup
                                    // — and a discard is best effort
                                    // (`docs/transfers.md` §5).
                                    Err(error) if is_not_found(&error) => Ok(()),
                                    Err(error) => Err(error),
                                }
                            })
                        })
                        .await
                }
            }
        })
    }

    fn remove<'a>(&'a mut self, at: &'a Location) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            match &at.backend {
                BackendId::Local => self.local.remove(at).await,
                BackendId::Ssh(alias) => {
                    let path = as_str(&at.path);
                    // v3 has no "remove whatever is there": `REMOVE` on a
                    // directory is the server's refusal, so the stat decides the
                    // packet (the same rule as `crates/dtr-vfs/src/ssh.rs`).
                    self.sessions
                        .verb(alias, &format!("remove {}", at.describe()), |session| {
                            let path = path.clone();
                            Box::pin(async move {
                                let entry = session.stat(&path).await?;
                                if entry.dir {
                                    session.remove_dir(&path).await
                                } else {
                                    session.remove(&path).await
                                }
                            })
                        })
                        .await
                }
            }
        })
    }

    fn preflight<'a>(
        &'a mut self,
        destination: &'a Location,
        bytes: u64,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            match &destination.backend {
                // The local transport's hook is the trait's default: this machine
                // cannot tell in advance either, and a failed `write` is the
                // honest failure.
                BackendId::Local => self.local.preflight(destination, bytes).await,
                BackendId::Ssh(alias) => {
                    let path = as_str(&destination.path);
                    let available = self
                        .sessions
                        .verb(
                            alias,
                            &format!("statvfs {}", destination.describe()),
                            |session| {
                                let path = path.clone();
                                Box::pin(async move {
                                    // A server without the extension cannot be asked,
                                    // and a made-up number is worse than none: the
                                    // transfer proceeds and lets `WRITE` fail
                                    // (`docs/sftp.md` §7).
                                    if !session.supports(STATVFS) {
                                        return Ok(None);
                                    }
                                    session.statvfs(&path).await.map(Some)
                                })
                            },
                        )
                        .await?;
                    let Some(available) = available else {
                        return Ok(());
                    };
                    if available.available_bytes < bytes {
                        bail!(
                            "not enough room on {}: {} bytes available, {} needed",
                            destination.describe(),
                            available.available_bytes,
                            bytes
                        );
                    }
                    Ok(())
                }
            }
        })
    }
}

/// Whether a codec error is the server's `SSH_FX_NO_SUCH_FILE`.
///
/// `dtr-sftp` decodes the status code and then formats it into the error's
/// message, so the code itself is not reachable from here; the server's own
/// `status_to_message` text is, and OpenSSH says "No such file". Matching the
/// message is the adapter's one concession to that gap — see
/// `docs/transfers.md`'s account; a typed `Session::stat_opt` would remove it.
fn is_not_found(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        let text = cause.to_string().to_ascii_lowercase();
        text.contains("no such file") || text.contains("not found") || text.trim() == "status 2"
    })
}

/// A protocol path: UTF-8, lossily, because the file manager printed it (the
/// same conversion `crates/dtr-vfs/src/ssh.rs` uses).
fn as_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// The queue's `Meta` from the codec's `Entry`.
fn meta_of(entry: dtr_sftp::Entry) -> Meta {
    Meta {
        kind: if entry.dir { Kind::Dir } else { Kind::File },
        len: entry.size,
        modified: entry
            .modified
            .and_then(|seconds| UNIX_EPOCH.checked_add(Duration::from_secs(seconds))),
    }
}
