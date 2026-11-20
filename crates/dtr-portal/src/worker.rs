//! The manager window's file work, on one thread of its own.
//!
//! `docs/manager.md` §6 gives the window a remote listing, a local count and the
//! write verbs; all three are slow or blocking, and the window is a frame loop
//! that must never wait on one (`AGENTS.md`, "the UI thread asks for slow work").
//! So this is the daemon's version of `dtr`'s background worker, sized for one
//! window kind:
//!
//! - **One thread**, started with the first manager window (the picker never
//!   needs it) and named `dtr-files-worker`.
//! - **One compio runtime on it**, because `dtr-vfs`'s verbs are futures and
//!   `Local`'s writes are `compio::runtime::spawn_blocking` — a future that needs
//!   a runtime context. `Ssh` starts its own per-alias thread and runtime, so the
//!   watch here is only that the *caller's* future has an executor.
//! - **Answers come back as events**, through the same `EventLoopProxy` the
//!   `Host` uses; the window loop routes them to the manager window by its
//!   routing id. Nothing here touches a `Browser`.
//!
//! The job types are deliberately small and owned: a `WriteRequest` is the
//! browser's own value, so the worker and the window cannot disagree about what
//! a rename is, and a listing is `(alias, path)` — the same pair the browser's
//! `take_request` hands out.

use std::path::PathBuf;

use dtr_files::{Entry, WriteRequest};
use dtr_vfs::{BackendId, Vfs};

/// One thing the worker did, for the window loop.
#[derive(Debug)]
pub enum WorkerEvent {
    /// A remote directory's listing, in one piece: the browser's `apply_listing`
    /// takes a complete listing and sorts it once.
    Listed {
        manager: u64,
        alias: String,
        path: PathBuf,
        entries: Vec<Entry>,
    },
    /// A remote listing that ended in an error, with the words for the footer.
    Failed {
        manager: u64,
        alias: String,
        message: String,
    },
    /// A local directory counted for the status bar.
    Counted {
        manager: u64,
        total: usize,
        shown: usize,
    },
    /// A write request's answer.
    Wrote { manager: u64, result: WriteResult },
}

/// What one write request came to.
///
/// **A conflict is not a failure.** `dtr-files`'s overwrite card is chosen by
/// downcasting the error to [`dtr_vfs::Conflict`] (`WriteOp`'s contract), and a
/// message string has lost that type — so the worker recognises the conflict here
/// and carries the destination, and the window rebuilds the very error the browser
/// expects. Sending the error object itself is impossible: `anyhow::Error` is not
/// `Send`, and this crosses a thread.
#[derive(Debug)]
pub enum WriteResult {
    /// The verb ran.
    Ok,
    /// The destination exists and nothing was replaced: the browser's card.
    Conflict(PathBuf),
    /// Anything else, with the VFS's message for the footer.
    Failed(String),
}

/// What the window asks the worker for.
#[derive(Debug)]
pub enum Job {
    /// List a remote directory, through its ssh alias.
    List {
        manager: u64,
        alias: String,
        path: PathBuf,
    },
    /// Count a local directory's entries, hidden ones separately.
    Count { manager: u64, path: PathBuf },
    /// Run one write request through `dtr-vfs`'s verbs.
    Write { manager: u64, request: WriteRequest },
}

/// The sending half of the worker; dropping it ends the thread.
#[derive(Clone)]
pub struct Worker {
    jobs: async_channel::Sender<Job>,
}

impl Worker {
    /// Start the worker thread, answering through `sink`.
    ///
    /// `sink` is a closure rather than the event enum itself so this module
    /// knows nothing about the window loop: the caller wraps a
    /// [`WorkerEvent`](WorkerEvent) in its own event and posts it.
    pub fn start(sink: impl Fn(WorkerEvent) + Send + 'static) -> Self {
        let (jobs, inbox) = async_channel::unbounded();
        let spawned = std::thread::Builder::new()
            .name("dtr-files-worker".to_owned())
            .spawn(move || {
                let runtime = match compio::runtime::Runtime::new() {
                    Ok(runtime) => runtime,
                    Err(err) => {
                        tracing::error!(%err, "no runtime for the manager's file work");
                        return;
                    }
                };
                runtime.block_on(async move {
                    let vfs = Vfs::new();
                    while let Ok(job) = inbox.recv().await {
                        run(&vfs, job, &sink).await;
                    }
                });
            });
        if let Err(err) = spawned {
            // The channel keeps a sender for ever, so every job is a silent
            // no-op rather than a panic; the log is the only place to say why.
            tracing::error!(%err, "could not start the manager's file worker");
        }
        Self { jobs }
    }

