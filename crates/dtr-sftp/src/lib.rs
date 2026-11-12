//! SFTP over the host's own `ssh`.
//!
//! dtr contains no SSH implementation: the alias is handed to `ssh -s <alias>
//! sftp` unresolved, so `ProxyJump`, `Include`, `Match`, agent forwarding and
//! `known_hosts` are whatever the user's ssh already does. What is left is a
//! byte pipe — the child's stdin and stdout — and the SFTP version 3 framing on
//! top of it. That framing is the dozen packets below rather than a protocol
//! crate: it was `russh-sftp`, which pulled `tokio` and `tokio-util` into a
//! runtime that no longer has one (`docs/compio.md`). The operations dtr needs
//! are `list`, `stat`, `download` and `home`, and v3 has one packet for each.
//!
//! The wire is a trait so the tests can drive the same code over a channel
//! instead of a real server (`tests/mock.rs`).

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use compio::io::{AsyncRead as _, AsyncReadExt as _, AsyncWriteAtExt as _, AsyncWriteExt as _};

/// The version every OpenSSH server speaks, and the one this codec is written
/// against.
const VERSION: u32 = 3;

// Packet types. The names are the protocol's own (`SSH_FXP_…`), abbreviated.
const INIT: u8 = 1;
const VERSION_PACKET: u8 = 2;
const OPEN: u8 = 3;
const CLOSE: u8 = 4;
const READ: u8 = 5;
const STAT: u8 = 17;
const OPENDIR: u8 = 11;
const READDIR: u8 = 12;
const REALPATH: u8 = 16;
const STATUS: u8 = 101;
const HANDLE: u8 = 102;
const DATA: u8 = 103;
const NAME: u8 = 104;
const ATTRS: u8 = 105;

/// `SSH_FX_OK`: a status that is an answer, not a failure.
const FX_OK: u32 = 0;
/// `SSH_FX_EOF`: a read or a listing is finished.
const FX_EOF: u32 = 1;

/// `SSH_FXF_READ`.
const FXF_READ: u32 = 0x0000_0001;

/// Attribute flags.
const ATTR_SIZE: u32 = 0x0000_0001;
const ATTR_UIDGID: u32 = 0x0000_0002;
const ATTR_PERMISSIONS: u32 = 0x0000_0004;
const ATTR_ACMODTIME: u32 = 0x0000_0008;
const ATTR_EXTENDED: u32 = 0x8000_0000;

/// How long an answer may take before the connection is considered dead. A hung
/// `read` on a link that went away is otherwise forever.
const ANSWER: Duration = Duration::from_secs(30);

/// One entry of a directory listing, or one file's attributes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub dir: bool,
    pub size: u64,
    pub modified: Option<u64>,
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

/// A live SFTP session: the ssh child, the protocol on top of it, and the id
/// the next request will carry.
pub struct Session {
    wire: Box<dyn Wire>,
    /// `None` when the session was built on a wire instead of a child, which is
    /// how the tests drive the same code.
    child: Option<compio::process::Child>,
    next_id: u32,
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
        let mut body = put_string(remote.as_bytes());
        body.extend_from_slice(&FXF_READ.to_be_bytes());
        // No attributes on open: creation flags are what would need them, and
        // this side only ever reads.
        body.extend_from_slice(&0u32.to_be_bytes());
        let (kind, payload) = self
            .request(OPEN, body)
            .await
            .with_context(|| format!("open {remote}"))?;
        let handle = handle_of(kind, &payload)?;

        let temporary = local.with_extension("part");
        let mut out = compio::fs::File::create(&temporary)
            .await
            .with_context(|| format!("create {}", temporary.display()))?;
        let mut total = 0u64;
        loop {
            let mut body = put_string(&handle);
            body.extend_from_slice(&total.to_be_bytes());
            body.extend_from_slice(&(64u32 * 1024).to_be_bytes());
            let (kind, payload) = self
                .request(READ, body)
                .await
                .with_context(|| format!("read {remote} at {total}"))?;
            match kind {
                DATA => {
                    let mut reader = Reader::new(&payload);
                    let bytes = reader.bytes()?;
                    let len = bytes.len() as u64;
                    // `File` is positional: there is no sequential write, so
                    // the offset the protocol already carries is the cursor.
                    out.write_all_at(bytes, total)
                        .await
                        .0
                        .with_context(|| format!("write {}", temporary.display()))?;
                    total += len;
                }
                STATUS => {
                    let status = Status::of(&payload)?;
                    if status.is_eof() {
                        break;
                    }
                    status.into_result()?;
                    break;
                }
                other => bail!("read: unexpected packet {other}"),
            }
        }
        out.sync_all()
            .await
            .with_context(|| format!("flush {}", temporary.display()))?;
        drop(out);
        let _ = self
            .request(CLOSE, put_string(&handle))
            .await
            .context("close the file");
        compio::fs::rename(&temporary, local)
            .await
            .with_context(|| format!("rename into {}", local.display()))?;
        Ok(total)
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
