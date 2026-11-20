//! SFTP over the host's own `ssh`.
//!
//! dtr contains no SSH implementation: the alias is handed to `ssh -s <alias>
//! sftp` unresolved, so `ProxyJump`, `Include`, `Match`, agent forwarding and
//! `known_hosts` are whatever the user's ssh already does. What is left is a
//! byte pipe — the child's stdin and stdout — and the SFTP version 3 framing on
//! top of it. That framing is the dozen packets below rather than a protocol
//! crate: it was `russh-sftp`, which pulled `tokio` and `tokio-util` into a
//! runtime that no longer has one (`docs/compio.md`). The operations dtr needs
//! are `list`, `stat`, `download`, `home` and the write verbs — `mkdir`,
//! `rmdir`, `remove`, `rename`, `setstat` and a chunked `upload` built from
//! `OPEN`/`WRITE`/`CLOSE` — and v3 has one packet for each. `rename` prefers
//! `posix-rename@openssh.com` and `statvfs` needs `statvfs@openssh.com`; both
//! are advertised in the server's `SSH_FXP_VERSION` and remembered here.
//!
//! The wire is a trait so the tests can drive the same code over a channel
//! instead of a real server (`tests/mock.rs`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use compio::io::{
    AsyncRead as _, AsyncReadAtExt as _, AsyncReadExt as _, AsyncWriteAtExt as _,
    AsyncWriteExt as _,
};

/// The version every OpenSSH server speaks, and the one this codec is written
/// against.
const VERSION: u32 = 3;

// Packet types. The names are the protocol's own (`SSH_FXP_…`), abbreviated.
const INIT: u8 = 1;
const VERSION_PACKET: u8 = 2;
const OPEN: u8 = 3;
const CLOSE: u8 = 4;
const READ: u8 = 5;
const WRITE: u8 = 6;
const SETSTAT: u8 = 9;
const OPENDIR: u8 = 11;
const READDIR: u8 = 12;
const REMOVE: u8 = 13;
const MKDIR: u8 = 14;
const RMDIR: u8 = 15;
const REALPATH: u8 = 16;
const STAT: u8 = 17;
const RENAME: u8 = 18;
const EXTENDED: u8 = 200;
const STATUS: u8 = 101;
const HANDLE: u8 = 102;
const DATA: u8 = 103;
const NAME: u8 = 104;
const ATTRS: u8 = 105;
const EXTENDED_REPLY: u8 = 201;

/// `SSH_FX_OK`: a status that is an answer, not a failure.
const FX_OK: u32 = 0;
/// `SSH_FX_EOF`: a read or a listing is finished.
const FX_EOF: u32 = 1;

/// `SSH_FXF_READ`.
const FXF_READ: u32 = 0x0000_0001;
/// `SSH_FXF_WRITE`.
const FXF_WRITE: u32 = 0x0000_0002;
/// `SSH_FXF_CREAT`.
const FXF_CREAT: u32 = 0x0000_0008;
/// `SSH_FXF_TRUNC`.
const FXF_TRUNC: u32 = 0x0000_0010;

/// The mode a directory created by [`Session::create_dir`] gets.
const DIRECTORY_MODE: u32 = 0o755;

/// The extension that makes a rename an atomic replace, instead of a rename
/// that fails when the destination exists.
pub const POSIX_RENAME: &str = "posix-rename@openssh.com";
/// The extension that answers how much room a filesystem has left.
pub const STATVFS: &str = "statvfs@openssh.com";

/// Attribute flags.
const ATTR_SIZE: u32 = 0x0000_0001;
const ATTR_UIDGID: u32 = 0x0000_0002;
const ATTR_PERMISSIONS: u32 = 0x0000_0004;
const ATTR_ACMODTIME: u32 = 0x0000_0008;
const ATTR_EXTENDED: u32 = 0x8000_0000;

/// How long an answer may take before the connection is considered dead. A hung
/// `read` on a link that went away is otherwise forever.
const ANSWER: Duration = Duration::from_secs(30);

/// The most bytes one `READ` or `WRITE` packet carries. A read answer is
/// bounded by [`ANSWER`] as well: a chunk that cannot cross the link in 30 s is
/// a chunk too big, which is why the transfer queue's segments use this figure.
pub const CHUNK: usize = 64 * 1024;

/// One entry of a directory listing, or one file's attributes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub dir: bool,
    pub size: u64,
    pub modified: Option<u64>,
}

/// How full a remote filesystem is, from `statvfs@openssh.com`.
///
/// The extension reports blocks, the way `statvfs(2)` does; the byte figures
/// here are the blocks multiplied by the fragment size, because "is there room
/// for this file" is a byte question. `available_bytes` is the one a transfer
/// may use — `free_bytes` counts what the superuser could write and can be
/// larger than the space this account is allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatVfs {
    /// `f_blocks × f_frsize`: the whole filesystem.
    pub total_bytes: u64,
    /// `f_bfree × f_frsize`: free blocks, including the root reserve.
    pub free_bytes: u64,
    /// `f_bavail × f_frsize`: free blocks this account may use.
    pub available_bytes: u64,
    /// `f_bsize`: the preferred block size.
    pub block_size: u64,
    /// `f_frsize`: the unit the block counts are in.
    pub fragment_size: u64,
}

impl StatVfs {
    /// The extension's version-2 answer is eleven `u64`s in `statvfs(2)`'s own
    /// order. Every one is read, so a short reply is an error rather than a
    /// struct with fields shifted underneath it.
    fn of(payload: &[u8]) -> Result<Self> {
        let mut reader = Reader::new(payload);
        let block_size = reader.u64()?;
        let fragment_size = reader.u64()?;
        let blocks = reader.u64()?;
        let bfree = reader.u64()?;
        let bavail = reader.u64()?;
        // f_files, f_ffree, f_favail, f_fsid, f_flag, f_namemax are read and
        // dropped: a queue needs bytes, and the flag word is not a byte count.
        for _ in 0..6 {
            let _ignored = reader.u64()?;
        }
        // `f_frsize` is allowed to be zero; `f_bsize` is the fallback POSIX
        // gives, and a zero unit would otherwise report every filesystem empty.
        let unit = if fragment_size == 0 {
            block_size
        } else {
            fragment_size
        };
        Ok(Self {
            total_bytes: blocks.saturating_mul(unit),
            free_bytes: bfree.saturating_mul(unit),
            available_bytes: bavail.saturating_mul(unit),
            block_size,
            fragment_size,
        })
    }
}

/// An open remote file being written: the handle `SSH_FXP_OPEN` returned.
///
/// The handle itself is the server's; dtr only carries it. An `Upload` dropped
/// without [`Session::finish_upload`] or [`Session::abort_upload`] leaves the
/// server's handle open until the session closes — the `ssh` child is killed on
/// drop, so nothing outlives the session, but a long-lived session leaks the
/// handle, which is why the two closing methods take `self`.
#[derive(Debug)]
pub struct Upload {
    handle: Vec<u8>,
}

