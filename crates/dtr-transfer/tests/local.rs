//! The local copy against a real filesystem: the `.part` really is a file, the
//! publish really is a rename, and a cancel really leaves nothing behind.
//!
//! These tests use a temporary directory per test, so they exercise the same
//! `LocalTransport` a local copy uses in the app. The scheduling tests in
//! `queue.rs` use an in-memory transport instead, because "no two operations on
//! one machine overlap" is a fact about the scheduler, not about the disk.

use std::fs;
use std::path::{Path, PathBuf};

use dtr_transfer::{Answer, Event, LocalTransport, Queue, State};
use dtr_vfs::Location;
use tempfile::TempDir;

fn destination_dir(name: &Path) -> Location {
    Location::local(name)
}

fn step(queue: &mut Queue, transport: &mut LocalTransport) -> Vec<Event> {
    pollster::block_on(queue.step(transport))
}

fn run_until_idle(queue: &mut Queue, transport: &mut LocalTransport) -> Vec<Event> {
    let mut events = Vec::new();
    let mut guard = 0;
    while queue.may_progress() {
        events.extend(step(queue, transport));
        guard += 1;
        assert!(guard < 100_000, "the queue did not settle");
    }
    events
}

/// A workspace with a source directory and a destination directory.
fn workspace() -> (TempDir, PathBuf, PathBuf) {
    let root = TempDir::new().expect("a temporary directory");
    let source = root.path().join("in");
    let destination = root.path().join("out");
    fs::create_dir_all(&source).expect("source directory");
    fs::create_dir_all(&destination).expect("destination directory");
    (root, source, destination)
}

fn progress_of(events: &[Event], job: dtr_transfer::JobId) -> Vec<u64> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Progress { job: id, done, .. } if *id == job => Some(*done),
            _ => None,
        })
        .collect()
}

fn failure_of(events: &[Event], job: dtr_transfer::JobId) -> Option<String> {
    events.iter().find_map(|event| match event {
        Event::Failed { job: id, error } if *id == job => Some(error.clone()),
        _ => None,
    })
}

#[test]
fn a_local_copy_writes_the_bytes_and_leaves_no_part() {
    let (_root, source, destination) = workspace();
    let file = source.join("hello.txt");
    fs::write(&file, b"hello, local world").expect("source file");
    let mut transport = LocalTransport::new();
    let mut queue = Queue::with_segment_size(4);
    let job = queue
        .copy(vec![Location::local(&file)], destination_dir(&destination))
        .expect("queued");
    let events = run_until_idle(&mut queue, &mut transport);

    assert_eq!(
        fs::read(destination.join("hello.txt")).expect("copied"),
        b"hello, local world"
    );
    assert!(!destination.join("hello.txt.part").exists());
    assert_eq!(queue.job(job).expect("job").state(), State::Done);
    assert_eq!(progress_of(&events, job).last().copied(), Some(18));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Finished { job: id, bytes: 18 } if *id == job))
    );
}

#[test]
fn a_local_copy_of_an_empty_file_works() {
    let (_root, source, destination) = workspace();
    let file = source.join("empty.txt");
    fs::write(&file, b"").expect("source file");
    let mut transport = LocalTransport::new();
    let mut queue = Queue::with_segment_size(4);
    let job = queue
        .copy(vec![Location::local(&file)], destination_dir(&destination))
        .expect("queued");
    run_until_idle(&mut queue, &mut transport);

    let copied = destination.join("empty.txt");
    assert!(copied.exists(), "the empty file is still a file");
    assert_eq!(fs::read(&copied).expect("copied").len(), 0);
    assert!(!destination.join("empty.txt.part").exists());
    let job = queue.job(job).expect("job");
    assert_eq!(job.total(), Some(0));
    assert_eq!(job.done(), 0);
    assert_eq!(job.state(), State::Done);
}

#[test]
fn a_local_copy_moves_one_segment_at_a_time() {
    let (_root, source, destination) = workspace();
    let file = source.join("data.bin");
    fs::write(&file, [7u8; 10]).expect("source file");
    let mut transport = LocalTransport::new();
    let mut queue = Queue::with_segment_size(4);
    let job = queue
        .copy(vec![Location::local(&file)], destination_dir(&destination))
        .expect("queued");
    let events = run_until_idle(&mut queue, &mut transport);

    let progress = progress_of(&events, job);
    assert_eq!(progress, vec![0, 4, 8, 10]);
    // Each step moved at most one segment: the buffer is the bound.
    let mut previous = 0;
    for done in &progress {
        assert!(done - previous <= 4, "a segment moved more than 4 bytes");
        previous = *done;
    }
    assert_eq!(
        fs::read(destination.join("data.bin")).expect("copied"),
        [7u8; 10]
    );
}

