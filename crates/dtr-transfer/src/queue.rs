//! The queue: one per window, jobs in creation order, one stream per machine.
//!
//! The scheduler's whole shape is §8 of `docs/manager.md`: **across machines it
//! is parallel, within one machine it is serial**. A job holds every machine it
//! touches — the sources' and the destination's — from the moment it starts
//! until it finishes, fails or is cancelled, and a queued job cannot start while
//! any of its machines is held. The destination is one of those machines, so two
//! jobs that could write the same destination name can never run at once; that
//! is why there is no second reservation scheme for destination names.
//!
//! A step advances each running job by exactly one bounded action: one `stat`,
//! one conflict probe, one open, one segment, one publish. That is what makes a
//! cancel answerable between segments and what lets a listing on the same host
//! interleave — the adapter releases the host's connection between the calls.
//!
//! The queue owns no runtime and no thread. It is a state machine that the
//! driver polls with a [`Transport`]; the driver lives where the transports live
//! (the `dtr-bg` worker, whose compio runtime polls the futures), and the UI
//! sees [`Event`]s and [`Queue::snapshot`]s. See `docs/transfers.md`.

use std::collections::HashMap;

use anyhow::Result;
use dtr_vfs::{BackendId, Kind, Location};

use crate::job::{
    Answer, Conflict, Direction, EnqueueError, Job, JobId, JobView, PendingQuestion, Phase,
    SourcePlan, State, describe_kind, part_path,
};
use crate::naming;
use crate::op::{OpenRequest, SEGMENT, Segment, Transport};

/// What the driver is told. The card folds these into its rows; the conflict
/// card opens on [`Event::NeedsAnswer`]; a toast comes from the terminal states.
#[derive(Clone, Debug)]
pub enum Event {
    /// The job got its machines and its first tick is next.
    Started { job: JobId, view: JobView },
    /// Bytes moved, or the total changed (a skipped source leaves it).
    Progress {
        job: JobId,
        done: u64,
        total: Option<u64>,
    },
    /// The destination exists and the queue will not guess. Answer once with
    /// [`Queue::answer`].
    NeedsAnswer(Conflict),
    /// One source is under its real destination name.
    Published {
        job: JobId,
        source: Location,
        destination: Location,
    },
    /// The user answered "skip": this source was not copied.
    Skipped { job: JobId, source: Location },
    /// Every source that was not skipped is published.
    Finished { job: JobId, bytes: u64 },
    /// The job stopped with an error. A part may remain; the real name does not
    /// exist unless an earlier source published.
    Failed { job: JobId, error: String },
    /// The job was cancelled. A cancelled part was removed; a warning means the
    /// adapter could not remove it.
    Cancelled { job: JobId, warning: Option<String> },
    /// A retry was accepted; the job is queued again at the phase it failed in.
    Retrying { job: JobId },
}

/// Misuse of [`Queue::answer`]: the answer does not correspond to a question.
/// A second answer for a question already answered lands here, which is what
/// "consumed once" means in practice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnswerError {
    UnknownJob,
    NoQuestion,
}

impl std::fmt::Display for AnswerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownJob => f.write_str("there is no such transfer"),
            Self::NoQuestion => f.write_str("no conflict question is waiting for an answer"),
        }
    }
}

impl std::error::Error for AnswerError {}

/// Misuse of [`Queue::cancel`]/[`Queue::retry`]: a terminal job has nothing left
/// to control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlError {
    UnknownJob,
    Finished,
    StillRunning,
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownJob => f.write_str("there is no such transfer"),
            Self::Finished => f.write_str("the transfer has already finished"),
            Self::StillRunning => f.write_str("the transfer is still running"),
        }
    }
}

impl std::error::Error for ControlError {}

/// The window's transfer queue.
///
/// `!Send` on purpose, through the ops it holds: a `dtr-sftp` session wraps
/// thread-local handles (`docs/compio.md` §10.3, trap 4), so the queue lives on
/// the worker thread that drives it and the UI keeps [`JobView`] snapshots.
pub struct Queue {
    jobs: Vec<Job>,
    next_number: u64,
    segment: usize,
    buffer: Vec<u8>,
    pending: Vec<Event>,
    /// The machine a running job holds, and the job holding it.
    held: HashMap<BackendId, JobId>,
}

