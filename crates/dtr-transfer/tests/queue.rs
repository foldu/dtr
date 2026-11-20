//! Deterministic queue tests: an in-memory transport, no sleeps, no filesystem.
//!
//! The transport is the adapter's stand-in. It records every operation's open
//! and close per machine, refuses an open on a machine that is already open
//! (which is the per-alias serialisation assertion, checked where it would
//! break rather than inferred afterwards), and can be told to fail one op at a
//! given segment.
//!
//! Every test steps the queue by hand: the queue does no work between calls, so
//! "after three segments" is a fact about the test, not a race with a thread.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

use anyhow::{Result, anyhow, bail};
use dtr_transfer::{
    Answer, AnswerError, ControlError, Direction, EnqueueError, Event, JobId, JobView, Op,
    OpenRequest, Queue, Reader, Segment, State, StreamOp, Transport, Writer,
};
use dtr_vfs::{BackendId, Kind, Location, Meta};

type Key = (BackendId, PathBuf);

/// The fake's machine name: `"local"` is this machine, anything else is an ssh
/// alias.
fn machine(name: &str) -> BackendId {
    if name == "local" {
        BackendId::Local
    } else {
        BackendId::Ssh(name.to_string())
    }
}

fn at(machine_name: &str, path: &str) -> Location {
    Location {
        backend: machine(machine_name),
        path: PathBuf::from(path),
    }
}

fn key(machine_name: &str, path: &str) -> Key {
    (machine(machine_name), PathBuf::from(path))
}

#[derive(Debug)]
struct Span {
    job: JobId,
    machines: Vec<BackendId>,
    start: u64,
    end: Option<u64>,
}

#[derive(Default)]
struct Wire {
    files: HashMap<Key, Vec<u8>>,
    dirs: HashSet<Key>,
    /// Opened-once-per-machine bookkeeping: the count, and the high-water mark.
    open: HashMap<BackendId, usize>,
    max_open: HashMap<BackendId, usize>,
    global_open: usize,
    max_global_open: usize,
    /// A record every time two operations held one machine at once. Must stay
    /// empty; this is the per-alias serialisation assertion.
    overlaps: Vec<String>,
    spans: Vec<Span>,
    seq: u64,
    /// Segment steps, in order: (job, source machine, bytes).
    steps: Vec<(JobId, BackendId, u64)>,
    /// Injected failures: fail the `step`th segment of `job`'s op.
    failures: HashMap<(JobId, usize), String>,
    preflight_error: Option<String>,
    publishes: Vec<(JobId, Key, bool)>,
    discards: Vec<Key>,
}

/// The in-memory [`Transport`], with fault injection and an event recorder.
#[derive(Clone, Default)]
struct Fake {
    wire: Rc<RefCell<Wire>>,
}

impl Fake {
    fn new() -> Self {
        Self::default()
    }

    fn with_dir(&self, machine_name: &str, path: &str) -> &Self {
        self.wire.borrow_mut().dirs.insert(key(machine_name, path));
        self
    }

    fn with_file(&self, machine_name: &str, path: &str, bytes: &[u8]) -> &Self {
        self.wire
            .borrow_mut()
            .files
            .insert(key(machine_name, path), bytes.to_vec());
        self
    }

    fn file(&self, machine_name: &str, path: &str) -> Option<Vec<u8>> {
        self.wire
            .borrow()
            .files
            .get(&key(machine_name, path))
            .cloned()
    }

    fn has(&self, machine_name: &str, path: &str) -> bool {
        self.wire
            .borrow()
            .files
            .contains_key(&key(machine_name, path))
    }

    fn fail_at(&self, job: JobId, step: usize, message: &str) {
        self.wire
            .borrow_mut()
            .failures
            .insert((job, step), message.to_string());
    }

    fn heal(&self, job: JobId) {
        self.wire
            .borrow_mut()
            .failures
            .retain(|(failed_job, _), _| *failed_job != job);
    }

    fn refuse_preflight(&self, message: &str) {
        self.wire.borrow_mut().preflight_error = Some(message.to_string());
    }

    fn overlaps(&self) -> Vec<String> {
        self.wire.borrow().overlaps.clone()
    }

    fn max_open(&self, machine_name: &str) -> usize {
        *self
            .wire
            .borrow()
            .max_open
            .get(&machine(machine_name))
            .unwrap_or(&0)
    }

    fn max_global_open(&self) -> usize {
        self.wire.borrow().max_global_open
    }

    fn spans(&self) -> Vec<(JobId, Vec<BackendId>, u64, Option<u64>)> {
        self.wire
            .borrow()
            .spans
            .iter()
            .map(|span| (span.job, span.machines.clone(), span.start, span.end))
            .collect()
    }

    fn steps(&self) -> Vec<(JobId, BackendId, u64)> {
        self.wire.borrow().steps.clone()
    }

    fn publishes(&self) -> Vec<(JobId, Key, bool)> {
        self.wire.borrow().publishes.clone()
    }

    fn discards(&self) -> Vec<Key> {
        self.wire.borrow().discards.clone()
    }
}

