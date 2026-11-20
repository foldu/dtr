//! Scripted-wire tests: this adapter over a fake SFTP server.
//!
//! `dtr_sftp::Session::from_wire` is the seam (`docs/sftp.md` §8): a session can
//! be built on any [`Wire`], so the whole adapter — dispatch, one packet per
//! segment, the deadline, the one reconnect, publish and discard — runs without
//! an `ssh` binary or a host. A fake server over `async_channel` answers the
//! packets the adapter sends and keeps a little filesystem, so a test asserts
//! what landed, not only which packet was chosen.
//!
//! What this cannot cover is a real OpenSSH server — its status texts and its
//! `posix-rename`/`statvfs` behaviour — and the 30 s deadline itself, which a
//! test would have to wait for. Both are recorded as gaps in the account
//! appended to `docs/transfers.md`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};
use dtr_sftp::{CHUNK, POSIX_RENAME, STATVFS, Session, Wire};
use dtr_transfer::{
    BoxFuture, Direction, Event, JobId, LocalTransport, Op, OpenRequest, Queue, Segment, State,
    Transport,
};
use dtr_transfer_ssh::{Dialer, SshTransport};
use dtr_vfs::{BackendId, Kind, Location};

// `SSH_FXP_*` packets, as the numbers on the wire.
const OPEN: u8 = 3;
const CLOSE: u8 = 4;
const READ: u8 = 5;
const WRITE: u8 = 6;
const REMOVE: u8 = 13;
const RMDIR: u8 = 15;
const STAT: u8 = 17;
const RENAME: u8 = 18;
const EXTENDED: u8 = 200;

// ---------------------------------------------------------------------------
// A fake SFTP server over a pair of channels.
// ---------------------------------------------------------------------------

type WireFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// One end of the in-memory byte pipe. The two ends are the same type with the
/// channels swapped, so the client and the server are written against one
/// [`Wire`] and meet in the middle.
struct End {
    out: async_channel::Sender<Vec<u8>>,
    incoming: async_channel::Receiver<Vec<u8>>,
    buffered: Vec<u8>,
}

fn pipe() -> (End, End) {
    let (client_out, server_in) = async_channel::unbounded();
    let (server_out, client_in) = async_channel::unbounded();
    (
        End {
            out: client_out,
            incoming: client_in,
            buffered: Vec::new(),
        },
        End {
            out: server_out,
            incoming: server_in,
            buffered: Vec::new(),
        },
    )
}

impl Wire for End {
    fn write_all<'a>(&'a mut self, bytes: Vec<u8>) -> WireFuture<'a, Result<()>> {
        Box::pin(async move {
            self.out
                .send(bytes)
                .await
                .map_err(|_| anyhow::anyhow!("the other end is gone"))
        })
    }

    fn read_exact<'a>(&'a mut self, len: usize) -> WireFuture<'a, Result<Vec<u8>>> {
        Box::pin(async move {
            while self.buffered.len() < len {
                let chunk = self
                    .incoming
                    .recv()
                    .await
                    .map_err(|_| anyhow::anyhow!("the other end is gone"))?;
                self.buffered.extend_from_slice(&chunk);
            }
            let rest = self.buffered.split_off(len);
            let wanted = std::mem::replace(&mut self.buffered, rest);
            Ok(wanted)
        })
    }
}

async fn read_frame(wire: &mut End) -> Result<Vec<u8>> {
    let header = wire.read_exact(4).await?;
    let len = u32::from_be_bytes(header.as_slice().try_into().expect("four bytes")) as usize;
    wire.read_exact(len).await
}

async fn write_frame(wire: &mut End, packet: &[u8]) -> Result<()> {
    let mut frame = (packet.len() as u32).to_be_bytes().to_vec();
    frame.extend_from_slice(packet);
    wire.write_all(frame).await
}

fn put_string(out: &mut Vec<u8>, text: &[u8]) {
    out.extend_from_slice(&(text.len() as u32).to_be_bytes());
    out.extend_from_slice(text);
}

/// `SSH_FXP_ATTRS`: size and permissions, the way OpenSSH sends them.
fn attrs(dir: bool, size: u64) -> Vec<u8> {
    let mode: u32 = if dir { 0o040755 } else { 0o100644 };
    let mut out = (0x1u32 | 0x4).to_be_bytes().to_vec(); // size | permissions
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(&mode.to_be_bytes());
    out
}

fn handle(id: u32, handle: &str) -> Vec<u8> {
    let mut out = vec![102];
    out.extend_from_slice(&id.to_be_bytes());
    put_string(&mut out, handle.as_bytes());
    out
}

fn status(id: u32, code: u32, message: &str) -> Vec<u8> {
    let mut out = vec![101];
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&code.to_be_bytes());
    put_string(&mut out, message.as_bytes());
    put_string(&mut out, b"");
    out
}

/// `statvfs@openssh.com` version 2's answer: eleven `u64`s in its own order.
/// `f_frsize` is 1, so `available_bytes` is `bavail` as given.
fn statvfs_reply(id: u32, available: u64) -> Vec<u8> {
    let values: [u64; 11] = [1, 1, available, available, available, 0, 0, 0, 1, 0, 255];
    let mut out = vec![201];
    out.extend_from_slice(&id.to_be_bytes());
    for value in values {
        out.extend_from_slice(&value.to_be_bytes());
    }
    out
}