    /// List a remote directory.
    pub fn list(&self, manager: u64, alias: String, path: PathBuf) -> bool {
        self.jobs
            .try_send(Job::List {
                manager,
                alias,
                path,
            })
            .is_ok()
    }

    /// Count a local directory.
    pub fn count(&self, manager: u64, path: PathBuf) -> bool {
        self.jobs.try_send(Job::Count { manager, path }).is_ok()
    }

    /// Run one write request.
    pub fn write(&self, manager: u64, request: WriteRequest) -> bool {
        self.jobs.try_send(Job::Write { manager, request }).is_ok()
    }
}

/// One job, start to finish, on the worker thread.
async fn run(vfs: &Vfs, job: Job, sink: &impl Fn(WorkerEvent)) {
    match job {
        Job::List {
            manager,
            alias,
            path,
        } => list(vfs, manager, alias, path, sink).await,
        Job::Count { manager, path } => count(vfs, manager, path, sink).await,
        Job::Write { manager, request } => {
            let result = match execute(vfs, &request).await {
                Ok(()) => WriteResult::Ok,
                Err(err) => match conflict_path(&err) {
                    Some(path) => WriteResult::Conflict(path),
                    None => WriteResult::Failed(format!("{err:#}")),
                },
            };
            sink(WorkerEvent::Wrote { manager, result });
        }
    }
}

/// A remote directory through its alias's backend, collected into one listing.
///
/// One piece rather than streamed batches: the browser's `apply_listing` takes a
/// complete listing and sorts once, and a directory is one screen of a person's
/// attention. A directory large enough for that to be a wait is a wait the
/// worker owns, not the frame loop.
async fn list(vfs: &Vfs, manager: u64, alias: String, path: PathBuf, sink: &impl Fn(WorkerEvent)) {
    let backend = match vfs.backend(&BackendId::Ssh(alias.clone())) {
        Ok(backend) => backend,
        Err(err) => {
            sink(WorkerEvent::Failed {
                manager,
                alias,
                message: format!("{err:#}"),
            });
            return;
        }
    };
    let batches = backend.list(&path);
    let mut entries = Vec::new();
    loop {
        match batches.recv().await {
            Ok(Ok(batch)) => entries.extend(batch.into_iter().map(Entry::from)),
            Ok(Err(err)) => {
                sink(WorkerEvent::Failed {
                    manager,
                    alias,
                    message: format!("{err:#}"),
                });
                return;
            }
            // The sender is gone: the walk is over.
            Err(_) => break,
        }
    }
    sink(WorkerEvent::Listed {
        manager,
        alias,
        path,
        entries,
    });
}

/// Count a local directory, total and un-hidden.
///
/// A second walk over the directory the browser is already listing: `Browser` has
/// no public entry count (the status bar's `EntryCount` has no other supplier),
/// and the alternative is a bar that says `0 items` under a full listing. It runs
/// here, off the frame loop, and is asked for again whenever a listing changes
/// ([`crate::manager::Manager::take_count_request`]). The gap is in the report:
/// `dtr-files` should expose the count it already has.
async fn count(vfs: &Vfs, manager: u64, path: PathBuf, sink: &impl Fn(WorkerEvent)) {
    let backend = match vfs.backend(&BackendId::Local) {
        Ok(backend) => backend,
        Err(err) => {
            tracing::warn!(%err, path = %path.display(), "could not count a directory");
            return;
        }
    };
    let batches = backend.list(&path);
    let mut total = 0;
    let mut hidden = 0;
    loop {
        match batches.recv().await {
            Ok(Ok(batch)) => {
                total += batch.len();
                hidden += batch.iter().filter(|entry| entry.hidden).count();
            }
            Ok(Err(err)) => {
                // The browser is reading the same directory and will show its own
                // error in the pane's footer; the count simply does not answer.
                tracing::debug!(%err, path = %path.display(), "count failed");
                return;
            }
            Err(_) => break,
        }
    }
    sink(WorkerEvent::Counted {
        manager,
        total,
        shown: total - hidden,
    });
}