impl Transport for Fake {
    fn stat<'a>(
        &'a mut self,
        at: &'a Location,
    ) -> dtr_transfer::BoxFuture<'a, Result<Option<Meta>>> {
        let wire = self.wire.clone();
        let target = (at.backend.clone(), at.path.clone());
        Box::pin(async move {
            let state = wire.borrow();
            if state.dirs.contains(&target) {
                return Ok(Some(Meta {
                    kind: Kind::Dir,
                    len: 0,
                    modified: None,
                }));
            }
            Ok(state.files.get(&target).map(|bytes| Meta {
                kind: Kind::File,
                len: bytes.len() as u64,
                modified: None,
            }))
        })
    }

    fn open<'a>(
        &'a mut self,
        request: OpenRequest,
    ) -> dtr_transfer::BoxFuture<'a, Result<Box<dyn Op>>> {
        let wire = self.wire.clone();
        Box::pin(async move {
            let mut machines = vec![request.source.backend.clone()];
            if !machines.contains(&request.part.backend) {
                machines.push(request.part.backend.clone());
            }
            let span = {
                let mut state = wire.borrow_mut();
                let mut overlaps = Vec::new();
                for machine in &machines {
                    let already = *state.open.get(machine).unwrap_or(&0);
                    if already > 0 {
                        overlaps.push(format!("{} was opened while it was already open", machine));
                    }
                    let open = state.open.entry(machine.clone()).or_insert(0);
                    *open += 1;
                    let open_now = *open;
                    let max = state.max_open.entry(machine.clone()).or_insert(0);
                    *max = (*max).max(open_now);
                }
                state.overlaps.extend(overlaps);
                state.global_open += 1;
                state.max_global_open = state.max_global_open.max(state.global_open);
                let start = state.seq;
                state.seq += 1;
                state.spans.push(Span {
                    job: request.job,
                    machines: machines.clone(),
                    start,
                    end: None,
                });
                // A fresh part: the op's contract is "create the path you were
                // given", which truncates whatever a failed attempt left.
                state.files.insert(
                    (request.part.backend.clone(), request.part.path.clone()),
                    Vec::new(),
                );
                state.spans.len() - 1
            };
            let reader = FakeReader {
                wire: wire.clone(),
                source: (request.source.backend.clone(), request.source.path.clone()),
                offset: 0,
            };
            let writer = FakeWriter {
                wire: wire.clone(),
                part: (request.part.backend.clone(), request.part.path.clone()),
            };
            let inner = StreamOp::new(Box::new(reader), Box::new(writer));
            Ok(Box::new(Guarded {
                inner: Box::new(inner),
                wire: wire.clone(),
                job: request.job,
                machines,
                span,
                steps: 0,
            }) as Box<dyn Op>)
        })
    }

    fn publish<'a>(
        &'a mut self,
        part: &'a Location,
        destination: &'a Location,
        overwrite: bool,
    ) -> dtr_transfer::BoxFuture<'a, Result<()>> {
        let wire = self.wire.clone();
        let part = (part.backend.clone(), part.path.clone());
        let destination = (destination.backend.clone(), destination.path.clone());
        Box::pin(async move {
            let mut state = wire.borrow_mut();
            let Some(bytes) = state.files.remove(&part) else {
                bail!("there is no part at {}", part.1.display());
            };
            if !overwrite && state.files.contains_key(&destination) {
                // Put it back: a retry may try again once the name is free.
                state.files.insert(part, bytes);
                bail!(
                    "{} exists and no replace was answered",
                    destination.1.display()
                );
            }
            state.files.insert(destination.clone(), bytes);
            if let Some(job) = state.spans.last().map(|span| span.job).filter(|_| true) {
                state.publishes.push((job, destination.clone(), overwrite));
            }
            Ok(())
        })
    }

    fn discard<'a>(&'a mut self, part: &'a Location) -> dtr_transfer::BoxFuture<'a, Result<()>> {
        let wire = self.wire.clone();
        let part = (part.backend.clone(), part.path.clone());
        Box::pin(async move {
            wire.borrow_mut().files.remove(&part);
            wire.borrow_mut().discards.push(part);
            Ok(())
        })
    }

    fn remove<'a>(&'a mut self, at: &'a Location) -> dtr_transfer::BoxFuture<'a, Result<()>> {
        let wire = self.wire.clone();
        let target = (at.backend.clone(), at.path.clone());
        Box::pin(async move {
            wire.borrow_mut().files.remove(&target);
            Ok(())
        })
    }

    fn preflight<'a>(
        &'a mut self,
        _destination: &'a Location,
        _bytes: u64,
    ) -> dtr_transfer::BoxFuture<'a, Result<()>> {
        let error = self.wire.borrow().preflight_error.clone();
        Box::pin(async move {
            if let Some(message) = error {
                bail!("{message}");
            }
            Ok(())
        })
    }
}

struct FakeReader {
    wire: Rc<RefCell<Wire>>,
    source: Key,
    offset: usize,
}

impl Reader for FakeReader {
    fn read<'a>(&'a mut self, buf: &'a mut [u8]) -> dtr_transfer::BoxFuture<'a, Result<usize>> {
        let wire = self.wire.clone();
        let source = self.source.clone();
        Box::pin(async move {
            let state = wire.borrow();
            let Some(bytes) = state.files.get(&source) else {
                bail!("the source {} vanished", source.1.display());
            };
            let end = bytes.len().min(self.offset + buf.len());
            let slice = &bytes[self.offset..end];
            buf[..slice.len()].copy_from_slice(slice);
            let read = slice.len();
            drop(state);
            self.offset += read;
            Ok(read)
        })
    }
}

struct FakeWriter {
    wire: Rc<RefCell<Wire>>,
    part: Key,
}

impl Writer for FakeWriter {
    fn write<'a>(&'a mut self, data: &'a [u8]) -> dtr_transfer::BoxFuture<'a, Result<()>> {
        let wire = self.wire.clone();
        let part = self.part.clone();
        Box::pin(async move {
            let mut state = wire.borrow_mut();
            state.files.entry(part).or_default().extend_from_slice(data);
            Ok(())
        })
    }