#[test]
fn cancelling_a_local_copy_removes_the_part() {
    let (_root, source, destination) = workspace();
    let file = source.join("big.bin");
    fs::write(&file, [3u8; 100]).expect("source file");
    let mut transport = LocalTransport::new();
    let mut queue = Queue::with_segment_size(4);
    let job = queue
        .copy(vec![Location::local(&file)], destination_dir(&destination))
        .expect("queued");

    // Drive until a non-empty part exists, so the cancel has something to
    // remove rather than racing the open.
    let mut guard = 0;
    while fs::metadata(destination.join("big.bin.part"))
        .map(|metadata| metadata.len() == 0)
        .unwrap_or(true)
    {
        step(&mut queue, &mut transport);
        guard += 1;
        assert!(guard < 1_000, "the copy never wrote a segment");
    }
    assert!(!destination.join("big.bin").exists());

    queue.cancel(job).expect("cancel accepted");
    let events = run_until_idle(&mut queue, &mut transport);

    assert!(
        events.iter().any(
            |event| matches!(event, Event::Cancelled { job: id, warning: None } if *id == job)
        )
    );
    assert!(
        !destination.join("big.bin").exists(),
        "a cancelled copy must not publish"
    );
    assert!(
        !destination.join("big.bin.part").exists(),
        "a cancelled copy must remove its part"
    );
    assert_eq!(queue.job(job).expect("job").state(), State::Cancelled);

    // And a retry copies it properly.
    queue.retry(job).expect("retry accepted");
    run_until_idle(&mut queue, &mut transport);
    assert_eq!(
        fs::read(destination.join("big.bin")).expect("copied"),
        [3u8; 100]
    );
    assert!(!destination.join("big.bin.part").exists());
    assert_eq!(queue.job(job).expect("job").state(), State::Done);
}

#[test]
fn a_move_copies_then_removes_the_source() {
    let (_root, source, destination) = workspace();
    let file = source.join("move-me.txt");
    fs::write(&file, b"moved bytes").expect("source file");
    let mut transport = LocalTransport::new();
    let mut queue = Queue::with_segment_size(4);
    let job = queue
        .move_to(vec![Location::local(&file)], destination_dir(&destination))
        .expect("queued");
    run_until_idle(&mut queue, &mut transport);

    assert_eq!(
        fs::read(destination.join("move-me.txt")).expect("copied"),
        b"moved bytes"
    );
    assert!(
        !file.exists(),
        "the source was removed after the copy published"
    );
    assert_eq!(queue.job(job).expect("job").state(), State::Done);
}

#[test]
fn replace_publishes_over_the_existing_local_file() {
    let (_root, source, destination) = workspace();
    let file = source.join("x.txt");
    fs::write(&file, b"new bytes").expect("source file");
    fs::write(destination.join("x.txt"), b"old bytes").expect("existing destination");
    let mut transport = LocalTransport::new();
    let mut queue = Queue::with_segment_size(4);
    let job = queue
        .copy(vec![Location::local(&file)], destination_dir(&destination))
        .expect("queued");

    let events = run_until_idle(&mut queue, &mut transport);
    let conflict = events
        .iter()
        .find_map(|event| match event {
            Event::NeedsAnswer(conflict) if conflict.job == job => Some(conflict.clone()),
            _ => None,
        })
        .expect("the collision was asked about");
    assert_eq!(conflict.destination.path, destination.join("x.txt"));
    queue.answer(job, Answer::Replace).expect("answered");
    run_until_idle(&mut queue, &mut transport);

    assert_eq!(
        fs::read(destination.join("x.txt")).expect("replaced"),
        b"new bytes"
    );
    assert!(!destination.join("x.txt.part").exists());
}

#[test]
fn keep_both_picks_the_next_local_name() {
    let (_root, source, destination) = workspace();
    let file = source.join("x.txt");
    fs::write(&file, b"new bytes").expect("source file");
    fs::write(destination.join("x.txt"), b"old bytes").expect("existing destination");
    fs::write(destination.join("x (2).txt"), b"older bytes").expect("second destination");
    let mut transport = LocalTransport::new();
    let mut queue = Queue::with_segment_size(4);
    let job = queue
        .copy(vec![Location::local(&file)], destination_dir(&destination))
        .expect("queued");
    run_until_idle(&mut queue, &mut transport);
    queue.answer(job, Answer::KeepBoth).expect("answered");
    run_until_idle(&mut queue, &mut transport);

    assert_eq!(
        fs::read(destination.join("x.txt")).expect("kept"),
        b"old bytes"
    );
    assert_eq!(
        fs::read(destination.join("x (2).txt")).expect("kept"),
        b"older bytes"
    );
    assert_eq!(
        fs::read(destination.join("x (3).txt")).expect("new name"),
        b"new bytes"
    );
    assert!(!destination.join("x (3).txt.part").exists());
}