/// An open remote file being read: the mirror of [`Upload`].
///
/// This is the primitive a remote-to-remote copy needs: read one [`CHUNK`] here
/// and write it there, with no local file in between. An `Upload` dropped
/// without [`Session::finish_read`] leaves the server's handle open until the
/// session closes, on the same terms as an upload.
#[derive(Debug)]
pub struct Read {
    handle: Vec<u8>,
}

/// Not `Send`: compio's handles carry a thread-local `Rc`, so a session belongs
/// to the thread that opened it — one thread per connection, messages between.
type BoxFuture<'a, T> = Pin<Box<dyn std::future::Future<Output = T> + 'a>>;

/// The byte stream a session speaks over: the ssh child, or a test's channel.
///
/// Framed rather than streamed: `write_all` gets one whole packet, `read_exact`
/// asks for exactly the bytes of one, because a packet length is read first and
/// the packet after it.
pub trait Wire {
    fn write_all<'a>(&'a mut self, bytes: Vec<u8>) -> BoxFuture<'a, Result<()>>;
    fn read_exact<'a>(&'a mut self, len: usize) -> BoxFuture<'a, Result<Vec<u8>>>;
}

/// A live SFTP session: the ssh child, the protocol on top of it, the id the
/// next request will carry, and whatever the server advertised beyond v3.
pub struct Session {
    wire: Box<dyn Wire>,
    /// `None` when the session was built on a wire instead of a child, which is
    /// how the tests drive the same code.
    child: Option<compio::process::Child>,
    next_id: u32,
    /// `SSH_FXP_VERSION` extension name → its version string, which is the
    /// whole of what the handshake says about optional verbs.
    extensions: HashMap<String, Vec<u8>>,
}

impl Session {
    /// Connect by ssh alias, through the user's own ssh.
    pub async fn connect(alias: &str) -> Result<Self> {
        let mut command = compio::process::Command::new("ssh");
        command.args(ssh_args(alias));
        command
            .stdin(Stdio::piped())
            .expect("piped stdin is not a fallible configuration");
        command
            .stdout(Stdio::piped())
            .expect("piped stdout is not a fallible configuration");
        // Piped, not null: when a handshake fails the child's stderr is the only
        // thing that says why. On its own the protocol error is "sender closed",
        // which tells nobody anything.
        command
            .stderr(Stdio::piped())
            .expect("piped stderr is not a fallible configuration");
        let mut child = command
            .spawn()
            .with_context(|| format!("spawn ssh for {alias}"))?;

        let stdin = child.stdin.take().context("ssh stdin")?;
        let stdout = child.stdout.take().context("ssh stdout")?;
        let stderr = child.stderr.take();
        let mut session = Self {
            wire: Box::new(ChildWire { stdin, stdout }),
            child: Some(child),
            next_id: 1,
            extensions: HashMap::new(),
        };
        match session.init().await {
            Ok(()) => Ok(session),
            Err(err) => {
                let said = read_stderr(stderr).await;
                let mut message = format!("sftp handshake with {alias} failed: {err}");
                if !said.is_empty() {
                    message.push_str(&format!("\nssh said: {said}"));
                }
                Err(anyhow::anyhow!(message))
            }
        }
    }

    /// A session over any wire: the tests' way in, and the reason `connect` is
    /// only about *finding* that wire.
    pub async fn from_wire(wire: Box<dyn Wire>) -> Result<Self> {
        let mut session = Self {
            wire,
            child: None,
            next_id: 1,
            extensions: HashMap::new(),
        };
        session.init().await.context("sftp handshake")?;
        Ok(session)
    }

    /// The directory's entries, sorted the way a file browser shows them:
    /// directories first, then by name.
    pub async fn list(&mut self, path: &str) -> Result<Vec<Entry>> {
        let (kind, payload) = self
            .request(OPENDIR, put_string(path.as_bytes()))
            .await
            .with_context(|| format!("opendir {path}"))?;
        let handle = handle_of(kind, &payload)?;
        let directory = Path::new(path);
        let mut entries = Vec::new();
        loop {
            let (kind, payload) = self
                .request(READDIR, put_string(&handle))
                .await
                .with_context(|| format!("readdir {path}"))?;
            match kind {
                NAME => {
                    let mut reader = Reader::new(&payload);
                    let count = reader.u32()?;
                    for _ in 0..count {
                        let name = reader.string()?;
                        let _longname = reader.string()?;
                        let attrs = reader.attrs()?;
                        if name == "." || name == ".." {
                            continue;
                        }
                        entries.push(Entry {
                            path: directory.join(&name),
                            name,
                            dir: attrs.is_dir(),
                            size: attrs.size,
                            modified: attrs.modified.map(u64::from),
                        });
                    }
                }
                STATUS => {
                    let status = Status::of(&payload)?;
                    if status.is_eof() {
                        break;
                    }
                    status.into_result()?;
                    break;
                }
                other => bail!("readdir: unexpected packet {other}"),
            }
        }
        let _ = self
            .request(CLOSE, put_string(&handle))
            .await
            .context("close the directory");
        sort_entries(&mut entries);
        Ok(entries)
    }

    /// One path's metadata, without listing its directory.
    ///
    /// A click on a file needs to know "is it a directory" before deciding what
    /// to do with it, and fetching a whole directory listing to answer that is
    /// a round trip for one bit.
    pub async fn stat(&mut self, path: &str) -> Result<Entry> {
        let (kind, payload) = self
            .request(STAT, put_string(path.as_bytes()))
            .await
            .with_context(|| format!("stat {path}"))?;
        let attrs = match kind {
            ATTRS => Reader::new(&payload).attrs()?,
            STATUS => {
                Status::of(&payload)?.into_result()?;
                bail!("stat: the server sent no attributes")
            }
            other => bail!("stat: unexpected packet {other}"),
        };
        Ok(Entry {
            name: Path::new(path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_owned()),
            path: PathBuf::from(path),
            dir: attrs.is_dir(),
            size: attrs.size,
            modified: attrs.modified.map(u64::from),
        })
    }

    /// The remote home directory, as the server reports it.
    pub async fn home(&mut self) -> Result<PathBuf> {
        let (kind, payload) = self
            .request(REALPATH, put_string(b"."))
            .await
            .context("realpath .")?;
        match kind {
            NAME => {
                let mut reader = Reader::new(&payload);
                let count = reader.u32()?;
                if count == 0 {
                    bail!("realpath: no answer");
                }
                let name = reader.string()?;
                let _longname = reader.string()?;
                let _attrs = reader.attrs()?;
                Ok(PathBuf::from(name))
            }
            STATUS => Status::of(&payload)?.into_result().map(|()| PathBuf::new()),
            other => bail!("realpath: unexpected packet {other}"),
        }
    }