    fn finish<'a>(&'a mut self) -> dtr_transfer::BoxFuture<'a, Result<()>> {
        Box::pin(async move { Ok(()) })
    }

    fn abort(&mut self) {}
}

/// The op the transport hands out: the real `StreamOp` with failure injection
/// and the open/close bookkeeping wrapped around it.
struct Guarded {
    inner: Box<dyn Op>,
    wire: Rc<RefCell<Wire>>,
    job: JobId,
    machines: Vec<BackendId>,
    span: usize,
    steps: usize,
}

impl Op for Guarded {
    fn step<'a>(&'a mut self, buf: &'a mut [u8]) -> dtr_transfer::BoxFuture<'a, Result<Segment>> {
        let step = self.steps + 1;
        self.steps = step;
        let failure = self.wire.borrow().failures.get(&(self.job, step)).cloned();
        if let Some(message) = failure {
            return Box::pin(async move { Err(anyhow!("{message}")) });
        }
        let wire = self.wire.clone();
        let job = self.job;
        let source_machine = self.machines[0].clone();
        let inner = &mut self.inner;
        Box::pin(async move {
            let segment = inner.step(buf).await?;
            if let Segment::Wrote(bytes) = segment {
                wire.borrow_mut().steps.push((job, source_machine, bytes));
            }
            Ok(segment)
        })
    }

    fn finish<'a>(&'a mut self) -> dtr_transfer::BoxFuture<'a, Result<()>> {
        self.inner.finish()
    }

    fn abort(&mut self) {
        self.inner.abort();
    }
}

impl Drop for Guarded {
    fn drop(&mut self) {
        let mut state = self.wire.borrow_mut();
        for machine in &self.machines {
            if let Some(open) = state.open.get_mut(machine) {
                *open = open.saturating_sub(1);
            }
        }
        state.global_open = state.global_open.saturating_sub(1);
        let end = state.seq;
        state.seq += 1;
        if let Some(span) = state.spans.get_mut(self.span) {
            span.end = Some(end);
        }
    }
}

fn step(queue: &mut Queue, transport: &Fake) -> Vec<Event> {
    let mut transport = transport.clone();
    pollster::block_on(queue.step(&mut transport))
}

/// Step until the queue can make no more progress on its own. A conflict
/// question, a held machine or a finished list all stop it; the caller decides
/// what to answer next.
fn run_until_idle(queue: &mut Queue, transport: &Fake) -> Vec<Event> {
    let mut events = Vec::new();
    let mut guard = 0;
    while queue.may_progress() {
        events.extend(step(queue, transport));
        guard += 1;
        assert!(
            guard < 100_000,
            "the queue did not settle: {:?}",
            queue.snapshot()
        );
    }
    events
}

// ---------------------------------------------------------------------------
// Scheduling: per-machine serial, across machines parallel.
// ---------------------------------------------------------------------------

#[test]
fn two_jobs_on_one_machine_never_overlap() {
    let fake = Fake::new();
    fake.with_dir("a", "/out")
        .with_file("a", "/in/one.txt", b"0123456789")
        .with_file("a", "/in/two.txt", b"abcdefghij");
    let mut queue = Queue::with_segment_size(4);

    let one = queue
        .copy(vec![at("a", "/in/one.txt")], at("a", "/out"))
        .expect("queued");
    let two = queue
        .copy(vec![at("a", "/in/two.txt")], at("a", "/out"))
        .expect("queued");

    let events = run_until_idle(&mut queue, &fake);
    assert!(
        fake.overlaps().is_empty(),
        "an op overlapped: {:?}",
        fake.overlaps()
    );
    assert_eq!(fake.max_open("a"), 1, "one machine, one stream");

    // The second job cannot start until the first released the machine, so its
    // segments are all after the first job's, and the two operation windows are
    // disjoint.
    let steps = fake.steps();
    let first_two = steps.iter().position(|(job, _, _)| *job == two);
    let last_one = steps.iter().rposition(|(job, _, _)| *job == one);
    assert!(
        first_two.expect("the second job moved") > last_one.expect("the first job moved"),
        "the two jobs interleaved on one machine: {steps:?}"
    );
    let spans = fake.spans();
    assert_eq!(spans.len(), 2);
    let (_, _, first_start, first_end) = spans[0].clone();
    let (_, _, second_start, second_end) = spans[1].clone();
    assert!(
        first_end.expect("closed") <= second_start && first_start <= second_start,
        "the operation windows overlapped on one machine: {spans:?}"
    );
    assert!(second_end.is_some());
    assert_eq!(
        fake.file("a", "/out/one.txt").as_deref(),
        Some(&b"0123456789"[..])
    );
    assert_eq!(
        fake.file("a", "/out/two.txt").as_deref(),
        Some(&b"abcdefghij"[..])
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Finished { job, .. } if *job == one))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Finished { job, .. } if *job == two))
    );
}

