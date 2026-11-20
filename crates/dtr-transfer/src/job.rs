//! The job model: what the queue owns and what the queue card draws.
//!
//! A job is one user action — "copy these files here", "move this to that
//! host" — and it is owned by the window's queue, not by the pane that started
//! it (`docs/manager.md` §8): closing a pane must not silently abandon a
//! transfer. The public half of [`Job`] is read-only state for the UI; the
//! write half lives in this crate because only the queue may move a phase.
//!
//! The model follows §8's list: sources, destination, direction, total where
//! `stat` knows it, bytes done, state, and the conflict answers. The
//! bookkeeping the phases need — the per-source plan, the operation in flight,
//! the question being asked — is crate-private, so an adapter can read a job
//! but cannot put it into a state the scheduler did not choose.

use std::path::PathBuf;

use dtr_vfs::{BackendId, Kind, Location, Meta};

use crate::op::Op;

/// What a job is called. Opaque and monotonic: the card keys its rows by it,
/// and a dismissed job's id is never reused within a queue.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct JobId(u64);

impl JobId {
    /// A number the queue assigned, or one a test uses for "no such job".
    ///
    /// Not for production code: the only ids a driver should hold are the ones
    /// [`Queue::copy`](crate::Queue::copy) and friends returned.
    #[doc(hidden)]
    pub fn new(number: u64) -> Self {
        Self(number)
    }

    /// The number, for a log line or a test that wants to name a job.
    pub fn number(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Which way the bytes go, as the card's word for it.
///
/// The direction is inferred from the two machines, never guessed by the
/// caller's menu: a source backend and a destination backend already answer
/// "is this an upload" (`docs/manager.md` §8). A move is a copy plus the source
/// removal, and it is its own word because a card that said "upload" for a
/// destructive operation would be lying about half of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// One machine onto itself: local to local, or a host onto the same host.
    Copy,
    /// Copy, publish, then remove the source. Any pair of machines.
    Move,
    /// This machine to an ssh host.
    Upload,
    /// An ssh host to this machine.
    Download,
    /// One ssh host to another, streamed through dtr.
    CrossHost,
}

impl Direction {
    /// Infer the direction from the endpoints.
    pub fn infer(source: &BackendId, destination: &BackendId, moving: bool) -> Self {
        if moving {
            return Self::Move;
        }
        match (source, destination) {
            (BackendId::Local, BackendId::Local) => Self::Copy,
            (BackendId::Local, BackendId::Ssh(_)) => Self::Upload,
            (BackendId::Ssh(_), BackendId::Local) => Self::Download,
            (BackendId::Ssh(from), BackendId::Ssh(to)) if from == to => Self::Copy,
            _ => Self::CrossHost,
        }
    }

    /// The word a row shows.
    pub fn word(self) -> &'static str {
        match self {
            Self::Copy => "copy",
            Self::Move => "move",
            Self::Upload => "upload",
            Self::Download => "download",
            Self::CrossHost => "copy",
        }
    }
}

/// Where a job is in its life. The five states §8 names, and nothing else: the
/// conflict question is a property of a running job ([`Job::question`]), not a
/// sixth state, because the job is still the running one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl State {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }

    pub fn word(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// The answer to "the destination exists" — §7's one card with three buttons.
///
/// The queue asks once per destination name and consumes the answer once: a
/// second "replace" must not silently replace the next file as well. That is
/// the difference between a conflict card and a "don't ask again" checkbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Replace,
    KeepBoth,
    Skip,
}

/// "This file already exists" — the question the queue asks the driver.
///
/// It is data, not a callback: the driver draws a card, and answers with
/// [`Queue::answer`](crate::Queue::answer). While the question is open the job
/// holds its machine but does no I/O, so the listing on that host is not stalled
/// by a person thinking.
#[derive(Clone, Debug)]
pub struct Conflict {
    pub job: JobId,
    /// The file being copied.
    pub source: Location,
    /// The file that is already there.
    pub destination: Location,
    /// What is there: size, kind. A directory is refused before this question,
    /// so this is a regular file.
    pub existing: Meta,
}