    /// Copy one file down. Bytes land in `local` only when the transfer
    /// finished, so a cancelled download cannot truncate a good file.
    pub async fn download(&mut self, remote: &str, local: &Path) -> Result<u64> {
        let reading = self
            .begin_read(remote)
            .await
            .with_context(|| format!("open {remote}"))?;

        let temporary = local.with_extension("part");
        let mut out = compio::fs::File::create(&temporary)
            .await
            .with_context(|| format!("create {}", temporary.display()))?;
        let mut total = 0u64;
        loop {
            let Some(bytes) = self
                .read_chunk(&reading, total)
                .await
                .with_context(|| format!("read {remote} at {total}"))?
            else {
                break;
            };
            let len = bytes.len() as u64;
            // `File` is positional: there is no sequential write, so the
            // offset the protocol already carries is the cursor.
            out.write_all_at(bytes, total)
                .await
                .0
                .with_context(|| format!("write {}", temporary.display()))?;
            total += len;
        }
        out.sync_all()
            .await
            .with_context(|| format!("flush {}", temporary.display()))?;
        drop(out);
        // A close that fails after every byte arrived does not make the copy
        // wrong, so it is reported the way it was before: ignored.
        let _ = self.finish_read(reading).await.context("close the file");
        compio::fs::rename(&temporary, local)
            .await
            .with_context(|| format!("rename into {}", local.display()))?;
        Ok(total)
    }

    /// Open `remote` for reading: [`Session::read_chunk`] hands the bytes back a
    /// [`CHUNK`] at a time, so a caller can stream them somewhere other than a
    /// local file — a remote-to-remote copy writes each chunk straight back with
    /// [`Session::write_upload_chunk`].
    pub async fn begin_read(&mut self, remote: &str) -> Result<Read> {
        let mut body = put_string(remote.as_bytes());
        body.extend_from_slice(&FXF_READ.to_be_bytes());
        // No attributes on open: creation flags are what would need them, and
        // this side only ever reads.
        body.extend_from_slice(&0u32.to_be_bytes());
        let (kind, payload) = self
            .request(OPEN, body)
            .await
            .with_context(|| format!("open {remote}"))?;
        Ok(Read {
            handle: handle_of(kind, &payload)?,
        })
    }

    /// The next at most [`CHUNK`] bytes of `read`, starting at `offset`, or
    /// `None` at end of file.
    ///
    /// A `DATA` packet that carries no bytes is reported as the end: v3 ends a
    /// read with `SSH_FX_EOF`, and an offset that did not move would ask for the
    /// same packet for ever. A server that answers more than the [`CHUNK`] it was
    /// asked for is a protocol error, not something to grow a buffer with.
    pub async fn read_chunk(&mut self, read: &Read, offset: u64) -> Result<Option<Vec<u8>>> {
        let mut body = put_string(&read.handle);
        body.extend_from_slice(&offset.to_be_bytes());
        body.extend_from_slice(&(CHUNK as u32).to_be_bytes());
        let (kind, payload) = self
            .request(READ, body)
            .await
            .with_context(|| format!("read at {offset}"))?;
        match kind {
            DATA => {
                let mut reader = Reader::new(&payload);
                let bytes = reader.bytes()?;
                if bytes.is_empty() {
                    return Ok(None);
                }
                if bytes.len() > CHUNK {
                    bail!(
                        "a read of {} bytes exceeds the {CHUNK}-byte chunk bound",
                        bytes.len()
                    );
                }
                Ok(Some(bytes))
            }
            STATUS => {
                let status = Status::of(&payload)?;
                if status.is_eof() {
                    Ok(None)
                } else {
                    status.into_result()?;
                    bail!("read: the server sent neither data nor an end of file")
                }
            }
            other => bail!("read: unexpected packet {other}"),
        }
    }

    /// Close the read handle: the server can forget the file.
    pub async fn finish_read(&mut self, read: Read) -> Result<()> {
        self.close_handle(&read.handle, "finish the read").await
    }

    /// Make one directory at `path`, mode 0755.
    pub async fn create_dir(&mut self, path: &str) -> Result<()> {
        let mut body = put_string(path.as_bytes());
        body.extend_from_slice(&permissions(DIRECTORY_MODE));
        self.status_only(MKDIR, body, &format!("mkdir {path}"))
            .await
    }

    /// Delete one file. A directory is [`Session::remove_dir`]'s, and the
    /// server refuses it; a recursive delete is a caller's decision, not a
    /// codec's.
    pub async fn remove(&mut self, path: &str) -> Result<()> {
        self.status_only(
            REMOVE,
            put_string(path.as_bytes()),
            &format!("remove {path}"),
        )
        .await
    }

    /// Delete one empty directory.
    pub async fn remove_dir(&mut self, path: &str) -> Result<()> {
        self.status_only(RMDIR, put_string(path.as_bytes()), &format!("rmdir {path}"))
            .await
    }

    /// Rename `from` to `to`.
    ///
    /// `overwrite` is the caller's answer to the conflict question, and it is
    /// not sent to the server: v3's `SSH_FXP_RENAME` has no flag for it. It
    /// does choose the packet. `true` uses `posix-rename@openssh.com` when the
    /// server advertised it — that extension *is* the atomic replace — and
    /// falls back to `SSH_FXP_RENAME` otherwise. `false` always uses plain
    /// `SSH_FXP_RENAME`, which fails if `to` exists, so a "keep both" that
    /// raced with a new file cannot clobber what it kept.
    pub async fn rename(&mut self, from: &str, to: &str, overwrite: bool) -> Result<()> {
        let doing = format!("rename {from} to {to}");
        if overwrite && self.supports(POSIX_RENAME) {
            let mut body = put_string(POSIX_RENAME.as_bytes());
            body.extend_from_slice(&put_string(from.as_bytes()));
            body.extend_from_slice(&put_string(to.as_bytes()));
            self.status_only(EXTENDED, body, &doing).await
        } else {
            let mut body = put_string(from.as_bytes());
            body.extend_from_slice(&put_string(to.as_bytes()));
            self.status_only(RENAME, body, &doing).await
        }
    }

    /// Set one path's permission bits: `SSH_FXP_SETSTAT` with only the
    /// permissions attribute, so the server changes nothing else.
    pub async fn setstat(&mut self, path: &str, mode: u32) -> Result<()> {
        let mut body = put_string(path.as_bytes());
        body.extend_from_slice(&permissions(mode));
        self.status_only(SETSTAT, body, &format!("setstat {path}"))
            .await
    }

    /// [`Session::setstat`] under the name Unix gives it.
    pub async fn chmod(&mut self, path: &str, mode: u32) -> Result<()> {
        self.setstat(path, mode).await
    }