/// The little filesystem the fake serves, and what it was asked.
#[derive(Default)]
struct ServerState {
    files: HashMap<String, Vec<u8>>,
    dirs: HashSet<String>,
    /// The extension names the server advertises in `SSH_FXP_VERSION`.
    advertise: Vec<String>,
    /// Every request, in arrival order: packet kind and its first string.
    log: Vec<(u8, String)>,
    /// Every packet kind, in order: what the wire says, as a sequence.
    packets: Vec<u8>,
    /// Every `WRITE`, as (path, offset, length).
    writes: Vec<(String, u64, usize)>,
    /// Zero-based `WRITE` number that answers "no space".
    fail_write_at: Option<usize>,
    /// How many upcoming connections drop right after the handshake, which is a
    /// session that died without a protocol error.
    drop_connections: usize,
    /// What `statvfs` reports as available.
    free_bytes: u64,
    /// Upload handle → the path it writes.
    uploads: HashMap<String, String>,
}

/// A scripted alias: its filesystem, its extensions, and which connections died.
#[derive(Clone)]
struct Server {
    state: Arc<Mutex<ServerState>>,
    connections: Arc<AtomicUsize>,
}

impl Server {
    fn new() -> Self {
        let mut dirs = HashSet::new();
        dirs.insert("/srv".to_owned());
        dirs.insert("/srv/dir".to_owned());
        Self {
            state: Arc::new(Mutex::new(ServerState {
                dirs,
                free_bytes: 1 << 30,
                ..ServerState::default()
            })),
            connections: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn advertise(&self, extension: &str) {
        self.state
            .lock()
            .expect("the fake's state")
            .advertise
            .push(extension.to_owned());
    }

    fn set_free(&self, bytes: u64) {
        self.state.lock().expect("the fake's state").free_bytes = bytes;
    }

    fn fail_write_at(&self, index: usize) {
        self.state.lock().expect("the fake's state").fail_write_at = Some(index);
    }

    fn clear_failures(&self) {
        self.state.lock().expect("the fake's state").fail_write_at = None;
    }

    fn drop_next_connection(&self) {
        self.state
            .lock()
            .expect("the fake's state")
            .drop_connections += 1;
    }

    fn add_file(&self, path: &str, bytes: &[u8]) {
        self.state
            .lock()
            .expect("the fake's state")
            .files
            .insert(path.to_owned(), bytes.to_vec());
    }

    fn file(&self, path: &str) -> Option<Vec<u8>> {
        self.state
            .lock()
            .expect("the fake's state")
            .files
            .get(path)
            .cloned()
    }

    fn has_dir(&self, path: &str) -> bool {
        self.state
            .lock()
            .expect("the fake's state")
            .dirs
            .contains(path)
    }

    fn packets(&self) -> Vec<u8> {
        self.state.lock().expect("the fake's state").packets.clone()
    }

    fn writes(&self) -> Vec<(String, u64, usize)> {
        self.state.lock().expect("the fake's state").writes.clone()
    }

    fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    /// One connection's worth of session, with the server task running beside it.
    async fn session(&self) -> Result<Session> {
        self.connections.fetch_add(1, Ordering::SeqCst);
        let (client, server) = pipe();
        compio::runtime::spawn(serve(server, Arc::clone(&self.state))).detach();
        Session::from_wire(Box::new(client)).await
    }
}

/// A cursor over a request's payload: the client wrote it, so malformed fields
/// are a test bug rather than a hostile server.
struct Body<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Body<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn u32(&mut self) -> u32 {
        let value = u32::from_be_bytes(
            self.bytes[self.at..self.at + 4]
                .try_into()
                .expect("four bytes"),
        );
        self.at += 4;
        value
    }

    fn u64(&mut self) -> u64 {
        let value = u64::from_be_bytes(
            self.bytes[self.at..self.at + 8]
                .try_into()
                .expect("eight bytes"),
        );
        self.at += 8;
        value
    }

    fn string(&mut self) -> String {
        let len = u32::from_be_bytes(
            self.bytes[self.at..self.at + 4]
                .try_into()
                .expect("four bytes"),
        ) as usize;
        self.at += 4;
        let out = String::from_utf8_lossy(&self.bytes[self.at..self.at + len]).into_owned();
        self.at += len;
        out
    }

    /// `WRITE`'s payload is not text.
    fn bytes(&mut self) -> Vec<u8> {
        let len = u32::from_be_bytes(
            self.bytes[self.at..self.at + 4]
                .try_into()
                .expect("four bytes"),
        ) as usize;
        self.at += 4;
        let out = self.bytes[self.at..self.at + len].to_vec();
        self.at += len;
        out
    }
}

fn move_path(state: &mut ServerState, from: &str, to: &str, replace: bool) -> bool {
    if !replace && (state.files.contains_key(to) || state.dirs.contains(to)) {
        return false;
    }
    if replace {
        state.files.remove(to);
        state.dirs.remove(to);
    }
    if let Some(bytes) = state.files.remove(from) {
        state.files.insert(to.to_owned(), bytes);
        return true;
    }
    if state.dirs.remove(from) {
        state.dirs.insert(to.to_owned());
        return true;
    }
    false
}

/// Answer one request the way an OpenSSH server would.
fn respond(kind: u8, id: u32, payload: &[u8], shared: &Arc<Mutex<ServerState>>) -> Vec<u8> {
    let mut state = shared.lock().expect("the fake's state");
    let mut body = Body::new(payload);
    state.packets.push(kind);
    match kind {
        OPEN => {
            let path = body.string();
            let flags = body.u32();
            let _attrs = body.u32();
            state.log.push((kind, path.clone()));
            const FXF_WRITE: u32 = 0x0000_0002;
            if flags & FXF_WRITE != 0 {
                // `WRITE|CREAT|TRUNC`: the part is created (or truncated) now,
                // and every `WRITE` lands in it immediately — which is what a
                // failed transfer's part being on the server means.
                state.files.insert(path.clone(), Vec::new());
                let upload_handle = format!("upload:{id}");
                state.uploads.insert(upload_handle.clone(), path);
                handle(id, &upload_handle)
            } else if state.files.contains_key(&path) {
                handle(id, &path)
            } else {
                status(id, 2, "No such file")
            }
        }
        CLOSE => {
            let opened = body.string();
            state.log.push((kind, opened.clone()));
            state.uploads.remove(&opened);
            status(id, 0, "")
        }
        READ => {
            let opened = body.string();
            let offset = body.u64() as usize;
            let wanted = body.u32() as usize;
            state.log.push((kind, opened.clone()));
            let Some(bytes) = state.files.get(&opened) else {
                return status(id, 2, "No such file");
            };
            if offset >= bytes.len() {
                return status(id, 1, "eof");
            }
            let end = (offset + wanted).min(bytes.len());
            let mut out = vec![103];
            out.extend_from_slice(&id.to_be_bytes());
            put_string(&mut out, &bytes[offset..end]);
            out
        }
        WRITE => {
            let opened = body.string();
            let offset = body.u64();
            let bytes = body.bytes();
            let index = state.writes.len();
            let path = state.uploads.get(&opened).cloned().unwrap_or_default();
            state.writes.push((path.clone(), offset, bytes.len()));
            state.log.push((kind, opened.clone()));
            if state.fail_write_at == Some(index) {
                return status(id, 4, "no space");
            }
            let Some(path) = state.uploads.get(&opened).cloned() else {
                return status(id, 2, "No such file");
            };
            let file = state.files.entry(path).or_default();
            let end = offset as usize + bytes.len();
            if file.len() < end {
                file.resize(end, 0);
            }
            file[offset as usize..end].copy_from_slice(&bytes);
            status(id, 0, "")
        }
        REMOVE => {
            let path = body.string();
            state.log.push((kind, path.clone()));
            if state.files.remove(&path).is_some() {
                status(id, 0, "")
            } else {
                status(id, 2, "No such file")
            }
        }
        RMDIR => {
            let path = body.string();
            state.log.push((kind, path.clone()));
            if state.dirs.remove(&path) {
                status(id, 0, "")
            } else {
                status(id, 2, "No such directory")
            }
        }
        STAT => {
            let path = body.string();
            state.log.push((kind, path.clone()));
            if let Some(bytes) = state.files.get(&path) {
                let size = bytes.len() as u64;
                let mut out = vec![105];
                out.extend_from_slice(&id.to_be_bytes());
                out.extend_from_slice(&attrs(false, size));
                out
            } else if state.dirs.contains(&path) {
                let mut out = vec![105];
                out.extend_from_slice(&id.to_be_bytes());
                out.extend_from_slice(&attrs(true, 0));
                out
            } else {
                status(id, 2, "No such file")
            }
        }
        RENAME => {
            // Plain `SSH_FXP_RENAME` fails when the destination exists — the
            // no-clobber publish the queue's "keep both" answer relies on.
            let from = body.string();
            let to = body.string();
            state.log.push((kind, from.clone()));
            if move_path(&mut state, &from, &to, false) {
                status(id, 0, "")
            } else if state.files.contains_key(&to) || state.dirs.contains(&to) {
                status(id, 4, "Failure")
            } else {
                status(id, 2, "No such file")
            }
        }
        EXTENDED => {
            let extension = body.string();
            state.log.push((kind, extension.clone()));
            match extension.as_str() {
                POSIX_RENAME => {
                    let from = body.string();
                    let to = body.string();
                    if move_path(&mut state, &from, &to, true) {
                        status(id, 0, "")
                    } else {
                        status(id, 2, "No such file")
                    }
                }
                STATVFS => {
                    let _path = body.string();
                    statvfs_reply(id, state.free_bytes)
                }
                _ => status(id, 8, "unsupported extension"),
            }
        }
        other => status(id, 8, &format!("unsupported packet {other}")),
    }
}

/// Answer whatever the client sends, the way an OpenSSH server would.
async fn serve(mut wire: End, state: Arc<Mutex<ServerState>>) {
    let Ok(init) = read_frame(&mut wire).await else {
        return;
    };
    assert_eq!(init[0], 1, "a session starts with SSH_FXP_INIT");
    let (extensions, drop) = {
        let mut state = state.lock().expect("the fake's state");
        let extensions = state.advertise.clone();
        let drop = if state.drop_connections > 0 {
            state.drop_connections -= 1;
            true
        } else {
            false
        };
        (extensions, drop)
    };
    let mut version = vec![2u8];
    version.extend_from_slice(&3u32.to_be_bytes());
    for name in extensions {
        put_string(&mut version, name.as_bytes());
        put_string(&mut version, b"1");
    }
    if write_frame(&mut wire, &version).await.is_err() {
        return;
    }
    if drop {
        // A session that died without a protocol error: the client's next
        // request finds the pipe closed.
        return;
    }
    loop {
        let Ok(frame) = read_frame(&mut wire).await else {
            return;
        };
        assert!(frame.len() >= 5, "a request carries an id");
        let kind = frame[0];
        let id = u32::from_be_bytes(frame[1..5].try_into().expect("four bytes of id"));
        let answer = respond(kind, id, &frame[5..], &state);
        if write_frame(&mut wire, &answer).await.is_err() {
            return;
        }
    }
}

// ---------------------------------------------------------------------------
// The transport under test.
// ---------------------------------------------------------------------------

fn dialer(servers: &[(&str, Server)]) -> Dialer {
    let servers: HashMap<String, Server> = servers
        .iter()
        .map(|(alias, server)| ((*alias).to_owned(), server.clone()))
        .collect();
    Rc::new(move |alias: &str| {
        let alias = alias.to_owned();
        let server = servers.get(&alias).cloned();
        Box::pin(async move {
            let Some(server) = server else {
                bail!("the test scripted no fake server for the alias {alias}")
            };
            server.session().await
        })
    })
}

/// A transport over a fake local filesystem and the given fake aliases.
fn transport_over(local: Box<dyn Transport>, servers: &[(&str, Server)]) -> SshTransport {
    SshTransport::with_parts(local, Some(dialer(servers)))
}

/// The common world: real local files, and two fake hosts.
fn world() -> (SshTransport, Server, Server) {
    let a = Server::new();
    let b = Server::new();
    let transport = transport_over(
        Box::new(LocalTransport::new()),
        &[("a", a.clone()), ("b", b.clone())],
    );
    (transport, a, b)
}

fn local(path: impl Into<PathBuf>) -> Location {
    Location::local(path)
}

fn remote(alias: &str, path: &str) -> Location {
    Location {
        backend: BackendId::Ssh(alias.to_owned()),
        path: PathBuf::from(path),
    }
}

fn request(job: u64, source: Location, part: Location, direction: Direction) -> OpenRequest {
    OpenRequest {
        job: JobId::new(job),
        source,
        part,
        direction,
    }
}

/// Step an op to `Done`, finish it, and report each segment's byte count.
async fn steps(op: &mut Box<dyn Op>, buf: &mut [u8]) -> Result<Vec<u64>> {
    let mut wrote = Vec::new();
    loop {
        match op.step(buf).await? {
            Segment::Done => {
                op.finish().await?;
                return Ok(wrote);
            }
            Segment::Wrote(0) => bail!("the op claimed a zero-byte segment"),
            Segment::Wrote(n) => wrote.push(n),
        }
    }
}

fn content(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|index| seed.wrapping_add((index % 251) as u8))
        .collect()
}

