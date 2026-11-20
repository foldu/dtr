//! The transfer queue's worker: one thread, one compio runtime, one `Queue`.
//!
//! `docs/manager.md` §8 decides that a copy is a queue of jobs with progress,
//! cancel and retry rather than a blocking verb, and `docs/transfers.md` §12.7
//! says the wiring instantiates one transport beside one queue on the thread
//! that owns the runtime. This module is that thread and the mailbox in front of
//! it.
//!
//! **The queue never leaves the worker.** `Queue` is `!Send` through the ops it
//! holds (a `dtr-sftp` session wraps thread-local handles, `docs/compio.md`
//! §10.3), and so is the one `SshTransport` created beside it. The UI owns
//! [`Command`]s going in and [`Report`]s coming out: a report is the events a
//! step produced, the rows the card draws ([`dtr_transfer::Queue::snapshot`]),
//! and the write requests that were accepted or resolved.
//!
//! **The bridge owns the pre-answer.** A file pane's write already went through
//! its own conflict card (`dtr_files::Writes`): a replace arrives here with
//! `request.replace = Some(how)`, and a keep-both arrives with a destination name
//! that is the queue's own numbered name. So the queue's `NeedsAnswer` is
//! pre-answered from that decision instead of opening a second card
//! ([`pre_answer`]), and a question that *escapes* — a destination that appeared
//! between the listing and the copy — becomes the same `dtr_vfs::Conflict` the
//! direct verb would have returned, so the browser's existing card asks it.
//!
//! **One report a frame at most.** A local copy moves a 64 KiB segment per step
//! and would otherwise wake the event loop thousands of times a second; reports
//! are coalesced on a [`REPORT_EVERY`] gate and flushed when the queue rests.
//! Commands are drained between steps, so a cancel lands on the next segment
//! rather than after the file.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use dtr_files::{WriteOp, WriteRequest};
use dtr_transfer::naming::{FIRST_SUFFIX, LAST_SUFFIX, numbered_name};
use dtr_transfer::{Answer, Event, JobId, JobView, Queue, Transport};
use dtr_vfs::Location;

/// How often the worker may wake the event loop while bytes are moving.
///
/// One report per frame is the most the UI can use; a step is one segment, and
/// a local copy runs thousands of them a second.
const REPORT_EVERY: Duration = Duration::from_millis(16);

/// The token a queued write request gets. The UI keeps it so a report's
/// `enqueued`/`finished` pairs can be matched to the browser request the job
/// answers; [`JobId`] cannot be known before the queue accepts the job.
pub type RequestId = u64;

/// What the UI asks the worker to do.
///
/// These are the queue's own synchronous methods (`copy`, `move_to`, `answer`,
/// `cancel`, `retry`, `dismiss`), wrapped so they can cross a thread boundary;
/// the worker only ever calls them on its own thread.
#[derive(Debug)]
pub enum Command {
    /// Queue one write request. `pre_answer` is the answer the browser already
    /// gave, applied the moment the queue asks ([`pre_answer`]).
    Enqueue {
        request: RequestId,
        sources: Vec<Location>,
        destination: Location,
        moving: bool,
        pre_answer: Option<Answer>,
    },
    /// Answer a conflict card the queue itself raised.
    Answer {
        job: JobId,
        answer: Answer,
    },
    Cancel {
        job: JobId,
    },
    CancelAll,
    Retry {
        job: JobId,
    },
    Dismiss {
        job: JobId,
    },
    /// Drop every finished row: what closing the card does.
    ClearFinished,
}

/// How a queued write request ended, from the browser's point of view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every source published (a skipped conflict counts: nothing was written
    /// and nothing was clobbered, which is what the browser's plan asked).
    Done,
    /// The transfer failed; the message is the queue's own words.
    Failed(String),
    /// The destination existed and the browser had not answered yet, so this
    /// becomes the same `dtr_vfs::Conflict` a direct verb returns and the
    /// browser's conflict card asks the person.
    Conflict { destination: PathBuf },
    /// A cancel, either the card's or the pane's.
    Cancelled,
}