impl Default for Queue {
    fn default() -> Self {
        Self::new()
    }
}

impl Queue {
    /// A queue whose segments are [`SEGMENT`] bytes.
    pub fn new() -> Self {
        Self::with_segment_size(SEGMENT)
    }

    /// A queue with a specific segment buffer. Tests use a handful of bytes to
    /// make a small file take many segments without a sleep in sight; a real
    /// transport keeps the default, because the segment is what one SFTP answer
    /// can carry.
    pub fn with_segment_size(segment: usize) -> Self {
        assert!(segment > 0, "a transfer segment cannot be empty");
        Self {
            jobs: Vec::new(),
            next_number: 0,
            segment,
            buffer: vec![0; segment],
            pending: Vec::new(),
            held: HashMap::new(),
        }
    }

    /// The segment size this queue was built with.
    pub fn segment_size(&self) -> usize {
        self.segment
    }

    /// Queue a copy of `sources` into the directory `destination`.
    ///
    /// All sources must live on one machine; the destination may be any. The
    /// direction is inferred from the two machines ([`Direction::infer`]), not
    /// asked of the caller.
    pub fn copy(
        &mut self,
        sources: Vec<Location>,
        destination: Location,
    ) -> Result<JobId, EnqueueError> {
        self.enqueue(sources, destination, false)
    }

    /// Queue a move: publish each copy, then remove its source.
    ///
    /// A move is copy-then-remove even on one machine, not a `rename`, because
    /// a rename cannot be made *no-clobber* portably: the queue's publish rule
    /// must not overwrite a file that appeared after the conflict question was
    /// answered, and `std::fs::rename` replaces unconditionally on Unix. The
    /// source is removed only after its copy is published, so an interrupted
    /// move leaves two files, never zero.
    pub fn move_to(
        &mut self,
        sources: Vec<Location>,
        destination: Location,
    ) -> Result<JobId, EnqueueError> {
        self.enqueue(sources, destination, true)
    }

    fn enqueue(
        &mut self,
        sources: Vec<Location>,
        destination: Location,
        moving: bool,
    ) -> Result<JobId, EnqueueError> {
        let Some(first) = sources.first() else {
            return Err(EnqueueError::NoSources);
        };
        let source_backend = first.backend.clone();
        for source in &sources {
            if source.backend != source_backend {
                return Err(EnqueueError::MixedSources);
            }
            if source.path.file_name().is_none() {
                return Err(EnqueueError::NoFileName {
                    path: source.path.clone(),
                });
            }
        }
        let mut aliases = Vec::with_capacity(2);
        for backend in [&source_backend, &destination.backend] {
            if !aliases.contains(backend) {
                aliases.push(backend.clone());
            }
        }
        let direction = Direction::infer(&source_backend, &destination.backend, moving);
        let id = JobId::new(self.next_number);
        self.next_number += 1;
        self.jobs
            .push(Job::new(id, sources, destination, direction, aliases));
        Ok(id)
    }

    /// Every job, in creation order — the order the card lists them in.
    pub fn jobs(&self) -> impl Iterator<Item = &Job> {
        self.jobs.iter()
    }

    pub fn job(&self, id: JobId) -> Option<&Job> {
        self.position(id).map(|index| &self.jobs[index])
    }

    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    /// A cloneable picture of every row, for a UI on another thread.
    pub fn snapshot(&self) -> Vec<JobView> {
        self.jobs.iter().map(Job::view).collect()
    }

    /// Whether any job is still queued or running.
    pub fn has_work(&self) -> bool {
        self.jobs.iter().any(|job| !job.state.is_terminal())
    }

    /// Whether calling [`step`](Self::step) once could change anything.
    ///
    /// False means the driver should wait for an answer, a cancel, a retry or a
    /// new job rather than spin: every running job is parked on a question and
    /// every queued job is behind a held machine.
    pub fn may_progress(&self) -> bool {
        self.jobs
            .iter()
            .any(|job| job.cancel_requested && !job.state.is_terminal())
            || self
                .jobs
                .iter()
                .any(|job| job.state == State::Running && job.question.is_none())
            || self
                .jobs
                .iter()
                .enumerate()
                .any(|(index, job)| job.state == State::Queued && self.machines_free(index))
    }