// ---------------------------------------------------------------------------
// The tests.
// ---------------------------------------------------------------------------

#[compio::test]
async fn an_upload_writes_one_packet_per_segment_and_publishes_with_a_rename() {
    let (mut transport, a, _b) = world();
    let dir = tempfile::tempdir().expect("a temp dir");
    let source = dir.path().join("notes.txt");
    let bytes = content(CHUNK * 2 + 100, 1);
    std::fs::write(&source, &bytes).expect("write the source");

    let part = remote("a", "/srv/dir/notes.txt.part");
    let real = remote("a", "/srv/dir/notes.txt");
    let mut op = transport
        .open(request(1, local(&source), part.clone(), Direction::Upload))
        .await
        .expect("open the upload");
    assert_eq!(
        a.file("/srv/dir/notes.txt.part"),
        Some(Vec::new()),
        "the part is created before the first segment"
    );

    let mut buf = vec![0u8; CHUNK];
    let wrote = steps(&mut op, &mut buf).await.expect("the upload");
    assert_eq!(wrote, [CHUNK as u64, CHUNK as u64, 100]);
    assert_eq!(
        a.packets(),
        [OPEN, WRITE, WRITE, WRITE, CLOSE],
        "one WRITE per segment and one CLOSE at the end"
    );
    assert_eq!(
        a.writes()
            .iter()
            .map(|(path, offset, len)| (path.as_str(), *offset, *len))
            .collect::<Vec<_>>(),
        [
            ("/srv/dir/notes.txt.part", 0, CHUNK),
            ("/srv/dir/notes.txt.part", CHUNK as u64, CHUNK),
            ("/srv/dir/notes.txt.part", 2 * CHUNK as u64, 100),
        ],
        "the writes walk the part's offsets, one packet each"
    );

    transport
        .publish(&part, &real, false)
        .await
        .expect("publish");
    assert_eq!(a.file("/srv/dir/notes.txt"), Some(bytes));
    assert_eq!(a.file("/srv/dir/notes.txt.part"), None, "the part is gone");
    assert_eq!(
        a.packets(),
        [OPEN, WRITE, WRITE, WRITE, CLOSE, RENAME],
        "publishing is one rename after the close"
    );
    assert_eq!(
        a.connections(),
        1,
        "open, every segment, finish and publish share one session"
    );
}