#[test]
fn two_machines_run_at_the_same_time() {
    let fake = Fake::new();
    fake.with_dir("a", "/out")
        .with_dir("b", "/out")
        .with_file("a", "/in/one.txt", b"01234567")
        .with_file("b", "/in/two.txt", b"abcdefgh");
    let mut queue = Queue::with_segment_size(2);

    let one = queue
        .copy(vec![at("a", "/in/one.txt")], at("a", "/out"))
        .expect("queued");
    let two = queue
        .copy(vec![at("b", "/in/two.txt")], at("b", "/out"))
        .expect("queued");
    let events = run_until_idle(&mut queue, &fake);

    assert!(
        fake.overlaps().is_empty(),
        "an op overlapped: {:?}",
        fake.overlaps()
    );
    assert_eq!(fake.max_open("a"), 1);
    assert_eq!(fake.max_open("b"), 1);
    assert_eq!(
        fake.max_global_open(),
        2,
        "two machines should have two operations open at once"
    );

    // Both jobs move a segment in the same step once both are transferring: the
    // sequence interleaves rather than running one machine's file after the
    // other's. (The first job is one tick ahead because a job starts one step
    // before the next can, so the very first segment is the only one alone.)
    let steps: Vec<JobId> = fake.steps().iter().map(|(job, _, _)| *job).collect();
    assert_eq!(steps.len(), 8);
    let first_two = steps
        .iter()
        .position(|job| *job == two)
        .expect("second moved");
    let last_one = steps
        .iter()
        .rposition(|job| *job == one)
        .expect("first moved");
    let first_one = steps
        .iter()
        .position(|job| *job == one)
        .expect("first moved");
    let last_two = steps
        .iter()
        .rposition(|job| *job == two)
        .expect("second moved");
    assert!(
        first_two < last_one && first_one < last_two,
        "the segments did not interleave: {steps:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Finished { .. }))
    );
    let spans = fake.spans();
    assert_eq!(spans.len(), 2);
    // The two operations were open together.
    assert!(spans[0].3.is_some());
    let one_span = spans[0].clone();
    let two_span = spans[1].clone();
    assert!(
        one_span.2 < two_span.2 && two_span.2 < one_span.3.expect("closed"),
        "the operations did not overlap: {one_span:?} {two_span:?}"
    );
    assert_eq!(fake.max_open("a"), 1);
    assert_eq!(fake.max_open("b"), 1);
}

#[test]
fn a_cross_host_job_holds_both_machines() {
    let fake = Fake::new();
    fake.with_dir("b", "/out")
        .with_dir("c", "/out")
        .with_file("a", "/in/one.txt", b"01234567")
        .with_file("b", "/in/two.txt", b"abcdefgh");
    let mut queue = Queue::with_segment_size(2);

    // A -> B holds a and b; B -> C holds b and c. They share b, so the second
    // must wait for the first even though they could otherwise be parallel.
    let first = queue
        .copy(vec![at("a", "/in/one.txt")], at("b", "/out"))
        .expect("queued");
    let second = queue
        .copy(vec![at("b", "/in/two.txt")], at("c", "/out"))
        .expect("queued");
    assert_eq!(
        queue.job(first).expect("job").direction(),
        Direction::CrossHost
    );
    run_until_idle(&mut queue, &fake);

    assert!(
        fake.overlaps().is_empty(),
        "an op overlapped: {:?}",
        fake.overlaps()
    );
    assert_eq!(fake.max_open("b"), 1);
    let spans = fake.spans();
    assert_eq!(spans.len(), 2);
    let first_span = spans[0].clone();
    let second_span = spans[1].clone();
    assert_eq!(first_span.0, first);
    assert_eq!(second_span.0, second);
    assert!(
        first_span.3.expect("closed") < second_span.2,
        "the second job started while the first held one of its machines"
    );
    assert_eq!(
        fake.file("b", "/out/one.txt").as_deref(),
        Some(&b"01234567"[..])
    );
    assert_eq!(
        fake.file("c", "/out/two.txt").as_deref(),
        Some(&b"abcdefgh"[..])
    );
}

#[test]
fn the_scheduler_does_not_let_a_blocked_job_starve_a_free_machine() {
    let fake = Fake::new();
    fake.with_dir("a", "/out")
        .with_dir("b", "/out")
        .with_file("a", "/in/one.txt", b"01234567")
        .with_file("a", "/in/two.txt", b"89abcdef")
        .with_file("b", "/in/three.txt", b"ABCDEFGH");
    let mut queue = Queue::with_segment_size(2);

    // Two jobs queue for `a` first; the third is for `b` and must not wait
    // behind them.
    let _first = queue
        .copy(vec![at("a", "/in/one.txt")], at("a", "/out"))
        .expect("queued");
    let _second = queue
        .copy(vec![at("a", "/in/two.txt")], at("a", "/out"))
        .expect("queued");
    let third = queue
        .copy(vec![at("b", "/in/three.txt")], at("b", "/out"))
        .expect("queued");
    run_until_idle(&mut queue, &fake);

    let steps = fake.steps();
    let third_steps = steps.iter().filter(|(job, _, _)| *job == third).count();
    assert_eq!(third_steps, 4);
    assert!(fake.overlaps().is_empty());
    assert_eq!(fake.max_open("a"), 1);
    assert_eq!(fake.max_open("b"), 1);
    assert!(fake.max_global_open() >= 2);
}

// ---------------------------------------------------------------------------
// Progress accounting.
// ---------------------------------------------------------------------------

#[test]
fn a_multi_chunk_file_reports_monotonic_progress() {
    let fake = Fake::new();
    fake.with_dir("a", "/out")
        .with_file("a", "/in/data.bin", b"01234567890123456789");
    let mut queue = Queue::with_segment_size(4);
    let job = queue
        .copy(vec![at("a", "/in/data.bin")], at("a", "/out"))
        .expect("queued");
    let events = run_until_idle(&mut queue, &fake);

    let progress: Vec<(u64, Option<u64>)> = events
        .iter()
        .filter_map(|event| match event {
            Event::Progress {
                job: id,
                done,
                total,
            } if *id == job => Some((*done, *total)),
            _ => None,
        })
        .collect();
    assert_eq!(
        progress,
        vec![
            (0, Some(20)),
            (4, Some(20)),
            (8, Some(20)),
            (12, Some(20)),
            (16, Some(20)),
            (20, Some(20)),
        ]
    );
    assert_eq!(
        fake.file("a", "/out/data.bin").as_deref(),
        Some(&b"01234567890123456789"[..])
    );
    let finished = events
        .iter()
        .find_map(|event| match event {
            Event::Finished { job: id, bytes } if *id == job => Some(*bytes),
            _ => None,
        })
        .expect("finished");
    assert_eq!(finished, 20);
    let live = queue.job(job).expect("job");
    assert_eq!(live.state(), State::Done);
    assert_eq!(live.progress(), Some(1.0));
}