/// A job's plan for one source file.
///
/// Filled during the prepare pass (stat, then the conflict answer), and read by
/// the run pass. Kept across a retry: a retried job re-copies the file that
/// failed, not the ones it already published.
#[derive(Clone, Debug)]
pub(crate) struct SourcePlan {
    /// The source's file name — the name a keep-both answer numbers.
    pub(crate) name: PathBuf,
    /// The real destination path: destination directory + final name.
    pub(crate) real: Location,
    /// `<real>.part`, the only path the op writes.
    pub(crate) part: Location,
    /// The source's size where `stat` knows it.
    pub(crate) len: u64,
    /// Publish with replace: true only after the user answered "replace".
    pub(crate) overwrite: bool,
    /// The user answered "skip": no part, no publish, and the bytes come out of
    /// the job's total.
    pub(crate) skip: bool,
    /// The conflict pass has decided this source; a retry must not re-ask.
    pub(crate) resolved: bool,
    /// The part has been renamed to the real name.
    pub(crate) published: bool,
}

impl SourcePlan {
    pub(crate) fn new(source: &Location, destination: &Location) -> Self {
        let name = source
            .path
            .file_name()
            .map(PathBuf::from)
            .unwrap_or_default();
        let real_path = destination.path.join(&name);
        Self {
            name: name.clone(),
            real: Location {
                backend: destination.backend.clone(),
                path: real_path.clone(),
            },
            part: Location {
                backend: destination.backend.clone(),
                path: part_path(&real_path),
            },
            len: 0,
            overwrite: false,
            skip: false,
            resolved: false,
            published: false,
        }
    }
}

/// The path a partial transfer lives at: the real name with `.part` appended to
/// the *file name*, so it sits in the destination directory and a rename
/// publishes it without crossing a filesystem.
pub(crate) fn part_path(real: &std::path::Path) -> PathBuf {
    let mut name = real
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    name.push(".part");
    real.with_file_name(name)
}

/// One bounded action in a job's life.
///
/// The queue advances an active job by exactly one phase per step, so a step is
/// bounded work, a cancel lands between two of them, and the interleaving the
/// scheduling proof records is this machine's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Once per job: is the destination a directory?
    Destination,
    /// Stat source `i` (prepare pass).
    Stat(usize),
    /// Probe the destination name for source `i` (prepare pass).
    Conflict(usize),
    /// Once per attempt, after the prepare pass.
    Preflight,
    /// Open source `i`'s operation.
    Open(usize),
    /// Move one segment of source `i`.
    Transfer(usize),
    /// Flush and close source `i`'s part.
    Finish(usize),
    /// Rename the part to the real name.
    Publish(usize),
    /// Remove the source after a move.
    Remove(usize),
    /// Advance to the next source, or finish the job.
    Next(usize),
    /// All sources are done; the next tick reports it.
    Complete,
}

impl Phase {
    /// The phase a retry re-enters after a failure here.
    ///
    /// A failed segment, flush, or open restarts the *file* from byte zero (the
    /// part is truncated); a failed publish retries the rename only, because the
    /// part is complete; a failed source removal retries the removal, because
    /// the copy is already published. Prepare failures repeat the prepare step.
    pub(crate) fn redo(&self) -> Phase {
        match self {
            Self::Transfer(index) | Self::Finish(index) | Self::Open(index) => Self::Open(*index),
            other => other.clone(),
        }
    }
}

/// A question waiting for an answer, plus the source it is about.
#[derive(Clone, Debug)]
pub(crate) struct PendingQuestion {
    pub(crate) index: usize,
    pub(crate) conflict: Conflict,
}