    /// How much room the filesystem holding `path` has left, when the server
    /// advertises `statvfs@openssh.com`.
    ///
    /// Absent extension is an error the caller may ignore — a transfer that
    /// cannot ask simply proceeds and lets `WRITE` fail — never a made-up
    /// number.
    pub async fn statvfs(&mut self, path: &str) -> Result<StatVfs> {
        if !self.supports(STATVFS) {
            bail!("the server does not advertise {STATVFS}");
        }
        let mut body = put_string(STATVFS.as_bytes());
        body.extend_from_slice(&put_string(path.as_bytes()));
        let (kind, payload) = self
            .request(EXTENDED, body)
            .await
            .with_context(|| format!("statvfs {path}"))?;
        match kind {
            EXTENDED_REPLY => StatVfs::of(&payload),
            STATUS => {
                Status::of(&payload)?.into_result()?;
                bail!("statvfs: the server sent no filesystem statistics")
            }
            other => bail!("statvfs: unexpected packet {other}"),
        }
    }

    /// Open `remote` for writing, creating it and truncating what was there.
    ///
    /// This, [`Session::write_upload_chunk`] and [`Session::finish_upload`] are
    /// the segmented upload a transfer queue needs: one packet per call, so the
    /// queue can release the per-alias lock between segments, interleave a
    /// listing and answer a cancel. The path is used exactly as given — no
    /// `.part` convention lives in this crate; publishing is a
    /// [`Session::rename`] the caller performs.
    pub async fn begin_upload(&mut self, remote: &str) -> Result<Upload> {
        let mut body = put_string(remote.as_bytes());
        body.extend_from_slice(&(FXF_WRITE | FXF_CREAT | FXF_TRUNC).to_be_bytes());
        // No attributes: the server's creation mode and umask are the right
        // default, and a caller that wants a mode calls [`Session::chmod`].
        body.extend_from_slice(&0u32.to_be_bytes());
        let (kind, payload) = self
            .request(OPEN, body)
            .await
            .with_context(|| format!("open {remote} for writing"))?;
        Ok(Upload {
            handle: handle_of(kind, &payload)?,
        })
    }

    /// Write `data` at `offset`, one packet.
    ///
    /// At most [`CHUNK`] bytes: that is the segment size the 30 s per-answer
    /// deadline supports, and refusing more here keeps one packet from growing
    /// without a bound.
    pub async fn write_upload_chunk(
        &mut self,
        upload: &Upload,
        offset: u64,
        data: &[u8],
    ) -> Result<()> {
        if data.len() > CHUNK {
            bail!(
                "a write of {} bytes exceeds the {CHUNK}-byte chunk bound",
                data.len()
            );
        }
        let mut body = put_string(&upload.handle);
        body.extend_from_slice(&offset.to_be_bytes());
        body.extend_from_slice(&put_string(data));
        self.status_only(WRITE, body, &format!("write at {offset}"))
            .await
    }

    /// Close the handle: the server considers the file complete.
    pub async fn finish_upload(&mut self, upload: Upload) -> Result<()> {
        self.close_upload(upload, "finish the upload").await
    }

    /// Close the handle after a failure or a cancel, best effort. Whatever was
    /// written stays on the server under the name given to
    /// [`Session::begin_upload`]; removing it is the caller's business, because
    /// the `.part` name is too.
    pub async fn abort_upload(&mut self, upload: Upload) -> Result<()> {
        self.close_upload(upload, "abandon the upload").await
    }

    /// Copy one local file up to `remote` in [`CHUNK`]-sized writes, then close
    /// the handle. Returns the bytes written.
    ///
    /// `remote` is used exactly as given: the queue uploads `<name>.part` and
    /// publishes with [`Session::rename`], so a cancelled or failed transfer
    /// never leaves a short file under the real name. On failure the handle is
    /// closed before the error is returned.
    pub async fn upload(&mut self, local: &Path, remote: &str) -> Result<u64> {
        let file = compio::fs::File::open(local)
            .await
            .with_context(|| format!("open {}", local.display()))?;
        let len = file
            .metadata()
            .await
            .with_context(|| format!("stat {}", local.display()))?
            .len();
        let upload = self.begin_upload(remote).await?;
        let mut offset = 0u64;
        while offset < len {
            let wanted = (len - offset).min(CHUNK as u64) as usize;
            let read = file.read_exact_at(vec![0u8; wanted], offset).await;
            let bytes = match read.0 {
                Ok(()) => read.1,
                Err(err) => {
                    let _ = self.abort_upload(upload).await;
                    return Err(err)
                        .with_context(|| format!("read {} at {offset}", local.display()));
                }
            };
            if let Err(err) = self.write_upload_chunk(&upload, offset, &bytes).await {
                let _ = self.abort_upload(upload).await;
                return Err(err).with_context(|| {
                    format!("upload {} to {remote} at {offset}", local.display())
                });
            }
            offset += wanted as u64;
        }
        self.finish_upload(upload)
            .await
            .with_context(|| format!("finish uploading to {remote}"))?;
        Ok(offset)
    }

    /// Whether the server advertised an `SSH_FXP_VERSION` extension.
    pub fn supports(&self, extension: &str) -> bool {
        self.extensions.contains_key(extension)
    }

    /// One request whose answer must be a status, and an OK one.
    async fn status_only(&mut self, kind: u8, body: Vec<u8>, doing: &str) -> Result<()> {
        let (answered, payload) = self
            .request(kind, body)
            .await
            .with_context(|| doing.to_owned())?;
        match answered {
            STATUS => Status::of(&payload)?.into_result(),
            other => bail!("{doing}: unexpected packet {other}"),
        }
    }

    /// Close one upload handle. Finishing and aborting send the same packet;
    /// only the caller's intent differs.
    async fn close_upload(&mut self, upload: Upload, doing: &str) -> Result<()> {
        self.close_handle(&upload.handle, doing).await
    }

    /// Close one `SSH_FXP_OPEN` handle: reads and uploads differ only in what
    /// the caller calls it.
    async fn close_handle(&mut self, handle: &[u8], doing: &str) -> Result<()> {
        let (answered, payload) = self
            .request(CLOSE, put_string(handle))
            .await
            .with_context(|| format!("{doing} by closing its handle"))?;
        match answered {
            STATUS => Status::of(&payload)?.into_result(),
            other => bail!("{doing}: closing the handle got packet {other}"),
        }
    }