#[test]
fn an_empty_file_finishes_with_zero_bytes() {
    let fake = Fake::new();
    fake.with_dir("a", "/out").with_file("a", "/in/empty", b"");
    let mut queue = Queue::with_segment_size(8);
    let job = queue
        .copy(vec![at("a", "/in/empty")], at("a", "/out"))
        .expect("queued");
    let events = run_until_idle(&mut queue, &fake);

    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Finished { job: id, bytes: 0 } if *id == job))
    );
    assert_eq!(fake.file("a", "/out/empty").as_deref(), Some(&b""[..]));
    let live = queue.job(job).expect("job");
    assert_eq!(live.total(), Some(0));
    assert_eq!(live.done(), 0);
    assert_eq!(live.progress(), Some(1.0));
    assert!(fake.steps().is_empty());
}

// ---------------------------------------------------------------------------
// Conflicts: asked, answered once, and each answer does what it says.
// ---------------------------------------------------------------------------

fn collision_fixture() -> (Fake, Queue, JobId) {
    let fake = Fake::new();
    fake.with_dir("a", "/out")
        .with_file("a", "/in/x.txt", b"new bytes")
        .with_file("a", "/out/x.txt", b"old bytes");
    let mut queue = Queue::with_segment_size(4);
    let job = queue
        .copy(vec![at("a", "/in/x.txt")], at("a", "/out"))
        .expect("queued");
    (fake, queue, job)
}

fn answer_next(queue: &mut Queue, fake: &Fake, expected: JobId, answer: Answer) -> Vec<Event> {
    let events = run_until_idle(queue, fake);
    let conflict = events
        .iter()
        .find_map(|event| match event {
            Event::NeedsAnswer(conflict) if conflict.job == expected => Some(conflict.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no conflict question: {events:?}"));
    assert_eq!(conflict.destination.path, PathBuf::from("/out/x.txt"));
    queue.answer(expected, answer).expect("answered");
    events
}

#[test]
fn replace_publishes_over_the_existing_file() {
    let (fake, mut queue, job) = collision_fixture();
    answer_next(&mut queue, &fake, job, Answer::Replace);
    let events = run_until_idle(&mut queue, &fake);
    assert_eq!(
        fake.file("a", "/out/x.txt").as_deref(),
        Some(&b"new bytes"[..])
    );
    assert!(!fake.has("a", "/out/x.txt.part"));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Finished { job: id, .. } if *id == job))
    );
    assert_eq!(fake.publishes().len(), 1);
    assert!(fake.publishes()[0].2, "replace means overwrite = true");
}

#[test]
fn keep_both_picks_the_next_free_name() {
    let fake = Fake::new();
    fake.with_dir("a", "/out")
        .with_file("a", "/in/x.txt", b"new bytes")
        .with_file("a", "/out/x.txt", b"old bytes")
        .with_file("a", "/out/x (2).txt", b"older bytes");
    let mut queue = Queue::with_segment_size(4);
    let job = queue
        .copy(vec![at("a", "/in/x.txt")], at("a", "/out"))
        .expect("queued");
    answer_next(&mut queue, &fake, job, Answer::KeepBoth);
    run_until_idle(&mut queue, &fake);

    assert_eq!(
        fake.file("a", "/out/x.txt").as_deref(),
        Some(&b"old bytes"[..])
    );
    assert_eq!(
        fake.file("a", "/out/x (2).txt").as_deref(),
        Some(&b"older bytes"[..])
    );
    assert_eq!(
        fake.file("a", "/out/x (3).txt").as_deref(),
        Some(&b"new bytes"[..])
    );
    assert!(!fake.has("a", "/out/x (3).txt.part"));
}

#[test]
fn skip_leaves_the_destination_untouched() {
    let (fake, mut queue, job) = collision_fixture();
    answer_next(&mut queue, &fake, job, Answer::Skip);
    let events = run_until_idle(&mut queue, &fake);

    assert_eq!(
        fake.file("a", "/out/x.txt").as_deref(),
        Some(&b"old bytes"[..])
    );
    assert!(!fake.has("a", "/out/x.txt.part"));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Skipped { job: id, .. } if *id == job))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Finished { job: id, bytes: 0 } if *id == job))
    );
    assert!(fake.steps().is_empty(), "a skipped file moves no bytes");
}