    /// Advance every running job by one bounded action, start at most one
    /// queued job, and return what happened.
    ///
    /// Segments for two machines are moved in the same call, in job order, so a
    /// driver on one thread still gets §8's across-machine parallelism: both
    /// operations are open, and each call gives each one a turn.
    pub async fn step(&mut self, transport: &mut dyn Transport) -> Vec<Event> {
        let mut events = std::mem::take(&mut self.pending);
        self.process_cancels(transport, &mut events).await;
        let running: Vec<JobId> = self
            .jobs
            .iter()
            .filter(|job| job.state == State::Running)
            .map(|job| job.id)
            .collect();
        for id in running {
            let Some(index) = self.position(id) else {
                continue;
            };
            if self.jobs[index].state != State::Running
                || self.jobs[index].question.is_some()
                || self.jobs[index].cancel_requested
            {
                continue;
            }
            self.advance(index, transport, &mut events).await;
        }
        self.start_one(&mut events);
        events
    }

    /// Answer the open conflict question for `job`.
    ///
    /// The answer is stored against the one source it was asked about and
    /// consumed when the next step applies it; a second answer for the same
    /// question is [`AnswerError::NoQuestion`]. A "replace" is not remembered
    /// for the next file — each collision is its own question.
    pub fn answer(&mut self, id: JobId, answer: Answer) -> Result<(), AnswerError> {
        let Some(index) = self.position(id) else {
            return Err(AnswerError::UnknownJob);
        };
        let Some(pending) = self.jobs[index].question.as_ref() else {
            return Err(AnswerError::NoQuestion);
        };
        let source_index = pending.index;
        let job = &mut self.jobs[index];
        if job.answers[source_index].is_some() {
            // Defensive: a question is cleared when answered, so this should be
            // unreachable; it keeps "consumed once" true even if a state bug
            // ever left the question in place.
            return Err(AnswerError::NoQuestion);
        }
        job.answers[source_index] = Some(answer);
        job.question = None;
        Ok(())
    }

    /// Ask a job to stop at the next segment boundary.
    ///
    /// The state change happens on the next [`step`](Self::step), because
    /// removing a part is the transport's job and a part can only be discarded
    /// with the adapter's session. A queued job is cancelled without I/O; a
    /// running one aborts its op and removes its part first.
    pub fn cancel(&mut self, id: JobId) -> Result<(), ControlError> {
        let Some(index) = self.position(id) else {
            return Err(ControlError::UnknownJob);
        };
        if self.jobs[index].state.is_terminal() {
            return Err(ControlError::Finished);
        }
        self.jobs[index].cancel_requested = true;
        Ok(())
    }

    /// Cancel every job that has not finished.
    pub fn cancel_all(&mut self) {
        for job in &mut self.jobs {
            if !job.state.is_terminal() {
                job.cancel_requested = true;
            }
        }
    }

    /// Retry a failed or cancelled job.
    ///
    /// The job resumes at the phase it stopped in: published sources stay
    /// published, a source that was mid-transfer restarts from byte zero (the
    /// part is truncated by the open), and a publish that failed retries the
    /// rename. Progress drops back to what was published, which is the only
    /// honest number after partial work was thrown away.
    pub fn retry(&mut self, id: JobId) -> Result<(), ControlError> {
        let Some(index) = self.position(id) else {
            return Err(ControlError::UnknownJob);
        };
        match self.jobs[index].state {
            State::Failed | State::Cancelled => {}
            State::Done => return Err(ControlError::Finished),
            State::Queued | State::Running => return Err(ControlError::StillRunning),
        }
        let resume = self.jobs[index].resume.clone();
        let job = &mut self.jobs[index];
        job.state = State::Queued;
        job.error = None;
        job.question = None;
        job.cancel_requested = false;
        job.op = None;
        job.current_part = None;
        job.done = job.committed;
        job.written = 0;
        job.phase = resume;
        job.attempt += 1;
        self.release(index);
        self.pending.push(Event::Retrying { job: id });
        Ok(())
    }

    /// Drop a finished job from the list. Returns whether it was there to drop.
    pub fn dismiss(&mut self, id: JobId) -> bool {
        let Some(index) = self.position(id) else {
            return false;
        };
        if !self.jobs[index].state.is_terminal() {
            return false;
        }
        self.jobs.remove(index);
        true
    }