/// One transfer, as the queue owns it.
pub struct Job {
    pub(crate) id: JobId,
    pub(crate) sources: Vec<Location>,
    pub(crate) destination: Location,
    pub(crate) direction: Direction,
    /// Every machine this job holds while it runs: the sources' backend and the
    /// destination's. One job never holds a machine twice, and two jobs never
    /// hold the same machine at once (`docs/manager.md` §8).
    pub(crate) aliases: Vec<BackendId>,
    pub(crate) state: State,
    /// Known only once the prepare pass has stat'ed every source.
    pub(crate) total: Option<u64>,
    /// Bytes moved, monotonic within one attempt. Includes the file in flight;
    /// a failure or a cancel drops back to what was published.
    pub(crate) done: u64,
    /// Bytes of sources that are fully published or skipped.
    pub(crate) committed: u64,
    pub(crate) error: Option<String>,
    pub(crate) plans: Vec<Option<SourcePlan>>,
    pub(crate) phase: Phase,
    pub(crate) op: Option<Box<dyn Op>>,
    /// Progress of the source in flight, reset at every `Open`.
    pub(crate) written: u64,
    pub(crate) question: Option<PendingQuestion>,
    /// One slot per source: an answer is consumed by the source it was asked
    /// about and never offered to another.
    pub(crate) answers: Vec<Option<Answer>>,
    pub(crate) cancel_requested: bool,
    pub(crate) attempt: u32,
    /// Where a retry starts: the failed phase's [`Phase::redo`].
    pub(crate) resume: Phase,
    /// The part the current op is writing, for a cancel's cleanup.
    pub(crate) current_part: Option<Location>,
}

impl Job {
    pub(crate) fn new(
        id: JobId,
        sources: Vec<Location>,
        destination: Location,
        direction: Direction,
        aliases: Vec<BackendId>,
    ) -> Self {
        let count = sources.len();
        Self {
            id,
            plans: vec![None; count],
            total: None,
            sources,
            destination,
            direction,
            aliases,
            state: State::Queued,
            done: 0,
            committed: 0,
            error: None,
            phase: Phase::Destination,
            op: None,
            written: 0,
            question: None,
            answers: vec![None; count],
            cancel_requested: false,
            attempt: 1,
            resume: Phase::Destination,
            current_part: None,
        }
    }

    /// The id the card keys the row by.
    pub fn id(&self) -> JobId {
        self.id
    }

    pub fn sources(&self) -> &[Location] {
        &self.sources
    }

    pub fn destination(&self) -> &Location {
        &self.destination
    }

    pub fn direction(&self) -> Direction {
        self.direction
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// Total bytes, once every source has been stat'ed. `None` during the
    /// prepare pass is honest: a row that showed `0 %` for a 10 GB copy because
    /// it had only measured the first file would be worse than "working…".
    pub fn total(&self) -> Option<u64> {
        self.total
    }

    /// Bytes moved so far.
    pub fn done(&self) -> u64 {
        self.done
    }

    /// Attempt number: 1 for the first run, incremented by every retry.
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The conflict question, while the job waits for an answer.
    pub fn question(&self) -> Option<&Conflict> {
        self.question.as_ref().map(|pending| &pending.conflict)
    }

    /// How far along, for a stripe. `None` while the total is unknown;
    /// an empty file that is done is 1.0, not a division by zero.
    pub fn progress(&self) -> Option<f32> {
        let total = self.total?;
        if total == 0 {
            return Some(if self.state == State::Done { 1.0 } else { 0.0 });
        }
        Some((self.done as f32 / total as f32).clamp(0.0, 1.0))
    }

    /// What a row calls the job: the file for one source, a count for many.
    pub fn name(&self) -> String {
        if let [source] = self.sources.as_slice() {
            if let Some(name) = source.path.file_name() {
                return name.to_string_lossy().into_owned();
            }
            return source.path.display().to_string();
        }
        format!("{} items", self.sources.len())
    }

    /// A cloneable snapshot for the card. The queue keeps the live job; the UI
    /// keeps these, because the queue lives on the worker thread and the card
    /// draws on the UI thread (`docs/transfers.md`).
    pub fn view(&self) -> JobView {
        JobView {
            id: self.id,
            name: self.name(),
            direction: self.direction,
            state: self.state,
            done: self.done,
            total: self.total,
            error: self.error.clone(),
            waiting: self.question.is_some(),
            attempt: self.attempt,
            can_cancel: !self.state.is_terminal(),
            can_retry: matches!(self.state, State::Failed | State::Cancelled),
        }
    }
}

/// One row of the queue card, without the live operation behind it.
#[derive(Clone, Debug, PartialEq)]
pub struct JobView {
    pub id: JobId,
    pub name: String,
    pub direction: Direction,
    pub state: State,
    pub done: u64,
    pub total: Option<u64>,
    pub error: Option<String>,
    /// A conflict question is open; the card shows a "waiting for you" mark and
    /// the conflict card is the thing that must be answered.
    pub waiting: bool,
    pub attempt: u32,
    pub can_cancel: bool,
    pub can_retry: bool,
}

impl JobView {
    /// The stripe's fraction, on the same terms as [`Job::progress`].
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total?;
        if total == 0 {
            return Some(if self.state == State::Done { 1.0 } else { 0.0 });
        }
        Some((self.done as f32 / total as f32).clamp(0.0, 1.0))
    }
}