/// What one worker wake-up tells the UI.
#[derive(Debug)]
pub struct Report {
    /// The queue's events, with pre-answered questions and the internal
    /// conflict attempt's own cleanup filtered out.
    pub events: Vec<Event>,
    /// The rows the queue card draws, as of this report.
    pub rows: Vec<JobView>,
    /// Jobs the queue accepted: which request they answer.
    pub enqueued: Vec<(RequestId, JobId)>,
    /// Requests that are over, with how they ended. A request appears once.
    pub finished: Vec<(RequestId, Outcome)>,
}

/// Where reports go. Production sends a [`UserEvent`](crate::app::UserEvent)
/// to the event loop; a test collects them on a channel.
type Sink = Box<dyn Fn(Report) + Send + 'static>;

/// The transport is built on the worker thread: `SshTransport` starts sessions
/// on the thread that polls their futures and must not be constructed off it.
type Factory = Box<dyn FnOnce() -> Box<dyn Transport> + Send + 'static>;

/// The transfer queue's thread.
pub struct Worker {
    commands: async_channel::Sender<Command>,
    /// Held so the thread's name and lifetime are visible; dropping the sender
    /// is what ends it.
    _thread: std::thread::JoinHandle<()>,
}

impl Worker {
    /// Start the window's transfer thread with production's one transport.
    ///
    /// `sink` is called on the worker thread and must hand the report to the
    /// UI (an `EventLoopProxy` is what production uses).
    pub fn spawn(sink: impl Fn(Report) + Send + 'static) -> std::io::Result<Self> {
        Self::spawn_with(sink, || Box::new(dtr_transfer_ssh::SshTransport::new()))
    }

    /// The same, with the transport the caller builds on the worker thread.
    ///
    /// `#[doc(hidden)]` test seam: the worker-thread test drives a real `Queue`
    /// through [`dtr_transfer::LocalTransport`] and the real filesystem, which is
    /// the only place the bridge's success and failure paths can be observed
    /// without a window.
    #[doc(hidden)]
    pub fn spawn_with(
        sink: impl Fn(Report) + Send + 'static,
        make: impl FnOnce() -> Box<dyn Transport> + Send + 'static,
    ) -> std::io::Result<Self> {
        let (commands, inbox) = async_channel::unbounded::<Command>();
        let thread = std::thread::Builder::new()
            .name("dtr-transfer".to_owned())
            .spawn(move || {
                let Ok(runtime) = compio::runtime::Runtime::new() else {
                    tracing::error!("no runtime for dtr-transfer");
                    return;
                };
                runtime.block_on(drive(inbox, Box::new(make), Box::new(sink)));
            })?;
        Ok(Self {
            commands,
            _thread: thread,
        })
    }

    /// Hand a command to the worker.
    ///
    /// Never blocks; a closed mailbox means the worker is gone with the window.
    pub fn submit(&self, command: Command) {
        let _ = self.commands.try_send(command);
    }
}

/// The worker's loop: apply commands, step while the queue can move, report.
async fn drive(commands: async_channel::Receiver<Command>, make: Factory, sink: Sink) {
    let mut transport = make();
    let mut queue = Queue::new();
    let mut bridge = Bridge::default();
    let mut pending: Vec<Event> = Vec::new();
    let mut last = Instant::now();
    loop {
        while let Ok(command) = commands.try_recv() {
            bridge.apply(&mut queue, command);
        }
        if queue.may_progress() {
            loop {
                let events = queue.step(&mut *transport).await;
                pending.extend(bridge.absorb(&mut queue, events));
                // A cancel arrives as a command, not as a future: draining
                // between steps is what lets it land on the next segment.
                while let Ok(command) = commands.try_recv() {
                    bridge.apply(&mut queue, command);
                }
                if last.elapsed() >= REPORT_EVERY && bridge.has_news(&pending) {
                    bridge.send(&sink, &queue, &mut pending);
                    last = Instant::now();
                }
                if !queue.may_progress() {
                    break;
                }
            }
            // Flush the tail of a burst: the last progress event and the
            // terminal event are the two the UI must not miss.
            if bridge.has_news(&pending) {
                bridge.send(&sink, &queue, &mut pending);
                last = Instant::now();
            }
            continue;
        }
        if bridge.has_news(&pending) {
            bridge.send(&sink, &queue, &mut pending);
            last = Instant::now();
        }
        match commands.recv().await {
            Ok(command) => bridge.apply(&mut queue, command),
            // The sender went away with the window.
            Err(_) => break,
        }
    }
}