#[test]
fn skip_leaves_the_local_destination_untouched() {
    let (_root, source, destination) = workspace();
    let file = source.join("x.txt");
    fs::write(&file, b"new bytes").expect("source file");
    fs::write(destination.join("x.txt"), b"old bytes").expect("existing destination");
    let mut transport = LocalTransport::new();
    let mut queue = Queue::with_segment_size(4);
    let job = queue
        .copy(vec![Location::local(&file)], destination_dir(&destination))
        .expect("queued");
    run_until_idle(&mut queue, &mut transport);
    queue.answer(job, Answer::Skip).expect("answered");
    let events = run_until_idle(&mut queue, &mut transport);

    assert_eq!(
        fs::read(destination.join("x.txt")).expect("kept"),
        b"old bytes"
    );
    assert!(!destination.join("x.txt.part").exists());
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Skipped { job: id, .. } if *id == job))
    );
}

#[test]
fn a_local_directory_source_is_refused() {
    let (_root, source, destination) = workspace();
    let directory = source.join("sub");
    fs::create_dir(&directory).expect("source directory");
    let mut transport = LocalTransport::new();
    let mut queue = Queue::with_segment_size(4);
    let job = queue
        .copy(
            vec![Location::local(&directory)],
            destination_dir(&destination),
        )
        .expect("queued");
    let events = run_until_idle(&mut queue, &mut transport);

    let error = failure_of(&events, job).expect("failed");
    assert!(error.contains("directory"), "{error}");
    assert!(error.contains("recursive"), "{error}");
    assert!(!destination.join("sub").exists());
    assert!(!destination.join("sub.part").exists());
}

#[test]
fn a_missing_local_source_fails_before_a_part_is_created() {
    let (_root, source, destination) = workspace();
    let mut transport = LocalTransport::new();
    let mut queue = Queue::with_segment_size(4);
    let job = queue
        .copy(
            vec![Location::local(source.join("gone.txt"))],
            destination_dir(&destination),
        )
        .expect("queued");
    let events = run_until_idle(&mut queue, &mut transport);

    let error = failure_of(&events, job).expect("failed");
    assert!(error.contains("does not exist"), "{error}");
    assert!(!destination.join("gone.txt.part").exists());
}

#[test]
fn a_file_that_appears_after_the_answer_is_not_clobbered() {
    let (_root, source, destination) = workspace();
    let file = source.join("x.txt");
    fs::write(&file, b"new bytes").expect("source file");
    let mut transport = LocalTransport::new();
    let mut queue = Queue::with_segment_size(4);
    let job = queue
        .copy(vec![Location::local(&file)], destination_dir(&destination))
        .expect("queued");

    // The conflict pass saw no destination. Race one in while the transfer is
    // running: the publish must fail rather than replace what the user did not
    // agree to replace.
    let mut guard = 0;
    while !destination.join("x.txt.part").exists() {
        step(&mut queue, &mut transport);
        guard += 1;
        assert!(guard < 1_000, "the transfer never opened its part");
    }
    fs::write(destination.join("x.txt"), b"raced").expect("racing file");
    let events = run_until_idle(&mut queue, &mut transport);

    let error = failure_of(&events, job).expect("the publish must fail");
    assert!(error.contains("appeared"), "{error}");
    assert_eq!(
        fs::read(destination.join("x.txt")).expect("raced file kept"),
        b"raced"
    );
    assert!(
        destination.join("x.txt.part").exists(),
        "the complete part is kept for the retry"
    );
}

#[test]
fn a_queued_local_job_can_be_dismissed_after_it_finishes() {
    let (_root, source, destination) = workspace();
    let file = source.join("x.txt");
    fs::write(&file, b"bytes").expect("source file");
    let mut transport = LocalTransport::new();
    let mut queue = Queue::with_segment_size(4);
    let job = queue
        .copy(vec![Location::local(&file)], destination_dir(&destination))
        .expect("queued");
    run_until_idle(&mut queue, &mut transport);
    assert!(queue.dismiss(job));
    assert!(queue.snapshot().is_empty());
    let _ = Path::new("/");
}