/// Why a job could not be created. The driver's own validation, not a transfer
/// failure: nothing is queued, so nothing has to be reported as a failed row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnqueueError {
    /// A transfer of nothing is a click on a disabled button, not a job.
    NoSources,
    /// The sources came from more than one machine. One job is one stream on
    /// one machine (`docs/manager.md` §8); a selection that spans a split view
    /// is several jobs, which is the driver's call, not this crate's.
    MixedSources,
    /// A source with no file name (a root, a `..`) has no destination name to
    /// keep, so it cannot be copied.
    NoFileName { path: PathBuf },
}

impl std::fmt::Display for EnqueueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSources => f.write_str("there is nothing to transfer"),
            Self::MixedSources => {
                f.write_str("the sources are on more than one machine; transfer them separately")
            }
            Self::NoFileName { path } => write!(
                f,
                "{} has no file name to copy to the destination",
                path.display()
            ),
        }
    }
}

impl std::error::Error for EnqueueError {}

/// The kind of thing a source is, for the error message that refuses a
/// directory: recursive copies are F6+, and a silent skipped directory would be
/// worse than a failure. [`Kind`] is the VFS's answer.
pub(crate) fn describe_kind(kind: Kind) -> &'static str {
    match kind {
        Kind::File => "a file",
        Kind::Dir => "a directory",
        Kind::Other => "not a regular file",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_direction_follows_the_two_machines() {
        let local = BackendId::Local;
        let a = BackendId::Ssh("a".into());
        let b = BackendId::Ssh("b".into());
        assert_eq!(Direction::infer(&local, &local, false), Direction::Copy);
        assert_eq!(Direction::infer(&local, &a, false), Direction::Upload);
        assert_eq!(Direction::infer(&a, &local, false), Direction::Download);
        assert_eq!(Direction::infer(&a, &a, false), Direction::Copy);
        assert_eq!(Direction::infer(&a, &b, false), Direction::CrossHost);
        assert_eq!(Direction::infer(&a, &local, true), Direction::Move);
    }

    #[test]
    fn the_part_of_foo_txt_is_foo_txt_part() {
        assert_eq!(
            part_path(std::path::Path::new("/d/foo.txt")),
            PathBuf::from("/d/foo.txt.part")
        );
        assert_eq!(
            part_path(std::path::Path::new("/d/.env")),
            PathBuf::from("/d/.env.part")
        );
    }

    #[test]
    fn a_redo_of_a_transfer_restarts_the_file() {
        assert_eq!(Phase::Transfer(3).redo(), Phase::Open(3));
        assert_eq!(Phase::Finish(3).redo(), Phase::Open(3));
        assert_eq!(Phase::Publish(3).redo(), Phase::Publish(3));
        assert_eq!(Phase::Remove(3).redo(), Phase::Remove(3));
        assert_eq!(Phase::Conflict(3).redo(), Phase::Conflict(3));
    }
}