#[compio::test]
async fn a_download_reads_a_segment_at_a_time_into_a_local_part() {
    let (mut transport, a, _b) = world();
    let bytes = content(CHUNK + 123, 2);
    a.add_file("/srv/notes.txt", &bytes);
    let dir = tempfile::tempdir().expect("a temp dir");
    let part = dir.path().join("notes.txt.part");
    let real = dir.path().join("notes.txt");

    let mut op = transport
        .open(request(
            1,
            remote("a", "/srv/notes.txt"),
            local(&part),
            Direction::Download,
        ))
        .await
        .expect("open the download");
    let mut buf = vec![0u8; CHUNK];
    let wrote = steps(&mut op, &mut buf).await.expect("the download");
    assert_eq!(wrote, [CHUNK as u64, 123]);
    assert_eq!(std::fs::read(&part).expect("the part"), bytes);
    assert_eq!(
        a.packets(),
        [OPEN, READ, READ, READ, CLOSE],
        "two data reads, the EOF read, then the handle's close"
    );

    transport
        .publish(&local(&part), &local(&real), false)
        .await
        .expect("publish the local part");
    assert_eq!(std::fs::read(&real).expect("the published file"), bytes);
    assert!(!part.exists(), "the local part is gone");
    assert_eq!(
        a.packets(),
        [OPEN, READ, READ, READ, CLOSE],
        "a download's publish is local: the host sees no rename"
    );
}