/// What the bridge remembers between steps: which job answers which request,
/// the pre-answer for each job, and the news not yet reported.
#[derive(Default)]
struct Bridge {
    /// The answer the browser already gave, per job; `Some(None)` is "let the
    /// queue ask, and turn the question into the browser's own conflict".
    answers: HashMap<JobId, Option<Answer>>,
    /// The request a job answers. Removed when the request resolves: a retried
    /// job is controlled from the card, not from a browser plan that is over.
    request_of: HashMap<JobId, RequestId>,
    enqueued: Vec<(RequestId, JobId)>,
    finished: Vec<(RequestId, Outcome)>,
    /// Jobs whose conflict was answered "skip" only so they can be dismissed:
    /// their terminal events are the bridge's bookkeeping, not the card's news.
    swallowing: HashSet<JobId>,
}

impl Bridge {
    fn apply(&mut self, queue: &mut Queue, command: Command) {
        match command {
            Command::Enqueue {
                request,
                sources,
                destination,
                moving,
                pre_answer,
            } => {
                let result = if moving {
                    queue.move_to(sources, destination)
                } else {
                    queue.copy(sources, destination)
                };
                match result {
                    Ok(job) => {
                        self.answers.insert(job, pre_answer);
                        self.request_of.insert(job, request);
                        self.enqueued.push((request, job));
                    }
                    // The queue refused before anything was touched: the
                    // browser has to hear it, or its plan waits forever.
                    Err(err) => self
                        .finished
                        .push((request, Outcome::Failed(err.to_string()))),
                }
            }
            Command::Answer { job, answer } => {
                if let Err(err) = queue.answer(job, answer) {
                    // A question already answered or a card for a job that
                    // moved on is a race with the drawing frame, not a bug.
                    tracing::debug!(%job, %err, "transfer answer ignored");
                }
            }
            Command::Cancel { job } => {
                if let Err(err) = queue.cancel(job) {
                    tracing::debug!(%job, %err, "transfer cancel ignored");
                }
            }
            Command::CancelAll => queue.cancel_all(),
            Command::Retry { job } => {
                if let Err(err) = queue.retry(job) {
                    tracing::debug!(%job, %err, "transfer retry ignored");
                }
            }
            Command::Dismiss { job } => {
                if queue.dismiss(job) {
                    self.forget(job);
                }
            }
            Command::ClearFinished => {
                let finished: Vec<JobId> = queue
                    .jobs()
                    .filter(|job| job.state().is_terminal())
                    .map(|job| job.id())
                    .collect();
                for job in finished {
                    queue.dismiss(job);
                    self.forget(job);
                }
            }
        }
    }

