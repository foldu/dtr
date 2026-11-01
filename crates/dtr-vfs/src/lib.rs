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

pub mod cache;
pub mod local;
pub mod memory;
pub mod ssh;

pub use local::Local;
pub use memory::Memory;
pub use ssh::Ssh;

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

    /// Copy a file to `dest`, returning how many bytes arrived.
    ///
    /// Writing to a destination rather than returning a stream is what the two
    /// cases share: locally it is a copy, remotely it is the download that
    /// produces the copy the OS handler will open. A stream will come back when
    /// the pager needs one; a file is what "open this in a viewer" needs today.
    fn fetch<'a>(&'a self, path: &'a Path, dest: &'a Path) -> BoxFuture<'a, Result<u64>>;

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
    /// Where fetched remote files land.
    root: PathBuf,
}

impl Default for Vfs {
    fn default() -> Self {
        Self::new()
    }
}

impl Vfs {
    pub fn new() -> Self {
        let root = dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("dtr")
            .join("remote");
        Self::with_root(root)
    }

    /// A VFS whose fetched files land under `root`. Tests use this so a fetch
    /// cannot touch the real cache.
    pub fn with_root(root: PathBuf) -> Self {
        let mut backends: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
        backends.insert(BackendId::Local, Arc::new(Local::new()));
        Self {
            backends: Mutex::new(backends),
            root,
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
                let mut backends = self.backends.lock().expect("vfs lock");
                if let Some(backend) = backends.get(id) {
                    return Ok(Arc::clone(backend));
                }
                // Cheap to build: the connection happens on the first
                // operation, and keeping one entry per alias is what makes the
                // pool a pool rather than a connection per click.
                let backend: Arc<dyn Backend> = Arc::new(Ssh::new(alias.clone()));
                backends.insert(id.clone(), Arc::clone(&backend));
                Ok(backend)
            }
        }
    }

    pub async fn stat(&self, at: &Location) -> Result<Meta> {
        let backend = self.backend(&at.backend)?;
        backend.stat(&at.path).await
    }

    /// A path another program can open.
    ///
    /// Local files are their own path — no copy, ever. A remote one is fetched
    /// into the cache first, because `xdg-open` and every viewer it starts can
    /// only open paths, and a remote path is not one. The copy keeps the remote
    /// name and tree, so the title bar of whatever opens it says where it came
    /// from.
    pub async fn materialise(&self, at: &Location) -> Result<PathBuf> {
        if matches!(at.backend, BackendId::Local) {
            return Ok(at.path.clone());
        }
        let backend = self.backend(&at.backend)?;
        let dest = cache::path_for(&self.root, &at.backend, &at.path);
        if let Some(parent) = dest.parent() {
            let parent = parent.to_path_buf();
            tokio::task::spawn_blocking(move || cache::ensure_dir(&parent))
                .await
                .map_err(|err| anyhow::anyhow!("{err}"))??;
        }
        backend
            .fetch(&at.path, &dest)
            .await
            .with_context(|| format!("fetch {}", at.describe()))?;
        // Private by default: this is somebody's remote file, not ours to
        // leave world-readable because the umbrella happened to be 0022.
        let owned = dest.clone();
        tokio::task::spawn_blocking(move || cache::restrict(&owned))
            .await
            .map_err(|err| anyhow::anyhow!("{err}"))??;
        Ok(dest)
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

        let dest = dir.path().join("copy.txt");
        let copied = backend
            .fetch(&dir.path().join("a.txt"), &dest)
            .await
            .unwrap();
        assert_eq!(copied, 5);
        assert_eq!(std::fs::read_to_string(dest).unwrap(), "hello");
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
        let cache = tempfile::tempdir().unwrap();
        let dest = cache.path().join("notes.txt");
        mem.fetch(Path::new("/home/lab/notes.txt"), &dest)
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(dest).unwrap(), "hello");
    }

    /// A remote location is answered by its own backend, never by the local
    /// one: the failure mode this guards against is `/etc/hosts` from a remote
    /// program being served from *this* machine's `/etc/hosts`.
    #[test]
    fn a_remote_path_gets_a_remote_backend() {
        let vfs = Vfs::new();
        let backend = vfs
            .backend(&BackendId::Ssh("jupyter".into()))
            .expect("a backend");
        assert_eq!(*backend.id(), BackendId::Ssh("jupyter".into()));
        // And the same alias gets the same backend, because that Arc is the
        // connection pool.
        let again = vfs
            .backend(&BackendId::Ssh("jupyter".into()))
            .expect("a backend");
        assert!(Arc::ptr_eq(&backend, &again));
    }

    /// A local file is its own path: materialising it must never copy, or every
    /// click would fork the file the user is working on.
    #[tokio::test]
    async fn a_local_file_materialises_to_itself() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, b"x").unwrap();
        let vfs = Vfs::with_root(dir.path().join("cache"));
        let at = Location::local(&path);
        assert_eq!(vfs.materialise(&at).await.unwrap(), path);
    }

    /// A remote file is fetched under its own machine and tree, so whatever
    /// opens it shows a name that says where it came from.
    #[tokio::test]
    async fn a_remote_file_materialises_into_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        // Stand in for the ssh backend: the cache path and the write are what
        // this is about, and an ssh server is not available in a test.
        let backend = Memory::new();
        backend.write("/var/log/syslog", b"line\n");
        let dest = crate::cache::path_for(
            &dir.path().join("cache"),
            &BackendId::Ssh("jupyter".into()),
            Path::new("/var/log/syslog"),
        );
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        backend
            .fetch(Path::new("/var/log/syslog"), &dest)
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "line\n");
        assert!(dest.ends_with("jupyter/var/log/syslog"));
    }
}