#[compio::test]
async fn a_cross_host_copy_streams_a_to_b_with_one_byte_count() {
    let (mut transport, a, b) = world();
    let bytes = content(CHUNK + 512, 3);
    a.add_file("/srv/source.bin", &bytes);
    let part = remote("b", "/srv/dir/source.bin.part");
    let real = remote("b", "/srv/dir/source.bin");

    let mut op = transport
        .open(request(
            1,
            remote("a", "/srv/source.bin"),
            part.clone(),
            Direction::CrossHost,
        ))
        .await
        .expect("open the stream");
    assert_eq!(
        b.file("/srv/dir/source.bin"),
        None,
        "the real name does not exist while the bytes move"
    );

    let mut buf = vec![0u8; CHUNK];
    let wrote = steps(&mut op, &mut buf).await.expect("the stream");
    assert_eq!(
        wrote,
        [CHUNK as u64, 512],
        "one progress figure: one Wrote a segment"
    );
    assert_eq!(
        wrote.iter().sum::<u64>(),
        bytes.len() as u64,
        "the byte count is the file's, once"
    );
    assert_eq!(
        a.packets(),
        [OPEN, READ, READ, READ, CLOSE],
        "the source is read in chunks and its read handle is closed"
    );
    assert_eq!(
        b.packets(),
        [OPEN, WRITE, WRITE, CLOSE],
        "the destination writes the same chunks under the part name"
    );
    assert_eq!(a.connections(), 1);
    assert_eq!(b.connections(), 1);
    assert_eq!(b.file("/srv/dir/source.bin.part"), Some(bytes.clone()));
    assert_eq!(b.file("/srv/dir/source.bin"), None);

    transport
        .publish(&part, &real, false)
        .await
        .expect("publish");
    assert_eq!(b.file("/srv/dir/source.bin"), Some(bytes));
    assert_eq!(b.file("/srv/dir/source.bin.part"), None);
}

#[compio::test]
async fn a_same_host_remote_copy_holds_two_handles_on_one_session() {
    let (mut transport, a, _b) = world();
    let bytes = content(CHUNK + 7, 4);
    a.add_file("/srv/source.bin", &bytes);
    let part = remote("a", "/srv/dir/source.bin.part");

    let mut op = transport
        .open(request(
            1,
            remote("a", "/srv/source.bin"),
            part,
            Direction::Copy,
        ))
        .await
        .expect("open the copy");
    let mut buf = vec![0u8; CHUNK];
    let wrote = steps(&mut op, &mut buf).await.expect("the copy");
    assert_eq!(wrote, [CHUNK as u64, 7]);
    assert_eq!(
        a.packets(),
        [OPEN, OPEN, READ, WRITE, READ, WRITE, READ, CLOSE, CLOSE],
        "a read and a write per segment, then both handles close"
    );
    assert_eq!(
        a.connections(),
        1,
        "both ends are the same alias, so the copy uses its one session"
    );
    assert_eq!(a.file("/srv/dir/source.bin.part"), Some(bytes));
}