    /// Kill the child: a session is a process, and an orphaned `ssh` would
    /// outlive the window that asked for it.
    pub fn close(mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
        }
        self.child = None;
    }

    /// `SSH_FXP_INIT` / `SSH_FXP_VERSION`: the one exchange with no ids.
    async fn init(&mut self) -> Result<()> {
        let mut packet = vec![INIT];
        packet.extend_from_slice(&VERSION.to_be_bytes());
        self.write_frame(packet).await?;
        let body = self.read_frame().await?;
        let mut reader = Reader::new(&body);
        let kind = reader.u8()?;
        if kind != VERSION_PACKET {
            bail!("expected SSH_FXP_VERSION, got packet {kind}");
        }
        let version = reader.u32()?;
        if version < VERSION {
            bail!("the server speaks SFTP {version}, and this client needs {VERSION}");
        }
        // `SSH_FXP_VERSION` ends with zero or more (name, data) pairs: the
        // extensions beyond v3 that this server offers. Remember them raw; what
        // a version string means is the operation's business.
        let mut extensions = HashMap::new();
        while reader.remaining() > 0 {
            let name = reader.string()?;
            let data = reader.bytes()?;
            extensions.insert(name, data);
        }
        self.extensions = extensions;
        Ok(())
    }

    /// One request and its answer, with every answer checked for the id it
    /// belongs to: a late reply from a request that timed out must not be read
    /// as the answer to the next one.
    async fn request(&mut self, kind: u8, body: Vec<u8>) -> Result<(u8, Vec<u8>)> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let mut packet = Vec::with_capacity(5 + body.len());
        packet.push(kind);
        packet.extend_from_slice(&id.to_be_bytes());
        packet.extend_from_slice(&body);
        let answer = compio::time::timeout(ANSWER, async {
            self.write_frame(packet).await?;
            self.read_frame().await
        })
        .await
        .map_err(|_| anyhow::anyhow!("no answer in {}s", ANSWER.as_secs()))??;
        let mut reader = Reader::new(&answer);
        let answered = reader.u8()?;
        let answered_id = reader.u32()?;
        if answered_id != id {
            bail!("answer {answered_id} is not for request {id}");
        }
        Ok((answered, reader.rest().to_vec()))
    }

    async fn write_frame(&mut self, packet: Vec<u8>) -> Result<()> {
        let mut frame = Vec::with_capacity(4 + packet.len());
        frame.extend_from_slice(&(packet.len() as u32).to_be_bytes());
        frame.extend_from_slice(&packet);
        self.wire.write_all(frame).await
    }

    async fn read_frame(&mut self) -> Result<Vec<u8>> {
        let header = self.wire.read_exact(4).await?;
        let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        // A length of zero would loop forever on a dead stream, and 64 MiB is
        // past any packet an OpenSSH server sends for these operations.
        if len == 0 || len > 64 * 1024 * 1024 {
            bail!("implausible packet length {len}");
        }
        self.wire.read_exact(len).await
    }
}

impl Drop for Session {
    /// A session that is dropped is still a child process: killing it here is
    /// what stops a forgotten `ssh` holding a connection open.
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
        }
    }
}

/// The child's stdin and stdout as one framed wire.
struct ChildWire {
    stdin: compio::process::ChildStdin,
    stdout: compio::process::ChildStdout,
}

impl Wire for ChildWire {
    fn write_all<'a>(&'a mut self, bytes: Vec<u8>) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.stdin.write_all(bytes).await.0?;
            Ok(())
        })
    }

    fn read_exact<'a>(&'a mut self, len: usize) -> BoxFuture<'a, Result<Vec<u8>>> {
        Box::pin(async move {
            let read = self.stdout.read_exact(vec![0u8; len]).await;
            read.0?;
            Ok(read.1)
        })
    }
}

/// One file's attributes, as far as dtr asks about them.
#[derive(Clone, Copy, Debug, Default)]
struct Attr {
    size: u64,
    permissions: Option<u32>,
    modified: Option<u32>,
}

impl Attr {
    /// SFTP has no type field: the mode bits are the type.
    fn is_dir(&self) -> bool {
        self.permissions
            .is_some_and(|mode| mode & 0o170_000 == 0o040_000)
    }
}

/// One `SSH_FXP_STATUS`, decoded.
struct Status {
    code: u32,
    message: String,
}

impl Status {
    fn of(payload: &[u8]) -> Result<Self> {
        let mut reader = Reader::new(payload);
        let code = reader.u32()?;
        let message = reader.string().unwrap_or_default();
        Ok(Self { code, message })
    }

    fn is_eof(&self) -> bool {
        self.code == FX_EOF
    }

    fn into_result(self) -> Result<()> {
        if self.code == FX_OK {
            return Ok(());
        }
        let detail = if self.message.is_empty() {
            format!("status {}", self.code)
        } else {
            self.message
        };
        bail!("{detail}")
    }
}

/// A read cursor over one packet's payload.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn rest(&self) -> &'a [u8] {
        &self.bytes[self.at.min(self.bytes.len())..]
    }

    /// Bytes left to read: the extension list is read until this is zero.
    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.at)
    }

    fn u8(&mut self) -> Result<u8> {
        let Some(byte) = self.bytes.get(self.at) else {
            bail!("packet ended early");
        };
        self.at += 1;
        Ok(*byte)
    }

    fn u32(&mut self) -> Result<u32> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes(bytes.try_into().expect("four bytes")))
    }

    fn u64(&mut self) -> Result<u64> {
        let bytes = self.take(8)?;
        Ok(u64::from_be_bytes(bytes.try_into().expect("eight bytes")))
    }

    fn bytes(&mut self) -> Result<Vec<u8>> {
        let len =
            u32::from_be_bytes(self.take(4)?.try_into().expect("four bytes of length")) as usize;
        Ok(self.take(len)?.to_vec())
    }

    fn string(&mut self) -> Result<String> {
        let bytes = self.bytes()?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(len).context("packet length overflow")?;
        if end > self.bytes.len() {
            bail!("packet ended early");
        }
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    fn attrs(&mut self) -> Result<Attr> {
        let flags = self.u32()?;
        let mut attr = Attr::default();
        if flags & ATTR_SIZE != 0 {
            attr.size = self.u64()?;
        }
        if flags & ATTR_UIDGID != 0 {
            let _uid = self.u32()?;
            let _gid = self.u32()?;
        }
        if flags & ATTR_PERMISSIONS != 0 {
            attr.permissions = Some(self.u32()?);
        }
        if flags & ATTR_ACMODTIME != 0 {
            let _atime = self.u32()?;
            attr.modified = Some(self.u32()?);
        }
        if flags & ATTR_EXTENDED != 0 {
            let count = self.u32()?;
            for _ in 0..count {
                let _kind = self.string()?;
                let _data = self.bytes()?;
            }
        }
        Ok(attr)
    }
}

/// A string field: length first, then the bytes.
fn put_string(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + bytes.len());
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
    out
}

/// The attributes blob for "these permission bits and nothing else": one flags
/// word, then the fields the flags name.
fn permissions(mode: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(8);
    out.extend_from_slice(&ATTR_PERMISSIONS.to_be_bytes());
    out.extend_from_slice(&mode.to_be_bytes());
    out
}

/// The handle an `SSH_FXP_HANDLE` carries, or the failure it carried instead.
fn handle_of(kind: u8, payload: &[u8]) -> Result<Vec<u8>> {
    match kind {
        HANDLE => Reader::new(payload).bytes(),
        STATUS => Status::of(payload)?.into_result().map(|()| Vec::new()),
        other => bail!("expected a handle, got packet {other}"),
    }
}