#[test]
fn an_answer_is_consumed_once_and_never_leaks_to_the_next_file() {
    let fake = Fake::new();
    fake.with_dir("a", "/out")
        .with_file("a", "/in/x.txt", b"new x")
        .with_file("a", "/in/y.txt", b"new y")
        .with_file("a", "/out/x.txt", b"old x")
        .with_file("a", "/out/y.txt", b"old y");
    let mut queue = Queue::with_segment_size(4);
    let job = queue
        .copy(
            vec![at("a", "/in/x.txt"), at("a", "/in/y.txt")],
            at("a", "/out"),
        )
        .expect("queued");

    // The first collision is asked about, answered, and a second answer for the
    // same question is refused: the answer is consumed by that file.
    let events = run_until_idle(&mut queue, &fake);
    let first = events
        .iter()
        .find_map(|event| match event {
            Event::NeedsAnswer(conflict) => Some(conflict.clone()),
            _ => None,
        })
        .expect("the first conflict");
    assert_eq!(first.source.path, PathBuf::from("/in/x.txt"));
    queue.answer(job, Answer::Replace).expect("answered");
    assert_eq!(
        queue.answer(job, Answer::Replace),
        Err(AnswerError::NoQuestion),
        "a second answer for one question must be refused"
    );

    // The second file collides on its own and asks its own question: the
    // "replace" was not remembered for it.
    let events = run_until_idle(&mut queue, &fake);
    let second = events
        .iter()
        .find_map(|event| match event {
            Event::NeedsAnswer(conflict) => Some(conflict.clone()),
            _ => None,
        })
        .expect("the second conflict");
    assert_eq!(second.source.path, PathBuf::from("/in/y.txt"));
    queue.answer(job, Answer::Skip).expect("answered");
    run_until_idle(&mut queue, &fake);

    assert_eq!(fake.file("a", "/out/x.txt").as_deref(), Some(&b"new x"[..]));
    assert_eq!(fake.file("a", "/out/y.txt").as_deref(), Some(&b"old y"[..]));
    assert!(queue.job(job).expect("job").state() == State::Done);
    // The skip took its bytes out of the total.
    assert_eq!(queue.job(job).expect("job").total(), Some(5));
}

#[test]
fn answers_without_a_question_are_refused() {
    let fake = Fake::new();
    fake.with_dir("a", "/out")
        .with_file("a", "/in/x.txt", b"bytes");
    let mut queue = Queue::with_segment_size(4);
    let job = queue
        .copy(vec![at("a", "/in/x.txt")], at("a", "/out"))
        .expect("queued");
    assert_eq!(
        queue.answer(job, Answer::Replace),
        Err(AnswerError::NoQuestion)
    );
    run_until_idle(&mut queue, &fake);
    assert_eq!(
        queue.answer(job, Answer::Replace),
        Err(AnswerError::NoQuestion),
        "an answer after the transfer finished is not an answer to anything"
    );
    assert_eq!(
        queue.answer(JobId::new(99), Answer::Replace),
        Err(AnswerError::UnknownJob)
    );
}

// ---------------------------------------------------------------------------
// Cancel and retry.
// ---------------------------------------------------------------------------

#[test]
fn cancel_between_segments_removes_the_part_and_reports_cancelled() {
    let fake = Fake::new();
    fake.with_dir("a", "/out")
        .with_file("a", "/in/big.bin", &[7u8; 100]);
    let mut queue = Queue::with_segment_size(4);
    let job = queue
        .copy(vec![at("a", "/in/big.bin")], at("a", "/out"))
        .expect("queued");

    // Drive until a segment has landed, so there is a part to discard.
    let mut events = Vec::new();
    let mut guard = 0;
    while !fake.has("a", "/out/big.bin.part")
        || fake
            .file("a", "/out/big.bin.part")
            .unwrap_or_default()
            .is_empty()
    {
        events.extend(step(&mut queue, &fake));
        guard += 1;
        assert!(guard < 1_000, "never started moving bytes");
    }
    assert!(!fake.has("a", "/out/big.bin"));
    queue.cancel(job).expect("cancel accepted");
    events.extend(run_until_idle(&mut queue, &fake));

    assert!(
        events.iter().any(
            |event| matches!(event, Event::Cancelled { job: id, warning: None } if *id == job)
        ),
        "no cancellation: {events:?}"
    );
    assert!(
        !fake.has("a", "/out/big.bin"),
        "a cancelled copy must not publish"
    );
    assert!(
        !fake.has("a", "/out/big.bin.part"),
        "a cancelled copy must remove its part"
    );
    assert_eq!(queue.job(job).expect("job").state(), State::Cancelled);
    assert_eq!(queue.job(job).expect("job").done(), 0);
    assert!(!fake.discards().is_empty());
}

#[test]
fn cancel_of_a_queued_job_touches_no_file() {
    let fake = Fake::new();
    fake.with_dir("a", "/out")
        .with_file("a", "/in/one.txt", b"0123")
        .with_file("a", "/in/two.txt", b"4567");
    let mut queue = Queue::with_segment_size(2);
    let first = queue
        .copy(vec![at("a", "/in/one.txt")], at("a", "/out"))
        .expect("queued");
    let second = queue
        .copy(vec![at("a", "/in/two.txt")], at("a", "/out"))
        .expect("queued");
    // Let the first start and take the machine, then cancel the second while it
    // waits.
    let mut guard = 0;
    while queue.job(first).expect("job").state() != State::Running {
        step(&mut queue, &fake);
        guard += 1;
        assert!(guard < 100);
    }
    queue.cancel(second).expect("cancel accepted");
    let events = run_until_idle(&mut queue, &fake);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Cancelled { job, .. } if *job == second))
    );
    assert!(!fake.has("a", "/out/two.txt"));
    assert!(!fake.has("a", "/out/two.txt.part"));
    assert_eq!(
        queue.cancel(second),
        Err(ControlError::Finished),
        "cancelling a cancelled job is not a second cancel"
    );
}