    /// Drop every finished job. Returns how many rows went away.
    pub fn clear_finished(&mut self) -> usize {
        let before = self.jobs.len();
        self.jobs.retain(|job| !job.state.is_terminal());
        before - self.jobs.len()
    }

    fn position(&self, id: JobId) -> Option<usize> {
        self.jobs.iter().position(|job| job.id == id)
    }

    fn machines_free(&self, index: usize) -> bool {
        self.jobs[index]
            .aliases
            .iter()
            .all(|machine| !self.held.contains_key(machine))
    }

    fn hold(&mut self, index: usize) {
        let machines = self.jobs[index].aliases.clone();
        let id = self.jobs[index].id;
        for machine in machines {
            self.held.insert(machine, id);
        }
    }

    fn release(&mut self, index: usize) {
        let id = self.jobs[index].id;
        self.held.retain(|_, owner| *owner != id);
    }

    fn start_one(&mut self, events: &mut Vec<Event>) {
        for index in 0..self.jobs.len() {
            if self.jobs[index].state != State::Queued || !self.machines_free(index) {
                continue;
            }
            self.hold(index);
            self.jobs[index].state = State::Running;
            events.push(Event::Started {
                job: self.jobs[index].id,
                view: self.jobs[index].view(),
            });
            // Bounded work: one start per step. Two machines come up on two
            // consecutive steps and then run together.
            break;
        }
    }

    async fn process_cancels(&mut self, transport: &mut dyn Transport, events: &mut Vec<Event>) {
        let cancelling: Vec<JobId> = self
            .jobs
            .iter()
            .filter(|job| job.cancel_requested && !job.state.is_terminal())
            .map(|job| job.id)
            .collect();
        for id in cancelling {
            let Some(index) = self.position(id) else {
                continue;
            };
            let part = self.jobs[index].current_part.take();
            if let Some(mut op) = self.jobs[index].op.take() {
                op.abort();
            }
            let mut warning = None;
            if let Some(part) = part {
                warning = transport.discard(&part).await.err().map(|error| {
                    format!(
                        "could not remove the partial file {}: {error:#}",
                        part.describe()
                    )
                });
            }
            self.jobs[index].question = None;
            self.jobs[index].cancel_requested = false;
            self.jobs[index].current_part = None;
            self.jobs[index].state = State::Cancelled;
            self.jobs[index].done = self.jobs[index].committed;
            self.jobs[index].resume = self.jobs[index].phase.redo();
            self.release(index);
            events.push(Event::Cancelled { job: id, warning });
        }
    }