/// Whatever the child said before it died, bounded and time-boxed: a wedged ssh
/// must not hold the worker.
async fn read_stderr(stderr: Option<compio::process::ChildStderr>) -> String {
    let Some(mut stderr) = stderr else {
        return String::new();
    };
    let mut scratch = Vec::new();
    let _ = compio::time::timeout(Duration::from_secs(2), async {
        loop {
            let read = stderr.read(vec![0u8; 4096]).await;
            let Ok(n) = read.0 else { return };
            if n == 0 {
                return;
            }
            scratch.extend_from_slice(&read.1[..n]);
        }
    })
    .await;
    let text = String::from_utf8_lossy(&scratch).trim().to_owned();
    // The last line is the one that says why; earlier ones are banners.
    let last = text.lines().last().unwrap_or("").to_owned();
    if last.is_empty() { text } else { last }
}

/// The ssh invocation, in the order ssh wants it: every option before the
/// destination. After the destination ssh treats arguments as a command, so
/// `-s host sftp -o …` would hand `-o` to the subsystem instead of parsing it.
fn ssh_args(alias: &str) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    // A broken config is a real failure mode (one store drop-in with wrong
    // permissions makes every connection fail), so there is a way out.
    if let Ok(config) = std::env::var("DTR_SSH_CONFIG") {
        args.push("-F".to_owned());
        args.push(config);
    }
    // Batch mode makes a missing credential a fast, readable failure instead of
    // a silent wait: the child has no tty to prompt on. `DTR_SSH_BATCH=0` turns
    // it off for diagnosis — with a pty underneath (docs/sftp.md §5.3) a prompt
    // is answerable, without one it hangs, which is why it is on by default.
    if std::env::var("DTR_SSH_BATCH").as_deref() != Ok("0") {
        args.push("-o".to_owned());
        args.push("BatchMode=yes".to_owned());
    }
    args.push("-s".to_owned());
    args.push(alias.to_owned());
    args.push("sftp".to_owned());
    args
}

/// Directories first, then by name: the order every file manager shows, and the
/// one a person's eye expects when the listing changes underneath them.
fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.cmp(&b.name)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, dir: bool) -> Entry {
        Entry {
            name: name.to_owned(),
            path: PathBuf::from("/x").join(name),
            dir,
            size: 0,
            modified: None,
        }
    }

    #[test]
    fn directories_come_before_files() {
        let mut entries = vec![
            entry("z.txt", false),
            entry("a", true),
            entry("b.txt", false),
        ];
        sort_entries(&mut entries);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["a", "b.txt", "z.txt"]);
    }

    #[test]
    fn a_session_is_a_process() {
        // `close` must take the child down with it: an ssh left behind holds a
        // connection and a `ControlMaster` alive for as long as it runs. The
        // `Drop` impl is what covers a session that is never closed.
        let source = include_str!("lib.rs");
        assert!(source.contains("impl Drop for Session"));
    }

    #[test]
    fn attributes_are_read_in_the_order_their_flags_name() {
        // size, then permissions, then times — the order `SSH_FXP_ATTRS` puts
        // them in, whatever a server decides to include.
        let mut payload = Vec::new();
        payload.extend_from_slice(&(ATTR_SIZE | ATTR_PERMISSIONS | ATTR_ACMODTIME).to_be_bytes());
        payload.extend_from_slice(&1234u64.to_be_bytes());
        payload.extend_from_slice(&0o100644u32.to_be_bytes());
        payload.extend_from_slice(&1u32.to_be_bytes());
        payload.extend_from_slice(&2u32.to_be_bytes());
        let mut reader = Reader::new(&payload);
        let attr = reader.attrs().expect("attributes");
        assert_eq!(attr.size, 1234);
        assert!(!attr.is_dir(), "a regular file is not a directory");
        assert_eq!(attr.modified, Some(2));
    }

    #[test]
    fn a_directory_is_something_the_mode_bits_say() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&ATTR_PERMISSIONS.to_be_bytes());
        payload.extend_from_slice(&0o040755u32.to_be_bytes());
        let attr = Reader::new(&payload).attrs().expect("attributes");
        assert!(attr.is_dir());
    }
}

#[cfg(test)]
mod ssh_args_tests {
    use super::ssh_args;

    #[test]
    fn every_option_comes_before_the_destination() {
        let args = ssh_args("jupiter");
        let position = |needle: &str| args.iter().position(|arg| arg == needle).unwrap();
        // After the destination ssh treats the rest as a command, so an option
        // that lands there is handed to the subsystem instead of parsed.
        assert!(position("-s") < position("jupiter"));
        assert!(position("-o") < position("jupiter"));
        assert!(position("BatchMode=yes") < position("jupiter"));
        assert_eq!(args.last().map(String::as_str), Some("sftp"));
    }
}

/// The write packets, driven by a wire that says exactly which bytes come back.
///
/// `tests/mock.rs` runs a whole server; this module is the scalpel. A canned
/// answer that is malformed is how "a bad reply is an error, not a panic" is
/// tested without needing a hostile server.
#[cfg(test)]
mod write_packets {
    use std::cell::RefCell;
    use std::rc::Rc;

    use anyhow::bail;

    use super::*;

    /// A wire that replays canned answer frames and records what was written.
    struct Scripted {
        incoming: Vec<u8>,
        at: usize,
        written: Rc<RefCell<Vec<Vec<u8>>>>,
    }

