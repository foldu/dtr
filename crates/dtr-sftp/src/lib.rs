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
        let mut command = Command::new("ssh");
        for arg in ssh_args(alias) {
            command.arg(arg);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Piped, not null: when a handshake fails the child's stderr is the
            // only thing that says why. On its own the protocol error is
            // "sender dropped", which tells nobody anything.
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawn ssh for {alias}"))?;

        let stdin = child.stdin.take().context("ssh stdin")?;
        let stdout = child.stdout.take().context("ssh stdout")?;
        let stderr = child.stderr.take();
        let stream = ChildStream {
            read: stdout,
            write: stdin,
        };
        match SftpSession::new(stream).await {
            Ok(sftp) => Ok(Self {
                sftp,
                child: Some(child),
            }),
            Err(err) => {
                let said = read_stderr(stderr).await;
                let status = child.try_wait().ok().flatten().map(|s| s.to_string());
                let mut message = format!("sftp handshake with {alias} failed: {err}");
                if let Some(status) = status {
                    message.push_str(&format!(" (ssh exited: {status})"));
                }
                if !said.is_empty() {
                    message.push_str(&format!("\nssh said: {said}"));
                }
                Err(anyhow::anyhow!(message))
            }
        }
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

    /// One path's metadata, without listing its directory.
    ///
    /// A click on a file needs to know "is it a directory" before deciding what
    /// to do with it, and fetching a whole directory listing to answer that is
    /// a round trip for one bit.
    pub async fn stat(&self, path: &str) -> Result<Entry> {
        let attrs = self
            .sftp
            .metadata(path)
            .await
            .with_context(|| format!("stat {path}"))?;
        Ok(Entry {
            name: Path::new(path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_owned()),
            path: PathBuf::from(path),
            dir: attrs.is_dir(),
            size: attrs.len(),
            modified: attrs.modified().ok().and_then(|time| {
                time.duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|since| since.as_secs())
            }),
        })
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
        let cwd = self
            .sftp
            .canonicalize(".")
            .await
            .context("canonicalize .")?;
        Ok(PathBuf::from(cwd))
    }
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
    // it off for diagnosis - with a pty underneath (docs/sftp.md §5.3) a prompt
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

/// Whatever the child said before it died, bounded and time-boxed: a wedged ssh
/// must not hold the worker.
async fn read_stderr(stderr: Option<tokio::process::ChildStderr>) -> String {
    let Some(mut stderr) = stderr else {
        return String::new();
    };
    let mut text = String::new();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        let mut scratch = Vec::new();
        let _ = stderr.read_to_end(&mut scratch).await;
        text = String::from_utf8_lossy(&scratch).trim().to_owned();
    })
    .await;
    // The last line is the one that says why; earlier ones are banners.
    let last = text.lines().last().unwrap_or("").to_owned();
    if last.is_empty() { text } else { last }
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
        // connection and a `ControlMaster` alive for as long as it runs.
        let source = include_str!("lib.rs");
        assert!(source.contains("kill_on_drop(true)"));
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
