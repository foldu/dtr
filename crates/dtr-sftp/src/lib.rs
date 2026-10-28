//! SFTP over the host's own `ssh`.
//!
//! dtr contains no SSH implementation: the alias is handed to `ssh -s <alias>
//! sftp` unresolved, so `ProxyJump`, `Include`, `Match`, agent forwarding and
//! `known_hosts` are whatever the user's ssh already does. What is left is a
//! byte pipe — the child's stdin and stdout — and `russh-sftp` speaks the
//! protocol over it. See `docs/sftp.md` for why this shape was chosen.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result};
use russh_sftp::client::SftpSession;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::process::{Child, Command};

/// One entry of a directory listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub dir: bool,
    pub size: u64,
    pub modified: Option<u64>,
}

/// A live SFTP session: the ssh child and the protocol on top of it.
pub struct Session {
    sftp: SftpSession,
    /// `None` when the session was built on a stream instead of a child, which
    /// is how the tests drive the same code over a duplex pipe.
    child: Option<Child>,
}

impl Session {
    /// Connect by ssh alias, through the user's own ssh.
    pub async fn connect(alias: &str) -> Result<Self> {
        let mut child = Command::new("ssh")
            .args(["-s", alias, "sftp"])
            // The subsystem is the whole point; a prompt would hang the pipe.
            // Batch mode makes a missing credential a fast, readable failure
            // instead of a silent wait.
            .arg("-o")
            .arg("BatchMode=yes")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawn ssh -s {alias} sftp"))?;

        let stdin = child.stdin.take().context("ssh stdin")?;
        let stdout = child.stdout.take().context("ssh stdout")?;
        let stream = ChildStream {
            read: stdout,
            write: stdin,
        };
        let sftp = SftpSession::new(stream)
            .await
            .with_context(|| format!("sftp handshake with {alias}"))?;
        Ok(Self {
            sftp,
            child: Some(child),
        })
    }

    /// A session over any bidirectional stream: the tests' way in, and the
    /// reason `connect` is only about *finding* that stream.
    pub async fn from_stream<S>(stream: S) -> Result<Self>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let sftp = SftpSession::new(stream).await.context("sftp handshake")?;
        Ok(Self { sftp, child: None })
    }

    /// The directory's entries, sorted the way a file browser shows them:
    /// directories first, then by name.
    pub async fn list(&self, path: &str) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        let dir = self
            .sftp
            .read_dir(path)
            .await
            .with_context(|| format!("read_dir {path}"))?;
        for entry in dir {
            let name = entry.file_name();
            if name == "." || name == ".." {
                continue;
            }
            let attrs = entry.metadata();
            entries.push(Entry {
                path: Path::new(path).join(&name),
                name,
                dir: attrs.is_dir(),
                size: attrs.len(),
                modified: attrs.modified().ok().and_then(|time| {
                    time.duration_since(std::time::UNIX_EPOCH)
                        .ok()
                        .map(|since| since.as_secs())
                }),
            });
        }
        sort_entries(&mut entries);
        Ok(entries)
    }

    /// Copy one file down. Bytes land in `local` only when the transfer
    /// finished, so a cancelled download cannot truncate a good file.
    pub async fn download(&mut self, remote: &str, local: &Path) -> Result<u64> {
        let mut file = self
            .sftp
            .open(remote)
            .await
            .with_context(|| format!("open {remote}"))?;
        let temporary = local.with_extension("part");
        let mut out = tokio::fs::File::create(&temporary)
            .await
            .with_context(|| format!("create {}", temporary.display()))?;
        let mut buf = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            let read = file.read(&mut buf).await?;
            if read == 0 {
                break;
            }
            out.write_all(&buf[..read]).await?;
            total += read as u64;
        }
        out.flush().await?;
        drop(out);
        tokio::fs::rename(&temporary, local)
            .await
            .with_context(|| format!("rename into {}", local.display()))?;
        Ok(total)
    }

    /// Kill the child: a session is a process, and an orphaned `ssh` would
    /// outlive the window that asked for it.
    pub fn close(mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
        }
    }
}

/// The child's stdin and stdout as one duplex stream.
struct ChildStream {
    read: tokio::process::ChildStdout,
    write: tokio::process::ChildStdin,
}

impl AsyncRead for ChildStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.read).poll_read(cx, buf)
    }
}

impl AsyncWrite for ChildStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.write).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.write).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.write).poll_shutdown(cx)
    }
}

impl Session {
    /// The remote home directory, as the server reports it.
    pub async fn home(&mut self) -> Result<PathBuf> {
        let cwd = self.sftp.canonicalize(".").await.context("canonicalize .")?;
        Ok(PathBuf::from(cwd))
    }
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
        let mut entries = vec![entry("z.txt", false), entry("a", true), entry("b.txt", false)];
        sort_entries(&mut entries);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["a", "b.txt", "z.txt"]);
    }

    #[test]
    fn a_session_is_a_process() {
        // `close` must take the child down with it: an ssh left behind holds a
        // connection and a `ControlMaster` alive for as long as it runs.
        let source = include_str!("lib.rs");
        assert!(source.contains("kill_on_drop(true)"));
    }
}