#[test]
fn a_failure_leaves_no_real_name_and_a_retry_restarts_the_file() {
    let fake = Fake::new();
    fake.with_dir("a", "/out")
        .with_file("a", "/in/big.bin", &[9u8; 16]);
    let mut queue = Queue::with_segment_size(4);
    let job = queue
        .copy(vec![at("a", "/in/big.bin")], at("a", "/out"))
        .expect("queued");
    fake.fail_at(job, 2, "the link dropped");

    let events = run_until_idle(&mut queue, &fake);
    assert!(
        events.iter().any(|event| matches!(event, Event::Failed { job: id, error } if *id == job && error.contains("the link dropped"))),
        "no failure: {events:?}"
    );
    assert!(
        !fake.has("a", "/out/big.bin"),
        "a failed transfer must not publish"
    );
    assert!(
        fake.has("a", "/out/big.bin.part"),
        "the part is kept for the retry"
    );
    assert_eq!(queue.job(job).expect("job").state(), State::Failed);
    assert!(queue.job(job).expect("job").view().can_retry);

    // Retry with the fault gone: the file starts over, finishes, and the part
    // becomes the real name.
    fake.heal(job);
    queue.retry(job).expect("retry accepted");
    assert_eq!(queue.job(job).expect("job").attempt(), 2);
    let events = run_until_idle(&mut queue, &fake);

    let retried = events.iter().find_map(|event| match event {
        Event::Retrying { job: id } if *id == job => Some(()),
        _ => None,
    });
    assert!(retried.is_some(), "no retrying event: {events:?}");
    assert_eq!(
        fake.file("a", "/out/big.bin").as_deref(),
        Some(&[9u8; 16][..])
    );
    assert!(!fake.has("a", "/out/big.bin.part"));
    assert_eq!(queue.job(job).expect("job").state(), State::Done);
    // The restarted file's first progress is one segment, not the old total.
    let after_retry = events
        .iter()
        .find_map(|event| match event {
            Event::Progress { job: id, done, .. } if *id == job => Some(*done),
            _ => None,
        })
        .expect("progress after retry");
    assert_eq!(after_retry, 4, "the retry restarts the file from zero");
    // Two operations were opened: the failed attempt and the retry.
    assert_eq!(fake.spans().len(), 2);
}

#[test]
fn retry_of_a_finished_job_is_refused() {
    let fake = Fake::new();
    fake.with_dir("a", "/out")
        .with_file("a", "/in/x.txt", b"bytes");
    let mut queue = Queue::with_segment_size(4);
    let job = queue
        .copy(vec![at("a", "/in/x.txt")], at("a", "/out"))
        .expect("queued");
    assert_eq!(
        queue.retry(job),
        Err(ControlError::StillRunning),
        "a queued job is retried by starting it, not by retrying it"
    );
    run_until_idle(&mut queue, &fake);
    assert_eq!(queue.retry(job), Err(ControlError::Finished));
    assert_eq!(queue.cancel(job), Err(ControlError::Finished));
}

// ---------------------------------------------------------------------------
// Failures that happen before any byte: directories, preflight, destinations.
// ---------------------------------------------------------------------------

#[test]
fn a_directory_source_is_refused_with_a_message() {
    let fake = Fake::new();
    fake.with_dir("a", "/out").with_dir("a", "/in/sub");
    let mut queue = Queue::with_segment_size(4);
    let job = queue
        .copy(vec![at("a", "/in/sub")], at("a", "/out"))
        .expect("queued");
    let events = run_until_idle(&mut queue, &fake);
    let failed = events
        .iter()
        .find_map(|event| match event {
            Event::Failed { job: id, error } if *id == job => Some(error.clone()),
            _ => None,
        })
        .expect("failed");
    assert!(failed.contains("directory"), "{failed}");
    assert!(failed.contains("recursive"), "{failed}");
    assert!(fake.spans().is_empty(), "nothing was opened");
    assert!(!fake.has("a", "/out/sub"));
    assert!(!fake.has("a", "/out/sub.part"));
}

#[test]
fn a_preflight_refusal_stops_the_job_before_it_opens_anything() {
    let fake = Fake::new();
    fake.with_dir("a", "/out")
        .with_file("a", "/in/big.bin", &[1u8; 32])
        .refuse_preflight("only 4 bytes left");
    let mut queue = Queue::with_segment_size(4);
    let job = queue
        .copy(vec![at("a", "/in/big.bin")], at("a", "/out"))
        .expect("queued");
    let events = run_until_idle(&mut queue, &fake);
    assert!(events.iter().any(|event| matches!(event, Event::Failed { job: id, error } if *id == job && error.contains("only 4 bytes left"))));
    assert!(fake.spans().is_empty(), "preflight runs before any open");
    assert!(!fake.has("a", "/out/big.bin.part"));
}

#[test]
fn a_missing_destination_directory_fails_the_job() {
    let fake = Fake::new();
    fake.with_file("a", "/in/x.txt", b"bytes");
    let mut queue = Queue::with_segment_size(4);
    let job = queue
        .copy(vec![at("a", "/in/x.txt")], at("a", "/nowhere"))
        .expect("queued");
    let events = run_until_idle(&mut queue, &fake);
    let failed = events
        .iter()
        .find_map(|event| match event {
            Event::Failed { job: id, error } if *id == job => Some(error.clone()),
            _ => None,
        })
        .expect("failed");
    assert!(failed.contains("does not exist"), "{failed}");
}

#[test]
fn a_missing_source_fails_the_job_before_a_part_exists() {
    let fake = Fake::new();
    fake.with_dir("a", "/out");
    let mut queue = Queue::with_segment_size(4);
    let job = queue
        .copy(vec![at("a", "/in/gone.txt")], at("a", "/out"))
        .expect("queued");
    let events = run_until_idle(&mut queue, &fake);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Failed { job: id, .. } if *id == job))
    );
    assert!(!fake.has("a", "/out/gone.txt.part"));
}

// ---------------------------------------------------------------------------
// Moves, cross-host streaming, enqueue validation, snapshots.
// ---------------------------------------------------------------------------