    /// Deal with the events of one step: consume pre-answers, turn an escaped
    /// question into the browser's conflict, and return what the UI should see.
    fn absorb(&mut self, queue: &mut Queue, events: Vec<Event>) -> Vec<Event> {
        let mut out = Vec::new();
        for event in events {
            match &event {
                Event::NeedsAnswer(conflict) => {
                    let job = conflict.job;
                    match self.answers.remove(&job) {
                        // The browser already decided: answer now, and do not
                        // raise a second card.
                        Some(Some(answer)) => {
                            if let Err(err) = queue.answer(job, answer) {
                                tracing::warn!(%job, %err, "pre-answer was refused");
                                out.push(event);
                            }
                        }
                        // The browser has not decided. Its own plan's conflict
                        // card is the one card (§7); this attempt is over, and
                        // the retry is a new request.
                        Some(None) | None => {
                            let destination = conflict.destination.path.clone();
                            self.finish(job, Outcome::Conflict { destination });
                            self.swallow(job, queue);
                        }
                    }
                }
                Event::Finished { job, .. } => {
                    let job = *job;
                    if self.swallowing.remove(&job) {
                        queue.dismiss(job);
                        self.forget(job);
                        continue;
                    }
                    self.finish(job, Outcome::Done);
                    out.push(event);
                }
                Event::Failed { job, error } => {
                    let job = *job;
                    if self.swallowing.remove(&job) {
                        queue.dismiss(job);
                        self.forget(job);
                        continue;
                    }
                    self.finish(job, Outcome::Failed(error.clone()));
                    out.push(event);
                }
                Event::Cancelled { job, .. } => {
                    let job = *job;
                    if self.swallowing.remove(&job) {
                        queue.dismiss(job);
                        self.forget(job);
                        continue;
                    }
                    self.finish(job, Outcome::Cancelled);
                    out.push(event);
                }
                _ => out.push(event),
            }
        }
        out
    }

    /// End a request exactly once and forget the job's mapping. A job whose
    /// request is gone is still controllable from the card; it simply has no
    /// browser plan left to answer.
    fn finish(&mut self, job: JobId, outcome: Outcome) {
        self.answers.remove(&job);
        if let Some(request) = self.request_of.remove(&job) {
            self.finished.push((request, outcome));
        }
    }

    /// Resolve a job's question with "skip" and hide the job from the card:
    /// the browser's conflict card is the representation of this attempt, and
    /// a row that said "cancelled" would be a second, wrong story about it.
    fn swallow(&mut self, job: JobId, queue: &mut Queue) {
        self.swallowing.insert(job);
        if let Err(err) = queue.answer(job, Answer::Skip) {
            // Nothing was asked any more; there is no row to dismiss yet.
            tracing::debug!(%job, %err, "could not settle a conflict attempt");
            self.swallowing.remove(&job);
        }
    }

    fn forget(&mut self, job: JobId) {
        self.answers.remove(&job);
        self.request_of.remove(&job);
        self.swallowing.remove(&job);
    }

    fn has_news(&self, pending: &[Event]) -> bool {
        !pending.is_empty() || !self.enqueued.is_empty() || !self.finished.is_empty()
    }

    /// Take the news gathered so far. The events are drained too: a report is
    /// the whole delta since the last one.
    fn report(&mut self, queue: &Queue, pending: &mut Vec<Event>) -> Report {
        Report {
            events: std::mem::take(pending),
            rows: queue.snapshot(),
            enqueued: std::mem::take(&mut self.enqueued),
            finished: std::mem::take(&mut self.finished),
        }
    }

    fn send(&mut self, sink: &Sink, queue: &Queue, pending: &mut Vec<Event>) {
        let report = self.report(queue, pending);
        sink(report);
    }
}

/// Whether a write request goes through the queue.
///
/// Copy and move are the queue's (`docs/manager.md` §8: both, on one machine or
/// two); a rename, a delete and a mkdir are one VFS verb each and stay direct.
pub fn is_queued(request: &WriteRequest) -> bool {
    matches!(request.op, WriteOp::Copy { .. } | WriteOp::Move { .. })
}

/// The queue job a write request becomes: its one source, the destination
/// directory, and whether the source is removed after publishing.
///
/// The queue names the destination after the source file (`dtr-transfer`'s
/// `Job::name`), which is exactly the browser's destination for a plain copy,
/// a replace and a move. A keep-both destination is a numbered name that the
/// queue's own `Answer::KeepBoth` reproduces ([`pre_answer`]).
pub fn job_of(request: &WriteRequest) -> Option<(Vec<Location>, Location, bool)> {
    let (from, to, moving) = match &request.op {
        WriteOp::Copy { from, to } => (from, to, false),
        WriteOp::Move { from, to } => (from, to, true),
        _ => return None,
    };
    let destination = Location {
        backend: request.backend.clone(),
        path: to.parent()?.to_path_buf(),
    };
    let source = Location {
        backend: request.backend.clone(),
        path: from.clone(),
    };
    Some((vec![source], destination, moving))
}

