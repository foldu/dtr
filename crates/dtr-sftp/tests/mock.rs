//! A mock SFTP server on an in-memory wire.
//!
//! It speaks the same version 3 framing the client does — one packet at a time,
//! a length first — and it is the only test of the codec that does not need a
//! host, an ssh binary or a server. It keeps a little filesystem so the write
//! verbs can be checked against their effect, and a log of what it was asked so
//! a test can assert *which* packet was chosen, not only that it worked.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use dtr_sftp::{CHUNK, POSIX_RENAME, STATVFS, Session, StatVfs, Wire};

type BoxFuture<'a, T> = Pin<Box<dyn std::future::Future<Output = T> + 'a>>;

/// One end of an in-memory byte pipe.
///
/// The two ends are the same type with the channels swapped, so the mock server
/// and the client can be written against one `Wire` and meet in the middle.
struct End {
    out: async_channel::Sender<Vec<u8>>,
    incoming: async_channel::Receiver<Vec<u8>>,
    /// What arrived beyond the bytes the last `read_exact` wanted.
    buffered: Vec<u8>,
}

fn pair() -> (End, End) {
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
    fn write_all<'a>(&'a mut self, bytes: Vec<u8>) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.out
                .send(bytes)
                .await
                .map_err(|_| anyhow::anyhow!("the other end is gone"))
        })
    }

    fn read_exact<'a>(&'a mut self, len: usize) -> BoxFuture<'a, Result<Vec<u8>>> {
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

/// The whole of `read_frame`, for the server's side.
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

/// `SSH_FXP_ATTRS` with permissions, and a size when it is a file.
fn attrs(dir: bool, size: u64) -> Vec<u8> {
    let mode: u32 = if dir { 0o040755 } else { 0o100644 };
    let mut out = Vec::new();
    // One flags word, then the fields it names, in its own order.
    out.extend_from_slice(&(0x1u32 | 0x4).to_be_bytes()); // size | permissions
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(&mode.to_be_bytes());
    out
}

/// `SSH_FXP_NAME` from `(name, dir, size)` triples.
fn name(id: u32, files: &[(String, bool, u64)]) -> Vec<u8> {
    let mut out = vec![104];
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&(files.len() as u32).to_be_bytes());
    for (file, dir, size) in files {
        put_string(&mut out, file.as_bytes());
        put_string(&mut out, file.as_bytes());
        out.extend_from_slice(&attrs(*dir, *size));
    }
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

fn handle(id: u32, handle: &str) -> Vec<u8> {
    let mut out = vec![102];
    out.extend_from_slice(&id.to_be_bytes());
    put_string(&mut out, handle.as_bytes());
    out
}

/// `statvfs@openssh.com` version 2's answer: eleven `u64`s, in its order.
fn statvfs_reply(id: u32) -> Vec<u8> {
    let values: [u64; 11] = [4096, 4096, 1000, 400, 300, 0, 0, 0, 1, 0, 255];
    let mut out = vec![201];
    out.extend_from_slice(&id.to_be_bytes());
    for value in values {
        out.extend_from_slice(&value.to_be_bytes());
    }
    out
}

const BODY: &[u8] = b"hello from the mock";

/// The little filesystem the mock serves, and what it was asked.
struct State {
    files: HashMap<String, Vec<u8>>,
    dirs: HashSet<String>,
    /// The `SSH_FXP_VERSION` extension pairs the server offers.
    advertise: Vec<(String, String)>,
    /// Every request, as (packet type, the first string it carried).
    log: Vec<(u8, String)>,
    /// Every `WRITE` in arrival order: (offset, length).
    writes: Vec<(u64, usize)>,
    /// Zero-based `WRITE` index the server fails, for a mid-upload failure.
    fail_write_at: Option<usize>,
    /// Handles that were closed, in order.
    closed: Vec<String>,
    /// Handles that have already answered one `READDIR` batch.
    listed: HashSet<String>,
    /// Upload handle -> (path, bytes so far), committed on close.
    uploads: HashMap<String, (String, Vec<u8>)>,
    /// The last mode a `SETSTAT` carried, per path.
    modes: HashMap<String, u32>,
}