    async fn advance(
        &mut self,
        index: usize,
        transport: &mut dyn Transport,
        events: &mut Vec<Event>,
    ) {
        match self.jobs[index].phase.clone() {
            Phase::Destination => {
                let destination = self.jobs[index].destination.clone();
                match transport.stat(&destination).await {
                    Ok(Some(meta)) if meta.kind == Kind::Dir => {
                        self.jobs[index].phase = Phase::Stat(0);
                    }
                    Ok(Some(_)) => self.fail(
                        index,
                        format!(
                            "the destination {} is not a directory",
                            destination.describe()
                        ),
                        events,
                    ),
                    Ok(None) => self.fail(
                        index,
                        format!(
                            "the destination directory {} does not exist",
                            destination.describe()
                        ),
                        events,
                    ),
                    Err(error) => self.fail(
                        index,
                        format!(
                            "could not look at the destination {}: {error:#}",
                            destination.describe()
                        ),
                        events,
                    ),
                }
            }

            Phase::Stat(source_index) => {
                let source = self.jobs[index].sources[source_index].clone();
                match transport.stat(&source).await {
                    Ok(Some(meta)) if meta.kind == Kind::File => {
                        let mut plan = SourcePlan::new(&source, &self.jobs[index].destination);
                        plan.len = meta.len;
                        self.jobs[index].plans[source_index] = Some(plan);
                        let before = self.jobs[index].total;
                        self.jobs[index].total = total_of(&self.jobs[index].plans);
                        if before.is_none() && self.jobs[index].total.is_some() {
                            // The prepare pass just learned the whole size; the
                            // stripe can stop being indeterminate before the
                            // first byte moves.
                            events.push(Event::Progress {
                                job: self.jobs[index].id,
                                done: self.jobs[index].done,
                                total: self.jobs[index].total,
                            });
                        }
                        self.jobs[index].phase =
                            if source_index + 1 < self.jobs[index].sources.len() {
                                Phase::Stat(source_index + 1)
                            } else {
                                Phase::Conflict(0)
                            };
                    }
                    Ok(Some(meta)) => {
                        let message = format!(
                            "{} is {}; recursive copy is not supported yet",
                            source.describe(),
                            describe_kind(meta.kind)
                        );
                        self.fail(index, message, events);
                    }
                    Ok(None) => {
                        let message = format!("the source {} does not exist", source.describe());
                        self.fail(index, message, events);
                    }
                    Err(error) => {
                        let message = format!(
                            "could not look at the source {}: {error:#}",
                            source.describe()
                        );
                        self.fail(index, message, events);
                    }
                }
            }

            Phase::Conflict(source_index) => {
                let Some(plan) = self.jobs[index].plans[source_index].clone() else {
                    self.fail(
                        index,
                        "internal: a source was not prepared".to_string(),
                        events,
                    );
                    return;
                };
                if plan.resolved {
                    self.jobs[index].phase = after_conflict(&self.jobs[index], source_index);
                } else {
                    let real = plan.real.clone();
                    match transport.stat(&real).await {
                        Ok(None) => {
                            // Nothing there: publish without replace, and a race
                            // that fills the name later makes the publish fail
                            // rather than clobber it.
                            self.jobs[index].plans[source_index]
                                .as_mut()
                                .expect("just checked")
                                .resolved = true;
                            self.jobs[index].phase =
                                after_conflict(&self.jobs[index], source_index);
                        }
                        Ok(Some(existing)) if existing.kind != Kind::File => {
                            let message =
                                format!("{} exists and is not a regular file", real.describe());
                            self.fail(index, message, events);
                        }
                        Ok(Some(existing)) => {
                            let answer = self.jobs[index].answers[source_index].take();
                            match answer {
                                Some(Answer::Replace) => {
                                    let plan = self.jobs[index].plans[source_index]
                                        .as_mut()
                                        .expect("just checked");
                                    plan.overwrite = true;
                                    plan.resolved = true;
                                    self.jobs[index].phase =
                                        after_conflict(&self.jobs[index], source_index);
                                }
                                Some(Answer::Skip) => {
                                    let plan = self.jobs[index].plans[source_index]
                                        .as_mut()
                                        .expect("just checked");
                                    plan.skip = true;
                                    plan.resolved = true;
                                    let len = plan.len;
                                    self.jobs[index].total = self.jobs[index]
                                        .total
                                        .map(|total| total.saturating_sub(len));
                                    let job = self.jobs[index].id;
                                    let source = self.jobs[index].sources[source_index].clone();
                                    events.push(Event::Skipped { job, source });
                                    events.push(Event::Progress {
                                        job,
                                        done: self.jobs[index].done,
                                        total: self.jobs[index].total,
                                    });
                                    self.jobs[index].phase =
                                        after_conflict(&self.jobs[index], source_index);
                                }
                                Some(Answer::KeepBoth) => {
                                    match self
                                        .choose_keep_both(index, source_index, transport)
                                        .await
                                    {
                                        Ok(()) => {
                                            self.jobs[index].phase =
                                                after_conflict(&self.jobs[index], source_index);
                                        }
                                        Err(error) => {
                                            let message = format!(
                                                "could not find a free name for {}: {error:#}",
                                                real.describe()
                                            );
                                            self.fail(index, message, events);
                                        }
                                    }
                                }
                                None => {
                                    let conflict = Conflict {
                                        job: self.jobs[index].id,
                                        source: self.jobs[index].sources[source_index].clone(),
                                        destination: real,
                                        existing,
                                    };
                                    self.jobs[index].question = Some(PendingQuestion {
                                        index: source_index,
                                        conflict: conflict.clone(),
                                    });
                                    events.push(Event::NeedsAnswer(conflict));
                                }
                            }
                        }
                        Err(error) => {
                            let message =
                                format!("could not look at {}: {error:#}", real.describe());
                            self.fail(index, message, events);
                        }
                    }
                }
            }

            Phase::Preflight => {
                let destination = self.jobs[index].destination.clone();
                let bytes = self.jobs[index].total.unwrap_or(0);
                match transport.preflight(&destination, bytes).await {
                    Ok(()) => self.jobs[index].phase = Phase::Open(0),
                    Err(error) => {
                        let message = format!("the destination refused the transfer: {error:#}");
                        self.fail(index, message, events);
                    }
                }
            }

            Phase::Open(source_index) => {
                let Some(plan) = self.jobs[index].plans[source_index].clone() else {
                    self.fail(
                        index,
                        "internal: a source was not prepared".to_string(),
                        events,
                    );
                    return;
                };
                if plan.skip {
                    self.jobs[index].phase = Phase::Next(source_index);
                    return;
                }
                let request = OpenRequest {
                    job: self.jobs[index].id,
                    source: self.jobs[index].sources[source_index].clone(),
                    part: plan.part.clone(),
                    direction: self.jobs[index].direction,
                };
                match transport.open(request).await {
                    Ok(op) => {
                        self.jobs[index].op = Some(op);
                        self.jobs[index].current_part = Some(plan.part);
                        self.jobs[index].written = 0;
                        self.jobs[index].phase = Phase::Transfer(source_index);
                    }
                    Err(error) => {
                        let message = format!(
                            "could not start copying {}: {error:#}",
                            self.jobs[index].sources[source_index].describe()
                        );
                        self.fail(index, message, events);
                    }
                }
            }

            Phase::Transfer(source_index) => {
                let Some(mut op) = self.jobs[index].op.take() else {
                    self.fail(index, "internal: no operation to step".to_string(), events);
                    return;
                };
                // The buffer is taken so the op and the queue's field can be
                // borrowed at once; it is put back before anything else runs.
                let mut buffer = std::mem::take(&mut self.buffer);
                let segment = self.segment;
                let result = op.step(&mut buffer[..segment]).await;
                self.buffer = buffer;
                match result {
                    Ok(Segment::Wrote(0)) => {
                        op.abort();
                        self.fail(
                            index,
                            "the operation made no progress; refusing to loop".to_string(),
                            events,
                        );
                    }
                    Ok(Segment::Wrote(written)) if written as usize > segment => {
                        op.abort();
                        self.fail(
                            index,
                            format!(
                                "the operation moved {written} bytes for a {segment}-byte segment"
                            ),
                            events,
                        );
                    }
                    Ok(Segment::Wrote(written)) => {
                        self.jobs[index].op = Some(op);
                        self.jobs[index].written += written;
                        self.jobs[index].done += written;
                        events.push(Event::Progress {
                            job: self.jobs[index].id,
                            done: self.jobs[index].done,
                            total: self.jobs[index].total,
                        });
                    }
                    Ok(Segment::Done) => {
                        self.jobs[index].op = Some(op);
                        self.jobs[index].phase = Phase::Finish(source_index);
                    }
                    Err(error) => {
                        op.abort();
                        let message = format!(
                            "copying {} failed: {error:#}",
                            self.jobs[index].sources[source_index].describe()
                        );
                        self.fail(index, message, events);
                    }
                }
            }

            Phase::Finish(source_index) => {
                let Some(mut op) = self.jobs[index].op.take() else {
                    self.fail(
                        index,
                        "internal: no operation to finish".to_string(),
                        events,
                    );
                    return;
                };
                match op.finish().await {
                    Ok(()) => {
                        drop(op);
                        self.jobs[index].phase = Phase::Publish(source_index);
                    }
                    Err(error) => {
                        op.abort();
                        let message = format!(
                            "could not close the partial copy of {}: {error:#}",
                            self.jobs[index].sources[source_index].describe()
                        );
                        self.fail(index, message, events);
                    }
                }
            }

            Phase::Publish(source_index) => {
                let Some(plan) = self.jobs[index].plans[source_index].clone() else {
                    self.fail(
                        index,
                        "internal: a source was not prepared".to_string(),
                        events,
                    );
                    return;
                };
                match transport
                    .publish(&plan.part, &plan.real, plan.overwrite)
                    .await
                {
                    Ok(()) => {
                        if let Some(plan) = self.jobs[index].plans[source_index].as_mut() {
                            plan.published = true;
                        }
                        self.jobs[index].committed += plan.len;
                        self.jobs[index].current_part = None;
                        let job = self.jobs[index].id;
                        events.push(Event::Published {
                            job,
                            source: self.jobs[index].sources[source_index].clone(),
                            destination: plan.real.clone(),
                        });
                        self.jobs[index].phase = if self.jobs[index].direction == Direction::Move {
                            Phase::Remove(source_index)
                        } else {
                            Phase::Next(source_index)
                        };
                    }
                    Err(error) => {
                        let message =
                            format!("could not publish {}: {error:#}", plan.real.describe());
                        self.fail(index, message, events);
                    }
                }
            }

            Phase::Remove(source_index) => {
                let source = self.jobs[index].sources[source_index].clone();
                match transport.remove(&source).await {
                    Ok(()) => self.jobs[index].phase = Phase::Next(source_index),
                    Err(error) => {
                        let message = format!(
                            "published the copy, but could not remove the source {}: {error:#}",
                            source.describe()
                        );
                        self.fail(index, message, events);
                    }
                }
            }

            Phase::Next(source_index) => {
                self.jobs[index].written = 0;
                if source_index + 1 < self.jobs[index].sources.len() {
                    self.jobs[index].phase = Phase::Open(source_index + 1);
                } else {
                    self.complete(index, events);
                }
            }

            Phase::Complete => self.complete(index, events),
        }
    }