#[compio::test]
async fn a_no_replace_publish_fails_when_the_name_appeared_and_keeps_the_part() {
    let (mut transport, a, _b) = world();
    a.add_file("/srv/dir/keep.txt", b"old");
    a.add_file("/srv/dir/keep.txt.part", b"new");
    let err = transport
        .publish(
            &remote("a", "/srv/dir/keep.txt.part"),
            &remote("a", "/srv/dir/keep.txt"),
            false,
        )
        .await
        .expect_err("the name is taken");
    assert!(
        format!("{err:#}").contains("rename"),
        "the failure names the publish: {err:#}"
    );
    assert_eq!(
        a.file("/srv/dir/keep.txt"),
        Some(b"old".to_vec()),
        "the file that appeared is not clobbered"
    );
    assert_eq!(
        a.file("/srv/dir/keep.txt.part"),
        Some(b"new".to_vec()),
        "the part is left for the queue's retry"
    );
    assert_eq!(
        a.packets(),
        [RENAME, RENAME],
        "a no-replace publish is the plain packet, retried once on a fresh child"
    );
    assert_eq!(
        a.connections(),
        2,
        "one reconnect, exactly as the verb rule says"
    );
}

#[compio::test]
async fn a_replace_publish_uses_posix_rename_when_the_server_advertises_it() {
    let (mut transport, a, _b) = world();
    a.advertise(POSIX_RENAME);
    a.add_file("/srv/dir/keep.txt", b"old");
    a.add_file("/srv/dir/keep.txt.part", b"new");
    transport
        .publish(
            &remote("a", "/srv/dir/keep.txt.part"),
            &remote("a", "/srv/dir/keep.txt"),
            true,
        )
        .await
        .expect("replace");
    assert_eq!(a.file("/srv/dir/keep.txt"), Some(b"new".to_vec()));
    assert_eq!(a.file("/srv/dir/keep.txt.part"), None);
    assert_eq!(
        a.packets(),
        [EXTENDED],
        "the replace answer is the atomic posix-rename extension, not a check-then-rename"
    );
}

#[compio::test]
async fn a_discard_removes_the_part_and_a_missing_part_is_not_an_error() {
    let (mut transport, a, _b) = world();
    a.add_file("/srv/dir/x.part", b"half");
    transport
        .discard(&remote("a", "/srv/dir/x.part"))
        .await
        .expect("discard");
    assert_eq!(a.file("/srv/dir/x.part"), None);
    transport
        .discard(&remote("a", "/srv/dir/x.part"))
        .await
        .expect("a part that is already gone is discarded");
    assert_eq!(a.packets(), [REMOVE, REMOVE]);
    assert_eq!(
        a.connections(),
        1,
        "not-found is an answer: a discard does not spend a reconnect"
    );
}

#[compio::test]
async fn remove_deletes_a_file_with_remove_and_a_directory_with_rmdir() {
    let (mut transport, a, _b) = world();
    a.add_file("/srv/x.txt", b"x");
    transport
        .remove(&remote("a", "/srv/x.txt"))
        .await
        .expect("remove the file");
    transport
        .remove(&remote("a", "/srv/dir"))
        .await
        .expect("remove the directory");
    assert_eq!(
        a.packets(),
        [STAT, REMOVE, STAT, RMDIR],
        "v3 has no remove-whatever-is-there, so the stat decides the packet"
    );
    assert_eq!(a.file("/srv/x.txt"), None);
    assert!(!a.has_dir("/srv/dir"));
}

#[compio::test]
async fn stat_answers_meta_and_maps_not_found_to_none() {
    let (mut transport, a, _b) = world();
    a.add_file("/srv/dir/notes.txt", b"hello");
    let meta = transport
        .stat(&remote("a", "/srv/dir/notes.txt"))
        .await
        .expect("stat")
        .expect("the file is there");
    assert_eq!(meta.kind, Kind::File);
    assert_eq!(meta.len, 5);
    assert!(
        transport
            .stat(&remote("a", "/srv/dir/missing.txt"))
            .await
            .expect("stat")
            .is_none(),
        "a missing path is Ok(None), not an error"
    );
    assert_eq!(a.packets(), [STAT, STAT]);
    assert_eq!(
        a.connections(),
        1,
        "a missing path is an answer, not a dead session"
    );
}

#[compio::test]
async fn a_dead_session_is_replaced_once_and_the_verb_succeeds() {
    let (mut transport, a, _b) = world();
    a.add_file("/srv/dir/notes.txt", b"hello");
    a.drop_next_connection();
    let meta = transport
        .stat(&remote("a", "/srv/dir/notes.txt"))
        .await
        .expect("the second attempt succeeds")
        .expect("the file is there");
    assert_eq!(meta.len, 5);
    assert_eq!(a.connections(), 2, "exactly one reconnect");
}

#[compio::test]
async fn preflight_refuses_when_statvfs_says_there_is_no_room() {
    let (mut transport, a, _b) = world();
    a.advertise(STATVFS);
    a.set_free(1024);
    let err = transport
        .preflight(&remote("a", "/srv/dir"), 4096)
        .await
        .expect_err("1 KiB cannot hold 4 KiB");
    let said = format!("{err:#}");
    assert!(said.contains("not enough room"), "{said}");
    assert!(said.contains("1024"), "the message names the space: {said}");
    assert_eq!(a.packets(), [EXTENDED]);

    a.set_free(8192);
    transport
        .preflight(&remote("a", "/srv/dir"), 4096)
        .await
        .expect("8 KiB holds 4 KiB");
    assert_eq!(a.packets(), [EXTENDED, EXTENDED]);
}