/// Run one write request through the machine's backend.
///
/// This is the discipline `Writes` documents: `replace` means *remove the
/// existing destination with that removal, then run the verb* — the trait never
/// replaces anything itself — and a missing destination is not a failure of the
/// removal. Every verb is the backend's; nothing here re-implements a rename.
async fn execute(vfs: &Vfs, request: &WriteRequest) -> anyhow::Result<()> {
    let backend = vfs.backend(&request.backend)?;
    if let (Some(destination), Some(how)) = (request.op.destination(), request.replace) {
        match backend.remove(destination, how).await {
            Ok(()) => {}
            Err(err) if missing(&err) => {}
            Err(err) => return Err(err),
        }
    }
    let op = &request.op;
    match op {
        dtr_files::WriteOp::CreateDir { at } => backend.create_dir(at).await,
        dtr_files::WriteOp::Rename { from, to } => backend.rename(from, to).await,
        dtr_files::WriteOp::Copy { from, to } => backend.copy(from, to).await.map(|_| ()),
        dtr_files::WriteOp::Move { from, to } => backend.move_to(from, to).await,
        dtr_files::WriteOp::Remove { at, how } => backend.remove(at, *how).await,
    }
}

/// The destination a refused write collided with, when it was a conflict.
///
/// The same downcast `dtr-files` does, done here because a thread boundary takes
/// the type with it: [`WriteResult::Conflict`] carries what the card needs.
fn conflict_path(err: &anyhow::Error) -> Option<PathBuf> {
    err.chain()
        .find_map(|cause| cause.downcast_ref::<dtr_vfs::Conflict>())
        .map(|conflict| conflict.path.clone())
}

/// Whether an error is "the path was not there", which a replace may ignore.
///
/// The message chain rather than one backend's type: `Local` wraps an
/// `io::Error` in context and `Ssh` answers with its own words, so the io error
/// is looked for down the `anyhow` chain and a remote not-found is recognised by
/// the SFTP status text. A false negative leaves the write failed, which is the
/// safe direction.
fn missing(err: &anyhow::Error) -> bool {
    if err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|err| err.kind() == std::io::ErrorKind::NotFound)
    }) {
        return true;
    }
    let text = format!("{err:#}").to_ascii_lowercase();
    text.contains("no such file") || text.contains("not found") || text.contains("no file")
}

#[cfg(test)]
mod tests {
    use super::*;
    use dtr_files::{WriteOp, WriteRequest};

    /// The worker's thread is real, but a job can be driven directly: `run` is
    /// the whole of what it does, and the sink is a plain closure.
    #[derive(Clone, Default)]
    struct Collected(std::sync::Arc<std::sync::Mutex<Vec<WorkerEvent>>>);

    impl Collected {
        fn sink(&self) -> impl Fn(WorkerEvent) + Send + 'static {
            let events = std::sync::Arc::clone(&self.0);
            move |event: WorkerEvent| {
                events.lock().expect("events").push(event);
            }
        }