/// What to answer the queue's conflict question with, from the decision the
/// browser already made.
///
/// - **replace** — the browser's card was answered "replace", so
///   `request.replace` is `Some(how)`; the queue publishes over the name.
/// - **keep both** — the browser renumbered the destination before sending
///   (its own "keep both", or a paste into the directory the file is already
///   in); the queue's `KeepBoth` walks the same `name (n).ext` rule from 2, so
///   it lands on the name the browser chose.
/// - **anything else** — no decision has been made yet. `None` lets the
///   question reach the browser's conflict card, rather than a queue-level card
///   asking a second time or, worse, a silent skip.
pub fn pre_answer(request: &WriteRequest) -> Option<Answer> {
    let (from, to) = match &request.op {
        WriteOp::Copy { from, to } | WriteOp::Move { from, to } => (from, to),
        _ => return None,
    };
    if request.replace.is_some() {
        return Some(Answer::Replace);
    }
    keeps_both(from, to).then_some(Answer::KeepBoth)
}

/// Whether `to` is the queue's own numbered name for `from`.
///
/// The browser's `next_free_name` and `dtr_transfer`'s `numbered_name` implement
/// the same rule (`name (2).ext`, counting up), so a keep-both destination is
/// one of the candidates. Anything else is a destination with a different name —
/// which the queue cannot express, and for which the answer is "ask".
fn keeps_both(from: &std::path::Path, to: &std::path::Path) -> bool {
    let (Some(source), Some(destination)) = (from.file_name(), to.file_name()) else {
        return false;
    };
    if source == destination {
        return false;
    }
    let source = std::path::Path::new(source);
    let destination = std::path::Path::new(destination);
    (FIRST_SUFFIX..=LAST_SUFFIX).any(|n| numbered_name(source, n) == destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    use dtr_transfer::{LocalTransport, State};
    use dtr_vfs::{BackendId, Removal};

    fn request(op: WriteOp, replace: Option<Removal>) -> WriteRequest {
        WriteRequest {
            backend: BackendId::Local,
            op,
            replace,
        }
    }

    fn copy(from: &str, to: &str) -> WriteRequest {
        request(
            WriteOp::Copy {
                from: PathBuf::from(from),
                to: PathBuf::from(to),
            },
            None,
        )
    }

    /// Drive a real `Queue` through `LocalTransport` with the bridge, until it
    /// has nothing left to do. No thread and no window: this is the scheduler
    /// the worker's loop runs, stepped by hand.
    fn run(queue: &mut Queue, bridge: &mut Bridge, transport: &mut LocalTransport) -> Vec<Report> {
        let mut reports = Vec::new();
        pollster::block_on(async {
            while queue.may_progress() {
                let events = queue.step(transport).await;
                let mut events = bridge.absorb(queue, events);
                if bridge.has_news(&events) {
                    reports.push(bridge.report(queue, &mut events));
                }
            }
        });
        reports
    }

    fn enqueue(
        queue: &mut Queue,
        bridge: &mut Bridge,
        request: RequestId,
        from: &Path,
        destination: &Path,
        pre_answer: Option<Answer>,
    ) {
        bridge.apply(
            queue,
            Command::Enqueue {
                request,
                sources: vec![Location::local(from.to_path_buf())],
                destination: Location::local(destination.to_path_buf()),
                moving: false,
                pre_answer,
            },
        );
    }

    fn outcomes(reports: &[Report]) -> Vec<(RequestId, Outcome)> {
        reports
            .iter()
            .flat_map(|report| report.finished.iter().cloned())
            .collect()
    }

    /// Copy and move are the queue's; the one-verb operations are not.
    #[test]
    fn copies_and_moves_are_queued_and_the_rest_are_direct() {
        assert!(is_queued(&copy("/a/x", "/b/x")));
        assert!(is_queued(&request(
            WriteOp::Move {
                from: PathBuf::from("/a/x"),
                to: PathBuf::from("/b/x"),
            },
            None
        )));
        for op in [
            WriteOp::CreateDir {
                at: PathBuf::from("/b/new"),
            },
            WriteOp::Rename {
                from: PathBuf::from("/a/x"),
                to: PathBuf::from("/a/y"),
            },
            WriteOp::Remove {
                at: PathBuf::from("/a/x"),
                how: Removal::Trash,
            },
        ] {
            assert!(!is_queued(&request(op, None)));
        }
    }

    /// A request becomes a job over its source and its destination directory;
    /// a verb with no destination cannot become one.
    #[test]
    fn a_request_names_its_source_and_its_destination_directory() {
        let (sources, destination, moving) = job_of(&copy("/a/x.txt", "/b/x.txt")).expect("a job");
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].backend, BackendId::Local);
        assert_eq!(sources[0].path, PathBuf::from("/a/x.txt"));
        assert_eq!(destination.path, PathBuf::from("/b"));
        assert!(!moving);
        let (_, _, moving) = job_of(&request(
            WriteOp::Move {
                from: PathBuf::from("/a/x.txt"),
                to: PathBuf::from("/b/x.txt"),
            },
            None,
        ))
        .expect("a job");
        assert!(moving);
        assert_eq!(
            job_of(&request(
                WriteOp::Remove {
                    at: PathBuf::from("/a/x"),
                    how: Removal::Trash
                },
                None
            )),
            None
        );
    }

    /// The browser's replace answer is the queue's replace, and the browser's
    /// keep-both rename is the queue's keep-both; a decision nobody made is
    /// left to the browser's card rather than guessed.
    #[test]
    fn the_pre_answer_is_the_browser_s_own_decision() {
        assert_eq!(pre_answer(&copy("/a/x.txt", "/b/x.txt")), None);
        let replaced = request(
            WriteOp::Copy {
                from: PathBuf::from("/a/x.txt"),
                to: PathBuf::from("/b/x.txt"),
            },
            Some(Removal::Trash),
        );
        assert_eq!(pre_answer(&replaced), Some(Answer::Replace));
        for (source, destination) in [
            ("/a/x.txt", "/b/x (2).txt"),
            ("/a/x.txt", "/b/x (7).txt"),
            ("/a/a.tar.gz", "/b/a.tar (2).gz"),
            ("/a/.env", "/b/.env (2)"),
            ("/a/report (2).txt", "/b/report (2) (2).txt"),
        ] {
            assert_eq!(
                pre_answer(&request(
                    WriteOp::Copy {
                        from: PathBuf::from(source),
                        to: PathBuf::from(destination),
                    },
                    None
                )),
                Some(Answer::KeepBoth),
                "{source} -> {destination}"
            );
        }
        // A destination with a different name is not a keep-both the queue can
        // reproduce: ask the browser instead of inventing a third name.
        assert_eq!(pre_answer(&copy("/a/x.txt", "/b/renamed.txt")), None);
    }

    /// A plain copy through the real queue: the report says done, the bytes are
    /// there, and no `.part` is left behind.
    #[test]
    fn a_plain_copy_ends_done_with_the_bytes_and_no_part() {
        let source = tempfile::tempdir().expect("source");
        let destination = tempfile::tempdir().expect("destination");
        let bytes = vec![7u8; 200_000];
        std::fs::write(source.path().join("big.bin"), &bytes).expect("a source");
        let mut queue = Queue::new();
        let mut bridge = Bridge::default();
        enqueue(
            &mut queue,
            &mut bridge,
            1,
            &source.path().join("big.bin"),
            destination.path(),
            None,
        );
        let mut transport = LocalTransport::new();
        let reports = run(&mut queue, &mut bridge, &mut transport);
        assert_eq!(outcomes(&reports), vec![(1, Outcome::Done)]);
        assert_eq!(
            std::fs::read(destination.path().join("big.bin")).expect("the copy"),
            bytes
        );
        assert!(
            !destination.path().join("big.bin.part").exists(),
            "the part must be renamed away, never left beside the real name"
        );
    }

    /// Replace is pre-answered from the browser's answer: the destination's
    /// bytes change, and no second card is raised.
    #[test]
    fn a_replace_pre_answer_overwrites_without_a_second_question() {
        let source = tempfile::tempdir().expect("source");
        let destination = tempfile::tempdir().expect("destination");
        std::fs::write(source.path().join("x.bin"), vec![2u8; 4096]).expect("a source");
        std::fs::write(destination.path().join("x.bin"), vec![1u8; 4096]).expect("an existing");
        let mut queue = Queue::new();
        let mut bridge = Bridge::default();
        enqueue(
            &mut queue,
            &mut bridge,
            1,
            &source.path().join("x.bin"),
            destination.path(),
            Some(Answer::Replace),
        );
        let mut transport = LocalTransport::new();
        let reports = run(&mut queue, &mut bridge, &mut transport);
        assert_eq!(outcomes(&reports), vec![(1, Outcome::Done)]);
        assert_eq!(
            std::fs::read(destination.path().join("x.bin")).expect("the copy"),
            vec![2u8; 4096]
        );
        assert!(
            !reports.iter().any(|report| report
                .events
                .iter()
                .any(|event| matches!(event, Event::NeedsAnswer(_)))),
            "a pre-answer must not show a second card"
        );
    }

    /// Keep both is pre-answered from the browser's numbered destination: the
    /// queue publishes beside the existing file, under the same `name (2).ext`.
    #[test]
    fn a_keep_both_pre_answer_publishes_the_numbered_name() {
        let source = tempfile::tempdir().expect("source");
        let destination = tempfile::tempdir().expect("destination");
        std::fs::write(source.path().join("x.bin"), vec![2u8; 4096]).expect("a source");
        std::fs::write(destination.path().join("x.bin"), vec![1u8; 4096]).expect("an existing");
        let mut queue = Queue::new();
        let mut bridge = Bridge::default();
        enqueue(
            &mut queue,
            &mut bridge,
            1,
            &source.path().join("x.bin"),
            destination.path(),
            Some(Answer::KeepBoth),
        );
        let mut transport = LocalTransport::new();
        let reports = run(&mut queue, &mut bridge, &mut transport);
        assert_eq!(outcomes(&reports), vec![(1, Outcome::Done)]);
        assert_eq!(
            std::fs::read(destination.path().join("x.bin")).expect("the existing"),
            vec![1u8; 4096]
        );
        assert_eq!(
            std::fs::read(destination.path().join("x (2).bin")).expect("the kept copy"),
            vec![2u8; 4096]
        );
        assert!(!reports.iter().any(|report| {
            report
                .events
                .iter()
                .any(|event| matches!(event, Event::NeedsAnswer(_)))
        }));
    }

    /// A destination that appeared after the browser listed the directory is a
    /// question nobody answered: the request resolves as the same
    /// `dtr_vfs::Conflict` a direct verb returns (so the browser's card asks),
    /// and the queue-level attempt is dismissed rather than left as a row.
    #[test]
    fn an_unanswered_conflict_becomes_the_browser_s_conflict() {
        let source = tempfile::tempdir().expect("source");
        let destination = tempfile::tempdir().expect("destination");
        std::fs::write(source.path().join("x.bin"), vec![2u8; 4096]).expect("a source");
        std::fs::write(destination.path().join("x.bin"), vec![1u8; 4096]).expect("an existing");
        let mut queue = Queue::new();
        let mut bridge = Bridge::default();
        enqueue(
            &mut queue,
            &mut bridge,
            1,
            &source.path().join("x.bin"),
            destination.path(),
            None,
        );
        let mut transport = LocalTransport::new();
        let reports = run(&mut queue, &mut bridge, &mut transport);
        assert_eq!(
            outcomes(&reports),
            vec![(
                1,
                Outcome::Conflict {
                    destination: destination.path().join("x.bin")
                }
            )]
        );
        assert!(
            !reports.iter().any(|report| report
                .events
                .iter()
                .any(|event| matches!(event, Event::NeedsAnswer(_)))),
            "the browser's own card is the one card"
        );
        assert_eq!(queue.len(), 0, "the conflict attempt is dismissed");
        assert_eq!(
            std::fs::read(destination.path().join("x.bin")).expect("the existing"),
            vec![1u8; 4096],
            "nothing was clobbered"
        );
    }

    /// Cancel is a command, and a queued job cancelled before it runs reports
    /// cancelled and writes nothing.
    #[test]
    fn a_cancel_before_the_first_segment_writes_nothing() {
        let source = tempfile::tempdir().expect("source");
        let destination = tempfile::tempdir().expect("destination");
        std::fs::write(source.path().join("x.bin"), vec![3u8; 8192]).expect("a source");
        let mut queue = Queue::new();
        let mut bridge = Bridge::default();
        enqueue(
            &mut queue,
            &mut bridge,
            1,
            &source.path().join("x.bin"),
            destination.path(),
            None,
        );
        bridge.apply(&mut queue, Command::Cancel { job: JobId::new(0) });
        let mut transport = LocalTransport::new();
        let reports = run(&mut queue, &mut bridge, &mut transport);
        assert_eq!(outcomes(&reports), vec![(1, Outcome::Cancelled)]);
        assert!(!destination.path().join("x.bin").exists());
        assert!(!destination.path().join("x.bin.part").exists());
    }

    /// The worker thread really owns the queue: a report arrives from another
    /// thread with the rows, the outcome and the bytes. This is the app-facing
    /// assertion the Xvfb run cannot make cheaply.
    #[test]
    fn the_worker_thread_reports_a_finished_copy() {
        let source = tempfile::tempdir().expect("source");
        let destination = tempfile::tempdir().expect("destination");
        let bytes = vec![9u8; 300_000];
        std::fs::write(source.path().join("worker.bin"), &bytes).expect("a source");
        let (tx, rx) = std::sync::mpsc::channel::<Report>();
        let worker = Worker::spawn_with(
            move |report| {
                let _ = tx.send(report);
            },
            || Box::new(LocalTransport::new()),
        )
        .expect("the worker thread");
        worker.submit(Command::Enqueue {
            request: 4,
            sources: vec![Location::local(source.path().join("worker.bin"))],
            destination: Location::local(destination.path().to_path_buf()),
            moving: false,
            pre_answer: None,
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut finished = Vec::new();
        while Instant::now() < deadline {
            let Ok(report) = rx.recv_timeout(Duration::from_millis(500)) else {
                continue;
            };
            finished.extend(report.finished);
            if !finished.is_empty() {
                break;
            }
        }
        assert_eq!(finished, vec![(4, Outcome::Done)]);
        assert_eq!(
            std::fs::read(destination.path().join("worker.bin")).expect("the copy"),
            bytes
        );
        assert!(!destination.path().join("worker.bin.part").exists());
    }

    /// The row mapping the card draws: a running job has a fraction once the
    /// total is known, and the job's name is the file's.
    #[test]
    fn a_report_row_carries_the_cards_facts() {
        let source = tempfile::tempdir().expect("source");
        let destination = tempfile::tempdir().expect("destination");
        std::fs::write(source.path().join("shown.bin"), vec![5u8; 123_456]).expect("a source");
        let mut queue = Queue::new();
        let mut bridge = Bridge::default();
        enqueue(
            &mut queue,
            &mut bridge,
            1,
            &source.path().join("shown.bin"),
            destination.path(),
            None,
        );
        let mut transport = LocalTransport::new();
        let reports = run(&mut queue, &mut bridge, &mut transport);
        let row = reports
            .iter()
            .flat_map(|report| report.rows.clone())
            .find(|row| row.state == State::Done)
            .expect("a done row");
        assert_eq!(row.name, "shown.bin");
        assert_eq!(row.direction, dtr_transfer::Direction::Copy);
        assert_eq!(row.total, Some(123_456));
        assert_eq!(row.fraction(), Some(1.0));
    }
}
