//! Files, whichever machine they live on.
//!
//! A terminal learns where a session is (OSC 7) and, with shell integration,
//! what it printed. Clicking a file name means asking *the machine the session
//! is on* for it — which is the same code path locally and over `ssh`, and the
//! reason this crate exists rather than `std::fs` calls sprinkled through the
//! UI.
//!
//! Three rules shape the API:
//!
//! - **A [`Location`] is the pane's, not the URI's.** `file:///etc/hosts` printed
//!   by a program on `jupyter` means jupyter's `/etc/hosts`. The pane knows which
//!   machine it is, so it resolves the path and the UI never parses a scheme on
//!   its own.
//! - **Listings arrive in batches.** A `readdir` answers in chunks, and a large
//!   directory over a slow link is the normal case; a single `Vec` would mean a
//!   spinner that grows with the directory. Dropping the receiver cancels the
//!   walk.
//! - **Capabilities are declared, not emulated.** A remote backend cannot watch a
//!   directory and does not know a directory's size. [`Backend::caps`] says so,
//!   and the UI hides what is not there instead of polling and pretending.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use anyhow::{Context as _, Result};
use tokio::io::AsyncRead;

pub mod local;
pub mod memory;

pub use local::Local;
pub use memory::Memory;

/// Which machine a path belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum BackendId {
    /// The machine dtr runs on.
    Local,
    /// A pane's `ssh` alias. There is deliberately no way to write this by
    /// hand: a remote backend comes from a session that *is* remote, so the
    /// answer to "which machine" is never guessed from a link.
    Ssh(String),
}

impl std::fmt::Display for BackendId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local => f.write_str("local"),
            Self::Ssh(alias) => write!(f, "{alias}"),
        }
    }
}

/// A file on a particular machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub backend: BackendId,
    pub path: PathBuf,
}

impl Location {
    pub fn local(path: impl Into<PathBuf>) -> Self {
        Self {
            backend: BackendId::Local,
            path: path.into(),
        }
    }

    /// What to call it in a tooltip or a toast: `jupyter:/var/log/syslog`.
    pub fn describe(&self) -> String {
        format!("{}:{}", self.backend, self.path.display())
    }
}

/// What a backend can do, so the UI can hide what it cannot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    /// Whether a directory can report changes without being re-listed.
    pub watch: bool,
    /// Whether a directory's size is known, rather than a guess.
    pub dir_size: bool,
}

/// What a path is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Other,
}

impl Kind {
    pub fn is_dir(self) -> bool {
        matches!(self, Self::Dir)
    }
}

/// What a path is, and how big.
#[derive(Clone, Debug)]
pub struct Meta {
    pub kind: Kind,
    pub len: u64,
    pub modified: Option<SystemTime>,
}

/// One row of a listing.
#[derive(Clone, Debug)]
pub struct Entry {
    /// The file name alone, not a path: the directory is the request's.
    pub name: String,
    pub kind: Kind,
    pub len: u64,
}

/// A batch of a listing, or the error that ended it. A dropped receiver
/// cancels the walk.
pub type Batches = tokio::sync::mpsc::Receiver<Result<Vec<Entry>>>;

/// Where futures come from. Spelled out rather than using a macro so the trait
/// stays object-safe without pulling in a proc macro.
pub type BoxFuture<'a, T> = Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// One machine's filesystem.
pub trait Backend: Send + Sync {
    fn id(&self) -> &BackendId;

    fn caps(&self) -> Caps;

    fn stat<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, Result<Meta>>;

    /// Open a file for reading. Remote backends stream it, so a caller that can
    /// consume bytes (a pager) never needs a temporary file.
    fn open<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, Result<Box<dyn AsyncRead + Send + Unpin>>>;

    /// Start a listing. Errors reach the caller through the batch channel, so a
    /// directory that fails halfway is a list *and* an error, never a silently
    /// short listing.
    fn list(&self, path: &Path) -> Batches;
}

/// The backends this app can reach, by machine.
///
/// Connections live here rather than in a pane: two tabs on the same alias must
/// share one, or the second hits the server's session limit.
pub struct Vfs {
    backends: Mutex<HashMap<BackendId, Arc<dyn Backend>>>,
}

