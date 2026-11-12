//! A mock SFTP server on an in-memory wire.
//!
//! It speaks the same version 3 framing the client does — one packet at a time,
//! a length first — and it is the only test of the codec that does not need a
//! host, an ssh binary or a server.

use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;

use anyhow::Result;
use dtr_sftp::{Session, Wire};

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
fn name(id: u32, files: &[(&str, bool, u64)]) -> Vec<u8> {
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

const BODY: &[u8] = b"hello from the mock";

/// One canned filesystem: a directory holding a file and a directory.
fn files() -> HashMap<&'static str, (bool, Vec<u8>)> {
    let mut files = HashMap::new();
    files.insert("/home/user/notes.txt", (false, BODY.to_vec()));
    files.insert("/home/user/src", (true, Vec::new()));
    files
}

/// Answer whatever the client sends, the way an OpenSSH server would.
async fn serve(mut wire: End) {
    let Ok(init) = read_frame(&mut wire).await else {
        return;
    };
    assert_eq!(init[0], 1, "a session starts with SSH_FXP_INIT");
    // SSH_FXP_VERSION: version 3, no extensions.
    if write_frame(&mut wire, &[2, 0, 0, 0, 3]).await.is_err() {
        return;
    }
    let files = files();
    let mut listed = false;
    loop {
        let Ok(frame) = read_frame(&mut wire).await else {
            return;
        };
        let kind = frame[0];
        let id = u32::from_be_bytes(frame[1..5].try_into().expect("four bytes of id"));
        let body = &frame[5..];
        let answer = match kind {
            11 => handle(id, "dir"), // OPENDIR
            12 => {
                // READDIR: one batch of entries, then EOF.
                if listed {
                    status(id, 1, "eof")
                } else {
                    listed = true;
                    name(
                        id,
                        &[("notes.txt", false, BODY.len() as u64), ("src", true, 0)],
                    )
                }
            }
            17 => {
                // STAT
                let path = read_string(body);
                match files.get(path.as_str()) {
                    Some((dir, bytes)) => {
                        let mut out = vec![105];
                        out.extend_from_slice(&id.to_be_bytes());
                        out.extend_from_slice(&attrs(*dir, bytes.len() as u64));
                        out
                    }
                    None => status(id, 2, "no such file"),
                }
            }
            16 => name(id, &[(".", true, 0)]), // REALPATH
            3 => {
                // OPEN
                let path = read_string(body);
                if path == "/home/user/notes.txt" {
                    handle(id, "notes.txt")
                } else {
                    status(id, 2, "no such file")
                }
            }
            5 => {
                // READ
                let mut at = 0;
                let handle = read_string(&body[at..]);
                at += 4 + handle.len();
                let offset =
                    u64::from_be_bytes(body[at..at + 8].try_into().expect("eight bytes")) as usize;
                at += 8;
                let len =
                    u32::from_be_bytes(body[at..at + 4].try_into().expect("four bytes")) as usize;
                if handle != "notes.txt" {
                    status(id, 2, "no such file")
                } else if offset >= BODY.len() {
                    status(id, 1, "eof")
                } else {
                    let end = (offset + len).min(BODY.len());
                    let mut out = vec![103];
                    out.extend_from_slice(&id.to_be_bytes());
                    put_string(&mut out, &BODY[offset..end]);
                    out
                }
            }
            4 => status(id, 0, ""), // CLOSE
            other => status(id, 8, &format!("unsupported packet {other}")),
        };
        if write_frame(&mut wire, &answer).await.is_err() {
            return;
        }
    }
}

/// The string at the front of a payload.
fn read_string(body: &[u8]) -> String {
    let len = u32::from_be_bytes(body[..4].try_into().expect("four bytes")) as usize;
    String::from_utf8_lossy(&body[4..4 + len]).into_owned()
}

/// A session over a wire, with its server running beside it.
async fn session() -> Session {
    let (client, server) = pair();
    compio::runtime::spawn(serve(server)).detach();
    Session::from_wire(Box::new(client))
        .await
        .expect("a handshake")
}

#[compio::test]
async fn a_listing_arrives_over_a_wire() {
    let mut session = session().await;
    let entries = session.list("/home/user").await.expect("list");
    let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
    // Directories first: the mock returns the file first on purpose.
    assert_eq!(names, ["src", "notes.txt"]);
    assert!(entries[0].dir, "the directory sorts first, and says so");
    assert_eq!(entries[1].size, BODY.len() as u64);
}

#[compio::test]
async fn a_download_is_renamed_into_place() {
    let mut session = session().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let tmp = dir.path().join("notes.txt");
    let written = session
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
async fn stat_answers_without_listing_the_directory() {
    let mut session = session().await;
    let entry = session.stat("/home/user/src").await.expect("stat");
    assert!(entry.dir);
    assert_eq!(entry.path, PathBuf::from("/home/user/src"));
}

#[compio::test]
async fn the_home_directory_is_what_the_server_names() {
    let mut session = session().await;
    assert_eq!(session.home().await.expect("realpath"), PathBuf::from("."));
}