    /// Probe the numbered names until one is free and call it the destination.
    async fn choose_keep_both(
        &mut self,
        index: usize,
        source_index: usize,
        transport: &mut dyn Transport,
    ) -> Result<()> {
        let Some(plan) = self.jobs[index].plans[source_index].clone() else {
            anyhow::bail!("a source was not prepared");
        };
        let backend = plan.real.backend.clone();
        let directory = self.jobs[index].destination.path.clone();
        let name = plan.name.clone();
        let mut chosen = None;
        for number in naming::FIRST_SUFFIX..=naming::LAST_SUFFIX {
            let candidate = naming::numbered_name(&name, number);
            let location = Location {
                backend: backend.clone(),
                path: directory.join(&candidate),
            };
            if transport.stat(&location).await?.is_none() {
                chosen = Some((candidate, location));
                break;
            }
        }
        let Some((candidate, location)) = chosen else {
            anyhow::bail!("every number up to {} is taken", naming::LAST_SUFFIX);
        };
        let plan = self.jobs[index].plans[source_index]
            .as_mut()
            .expect("just checked");
        plan.name = candidate;
        plan.part = Location {
            backend: location.backend.clone(),
            path: part_path(&location.path),
        };
        plan.real = location;
        plan.overwrite = false;
        plan.resolved = true;
        Ok(())
    }