impl Default for Vfs {
    fn default() -> Self {
        Self::new()
    }
}

impl Vfs {
    pub fn new() -> Self {
        let mut backends: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
        backends.insert(BackendId::Local, Arc::new(Local::new()));
        Self {
            backends: Mutex::new(backends),
        }
    }

    /// The backend for a machine. A remote one that is not wired yet is an
    /// error, not a silent local answer: a path from jupyter must never be read
    /// off this disk.
    pub fn backend(&self, id: &BackendId) -> Result<Arc<dyn Backend>> {
        match id {
            BackendId::Local => Ok(self
                .backends
                .lock()
                .expect("vfs lock")
                .get(&BackendId::Local)
                .expect("local backend")
                .clone()),
            BackendId::Ssh(alias) => {
                if let Some(backend) = self.backends.lock().expect("vfs lock").get(id) {
                    return Ok(Arc::clone(backend));
                }
                anyhow::bail!("remote files are not wired up yet ({alias})")
            }
        }
    }

    pub async fn stat(&self, at: &Location) -> Result<Meta> {
        let backend = self.backend(&at.backend)?;
        backend.stat(&at.path).await
    }
}

/// Hand a local file to whatever the desktop opens it with.
///
/// Deliberately a plain spawn of the platform's opener, never a shell: the path
/// comes from a program's output, and `sh -c` with it would be a hole rather
/// than a convenience. The child is detached; a failure to start is the only
/// thing this can report.
pub fn open_externally(path: &Path) -> Result<()> {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("explorer");
        command.arg(path);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        command.arg(path);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(path);
        command
    };

    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("could not open {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    async fn drain(mut batches: Batches) -> Vec<Entry> {
        let mut out = Vec::new();
        while let Some(batch) = batches.recv().await {
            out.extend(batch.expect("batch"));
        }
        out
    }

    #[tokio::test]
    async fn a_local_directory_streams_its_entries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"hello").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();

        let backend = Local::new();
        let mut names: Vec<String> = drain(backend.list(dir.path()))
            .await
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        names.sort();
        assert_eq!(names, ["a.txt", "sub"]);

        let meta = backend.stat(&dir.path().join("a.txt")).await.unwrap();
        assert_eq!(meta.kind, Kind::File);
        assert_eq!(meta.len, 5);

        let mut file = backend.open(&dir.path().join("a.txt")).await.unwrap();
        let mut text = String::new();
        file.read_to_string(&mut text).await.unwrap();
        assert_eq!(text, "hello");
    }

    #[tokio::test]
    async fn a_local_missing_path_is_an_error_not_an_empty_file() {
        let backend = Local::new();
        let err = backend
            .stat(Path::new("/definitely/not/here"))
            .await
            .expect_err("should fail");
        assert!(err.to_string().contains("definitely"), "{err}");
    }

    #[tokio::test]
    async fn the_memory_backend_behaves_like_a_filesystem() {
        let mem = Memory::new();
        mem.write("/home/lab/notes.txt", b"hello");
        mem.write("/home/lab/deeper/x", b"!");
        mem.dir("/home/lab");
        mem.dir("/home/lab/deeper");

        let mut names: Vec<String> = drain(mem.list(Path::new("/home/lab")))
            .await
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        names.sort();
        assert_eq!(names, ["deeper", "notes.txt"]);

        assert_eq!(
            mem.stat(Path::new("/home/lab/deeper")).await.unwrap().kind,
            Kind::Dir
        );
        let mut file = mem.open(Path::new("/home/lab/notes.txt")).await.unwrap();
        let mut text = String::new();
        file.read_to_string(&mut text).await.unwrap();
        assert_eq!(text, "hello");
    }

    #[tokio::test]
    async fn a_remote_path_is_never_read_off_this_disk() {
        let vfs = Vfs::new();
        let err = vfs
            .stat(&Location {
                backend: BackendId::Ssh("jupyter".into()),
                path: PathBuf::from("/etc/hosts"),
            })
            .await
            .expect_err("should not fall back to local");
        assert!(err.to_string().contains("jupyter"), "{err}");
    }
}