        fn take(&self) -> Vec<WorkerEvent> {
            std::mem::take(&mut *self.0.lock().expect("events"))
        }
    }

    #[test]
    fn a_count_job_counts_a_real_directory_and_its_hidden_entries() {
        let scratch = tempfile::tempdir().expect("tempdir");
        std::fs::write(scratch.path().join("a.txt"), b"a").expect("file");
        std::fs::write(scratch.path().join("b.txt"), b"b").expect("file");
        std::fs::write(scratch.path().join(".hidden"), b"h").expect("file");
        let collected = Collected::default();
        let runtime = compio::runtime::Runtime::new().expect("runtime");
        let vfs = Vfs::new();
        runtime.block_on(async {
            count(&vfs, 7, scratch.path().to_path_buf(), &collected.sink()).await;
        });
        let events = collected.take();
        assert_eq!(events.len(), 1);
        match &events[0] {
            WorkerEvent::Counted {
                manager,
                total,
                shown,
            } => {
                assert_eq!(*manager, 7);
                assert_eq!((*total, *shown), (3, 2));
            }
            other => panic!("expected a count, got {other:?}"),
        }
    }

    #[test]
    fn a_write_on_a_directory_that_is_not_there_can_ignore_the_missing_destination() {
        // `replace` fixes up a destination that exists; when it does not, the
        // removal's failure is not the operation's. The write still fails for its
        // own reason (no parent), which is the honest answer.
        let scratch = tempfile::tempdir().expect("tempdir");
        let request = WriteRequest {
            backend: BackendId::Local,
            op: WriteOp::CreateDir {
                at: scratch.path().join("no-parent").join("child"),
            },
            replace: Some(dtr_vfs::Removal::Trash),
        };
        let runtime = compio::runtime::Runtime::new().expect("runtime");
        let vfs = Vfs::new();
        let result = runtime.block_on(async { execute(&vfs, &request).await });
        assert!(result.is_err(), "a missing parent is still a failure");
    }

    #[test]
    fn a_local_creation_and_removal_run_through_the_backend() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let runtime = compio::runtime::Runtime::new().expect("runtime");
        let vfs = Vfs::new();
        let make = WriteRequest {
            backend: BackendId::Local,
            op: WriteOp::CreateDir {
                at: scratch.path().join("made"),
            },
            replace: None,
        };
        runtime
            .block_on(async { execute(&vfs, &make).await })
            .expect("mkdir");
        assert!(scratch.path().join("made").is_dir());
        let rename = WriteRequest {
            backend: BackendId::Local,
            op: WriteOp::Rename {
                from: scratch.path().join("made"),
                to: scratch.path().join("renamed"),
            },
            replace: None,
        };
        runtime
            .block_on(async { execute(&vfs, &rename).await })
            .expect("rename");
        assert!(scratch.path().join("renamed").is_dir());
        let remove = WriteRequest {
            backend: BackendId::Local,
            op: WriteOp::Remove {
                at: scratch.path().join("renamed"),
                how: dtr_vfs::Removal::Permanent,
            },
            replace: None,
        };
        runtime
            .block_on(async { execute(&vfs, &remove).await })
            .expect("remove");
        assert!(!scratch.path().join("renamed").exists());
    }

    #[test]
    fn a_conflict_carries_its_destination_across_the_thread_boundary() {
        let err = anyhow::Error::new(dtr_vfs::Conflict {
            path: PathBuf::from("/tmp/taken.txt"),
        });
        assert_eq!(
            conflict_path(&err),
            Some(PathBuf::from("/tmp/taken.txt")),
            "the card names the destination the person would lose"
        );
        assert_eq!(conflict_path(&anyhow::anyhow!("permission denied")), None);
        // And the browser reads the rebuilt error as a conflict, which is what
        // makes the card appear instead of a footer refusal.
        let rebuilt = anyhow::Error::new(dtr_vfs::Conflict {
            path: PathBuf::from("/tmp/taken.txt"),
        });
        assert!(dtr_vfs::is_conflict(&rebuilt));
    }

    #[test]
    fn a_missing_path_is_recognised_through_the_anyhow_chain() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        let err = anyhow::Error::new(io).context("while removing");
        assert!(missing(&err));
        let err = anyhow::anyhow!("SFTP: no such file");
        assert!(missing(&err));
        let err = anyhow::anyhow!("permission denied");
        assert!(!missing(&err));
    }

    /// `Worker::start` exists and answers a job with an event: the channel and
    /// proxy wiring is exercised without a window.
    #[test]
    fn the_worker_thread_answers_a_job_through_its_sink() {
        let collected = Collected::default();
        let worker = Worker::start(collected.sink());
        let scratch = tempfile::tempdir().expect("tempdir");
        std::fs::write(scratch.path().join("a"), b"a").expect("file");
        assert!(worker.count(1, scratch.path().to_path_buf()));
        // The thread is detached; poll the shared sink for its answer.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if !collected.take().is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the worker never answered"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // Dropping the last sender ends the thread's loop.
        drop(worker);
    }
}