#[test]
fn a_move_publishes_then_removes_the_source() {
    let fake = Fake::new();
    fake.with_dir("a", "/out")
        .with_file("a", "/in/x.txt", b"moving bytes");
    let mut queue = Queue::with_segment_size(4);
    let job = queue
        .move_to(vec![at("a", "/in/x.txt")], at("a", "/out"))
        .expect("queued");
    assert_eq!(queue.job(job).expect("job").direction(), Direction::Move);
    run_until_idle(&mut queue, &fake);
    assert_eq!(
        fake.file("a", "/out/x.txt").as_deref(),
        Some(&b"moving bytes"[..])
    );
    assert!(!fake.has("a", "/in/x.txt"), "the source was removed");
    assert!(!fake.has("a", "/out/x.txt.part"));
    assert_eq!(queue.job(job).expect("job").state(), State::Done);
}

#[test]
fn a_cross_host_copy_streams_with_one_progress_figure() {
    let fake = Fake::new();
    fake.with_dir("b", "/out")
        .with_file("a", "/in/data.bin", b"0123456789abcdefghij");
    let mut queue = Queue::with_segment_size(4);
    let job = queue
        .copy(vec![at("a", "/in/data.bin")], at("b", "/out"))
        .expect("queued");
    assert_eq!(
        queue.job(job).expect("job").direction(),
        Direction::CrossHost
    );
    let events = run_until_idle(&mut queue, &fake);

    let progress: Vec<u64> = events
        .iter()
        .filter_map(|event| match event {
            Event::Progress { job: id, done, .. } if *id == job => Some(*done),
            _ => None,
        })
        .collect();
    assert_eq!(progress, vec![0, 4, 8, 12, 16, 20]);
    assert_eq!(
        fake.file("b", "/out/data.bin").as_deref(),
        Some(&b"0123456789abcdefghij"[..])
    );
    // The part lived on the destination machine only, and went away.
    assert!(!fake.has("b", "/out/data.bin.part"));
    assert!(!fake.has("a", "/out/data.bin.part"));
}

#[test]
fn enqueue_rejects_an_empty_selection_mixed_machines_and_nameless_paths() {
    let mut queue = Queue::new();
    let out = at("local", "/out");
    assert_eq!(
        queue.copy(Vec::new(), out.clone()),
        Err(EnqueueError::NoSources)
    );
    assert_eq!(
        queue.copy(vec![at("a", "/x"), at("b", "/y")], out.clone()),
        Err(EnqueueError::MixedSources)
    );
    assert!(matches!(
        queue.copy(vec![at("local", "/")], out),
        Err(EnqueueError::NoFileName { .. })
    ));
    assert!(queue.is_empty());
}

#[test]
fn the_snapshot_says_what_a_row_needs() {
    let fake = Fake::new();
    fake.with_dir("a", "/out")
        .with_file("a", "/in/one.txt", b"01234567")
        .with_file("a", "/in/two.txt", b"89abcdef");
    let mut queue = Queue::with_segment_size(2);
    let job = queue
        .copy(vec![at("a", "/in/one.txt")], at("a", "/out"))
        .expect("queued");
    queue.snapshot();
    let rows: Vec<JobView> = queue.snapshot();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, job);
    assert_eq!(rows[0].name, "one.txt");
    assert_eq!(rows[0].direction, Direction::Copy);
    assert_eq!(rows[0].state, State::Queued);
    assert_eq!(rows[0].total, None);
    assert!(rows[0].can_cancel);
    assert!(!rows[0].can_retry);

    run_until_idle(&mut queue, &fake);
    let row = queue.snapshot().remove(0);
    assert_eq!(row.state, State::Done);
    assert_eq!(row.total, Some(8));
    assert_eq!(row.done, 8);
    assert_eq!(row.fraction(), Some(1.0));
    assert!(!row.can_cancel);
    assert!(!row.can_retry, "a done job has nothing to retry");

    assert!(queue.dismiss(job));
    assert!(queue.snapshot().is_empty());
}

#[test]
fn a_multi_source_job_finishes_every_file_and_counts_them_together() {
    let fake = Fake::new();
    fake.with_dir("a", "/out")
        .with_file("a", "/in/one.txt", b"0123456789")
        .with_file("a", "/in/two.txt", b"abcdefghij");
    let mut queue = Queue::with_segment_size(4);
    let job = queue
        .copy(
            vec![at("a", "/in/one.txt"), at("a", "/in/two.txt")],
            at("a", "/out"),
        )
        .expect("queued");
    let events = run_until_idle(&mut queue, &fake);
    assert_eq!(
        fake.file("a", "/out/one.txt").as_deref(),
        Some(&b"0123456789"[..])
    );
    assert_eq!(
        fake.file("a", "/out/two.txt").as_deref(),
        Some(&b"abcdefghij"[..])
    );
    let live = queue.job(job).expect("job");
    assert_eq!(live.total(), Some(20));
    assert_eq!(live.done(), 20);
    let finished: Vec<u64> = events
        .iter()
        .filter_map(|event| match event {
            Event::Finished { job: id, bytes } if *id == job => Some(*bytes),
            _ => None,
        })
        .collect();
    assert_eq!(finished, vec![20]);
    // The prepare pass stat'ed both files before the first byte moved.
    let progress: Vec<u64> = events
        .iter()
        .filter_map(|event| match event {
            Event::Progress { job: id, done, .. } if *id == job => Some(*done),
            _ => None,
        })
        .collect();
    assert_eq!(
        progress.first(),
        Some(&0),
        "the total is known before bytes move"
    );
    assert_eq!(progress.last(), Some(&20));
}