#[compio::test]
async fn preflight_is_ok_when_the_extension_is_absent() {
    let (mut transport, a, _b) = world();
    transport
        .preflight(&remote("a", "/srv/dir"), u64::MAX)
        .await
        .expect("a server that cannot be asked does not refuse");
    assert!(
        a.packets().is_empty(),
        "nothing is asked of a server that did not advertise statvfs"
    );
    assert_eq!(a.connections(), 1);
}

#[compio::test]
async fn an_abort_releases_the_alias_and_leaves_the_part_to_the_queue() {
    let (mut transport, a, _b) = world();
    let dir = tempfile::tempdir().expect("a temp dir");
    let source = dir.path().join("notes.txt");
    let bytes = content(CHUNK + 10, 5);
    std::fs::write(&source, &bytes).expect("write the source");
    let part = remote("a", "/srv/dir/notes.txt.part");
    let mut op = transport
        .open(request(1, local(&source), part.clone(), Direction::Upload))
        .await
        .expect("open the upload");
    let mut buf = vec![0u8; CHUNK];
    assert_eq!(
        op.step(&mut buf).await.expect("a segment"),
        Segment::Wrote(CHUNK as u64)
    );

    // `Op::abort` is synchronous and must not publish.
    op.abort();
    drop(op);
    assert_eq!(
        a.file("/srv/dir/notes.txt.part"),
        Some(bytes[..CHUNK].to_vec())
    );
    assert!(
        !a.packets().contains(&RENAME),
        "an abort never publishes: {:?}",
        a.packets()
    );

    // The session was released with the op; the queue's discard reconnects.
    transport
        .discard(&part)
        .await
        .expect("the queue discards the part");
    assert_eq!(a.file("/srv/dir/notes.txt.part"), None);
    assert_eq!(
        a.connections(),
        2,
        "abort released the child synchronously; discard opened a new one"
    );
}

/// The end-to-end failure rule, driven by the real queue: a segment that fails
/// fails the job, never publishes, and a retry restarts the file from byte zero.
#[compio::test]
async fn a_failed_segment_fails_the_job_and_a_retry_publishes_the_file() {
    let (mut transport, a, _b) = world();
    a.fail_write_at(1);
    let dir = tempfile::tempdir().expect("a temp dir");
    let source = dir.path().join("notes.txt");
    let bytes = content(CHUNK * 2 + 7, 6);
    std::fs::write(&source, &bytes).expect("write the source");

    let mut queue = Queue::new();
    let id = queue
        .copy(vec![local(&source)], remote("a", "/srv/dir"))
        .expect("enqueue");
    let mut events = Vec::new();
    for _ in 0..200 {
        if !queue.may_progress() {
            break;
        }
        events.extend(queue.step(&mut transport).await);
    }
    let failed = events.iter().find_map(|event| match event {
        Event::Failed { error, .. } => Some(error.clone()),
        _ => None,
    });
    let failed = failed.expect("the job failed");
    assert!(
        failed.contains("no space"),
        "the server's reason survives: {failed}"
    );
    assert_eq!(queue.job(id).expect("the job").state(), State::Failed);
    assert_eq!(
        a.file("/srv/dir/notes.txt"),
        None,
        "a failed transfer never puts anything under the real name"
    );
    assert_eq!(
        a.file("/srv/dir/notes.txt.part"),
        Some(bytes[..CHUNK].to_vec()),
        "the part is kept for a retry"
    );

    a.clear_failures();
    queue.retry(id).expect("retry");
    for _ in 0..200 {
        if !queue.may_progress() {
            break;
        }
        events.extend(queue.step(&mut transport).await);
    }
    assert_eq!(queue.job(id).expect("the job").state(), State::Done);
    assert_eq!(a.file("/srv/dir/notes.txt"), Some(bytes));
    assert_eq!(a.file("/srv/dir/notes.txt.part"), None);
}

// ---------------------------------------------------------------------------
// Routing: the local half never leaves the queue's own transport.
// ---------------------------------------------------------------------------

/// A local transport that records what it was asked and never touches a file.
struct RecordingLocal {
    calls: Rc<RefCell<Vec<&'static str>>>,
}

impl RecordingLocal {
    fn new(calls: Rc<RefCell<Vec<&'static str>>>) -> Self {
        Self { calls }
    }
}

struct NoopOp;

impl Op for NoopOp {
    fn step<'a>(&'a mut self, _buf: &'a mut [u8]) -> BoxFuture<'a, Result<Segment>> {
        Box::pin(async move { Ok(Segment::Done) })
    }

    fn finish<'a>(&'a mut self) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { Ok(()) })
    }

    fn abort(&mut self) {}
}

impl Transport for RecordingLocal {
    fn stat<'a>(&'a mut self, _at: &'a Location) -> BoxFuture<'a, Result<Option<dtr_vfs::Meta>>> {
        self.calls.borrow_mut().push("stat");
        Box::pin(async move { Ok(None) })
    }