    impl Wire for Scripted {
        fn write_all<'a>(&'a mut self, bytes: Vec<u8>) -> BoxFuture<'a, Result<()>> {
            Box::pin(async move {
                self.written.borrow_mut().push(bytes);
                Ok(())
            })
        }

        fn read_exact<'a>(&'a mut self, len: usize) -> BoxFuture<'a, Result<Vec<u8>>> {
            Box::pin(async move {
                let end = self
                    .at
                    .checked_add(len)
                    .context("the scripted wire ran out of answers")?;
                if end > self.incoming.len() {
                    bail!("the scripted wire ran out of answers");
                }
                let bytes = self.incoming[self.at..end].to_vec();
                self.at = end;
                Ok(bytes)
            })
        }
    }

    /// One frame: length first, then the packet.
    fn frame(packet: &[u8]) -> Vec<u8> {
        let mut out = (packet.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(packet);
        out
    }

    /// One `SSH_FXP_VERSION`, with the server's extension list.
    fn version(extensions: &[(&str, &str)]) -> Vec<u8> {
        let mut packet = vec![VERSION_PACKET];
        packet.extend_from_slice(&VERSION.to_be_bytes());
        for (name, data) in extensions {
            packet.extend_from_slice(&put_string(name.as_bytes()));
            packet.extend_from_slice(&put_string(data.as_bytes()));
        }
        frame(&packet)
    }

    /// One answer packet, framed, with the id it answers.
    fn answer(id: u32, kind: u8, payload: &[u8]) -> Vec<u8> {
        let mut packet = vec![kind];
        packet.extend_from_slice(&id.to_be_bytes());
        packet.extend_from_slice(payload);
        frame(&packet)
    }

    /// The payload of an `SSH_FXP_STATUS`.
    fn status_payload(code: u32, message: &str) -> Vec<u8> {
        let mut out = code.to_be_bytes().to_vec();
        out.extend_from_slice(&put_string(message.as_bytes()));
        // Language tag, which v3 sends and dtr never reads.
        out.extend_from_slice(&put_string(b""));
        out
    }

    fn ok(id: u32) -> Vec<u8> {
        answer(id, STATUS, &status_payload(FX_OK, ""))
    }

    /// A session over the canned stream, plus what it wrote.
    async fn session(
        extensions: &[(&str, &str)],
        answers: &[Vec<u8>],
    ) -> (Session, Rc<RefCell<Vec<Vec<u8>>>>) {
        let mut incoming = version(extensions);
        for reply in answers {
            incoming.extend_from_slice(reply);
        }
        let written = Rc::new(RefCell::new(Vec::new()));
        let wire = Scripted {
            incoming,
            at: 0,
            written: written.clone(),
        };
        let session = Session::from_wire(Box::new(wire))
            .await
            .expect("a handshake");
        (session, written)
    }

    /// The packet bodies written after the handshake, in order.
    fn sent(written: &Rc<RefCell<Vec<Vec<u8>>>>) -> Vec<Vec<u8>> {
        written
            .borrow()
            .iter()
            .skip(1)
            .map(|frame| frame[4..].to_vec())
            .collect()
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

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_be_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
    }

    fn u64_at(bytes: &[u8], at: usize) -> u64 {
        u64::from_be_bytes(bytes[at..at + 8].try_into().expect("eight bytes"))
    }

    #[compio::test]
    async fn the_handshake_remembers_the_extensions_the_server_advertised() {
        let (session, _) = session(&[(POSIX_RENAME, "1"), (STATVFS, "2")], &[]).await;
        assert!(session.supports(POSIX_RENAME));
        assert!(session.supports(STATVFS));
        assert!(!session.supports("limits@openssh.com"));
    }

    #[compio::test]
    async fn rename_uses_posix_rename_when_the_server_advertised_it() {
        let (mut session, written) = session(&[(POSIX_RENAME, "1")], &[ok(1)]).await;
        session
            .rename("/srv/old", "/srv/new", true)
            .await
            .expect("rename");
        let packets = sent(&written);
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0][0], EXTENDED);
        assert_eq!(
            strings(&packets[0][5..]),
            [POSIX_RENAME, "/srv/old", "/srv/new"]
        );
    }

    #[compio::test]
    async fn rename_without_the_extension_uses_the_plain_packet() {
        let (mut session, written) = session(&[], &[ok(1)]).await;
        session
            .rename("/srv/old", "/srv/new", true)
            .await
            .expect("rename");
        let packets = sent(&written);
        assert_eq!(packets[0][0], RENAME);
        assert_eq!(strings(&packets[0][5..]), ["/srv/old", "/srv/new"]);
    }

    #[compio::test]
    async fn rename_without_overwrite_never_uses_the_replacing_extension() {
        // The extension *is* the atomic replace; a caller that answered "keep
        // both" must not get it even when the server offers it.
        let (mut session, written) = session(&[(POSIX_RENAME, "1")], &[ok(1)]).await;
        session
            .rename("/srv/old", "/srv/new", false)
            .await
            .expect("rename");
        let packets = sent(&written);
        assert_eq!(packets[0][0], RENAME);
    }

    #[compio::test]
    async fn mkdir_sends_the_mode_it_creates() {
        let (mut session, written) = session(&[], &[ok(1)]).await;
        session.create_dir("/srv/new").await.expect("mkdir");
        let packets = sent(&written);
        assert_eq!(packets.len(), 1);
        let packet = &packets[0];
        assert_eq!(packet[0], MKDIR);
        let payload = &packet[5..];
        assert_eq!(
            strings(payload).first().map(String::as_str),
            Some("/srv/new")
        );
        let at = 4 + "/srv/new".len();
        assert_eq!(u32_at(payload, at), ATTR_PERMISSIONS);
        assert_eq!(u32_at(payload, at + 4), DIRECTORY_MODE);
    }

    #[compio::test]
    async fn remove_and_remove_dir_are_their_own_packets() {
        let (mut session, written) = session(&[], &[ok(1), ok(2)]).await;
        session.remove("/srv/file").await.expect("remove");
        session.remove_dir("/srv/dir").await.expect("rmdir");
        let packets = sent(&written);
        assert_eq!(packets.len(), 2);
        assert_eq!(packets[0][0], REMOVE);
        assert_eq!(strings(&packets[0][5..]), ["/srv/file"]);
        assert_eq!(packets[1][0], RMDIR);
        assert_eq!(strings(&packets[1][5..]), ["/srv/dir"]);
    }

    #[compio::test]
    async fn setstat_carries_only_the_permission_bits() {
        let (mut session, written) = session(&[], &[ok(1)]).await;
        session.setstat("/srv/file", 0o600).await.expect("setstat");
        let packets = sent(&written);
        assert_eq!(packets[0][0], SETSTAT);
        let payload = &packets[0][5..];
        assert_eq!(
            strings(payload).first().map(String::as_str),
            Some("/srv/file")
        );
        let at = 4 + "/srv/file".len();
        assert_eq!(u32_at(payload, at), ATTR_PERMISSIONS);
        assert_eq!(u32_at(payload, at + 4), 0o600);
    }

    #[compio::test]
    async fn statvfs_multiplies_blocks_by_the_fragment_size() {
        // f_bsize, f_frsize, f_blocks, f_bfree, f_bavail, f_files, f_ffree,
        // f_favail, f_fsid, f_flag, f_namemax — the extension's own order.
        let values: [u64; 11] = [512, 1024, 100, 40, 30, 7, 6, 5, 9, 0, 255];
        let payload: Vec<u8> = values
            .iter()
            .flat_map(|value| value.to_be_bytes())
            .collect();
        let (mut session, written) =
            session(&[(STATVFS, "2")], &[answer(1, EXTENDED_REPLY, &payload)]).await;
        assert!(session.supports(STATVFS));
        let stats = session.statvfs("/srv").await.expect("statvfs");
        assert_eq!(stats.total_bytes, 100 * 1024);
        assert_eq!(stats.free_bytes, 40 * 1024);
        assert_eq!(stats.available_bytes, 30 * 1024);
        assert_eq!(stats.block_size, 512);
        assert_eq!(stats.fragment_size, 1024);
        let packets = sent(&written);
        assert_eq!(packets[0][0], EXTENDED);
        assert_eq!(strings(&packets[0][5..]), [STATVFS, "/srv"]);
    }

    #[compio::test]
    async fn statvfs_without_the_extension_does_not_invent_a_number() {
        let (mut session, written) = session(&[], &[]).await;
        assert!(!session.supports(STATVFS));
        let err = session.statvfs("/srv").await.expect_err("no extension");
        assert!(
            format!("{err:#}").contains(STATVFS),
            "the error names the missing extension: {err:#}"
        );
        assert!(
            sent(&written).is_empty(),
            "nothing is asked when the extension was not advertised"
        );
    }

    #[compio::test]
    async fn a_truncated_statvfs_reply_is_an_error() {
        let (mut session, _) =
            session(&[(STATVFS, "2")], &[answer(1, EXTENDED_REPLY, &[0u8; 8])]).await;
        let err = session.statvfs("/srv").await.expect_err("truncated reply");
        assert!(format!("{err:#}").contains("packet ended early"), "{err:#}");
    }

    #[compio::test]
    async fn a_truncated_status_is_an_error_not_a_panic() {
        let (mut session, _) = session(&[], &[answer(1, STATUS, &[0, 0])]).await;
        let err = session
            .remove("/srv/file")
            .await
            .expect_err("truncated status");
        assert!(format!("{err:#}").contains("packet ended early"), "{err:#}");
    }

    #[compio::test]
    async fn an_unexpected_packet_type_is_an_error() {
        let (mut session, _) = session(&[], &[answer(1, 199, &[])]).await;
        let err = session
            .create_dir("/srv/new")
            .await
            .expect_err("wrong packet");
        assert!(
            format!("{err:#}").contains("unexpected packet 199"),
            "{err:#}"
        );
    }

    #[compio::test]
    async fn a_segmented_read_asks_at_the_offset_it_is_given() {
        // OPEN, a DATA answer, the EOF that ends it, then the CLOSE.
        let (mut session, written) = session(
            &[],
            &[
                answer(1, HANDLE, &put_string(b"r1")),
                answer(2, DATA, &put_string(b"abc")),
                answer(3, STATUS, &status_payload(FX_EOF, "eof")),
                ok(4),
            ],
        )
        .await;
        let reading = session.begin_read("/srv/a.bin").await.expect("open");
        assert_eq!(
            session.read_chunk(&reading, 0).await.expect("first chunk"),
            Some(b"abc".to_vec())
        );
        assert_eq!(
            session
                .read_chunk(&reading, 3)
                .await
                .expect("the end of the file"),
            None
        );
        session.finish_read(reading).await.expect("close");

        let packets = sent(&written);
        let kinds: Vec<u8> = packets.iter().map(|packet| packet[0]).collect();
        assert_eq!(kinds, [OPEN, READ, READ, CLOSE]);
        // `READ` is handle, offset, length — the offset is the caller's, and the
        // length is the chunk bound.
        let payload = &packets[1][5..];
        assert_eq!(strings(payload).first().map(String::as_str), Some("r1"));
        let at = 4 + "r1".len();
        assert_eq!(u64_at(payload, at), 0);
        assert_eq!(u32_at(payload, at + 8), CHUNK as u32);
        assert_eq!(u64_at(&packets[2][5..], at), 3);
    }

    #[compio::test]
    async fn a_data_packet_with_no_bytes_is_the_end_not_a_loop() {
        // A server that answers `DATA 0` instead of `SSH_FX_EOF` must not keep
        // the caller asking at an offset that never moves.
        let (mut session, written) = session(
            &[],
            &[
                answer(1, HANDLE, &put_string(b"r1")),
                answer(2, DATA, &put_string(b"")),
            ],
        )
        .await;
        let reading = session.begin_read("/srv/empty").await.expect("open");
        assert_eq!(
            session.read_chunk(&reading, 0).await.expect("the end"),
            None
        );
        let kinds: Vec<u8> = sent(&written).iter().map(|packet| packet[0]).collect();
        assert_eq!(kinds, [OPEN, READ], "one read, and no second ask");
    }

    #[compio::test]
    async fn a_read_bigger_than_the_chunk_bound_is_refused() {
        let oversized = vec![7u8; CHUNK + 1];
        let (mut session, _) = session(
            &[],
            &[
                answer(1, HANDLE, &put_string(b"r1")),
                answer(2, DATA, &put_string(&oversized)),
            ],
        )
        .await;
        let reading = session.begin_read("/srv/a.bin").await.expect("open");
        let err = session
            .read_chunk(&reading, 0)
            .await
            .expect_err("a server cannot answer more than it was asked");
        assert!(format!("{err:#}").contains("chunk bound"), "{err:#}");
    }

    #[compio::test]
    async fn a_read_of_the_wrong_packet_is_an_error() {
        let (mut session, _) = session(
            &[],
            &[answer(1, HANDLE, &put_string(b"r1")), answer(2, 199, &[])],
        )
        .await;
        let reading = session.begin_read("/srv/a.bin").await.expect("open");
        let err = session
            .read_chunk(&reading, 0)
            .await
            .expect_err("unexpected packet");
        assert!(
            format!("{err:#}").contains("unexpected packet 199"),
            "{err:#}"
        );
    }

    #[compio::test]
    async fn an_oversized_chunk_is_refused_before_it_is_sent() {
        let (mut session, written) = session(&[], &[answer(1, HANDLE, &put_string(b"h1"))]).await;
        let upload = session.begin_upload("/srv/file").await.expect("open");
        let err = session
            .write_upload_chunk(&upload, 0, &vec![0u8; CHUNK + 1])
            .await
            .expect_err("oversized chunk");
        assert!(format!("{err:#}").contains("chunk bound"), "{err:#}");
        let packets = sent(&written);
        assert_eq!(packets.len(), 1, "only the open was sent");
        assert_eq!(packets[0][0], OPEN);
        assert_eq!(strings(&packets[0][5..]), ["/srv/file"]);
        let flags = u32_at(&packets[0][5..], 4 + "/srv/file".len());
        assert_eq!(flags, FXF_WRITE | FXF_CREAT | FXF_TRUNC);
    }

    #[compio::test]
    async fn an_upload_that_fails_midway_closes_its_handle_and_the_session_still_works() {
        // OPEN, one good WRITE, one failing WRITE, the abort's CLOSE, then the
        // STAT that proves the session is still healthy.
        let mut attrs = ATTR_SIZE.to_be_bytes().to_vec();
        attrs.extend_from_slice(&0u64.to_be_bytes());
        let (mut session, written) = session(
            &[],
            &[
                answer(1, HANDLE, &put_string(b"up")),
                ok(2),
                answer(3, STATUS, &status_payload(4, "no space")),
                ok(4),
                answer(5, ATTRS, &attrs),
            ],
        )
        .await;
        let dir = tempfile::tempdir().expect("tempdir");
        let local = dir.path().join("big.bin");
        std::fs::write(&local, vec![7u8; 2 * CHUNK + 5]).expect("write the local file");

        let err = session
            .upload(&local, "/srv/big.bin.part")
            .await
            .expect_err("the second chunk fails");
        assert!(format!("{err:#}").contains("no space"), "{err:#}");

        let kinds: Vec<u8> = sent(&written).iter().map(|packet| packet[0]).collect();
        assert_eq!(
            kinds,
            [OPEN, WRITE, WRITE, CLOSE],
            "the failed upload closes its handle and sends nothing else"
        );
        let entry = session
            .stat("/srv/notes.txt")
            .await
            .expect("a session after a failed upload is still a session");
        assert_eq!(entry.size, 0);
    }
}