    fn complete(&mut self, index: usize, events: &mut Vec<Event>) {
        let job = self.jobs[index].id;
        self.jobs[index].op = None;
        self.jobs[index].current_part = None;
        self.jobs[index].phase = Phase::Complete;
        self.jobs[index].state = State::Done;
        self.jobs[index].done = self.jobs[index].committed;
        let bytes = self.jobs[index].committed;
        self.release(index);
        events.push(Event::Finished { job, bytes });
    }

    fn fail(&mut self, index: usize, error: String, events: &mut Vec<Event>) {
        if let Some(mut op) = self.jobs[index].op.take() {
            op.abort();
        }
        self.jobs[index].current_part = None;
        self.jobs[index].question = None;
        self.jobs[index].cancel_requested = false;
        self.jobs[index].done = self.jobs[index].committed;
        self.jobs[index].state = State::Failed;
        self.jobs[index].error = Some(error.clone());
        self.jobs[index].resume = self.jobs[index].phase.redo();
        let job = self.jobs[index].id;
        self.release(index);
        events.push(Event::Failed { job, error });
    }
}

/// Total bytes, or `None` while some source has not been stat'ed.
fn total_of(plans: &[Option<SourcePlan>]) -> Option<u64> {
    let mut total = 0u64;
    for plan in plans {
        total = total.checked_add(plan.as_ref()?.len)?;
    }
    Some(total)
}

/// The phase after source `index`'s conflict is decided.
fn after_conflict(job: &Job, index: usize) -> Phase {
    if index + 1 < job.sources.len() {
        Phase::Conflict(index + 1)
    } else {
        Phase::Preflight
    }
}