    fn open<'a>(&'a mut self, _request: OpenRequest) -> BoxFuture<'a, Result<Box<dyn Op>>> {
        self.calls.borrow_mut().push("open");
        Box::pin(async move { Ok(Box::new(NoopOp) as Box<dyn Op>) })
    }

    fn publish<'a>(
        &'a mut self,
        _part: &'a Location,
        _destination: &'a Location,
        _overwrite: bool,
    ) -> BoxFuture<'a, Result<()>> {
        self.calls.borrow_mut().push("publish");
        Box::pin(async move { Ok(()) })
    }

    fn discard<'a>(&'a mut self, _part: &'a Location) -> BoxFuture<'a, Result<()>> {
        self.calls.borrow_mut().push("discard");
        Box::pin(async move { Ok(()) })
    }

    fn remove<'a>(&'a mut self, _at: &'a Location) -> BoxFuture<'a, Result<()>> {
        self.calls.borrow_mut().push("remove");
        Box::pin(async move { Ok(()) })
    }

    fn preflight<'a>(
        &'a mut self,
        _destination: &'a Location,
        _bytes: u64,
    ) -> BoxFuture<'a, Result<()>> {
        self.calls.borrow_mut().push("preflight");
        Box::pin(async move { Ok(()) })
    }
}

#[compio::test]
async fn a_local_to_local_job_stays_on_the_local_transport() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let ssh_must_not_be_asked: Dialer = Rc::new(|_: &str| {
        Box::pin(async move { bail!("the ssh side was asked about a local-to-local transfer") })
    });
    let mut transport = SshTransport::with_parts(
        Box::new(RecordingLocal::new(Rc::clone(&calls))),
        Some(ssh_must_not_be_asked),
    );

    let source = local("/tmp/dtr/source.txt");
    let part = local("/tmp/dtr/source.txt.part");
    let real = local("/tmp/dtr/source.txt");
    assert!(
        transport.stat(&source).await.expect("stat").is_none(),
        "the local transport answered"
    );
    let op = transport
        .open(request(1, source.clone(), part.clone(), Direction::Copy))
        .await
        .expect("open");
    drop(op);
    transport
        .publish(&part, &real, false)
        .await
        .expect("publish");
    transport.discard(&part).await.expect("discard");
    transport.remove(&source).await.expect("remove");
    transport
        .preflight(&local("/tmp/dtr"), 0)
        .await
        .expect("preflight");

    assert_eq!(
        calls.borrow().clone(),
        vec!["stat", "open", "publish", "discard", "remove", "preflight"],
        "every local verb went to the queue's local transport"
    );
}

#[compio::test]
async fn a_remote_request_never_touches_the_local_transport() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let a = Server::new();
    let mut transport = transport_over(
        Box::new(RecordingLocal::new(Rc::clone(&calls))),
        &[("a", a.clone())],
    );
    let meta = transport
        .stat(&remote("a", "/srv/dir"))
        .await
        .expect("stat")
        .expect("the directory is there");
    assert_eq!(meta.kind, Kind::Dir);
    assert!(
        calls.borrow().is_empty(),
        "the local transport was not asked"
    );
    assert_eq!(a.packets(), [STAT]);
}

#[compio::test]
async fn a_download_respects_a_segment_buffer_smaller_than_the_codec_chunk() {
    // The queue can be built with a segment size of its own (its tests use a
    // handful of bytes); the codec's read always asks for a whole CHUNK, so the
    // adapter has to hold the remainder rather than claim a segment bigger than
    // the buffer it was given.
    let (mut transport, a, _b) = world();
    let bytes = content(20, 7);
    a.add_file("/srv/notes.txt", &bytes);
    let dir = tempfile::tempdir().expect("a temp dir");
    let part = dir.path().join("notes.txt.part");
    let mut op = transport
        .open(request(
            1,
            remote("a", "/srv/notes.txt"),
            local(&part),
            Direction::Download,
        ))
        .await
        .expect("open the download");
    let mut buf = [0u8; 7];
    let wrote = steps(&mut op, &mut buf).await.expect("the download");
    assert_eq!(wrote, [7, 7, 6], "no segment claims more than its buffer");
    assert_eq!(std::fs::read(&part).expect("the part"), bytes);
    assert_eq!(
        a.packets(),
        [OPEN, READ, READ, CLOSE],
        "one READ fetches the chunk, the EOF READ ends it"
    );
}

#[compio::test]
async fn the_queue_downloads_from_a_host_and_publishes_the_local_name() {
    let (mut transport, a, _b) = world();
    let bytes = content(CHUNK + 33, 8);
    a.add_file("/srv/dir/remote.bin", &bytes);
    let dir = tempfile::tempdir().expect("a temp dir");
    let mut queue = Queue::new();
    let id = queue
        .copy(vec![remote("a", "/srv/dir/remote.bin")], local(dir.path()))
        .expect("enqueue");
    for _ in 0..200 {
        if !queue.may_progress() {
            break;
        }
        queue.step(&mut transport).await;
    }
    assert_eq!(queue.job(id).expect("the job").state(), State::Done);
    assert_eq!(
        std::fs::read(dir.path().join("remote.bin")).expect("the downloaded file"),
        bytes
    );
    assert!(
        !dir.path().join("remote.bin.part").exists(),
        "the local part was published and is gone"
    );
    assert_eq!(
        a.packets(),
        [STAT, OPEN, READ, READ, READ, CLOSE],
        "the source is stat'ed once, read in chunks, and the publish is local"
    );
}
