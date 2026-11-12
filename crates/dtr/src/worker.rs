//! A thread with a runtime of its own, for work the drawing thread must not do.
//!
//! compio is thread-per-core: a `Runtime` is created on a thread and only that
//! thread can run it, so a runtime is not something the UI thread can keep and
//! spawn on — the tasks would never be polled. This owns both: the thread, the
//! runtime it built, and a mailbox of closures to run there.
//!
//! A job is a *closure*, not a future, because its future is built on the worker
//! thread: compio's handles carry a thread-local `Rc`, so anything created off
//! the thread that will poll it is the wrong object.

use std::future::Future;

use compio::runtime::Runtime;

/// A job to run on the worker's thread, with that thread's runtime.
type Job = Box<dyn FnOnce(&Runtime) + Send>;

/// The thread and its mailbox.
pub struct Worker {
    jobs: async_channel::Sender<Job>,
    /// Held only so the thread's name and lifetime are visible here; dropping
    /// the sender is what ends it.
    _thread: std::thread::JoinHandle<()>,
}

impl Worker {
    /// Start a thread named `name`, with a runtime of its own, that runs jobs
    /// until the app drops the worker.
    pub fn spawn(name: &'static str) -> std::io::Result<Self> {
        let (jobs, inbox) = async_channel::unbounded::<Job>();
        let thread = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                let Ok(runtime) = Runtime::new() else {
                    tracing::error!("no runtime for {name}");
                    return;
                };
                let worker = runtime.clone();
                // Driving the runtime is what polls the tasks the jobs spawn; the
                // loop itself only waits for work.
                runtime.block_on(async move {
                    while let Ok(job) = inbox.recv().await {
                        job(&worker);
                    }
                });
            })?;
        Ok(Self {
            jobs,
            _thread: thread,
        })
    }

    /// Hand `job` to the worker thread.
    ///
    /// Never blocks: the mailbox is unbounded, and a closed one means the
    /// worker is gone with the app.
    pub fn submit(&self, job: impl FnOnce(&Runtime) + Send + 'static) {
        let _ = self.jobs.try_send(Box::new(job));
    }

    /// Spawn `future` there and let it finish on its own.
    ///
    /// `make` runs on the worker thread and builds the future, which is where a
    /// compio future has to be made. Not a bare `spawn`: compio cancels a task
    /// when its `JoinHandle` is dropped.
    pub fn spawn_with<F, Fut>(&self, make: F)
    where
        F: FnOnce(&Runtime) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + 'static,
    {
        self.submit(move |runtime| {
            runtime.spawn(make(runtime)).detach();
        });
    }

    /// The same for a blocking call: it gets a thread of its own from the
    /// runtime's pool, and the answer comes back as an event.
    pub fn blocking<T: Send + 'static>(&self, work: impl FnOnce() -> T + Send + 'static) {
        self.submit(move |runtime| {
            runtime.spawn_blocking(work).detach();
        });
    }
}