impl State {
    fn new() -> Self {
        let mut files = HashMap::new();
        files.insert("/home/user/notes.txt".to_owned(), BODY.to_vec());
        let mut dirs = HashSet::new();
        dirs.insert("/home/user".to_owned());
        dirs.insert("/home/user/src".to_owned());
        Self {
            files,
            dirs,
            advertise: Vec::new(),
            log: Vec::new(),
            writes: Vec::new(),
            fail_write_at: None,
            closed: Vec::new(),
            listed: HashSet::new(),
            uploads: HashMap::new(),
            modes: HashMap::new(),
        }
    }
}

/// A cursor over a request's payload: the client wrote it, so these cannot fail.
struct Body<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Body<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
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

    /// The next string field, but as bytes: `WRITE`'s payload is not text.
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

/// The directory containing `path`.
fn parent_of(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The last component of `path`.
fn name_of(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn push(state: &mut State, kind: u8, first: String) {
    state.log.push((kind, first));
}

/// Answer one request the way an OpenSSH server would.
fn respond(kind: u8, id: u32, payload: &[u8], shared: &Arc<Mutex<State>>) -> Vec<u8> {
    let mut state = shared.lock().expect("the mock's state");
    let mut body = Body::new(payload);
    match kind {
        11 => {
            // OPENDIR
            let path = body.string();
            push(&mut state, kind, path.clone());
            // A fresh open starts a fresh listing, even for a directory that
            // was listed before.
            state.listed.remove(&format!("dir:{path}"));
            handle(id, &format!("dir:{path}"))
        }
        12 => {
            // READDIR: one batch per handle, then EOF.
            let opened = body.string();
            let path = opened.strip_prefix("dir:").unwrap_or(&opened).to_owned();
            push(&mut state, kind, opened.clone());
            if !state.listed.insert(opened) {
                return status(id, 1, "eof");
            }
            let mut entries: Vec<(String, bool, u64)> = state
                .files
                .iter()
                .filter(|(file, _)| parent_of(file) == path)
                .map(|(file, bytes)| (name_of(file), false, bytes.len() as u64))
                .collect();
            entries.extend(
                state
                    .dirs
                    .iter()
                    .filter(|dir| parent_of(dir) == path && **dir != path)
                    .map(|dir| (name_of(dir), true, 0)),
            );
            name(id, &entries)
        }
        16 => {
            // REALPATH
            push(&mut state, kind, body.string());
            name(id, &[(".".to_owned(), true, 0)])
        }
        17 => {
            // STAT
            let path = body.string();
            push(&mut state, kind, path.clone());
            if let Some(bytes) = state.files.get(&path) {
                let mut out = vec![105];
                out.extend_from_slice(&id.to_be_bytes());
                out.extend_from_slice(&attrs(false, bytes.len() as u64));
                out
            } else if state.dirs.contains(&path) {
                let mut out = vec![105];
                out.extend_from_slice(&id.to_be_bytes());
                out.extend_from_slice(&attrs(true, 0));
                out
            } else {
                status(id, 2, "no such file")
            }
        }
        3 => {
            // OPEN: reads are handles named by their path, writes are uploads.
            let path = body.string();
            let flags = body.u32();
            let _attrs = body.u32();
            push(&mut state, kind, path.clone());
            const FXF_WRITE: u32 = 0x0000_0002;
            if flags & FXF_WRITE != 0 {
                let upload_handle = format!("upload:{id}");
                state
                    .uploads
                    .insert(upload_handle.clone(), (path, Vec::new()));
                handle(id, &upload_handle)
            } else if state.files.contains_key(&path) {
                handle(id, &path)
            } else {
                status(id, 2, "no such file")
            }
        }
        5 => {
            // READ
            let opened = body.string();
            let offset = body.u64() as usize;
            let wanted = body.u32() as usize;
            push(&mut state, kind, opened.clone());
            let Some(bytes) = state.files.get(&opened) else {
                return status(id, 2, "no such file");
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
        6 => {
            // WRITE
            let opened = body.string();
            let offset = body.u64();
            let bytes = body.bytes();
            let index = state.writes.len();
            state.writes.push((offset, bytes.len()));
            push(&mut state, kind, opened.clone());
            if state.fail_write_at == Some(index) {
                return status(id, 4, "no space");
            }
            let Some((_, file)) = state.uploads.get_mut(&opened) else {
                return status(id, 2, "no such file");
            };
            let end = offset as usize + bytes.len();
            if file.len() < end {
                file.resize(end, 0);
            }
            file[offset as usize..end].copy_from_slice(&bytes);
            status(id, 0, "")
        }
        4 => {
            // CLOSE: an upload's bytes become a file.
            let opened = body.string();
            state.closed.push(opened.clone());
            push(&mut state, kind, opened.clone());
            if let Some((path, bytes)) = state.uploads.remove(&opened) {
                state.files.insert(path, bytes);
            }
            status(id, 0, "")
        }
        13 => {
            // REMOVE
            let path = body.string();
            push(&mut state, kind, path.clone());
            if state.files.remove(&path).is_some() {
                status(id, 0, "")
            } else {
                status(id, 2, "no such file")
            }
        }
        14 => {
            // MKDIR
            let path = body.string();
            let _flags = body.u32();
            let _mode = body.u32();
            push(&mut state, kind, path.clone());
            if state.dirs.contains(&path) {
                status(id, 4, "directory exists")
            } else {
                state.dirs.insert(path);
                status(id, 0, "")
            }
        }
        15 => {
            // RMDIR
            let path = body.string();
            push(&mut state, kind, path.clone());
            if state.dirs.remove(&path) {
                status(id, 0, "")
            } else {
                status(id, 2, "no such directory")
            }
        }
        18 => {
            // RENAME: like POSIX, it fails when the destination exists.
            let from = body.string();
            let to = body.string();
            push(&mut state, kind, from.clone());
            if state.files.contains_key(&to) || state.dirs.contains(&to) {
                return status(id, 4, "destination exists");
            }
            let moved = state
                .files
                .remove(&from)
                .map(|bytes| state.files.insert(to.clone(), bytes).is_none())
                .or_else(|| state.dirs.remove(&from).then(|| state.dirs.insert(to)));
            match moved {
                Some(_) => status(id, 0, ""),
                None => status(id, 2, "no such file"),
            }
        }
        9 => {
            // SETSTAT: permissions only.
            let path = body.string();
            let _flags = body.u32();
            let mode = body.u32();
            push(&mut state, kind, path.clone());
            state.modes.insert(path, mode);
            status(id, 0, "")
        }
        200 => {
            // EXTENDED
            let extension = body.string();
            push(&mut state, kind, extension.clone());
            match extension.as_str() {
                POSIX_RENAME => {
                    let from = body.string();
                    let to = body.string();
                    state.files.remove(&to);
                    state.dirs.remove(&to);
                    let moved = state
                        .files
                        .remove(&from)
                        .map(|bytes| state.files.insert(to.clone(), bytes).is_none())
                        .or_else(|| state.dirs.remove(&from).then(|| state.dirs.insert(to)));
                    match moved {
                        Some(_) => status(id, 0, ""),
                        None => status(id, 2, "no such file"),
                    }
                }
                STATVFS => {
                    if state
                        .advertise
                        .iter()
                        .any(|(name, _)| name.as_str() == STATVFS)
                    {
                        statvfs_reply(id)
                    } else {
                        status(id, 8, "unsupported extension")
                    }
                }
                _ => status(id, 8, "unsupported extension"),
            }
        }
        other => status(id, 8, &format!("unsupported packet {other}")),
    }
}

/// Answer whatever the client sends, the way an OpenSSH server would.
async fn serve(mut wire: End, state: Arc<Mutex<State>>) {
    let Ok(init) = read_frame(&mut wire).await else {
        return;
    };
    assert_eq!(init[0], 1, "a session starts with SSH_FXP_INIT");
    let extensions = state.lock().expect("the mock's state").advertise.clone();
    let mut version = vec![2u8];
    version.extend_from_slice(&3u32.to_be_bytes());
    for (name, data) in extensions {
        put_string(&mut version, name.as_bytes());
        put_string(&mut version, data.as_bytes());
    }
    if write_frame(&mut wire, &version).await.is_err() {
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

/// A session over a wire, with its mock server running beside it.
struct Mock {
    session: Session,
    state: Arc<Mutex<State>>,
}

async fn connect(advertise: &[(&str, &str)]) -> Mock {
    let mut state = State::new();
    state.advertise = advertise
        .iter()
        .map(|(name, data)| ((*name).to_owned(), (*data).to_owned()))
        .collect();
    let state = Arc::new(Mutex::new(state));
    let (client, server) = pair();
    compio::runtime::spawn(serve(server, state.clone())).detach();
    let session = Session::from_wire(Box::new(client))
        .await
        .expect("a handshake");
    Mock { session, state }
}

impl Mock {
    fn log(&self) -> Vec<(u8, String)> {
        self.state.lock().expect("the mock's state").log.clone()
    }

    fn files(&self) -> HashMap<String, Vec<u8>> {
        self.state.lock().expect("the mock's state").files.clone()
    }

    fn dirs(&self) -> HashSet<String> {
        self.state.lock().expect("the mock's state").dirs.clone()
    }

    fn writes(&self) -> Vec<(u64, usize)> {
        self.state.lock().expect("the mock's state").writes.clone()
    }

    fn closed(&self) -> Vec<String> {
        self.state.lock().expect("the mock's state").closed.clone()
    }

    fn modes(&self) -> HashMap<String, u32> {
        self.state.lock().expect("the mock's state").modes.clone()
    }
}

#[compio::test]
async fn a_listing_arrives_over_a_wire() {
    let mut mock = connect(&[]).await;
    let entries = mock.session.list("/home/user").await.expect("list");
    let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
    // Directories first: the mock returns the file first on purpose.
    assert_eq!(names, ["src", "notes.txt"]);
    assert!(entries[0].dir, "the directory sorts first, and says so");
    assert_eq!(entries[1].size, BODY.len() as u64);
}

#[compio::test]
async fn a_download_is_renamed_into_place() {
    let mut mock = connect(&[]).await;
    let dir = tempfile::tempdir().expect("tempdir");
    let tmp = dir.path().join("notes.txt");
    let written = mock
        .session
        .download("/home/user/notes.txt", &tmp)
        .await
        .expect("download");
    assert_eq!(written, BODY.len() as u64);
    assert_eq!(std::fs::read(&tmp).expect("read back"), BODY);
    assert!(
        !tmp.with_extension("part").exists(),
        "the .part file is renamed away"
    );
}

#[compio::test]
async fn a_segmented_read_streams_the_whole_file_and_closes() {
    // The remote-to-remote copy reads here and writes there, one chunk at a
    // time: the same server answers both, so this is the read half proved on
    // its own.
    let mut mock = connect(&[]).await;
    let reading = mock
        .session
        .begin_read("/home/user/notes.txt")
        .await
        .expect("open for reading");
    let mut bytes = Vec::new();
    let mut offset = 0u64;
    loop {
        let Some(chunk) = mock
            .session
            .read_chunk(&reading, offset)
            .await
            .expect("a chunk")
        else {
            break;
        };
        offset += chunk.len() as u64;
        bytes.extend_from_slice(&chunk);
    }
    mock.session.finish_read(reading).await.expect("close");
    assert_eq!(bytes, BODY, "every byte arrived, in order");
    assert_eq!(mock.closed().len(), 1, "the read handle is closed once");
}

#[compio::test]
async fn stat_answers_without_listing_the_directory() {
    let mut mock = connect(&[]).await;
    let entry = mock.session.stat("/home/user/src").await.expect("stat");
    assert!(entry.dir);
    assert_eq!(entry.path, PathBuf::from("/home/user/src"));
}

#[compio::test]
async fn the_home_directory_is_what_the_server_names() {
    let mut mock = connect(&[]).await;
    assert_eq!(
        mock.session.home().await.expect("realpath"),
        PathBuf::from(".")
    );
}

#[compio::test]
async fn make_and_remove_directories_and_files() {
    let mut mock = connect(&[]).await;
    mock.session
        .create_dir("/home/user/new")
        .await
        .expect("mkdir");
    assert!(mock.dirs().contains("/home/user/new"));
    mock.session
        .remove_dir("/home/user/new")
        .await
        .expect("rmdir");
    assert!(!mock.dirs().contains("/home/user/new"));
    mock.session
        .remove("/home/user/notes.txt")
        .await
        .expect("remove");
    assert!(!mock.files().contains_key("/home/user/notes.txt"));
    let kinds: Vec<u8> = mock.log().into_iter().map(|(kind, _)| kind).collect();
    assert_eq!(kinds, [14, 15, 13], "mkdir, rmdir, remove: one packet each");
}

#[compio::test]
async fn rename_uses_the_extension_when_the_server_advertised_it() {
    let mut mock = connect(&[(POSIX_RENAME, "1")]).await;
    assert!(mock.session.supports(POSIX_RENAME));
    mock.session
        .rename("/home/user/notes.txt", "/home/user/renamed.txt", true)
        .await
        .expect("rename");
    let files = mock.files();
    assert!(!files.contains_key("/home/user/notes.txt"));
    assert_eq!(
        files.get("/home/user/renamed.txt"),
        Some(&BODY.to_vec()),
        "the bytes moved with the name"
    );
    assert!(
        mock.log()
            .iter()
            .any(|(kind, first)| *kind == 200 && first == POSIX_RENAME),
        "the replace went through posix-rename@openssh.com"
    );
}

#[compio::test]
async fn rename_without_the_extension_is_a_plain_rename() {
    let mut mock = connect(&[]).await;
    assert!(!mock.session.supports(POSIX_RENAME));
    mock.session
        .rename("/home/user/notes.txt", "/home/user/renamed.txt", true)
        .await
        .expect("rename");
    assert_eq!(
        mock.files().get("/home/user/renamed.txt"),
        Some(&BODY.to_vec())
    );
    let log = mock.log();
    assert!(
        log.iter()
            .any(|(kind, first)| *kind == 18 && first == "/home/user/notes.txt"),
        "a plain SSH_FXP_RENAME was sent: {log:?}"
    );
    assert!(
        !log.iter().any(|(kind, _)| *kind == 200),
        "and no extension packet: {log:?}"
    );
}

#[compio::test]
async fn rename_without_overwrite_does_not_replace_an_existing_name() {
    // "Keep both" must not become "replace" just because the server offers the
    // atomic-replace extension.
    let mut mock = connect(&[(POSIX_RENAME, "1")]).await;
    let err = mock
        .session
        .rename("/home/user/notes.txt", "/home/user/src", false)
        .await
        .expect_err("the destination exists");
    assert!(format!("{err:#}").contains("exists"), "{err:#}");
    assert!(
        mock.files().contains_key("/home/user/notes.txt"),
        "the source is still there"
    );
    assert!(
        mock.log().iter().any(|(kind, _)| *kind == 18),
        "the plain packet was chosen"
    );
}

#[compio::test]
async fn setstat_sets_the_permission_bits() {
    let mut mock = connect(&[]).await;
    mock.session
        .chmod("/home/user/notes.txt", 0o600)
        .await
        .expect("chmod");
    assert_eq!(
        mock.modes().get("/home/user/notes.txt"),
        Some(&0o600),
        "SETSTAT carried the mode"
    );
    assert_eq!(mock.log()[0].0, 9, "SSH_FXP_SETSTAT");
}

#[compio::test]
async fn statvfs_reports_room_when_the_server_advertises_it() {
    let mut mock = connect(&[(STATVFS, "2")]).await;
    assert!(mock.session.supports(STATVFS));
    let stats = mock.session.statvfs("/home/user").await.expect("statvfs");
    assert_eq!(
        stats,
        StatVfs {
            total_bytes: 1000 * 4096,
            free_bytes: 400 * 4096,
            available_bytes: 300 * 4096,
            block_size: 4096,
            fragment_size: 4096,
        }
    );
}

#[compio::test]
async fn statvfs_without_the_extension_is_an_error_the_caller_can_ignore() {
    let mut mock = connect(&[]).await;
    assert!(!mock.session.supports(STATVFS));
    assert!(
        mock.session.statvfs("/home/user").await.is_err(),
        "a server without the extension is not a made-up number"
    );
    assert!(
        mock.log().is_empty(),
        "nothing is asked when the handshake did not offer it"
    );
}

#[compio::test]
async fn an_upload_writes_every_byte_in_chunks_and_closes() {
    let mut mock = connect(&[]).await;
    let dir = tempfile::tempdir().expect("tempdir");
    let local = dir.path().join("big.bin");
    let content: Vec<u8> = (0..(3 * CHUNK + 1234))
        .map(|index| (index % 251) as u8)
        .collect();
    std::fs::write(&local, &content).expect("write the local file");

    let written = mock
        .session
        .upload(&local, "/home/user/big.bin.part")
        .await
        .expect("upload");
    assert_eq!(written, content.len() as u64);
    assert_eq!(
        mock.files().get("/home/user/big.bin.part"),
        Some(&content),
        "every byte arrived, in order"
    );

    let writes = mock.writes();
    assert_eq!(
        writes.iter().map(|(offset, _)| *offset).collect::<Vec<_>>(),
        [0, CHUNK as u64, 2 * CHUNK as u64, 3 * CHUNK as u64],
        "chunks are written at consecutive offsets"
    );
    assert_eq!(
        writes.iter().map(|(_, len)| *len).collect::<Vec<_>>(),
        [CHUNK, CHUNK, CHUNK, 1234]
    );
    assert!(
        writes.iter().all(|(_, len)| *len <= CHUNK),
        "no packet carries more than the chunk bound"
    );
    assert_eq!(mock.closed().len(), 1, "the handle is closed once");
}

#[compio::test]
async fn an_upload_that_fails_midway_surfaces_the_error_and_closes() {
    let mut mock = connect(&[]).await;
    mock.state.lock().expect("the mock's state").fail_write_at = Some(1);
    let dir = tempfile::tempdir().expect("tempdir");
    let local = dir.path().join("big.bin");
    std::fs::write(&local, vec![7u8; 2 * CHUNK + 5]).expect("write the local file");

    let err = mock
        .session
        .upload(&local, "/home/user/big.bin.part")
        .await
        .expect_err("the second chunk fails");
    assert!(format!("{err:#}").contains("no space"), "{err:#}");
    assert_eq!(
        mock.closed().len(),
        1,
        "the failed upload closed its handle instead of leaking it"
    );
    let entry = mock
        .session
        .stat("/home/user/notes.txt")
        .await
        .expect("a session after a failed upload is still a session");
    assert_eq!(entry.size, BODY.len() as u64);
}
