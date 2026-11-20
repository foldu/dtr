# Transfers: the queue core

`crates/dtr-transfer` is the F5 core: the job model, the scheduler, the segment
seam, `.part`-then-publish, conflicts, cancel and retry, written with no window
in it. `docs/manager.md` §8 is the decision it implements; `docs/sftp.md` §7 is
the write codec the SFTP adapter will drive it with. This memo is the account:
what the API is, why the scheduler is shaped this way, what the F5 wiring calls,
and what an ssh adapter still has to supply.

The crate has exactly two dependencies. `dtr-vfs`, for `Location`/`BackendId`/
`Meta` — a transfer's endpoints are the file manager's answer to "which machine,
which path". And `anyhow`, as the rest of the tree uses. **No `compio`, no
`tokio`, no `egui`, no thread**: a queue is a state machine the driver polls.
There is one `dev-dependency` pair for tests: `pollster` to poll the futures, and
`tempfile` for the local-filesystem tests.

```
nix develop .#default --command bash -c \
  'cargo fmt --all && cargo test -p dtr-transfer && cargo build -p dtr-transfer'
```

All 50 tests pass: 13 unit tests (naming, the stream op, the job model), 12 local
filesystem tests, 24 scheduler tests, 1 doctest.

## 1. Why the seam is async, and why there is still no runtime

The real transports are `compio`: `dtr-vfs`'s backends and `dtr-sftp`'s `Session`
return futures that are `!Send`, because compio handles wrap thread-local `Rc`s
(`docs/compio.md` §10.3, trap 4). A synchronous core would therefore have to call
`block_on` per segment from a driver that is *already* on the runtime thread, and
compio's runtime is thread-bound (§10.3, trap 2). So `Op` and `Transport` return
boxed futures and the driver's runtime polls them on the driver's thread — the
same spelling `dtr-vfs` uses for `BoxFuture`, and runtime-agnostic in the only
sense that matters: nothing here starts an executor.

That makes `Queue` `!Send` (it holds `Box<dyn Op>`), so it lives on the worker
thread with its transport. The UI never touches it: the queue's public state for
the card is `Queue::snapshot() -> Vec<JobView>` and the `Event`s a step returns.
§8 says what the wiring does with those.

## 2. The model

A `Job` is one user action over a list of sources and one destination directory:

| field | meaning |
| --- | --- |
| `id` | opaque, monotonic, never reused; the card keys rows by it |
| `sources: Vec<Location>` | all on **one** machine; one job is one stream |
| `destination: Location` | a directory, on any machine |
| `direction` | `Copy`, `Move`, `Upload`, `Download`, `CrossHost`, inferred, not asked |
| `state` | `Queued`, `Running`, `Done`, `Failed`, `Cancelled` |
| `total: Option<u64>` | `None` until the prepare pass has stat'ed every source |
| `done: u64` | bytes moved, monotonic **within one attempt** |
| `attempt` | 1, then 2, 3… on retry |
| `error` | why it failed |
| `question` | the open `Conflict`, if any |

The write half of the job — the per-source plan, the phase, the op in flight —
is crate-private. An adapter reads a job; only the scheduler moves it.

`Direction::infer(source_backend, destination_backend, moving)` is the rule the
driver would otherwise duplicate: local→local and a host onto itself are `Copy`,
local→ssh is `Upload`, ssh→local is `Download`, ssh(a)→ssh(b) is `CrossHost`,
and any pair with `Move` set is `Move` (the word the card shows; the destructive
half is the point).

`JobView` is the cloneable row: `id`, `name` (the file for one source, `n items`
for many), `direction`, `state`, `done`, `total`, `error`, `waiting`, `attempt`,
`can_cancel`, `can_retry`, and `fraction()` for the stripe. `can_retry` is true
only for `Failed`/`Cancelled`; a `Done` job has nothing to retry. `waiting` is
the conflict question, which is *not* a sixth state: the job is still the running
one.

`Conflict` is the question: the source, the destination file that already
exists, and the `Meta` of what is there. `Answer` is §7's three buttons:
`Replace`, `KeepBoth`, `Skip`.

## 3. The scheduler: one machine, one stream

A job holds **every machine it touches** — the sources' backend and the
destination's — from `Started` until `Done`/`Failed`/`Cancelled`. `BackendId` is
the machine key, so `Local` is one machine too: local copies are serialised with
each other, deliberately, because "within one machine it is serial" is the rule
and giving the local disk one stream is the same policy.

Two consequences fall out:

- **Across machines it is parallel.** Different `BackendId`s, two jobs both run;
  their operations are open at the same time and each `step` gives both a turn.
- **Destination-name collisions cannot happen.** The destination machine is one
  of the held machines, so two jobs that could write the same name are already
  serialised. There is no second reservation scheme for destination names, and
  the `.part` path cannot be raced by another job in the same queue either.

`Queue::step(transport).await` does bounded work: for each running job whose
machines it still holds and that is not parked on a question, exactly **one**
phase action; then it may start **one** queued job. One start per step means two
machines come up on two consecutive steps and then transfer together; a blocked
job does not stop a later job whose machines are free from starting (the scan
continues), and the oldest runnable job wins.

`may_progress()` is what the driver loops on: a pending cancellation, a running
job not parked on a question, or a queued job whose machines are free. False
means "wait for a command or an answer" — the driver should not spin.

### 3.1 The phase machine

Each source is prepared before any byte moves (that is how `total` becomes known
and how conflict answers arrive before a part exists), then run:

| phase | bounded action |
| --- | --- |
| `Destination` | once: `stat` the destination — it must exist and be a directory |
| `Stat(i)` | `stat` source `i`; a directory is an error (recursive copy is F6+); the last one fixes `total` |
| `Conflict(i)` | probe the destination name; no answer needed, or consume the answer, or raise `NeedsAnswer` |
| `Preflight` | once per attempt: the adapter's `statvfs` hook |
| `Open(i)` | `Transport::open` — create the part, hold the reader/writer |
| `Transfer(i)` | one segment: `Op::step` |
| `Finish(i)` | `Op::finish` — flush and close the part |
| `Publish(i)` | rename the part to the real name |
| `Remove(i)` | a move only: remove the source after the copy is published |
| `Next(i)` | advance to the next source, or finish the job |

One action per phase per step is the cancellation granularity and the
"listing answers between segments" granularity: the SFTP adapter's methods are
one packet each, so the alias lock is released between them.

### 3.2 The scheduling proof

`tests/queue.rs` instruments the fake `Transport`: every `open` records a span
`(job, machines, start, close)`, increments a per-machine open count and a global
one, and **pushes an overlap violation if a machine is already open**. The
assertions are therefore at the place where the bug would be, not inferred from
totals:

- `two_jobs_on_one_machine_never_overlap`: two jobs copy a file each, source and
  destination on `a`. Assertions: `overlaps()` is empty; `max_open("a") == 1`;
  the operation windows are disjoint (`first.close <= second.start`); and the
  second job's first segment comes after the first job's last.
- `two_machines_run_at_the_same_time`: jobs on `a` and `b`, four segments each.
  Assertions: both per-machine maxima are 1, `max_global_open() == 2`, the two
  spans overlap, and the segment order interleaves (the second job's first
  segment is before the first job's last, and vice versa). The first segment is
  alone because a job starts one step before the next can.
- `a_cross_host_job_holds_both_machines`: job 1 is `a -> b`, job 2 is `b -> c`.
  They share `b`, so job 2 starts only after job 1 closed: `first.close <
  second.start`, with no overlap on `b`.
- `the_scheduler_does_not_let_a_blocked_job_starve_a_free_machine`: two jobs
  queue for `a`, a third for `b`; the third runs while they are blocked.

The fake never sleeps and the queue is stepped by hand, so "after three
segments" is a fact about the test, not a race with a thread. All 24 scheduler
tests use it.

## 4. The operation seam

```rust
pub trait Op {
    fn step<'a>(&'a mut self, buf: &'a mut [u8]) -> BoxFuture<'a, Result<Segment>>;
    fn finish<'a>(&'a mut self) -> BoxFuture<'a, Result<()>>;
    fn abort(&mut self);
}

pub trait Transport {
    fn stat(&mut self, at: &Location) -> BoxFuture<Result<Option<Meta>>>;
    fn open(&mut self, request: OpenRequest) -> BoxFuture<Result<Box<dyn Op>>>;
    fn publish(&mut self, part: &Location, destination: &Location, overwrite: bool)
        -> BoxFuture<Result<()>>;
    fn discard(&mut self, part: &Location) -> BoxFuture<Result<()>>;
    fn remove(&mut self, at: &Location) -> BoxFuture<Result<()>>;
    fn preflight(&mut self, destination: &Location, bytes: u64) -> BoxFuture<Result<()>>;
}
```

`Segment` is `Wrote(u64)` or `Done`. The queue owns **one** segment buffer (64
KiB by default, `SEGMENT`, the SFTP codec's packet bound) and lends it to each
step, so memory is bounded by the queue, not by file size; a file is never read
to find out how big it is (the prepare pass stats it). An op that claims to write
more than the buffer, or `Wrote(0)`, is a failed job rather than an infinite loop.

`abort` is deliberately synchronous and must not delete the part: it releases the
op's handles, and the **queue** owns the part's life (cancel discards, failure
keeps). That split keeps the policy in one place.

`StreamOp` is the generic operation: a `Reader` and a `Writer`, one segment at a
time. It is the cross-host copy §8 asks for (SFTP v3 has no server-to-server copy
and `copy-data` stays inside one server, so A→B streams through dtr), and an
upload or download can be built from it too. The tests use it for every fake
transfer, so the seam is exercised, not just compiled.

`LocalCopyOp` is the first concrete op: `std::fs` reads into the shared buffer and
writes to the part; `finish` flushes and `sync_all`s; `abort` drops the handles.
`LocalTransport` is the first concrete transport: `stat` maps `NotFound` to
`None`, `open` only accepts local-to-local requests, `publish` renames, `discard`
unlinks, `remove` unlinks a moved source. An adapter composes it by routing on
`Location::backend`.

## 5. `.part`, publish, and the real name

**The only path an op writes is `<name>.part` in the destination directory** —
the real name with `.part` appended to the file name, so the publish rename never
crosses a filesystem. A leftover part from a failed attempt is truncated by the
next open: parts are not user files.

- **Publish** is one `Transport::publish` after `Op::finish`: rename the part to
  the real name. `overwrite` is `true` only after the user answered "replace";
  with `false` the adapter must fail rather than clobber, because a "keep both"
  that lost a race must not overwrite what it kept.
- **Cancel** aborts the op, then the queue awaits `discard(part)`, marks the job
  `Cancelled`, and drops `done` back to what was published. A discard that fails
  is a `warning` on the `Cancelled` event, not a second failure: the real name is
  still absent.
- **Failure** aborts the op and keeps the part; `done` drops back to what was
  published; the real name was never touched. `docs/manager.md` §8 explicitly
  allows leaving the part for retry, and the part is visible evidence of what
  happened.
- **Retry restarts, it does not resume.** A retried source re-opens the part with
  truncation and starts at byte zero; `attempt` increments and progress drops to
  `committed`. That is the honest number after partial work is thrown away. The
  phases make a retry as cheap as correctness allows: a failed *publish* retries
  the rename only (the part is complete), and a failed *move cleanup* retries the
  source removal only (the copy is already published). Resuming from an offset is
  a future change — the codec has `FXF_APPEND` in mind (`docs/manager.md` §8) and
  it would be an `OpenRequest` offset, not a different queue.

**A move is copy → publish → remove**, even on one machine, not a `rename`. A
portable no-clobber rename does not exist in `std`: Unix's `rename` replaces
unconditionally, so a `Move` through the queue must obey the same publish rule as
a copy (never clobber a name the user did not agree to replace), and the
cross-device case would need the copy anyway. The source is removed only after
its copy is published, so an interrupted move leaves two files, never zero. If
the removal itself fails, the job is `Failed` with "published the copy, but could
not remove the source" and a retry retries the removal only.

Local's no-replace publish is `hard_link(part, real)` then remove part — the one
no-clobber publish `std` offers on one filesystem — with a check-then-rename
fallback for filesystems without hard links (a race that fallback admits). The
SFTP adapter has the better answer: `SSH_FXP_RENAME` fails if the name appeared,
and `posix-rename@openssh.com` is the atomic replace (`docs/sftp.md` §7).

## 6. Conflicts: asked, never guessed

When the destination name exists the job does not proceed: it raises
`Event::NeedsAnswer(Conflict)` and parks. It keeps its machine (the alternative —
releasing the machine while a person thinks — would let another job plan the same
destination name and forces a second reservation scheme; the machine is idle
anyway, so a listing still answers) and does no I/O.

`Queue::answer(job, answer)` stores the answer against the **one source** it was
asked about and clears the question; the next step consumes it. A second answer
for the same question is `AnswerError::NoQuestion`. A "replace" is not remembered
for the next file: each collision is its own question. That is the difference
between a conflict card and a "don't ask again" checkbox, and it is tested:
`an_answer_is_consumed_once_and_never_leaks_to_the_next_file` has two colliding
sources, answers the first `Replace`, sees the second ask again, answers `Skip`,
and checks both destinations.

- `Replace` → publish with `overwrite = true`.
- `Skip` → no open, no part, no publish; the source's bytes come out of `total`
  and a `Progress` event says so.
- `KeepBoth` → a new name in the destination directory: stem + ` (n)` + the last
  extension, `n` from 2 upward, first free (probed through the transport): `x.txt`
  → `x (2).txt`; `a.tar.gz` → `a.tar (2).gz`; `.env` → `.env (2)` (a dotfile has
  no extension). The name is a `Path`, so a non-UTF-8 Unix name keeps its bytes.
  After 2000 taken numbers the job fails rather than probing forever. Publishing
  stays no-clobber, so a file that appears during the question cannot be lost.

A destination that exists and is not a regular file fails the job before the
question ("… exists and is not a regular file"): replacing a directory with a
file is not a choice the card should offer.

## 7. Progress

- `total` is `None` until the prepare pass has stat'ed every source; the step
  that learns it emits one `Progress` so the stripe stops being indeterminate
  before the first byte moves.
- `done` grows by each `Segment::Wrote`; each step emits `Progress` (the card
  coalesces per frame). Within one attempt it never decreases.
- On a skip, `total` shrinks by that file; on a failure or cancel, `done` drops to
  the published bytes; on a retry, `done` is reset to the same figure. Those are
  the only decreases, and they are reported by `Failed`/`Cancelled`/`Retrying`.
- An empty file works: `total = Some(0)`, `done = 0`, one `Done` segment, and
  `fraction()` is 1.0 once the job is done rather than a division by zero.
- The move's source removal adds no bytes.

## 8. What the F5 UI integration calls

**Where it lives.** One `Queue` per window, owned by the app, but *driven on the
worker thread that owns the transport* (`dtr-bg`'s compio thread or a dedicated
transfer worker): `Queue` is `!Send` because its ops are. The UI holds
`Vec<JobView>` and the events, not the queue.

**Commands (UI → worker mailbox).** Each is a cheap, synchronous queue method:

| UI action | call |
| --- | --- |
| paste / drag-drop copy | `queue.copy(sources, destination_dir)` → `Result<JobId, EnqueueError>` |
| move | `queue.move_to(sources, destination_dir)` |
| conflict card's three answers | `queue.answer(job, Answer::Replace|KeepBoth|Skip)` |
| a row's ✕ | `queue.cancel(job)` |
| the card's "cancel all" | `queue.cancel_all()` |
| a failed row's retry | `queue.retry(job)` |
| closing a finished row | `queue.dismiss(job)` / `clear_finished()` |

`EnqueueError` (`NoSources`, `MixedSources`, `NoFileName`) is the driver's own
validation: show a toast, queue nothing. `AnswerError::NoQuestion` and
`ControlError::{Finished, StillRunning}` are races with a card the user just
acted on — log and ignore.

**The worker loop.** After every command and after events arrive:

```rust
while queue.may_progress() {
    let events = queue.step(&mut transport).await;
    send_to_ui(events, queue.snapshot()); // coalesce to a frame
}
```

`may_progress()` false means every running job is parked on a question and every
queued job is behind a held machine: block on the command channel, do not spin.
`snapshot()` is cheap enough to send after each loop; if the wiring prefers
events only, `Event` carries `Started { view }`, `Progress`, `NeedsAnswer`,
`Published`, `Skipped`, `Finished`, `Failed`, `Cancelled`, `Retrying`, and the
id of every job it concerns.

**What the card draws.** `dtr-widgets`' queue card (the parallel stream) takes
rows; map `JobView` → its row type: `name`, `direction.word()`, `state.word()`,
`done`/`total` (or `fraction()` for the stripe), `waiting`, `can_cancel`,
`can_retry`, `attempt`. A `NeedsAnswer` event opens the conflict card with
`conflict.source.describe()`, `conflict.destination.describe()` and the existing
size; the card's three answers map 1:1 onto `Answer`. The tab's own stripe reads
`JobView::fraction()` of the focused pane's job. `Published` is useful for a log
line; `Finished`/`Failed`/`Cancelled` are the toasts. Nothing in the UI computes
progress, names, or conflict state.

**Do not** call `Queue` from the UI thread, keep a second copy of the job state in
the app, or answer a question with a remembered answer to avoid a second card: the
queue's whole contract is that the card decides each collision.

## 9. What an SFTP/ssh adapter must implement

The adapter is the app's, not this crate's: it owns `dtr-vfs`'s per-alias
sessions (or the `dtr-sftp` codec directly) and routes by `Location::backend`,
delegating local pairs to `LocalTransport`. It implements `Transport` and builds
`Op`s out of the codec's segmented methods (`docs/sftp.md` §7). Concretely:

| method | adapter |
| --- | --- |
| `stat(at)` | the alias's `STAT`; map "no such file" to `Ok(None)`, other errors to `Err`. Local: `LocalTransport`. |
| `open(Upload)` | open the local source (`compio::fs::File`) and `begin_upload(<name>.part)` on the destination alias; the op's writer sends one `write_upload_chunk(upload, offset, &buf)` per segment, `finish_upload` in `Op::finish`, `abort_upload` (or a dropped handle) in `Op::abort`. One packet per segment is what lets the alias's connection go back to browsing between steps. |
| `open(Download)` | a segmented **read**: the codec currently exposes a whole-file `download` (OPEN/READ/…/CLOSE in a loop) but not one chunk at a time, so the adapter needs the read side of the segmented API that `begin_upload` has on the write side (`begin_read`/`read_chunk`/`finish_read`) or the local part written with an append/`write_all_at` per chunk. The remote part is `<name>.part` too, so a cancelled download is a local part removal. |
| `open(CrossHost)` | `StreamOp` over a segmented reader on A and a segmented writer on B: one segment = one read on A and one write on B, one progress figure. Both machines are held by the job, so no other queued job touches either session. |
| `publish(part, real, overwrite)` | the destination alias's `Session::rename(part, real, overwrite)` — `overwrite` is the codec's `posix-rename@openssh.com` vs plain `SSH_FXP_RENAME` choice (`docs/sftp.md` §7), and it is the queue's replace answer, never a guess. Local: `LocalTransport::publish`. |
| `discard(part)` | `Session::remove(part)`; a missing part is `Ok`. Local: unlink. |
| `remove(source)` | `Session::remove(source)` after a move's publish. Local: unlink. |
| `preflight(dest_dir, bytes)` | `Session::statvfs(dest_dir)`; a server without the extension is `Err` from the codec and the hook should answer `Ok(())` — a transfer that cannot ask proceeds and lets `WRITE` fail (`docs/sftp.md` §7), never a made-up number. With a number, `available_bytes < bytes` is an `Err` and the job fails in one round trip. Local can leave the default. |

Two things the adapter owns because they are session-shaped:

- **Handles, not sessions, inside an `Op`.** compio's `Session` is `!Send` and
  lives on the alias thread, so an op cannot own it. Follow `crates/dtr-vfs/src/ssh.rs`'s
  per-alias mailbox: `open` sends a command to the alias thread and gets back a
  handle (an upload id, a read id); each segment sends one command and awaits one
  answer. That is also exactly the granularity measurement 5 wants: a listing on
  the same alias is another command to the same thread, and it gets a turn between
  segments because each segment is one round trip and the queue does not hold the
  session.
- **`Op::abort` is synchronous.** It must not block and must not publish: queue a
  close/abort to the alias thread (fire-and-forget is fine) and release the
  handle; the queue then awaits `discard(part)`, which removes the file. If the
  server keeps an open handle, the `REMOVE` still works on Unix; the queued abort
  close keeps it tidy and is what the codec's `begin_upload` error path does.

Everything else — the `.part` name, the publish ordering, the conflict questions,
progress, cancel/retry, per-machine scheduling, the bounded segment buffer — is
the queue's, and an adapter must not duplicate it. `dtr-sftp` is complete for the
write side (upload primitives, `rename` with extension selection, `statvfs`,
`remove`); the segmented **read** API is the one gap this integration exposes.

## 10. What is deliberately not here

- **Recursive directory copies** (F6+): a directory source is a `Failed` job with
  a message saying so, never a silently skipped file. The destination directory
  must already exist; the queue does not `mkdir`.
- **Resume** (append from a `.part` offset), pause, bandwidth limits, ETA/rate,
  multiple destinations, per-file conflict defaults, trash/undo, permissions or
  timestamps (a part is created with the default mode; an adapter that wants to
  preserve mode calls `chmod` after publish), and the UI itself.
- **A `statvfs` implementation**: the hook exists (`Transport::preflight`), the
  policy is the adapter's.
- **A second mechanism for destination names**, for the reason in §3.

## 11. Tests

`tests/queue.rs` (24) is the scheduler, with an in-memory `Transport` that records
every operation's open/close per machine, refuses to open a machine that is
already open (the serialisation assertion), and can fail one op at a given
segment. `tests/local.rs` (12) runs `LocalTransport` against real temporary
directories: copy, empty file, segment-bounded progress, cancel-removes-part,
move, replace/keep-both/skip, directory source, missing source, a file that
appears mid-transfer (the publish must fail, not clobber), dismissal. The unit
tests cover the keep-both naming rule (`x.txt` → `x (2).txt`, `a.tar.gz` →
`a.tar (2).gz`, `.env` → `.env (2)`, non-UTF-8 bytes), the `StreamOp` segment
loop and its idempotent finish, and the phase/redo rules.

The names worth quoting for the gate:

```
two_jobs_on_one_machine_never_overlap ... ok
two_machines_run_at_the_same_time ... ok
a_cross_host_job_holds_both_machines ... ok
the_scheduler_does_not_let_a_blocked_job_starve_a_free_machine ... ok
cancel_between_segments_removes_the_part_and_reports_cancelled ... ok
a_failure_leaves_no_real_name_and_a_retry_restarts_the_file ... ok
an_answer_is_consumed_once_and_never_leaks_to_the_next_file ... ok
replace_publishes_over_the_existing_file ... ok
keep_both_picks_the_next_free_name ... ok
skip_leaves_the_destination_untouched ... ok
a_cross_host_copy_streams_with_one_progress_figure ... ok
an_empty_file_finishes_with_zero_bytes ... ok
a_multi_chunk_file_reports_monotonic_progress ... ok
a_file_that_appears_after_the_answer_is_not_clobbered ... ok
```

## 12. The ssh adapter, as built

`crates/dtr-transfer-ssh` is the other half of the F5 seam: its `SshTransport`
implements `Transport` for **every** job shape, so the wiring holds one transport
beside one queue. It depends on `dtr-transfer` (the contract), `dtr-sftp` (the
segmented codec), `dtr-vfs` (`Location`/`BackendId`/`Meta`), `compio` (its
timers — the crate has no runtime of its own), `anyhow`, and `tracing` for the
one reconnect line. Its dev-dependencies are `async-channel` (the fake wire's
two ends) and `tempfile` (the local ends).

```
nix develop .#default --command bash -c \
  'cargo fmt --all && cargo test -p dtr-transfer-ssh && cargo build -p dtr-transfer-ssh'
```

Eighteen integration tests pass (`tests/wire.rs`), driven through a fake SFTP
server on an in-memory wire. There are no unit tests: the unit is the protocol
conversation, and the fake is the smallest thing that has one.

§9's closing sentence — "the segmented **read** API is the one gap this
integration exposes" — is now historical. `begin_read`/`read_chunk`/
`finish_read` landed in `dtr-sftp` (`dtr-vfs`'s remote copy uses them too), and
this adapter's download and cross-host shapes are built on them.

### 12.1 Routing is the endpoints', not the direction's

| source | part (destination) | operation |
| --- | --- | --- |
| local | local | `LocalTransport`'s `LocalCopyOp`, untouched |
| local | an alias | `StreamOp` over a local `File` and the alias's segmented upload (`OPEN`/`WRITE`/`CLOSE`) |
| an alias | local | `StreamOp` over the alias's segmented read (`OPEN`/`READ`/`CLOSE`) and a local `File` |
| one alias | the same alias | `StreamOp` with two handles on that one session |
| one alias | another alias | `StreamOp` over a segmented read and a segmented write: v3 has no server-to-server copy, so the bytes pass through dtr |

`OpenRequest::direction` is the card's word, not a routing hint: `Copy` covers a
same-host remote copy, and `Move` is whatever the endpoints are (`Move` is
copy → publish → remove, so the bytes still go the way the locations say). The
adapter never branches on it. `stat`, `publish`, `discard`, `remove` and
`preflight` route on the one `Location`'s `BackendId` by the same rule, and a
local pair never opens an alias session — one queue of local jobs needs no
network at all.

### 12.2 The session: one per alias, created on the driver's thread

The queue's driver already runs inside a compio runtime on one thread
(`docs/compio.md` §10.3), and compio's handles are thread-local, so the adapter
**starts no thread and owns no mailbox**. It is created on the thread that will
poll its futures, and it keeps a `HashMap<String, Rc<RefCell<Option<Session>>>>`:
one session per alias. An `Op` does not own the session — a `Session` is `!Send`
and the op outlives one `open` — so it holds the alias's slot plus the codec's
handle (`Upload`/`Read`), both plain values.

A step borrows the slot for exactly the packets it needs and gives it back; a
`stat`, a `discard`, or another job on a free machine can use the alias between
segments. That is the interleaving measurement 5 of `docs/manager.md` §8 asks
for, and it is why the queue can keep its per-step cancel granularity. The plain
`RefCell` (rather than the per-alias thread `crates/dtr-vfs/src/ssh.rs` needs) is
sound because the queue holds every machine a job touches for its whole run
(§3): two ops never use one alias at once, so there is never a second borrower
to panic on.

Every request — and the `Session::connect` itself — has a 30 s deadline. The
`ssh` child's args are the codec's (`BatchMode=yes`, `-s <alias> sftp`), and a
child that hangs in connect is treated like a dead one.

Two rules differ:

- **A whole-session verb is retried once on a fresh child** — `stat`, `publish`,
  `discard`, `remove`, `preflight`. A failed session is not a session, and
  leaving a dead child in the slot makes every later call fail the same way,
  which reads as "this host is down" long after it came back (the rule
  `crates/dtr-vfs/src/ssh.rs` states). The first failure logs one
  `sftp failed; reconnecting` line with the alias and the operation.
- **A segment is never retried.** A reconnect loses the `SSH_FXP_OPEN` handle,
  so a failed segment fails the file; the queue's retry reopens the part from
  byte zero and `attempt` increments (§5). Resuming from an offset is not in the
  codec's API yet.

One session can serve more than one handle at a time, which is what a same-host
copy uses: two `OPEN`s on one session, then one `READ` and one `WRITE` per
segment. The requests are serialised by the codec's one-answer-at-a-time rule;
the session is still released between steps.

### 12.3 Publish, discard, remove, preflight

| method | adapter |
| --- | --- |
| `stat(at)` | local: `LocalTransport`. Alias: `Session::stat`, with the server's "no such file" mapped to `Ok(None)` (§12.5) and every other error a real error. |
| `open` | §12.1's four shapes. An upload creates the remote part with `begin_upload` *before* opening the local source, so a missing/unreadable source releases the session and leaves the server's part to the queue's `discard`; a download creates the local part after `begin_read` and does the same in reverse. |
| `publish(part, real, overwrite)` | the codec's `Session::rename(part, real, overwrite)` verbatim. The part and the real name must be on one machine (they always are: the part is in the destination directory). `overwrite = true` — only after the user answered "replace" — uses `posix-rename@openssh.com` when the server advertised it and the plain packet otherwise; `overwrite = false` always uses plain `SSH_FXP_RENAME`, which fails if the name appeared, so a "keep both" that lost a race cannot clobber what it kept (`docs/sftp.md` §7). A no-replace race therefore spends one reconnect and one repeated rename before it fails — the reconnect rule cannot tell a status error from a dead child — and the files are untouched. |
| `discard(part)` | `Session::remove(part)`; a missing part is `Ok(())`, classified inside the attempt so the normal "already gone" answer costs no reconnect. |
| `remove(at)` | local: `LocalTransport`. Alias: `Session::stat` then `REMOVE` or `RMDIR`, because v3's `REMOVE` on a directory is the server's refusal rather than a rule (the same choice `crates/dtr-vfs/src/ssh.rs` makes). The queue only calls this for a move's source, which its prepare pass stat'ed as a file, but the stat keeps the verb honest for anything else. |
| `preflight(dest_dir, bytes)` | local: the local transport's hook (the trait default: it cannot tell). Alias: one `Session::supports(STATVFS)` check — **a server that did not advertise the extension is `Ok(())` with no packet**, because a transfer that cannot ask proceeds and lets `WRITE` fail, never a made-up number. When it is advertised, `available_bytes < bytes` is an `Err` naming the host, path, available and needed bytes, so a 4 GB upload fails in one round trip. |

### 12.4 `Op::abort` is synchronous; burning is the release

`Op::abort` must not block and must not publish, so an op's `abort` takes the
alias's session out of the slot and kills the child (`Session::close`). No
destructor can await a `CLOSE`, and a fire-and-forget close would need a live
runtime on the calling thread and would race the queue's following `discard`.
Killing the child closes every server-side handle with the channel, and the next
verb reconnects: the queue's `discard(part)` removes the half-written part, and
a retry's `open` truncates it. The part itself is never deleted here — the
queue owns the part's lifecycle (§5, §9).

`RemoteRead`/`RemoteWrite` carry the same rule in their `Drop`/`abort`: a reader
that is dropped with its handle still open burns the session, because a server
handle left open would be spent by the next verb. A reader that reached EOF
closes its handle itself, so the normal path leaves the session clean for the
publish or the next job.

### 12.5 `stat`'s "not found" is the one string match

The queue asks "does the destination exist?" with `stat`, and `Ok(None)` is the
answer that decides whether a conflict question is needed. The codec's `stat`
error path decodes `SSH_FX_NO_SUCH_FILE` and then **formats the code into the
message**; the code itself is not reachable from outside the codec, so the
adapter classifies by the server's text (`no such file`, `not found`, or an
empty message's `status 2`). OpenSSH's `status_to_message` says "No such file",
and the fake server says the same thing. This is the adapter's one concession to
that gap: a typed `Session::stat_opt` or an error carrying the status code would
remove the match. It is reported here rather than patched into `dtr-sftp`,
which is not this crate's to change. Every other error is a real error, and a
missing path does not spend a reconnect.

### 12.6 Tests, and the gaps

`tests/wire.rs` drives the adapter through the real codec: a fake SFTP server
speaks v3 over a pair of `async_channel`s (`Session::from_wire`, the same seam
`crates/dtr-vfs/src/ssh.rs`'s tests and `dtr-sftp/tests/mock.rs` use), keeps a
little filesystem so effects are asserted, and can advertise extensions, refuse
a `WRITE`, or drop a connection after the handshake. The names worth quoting for
the gate:

```
an_upload_writes_one_packet_per_segment_and_publishes_with_a_rename ... ok
a_download_reads_a_segment_at_a_time_into_a_local_part ... ok
a_download_respects_a_segment_buffer_smaller_than_the_codec_chunk ... ok
a_cross_host_copy_streams_a_to_b_with_one_byte_count ... ok
a_same_host_remote_copy_holds_two_handles_on_one_session ... ok
a_no_replace_publish_fails_when_the_name_appeared_and_keeps_the_part ... ok
a_replace_publish_uses_posix_rename_when_the_server_advertises_it ... ok
a_discard_removes_the_part_and_a_missing_part_is_not_an_error ... ok
remove_deletes_a_file_with_remove_and_a_directory_with_rmdir ... ok
stat_answers_meta_and_maps_not_found_to_none ... ok
a_dead_session_is_replaced_once_and_the_verb_succeeds ... ok
preflight_refuses_when_statvfs_says_there_is_no_room ... ok
preflight_is_ok_when_the_extension_is_absent ... ok
an_abort_releases_the_alias_and_leaves_the_part_to_the_queue ... ok
a_failed_segment_fails_the_job_and_a_retry_publishes_the_file ... ok
the_queue_downloads_from_a_host_and_publishes_the_local_name ... ok
a_local_to_local_job_stays_on_the_local_transport ... ok
a_remote_request_never_touches_the_local_transport ... ok
```

The last two are the routing rule asserted rather than inferred: the local half
is a recording in-memory `Transport` (no real files), and a dialer that fails if
it is ever asked proves a local↔local job never opens a session. The two
queue-driven tests use the real `Queue` from `dtr-transfer`, so the sequence
stat → conflict → preflight → open → segment → finish → publish is exercised end
to end, including a mid-transfer failure, the part the queue keeps, and the
retry's restart from byte zero.

The gaps:

- **No real OpenSSH server.** The fake imitates the status texts, plain
  `RENAME`'s refusal of an existing destination and `posix-rename`/`statvfs`;
  the codec's own mock server covers a little more of that. An `#[ignore]`d run
  against `$DTR_SSH_TEST_HOST` would close it, and it is not written — the same
  gap `docs/sftp.md` §8 records, so it is claimed neither here nor there.
- **The 30 s deadline is not exercised.** A test would wait half a minute; only
  the reconnect-on-error path is (`a_dead_session_is_replaced_once…`). The
  timeout path is the same code with the deadline's `Err` where the error is.
- **`ssh` connect itself is untested here** (`BatchMode=yes`, `-s <alias>
  sftp`, the child's stderr in the handshake error). That is the codec's
  `connect`; its argument ordering has its own test there.

### 12.7 What the F5 wiring task instantiates and drives

On the worker thread that owns the compio runtime (the same place `dtr-bg`'s
mailbox runs; the UI thread can neither own the runtime nor a `Session`):

```rust
let mut transport = dtr_transfer_ssh::SshTransport::new();
let mut queue = dtr_transfer::Queue::new();
// commands from the UI are the queue's synchronous methods (copy/answer/cancel/…)
while queue.may_progress() {
    let events = queue.step(&mut transport).await;
    // fold events + queue.snapshot() into the frame, per §8
}
```

- **One transport, one queue, every job shape.** The wiring does not route: a
  local copy, an upload, a download, a same-host remote copy and a cross-host
  copy all go through the same `&mut transport`. `Queue::copy`/`move_to` infer
  the direction from the `Location`s, and this adapter infers the operation from
  the same two.
- **`SshTransport::new()` is production.** `with_parts` is the `#[doc(hidden)]`
  test seam (fake local transport / scripted dialer) and is not for the app.
- **Shutdown** is dropping the queue and then the transport: each `Session`'s
  `Drop` kills its child, and a killed child closes the handles it still had.
  Nothing in the transport needs a tick of its own: it has no timer, no thread
  and no mailbox, so the driver loop is the queue's loop with
  `queue.step(&mut transport)`.
- **A cancel between steps is enough**: `Queue::cancel` sets the flag, the next
  `step` aborts the op (a synchronous burn) and awaits `discard`, exactly as
  the queue's own tests describe — the adapter does not have a cancellation
  channel of its own.

Everything the card draws, the events it reacts to and the commands it sends are
§8's; this crate adds no UI-visible state.


## Appendix: decisions considered and rejected

- **Per-source machine release** (let another job interleave between two files of
  one multi-file job): rejected for F5. One job is one stream on one machine;
  releasing between files would make "ten files to one host" two streams and
  needs a second reservation for the names it has not planned yet. It can be
  revisited when throughput on a WAN is measured.
- **Destination-name reservations**: unnecessary, the destination machine is
  held (§3).
- **A sixth `State::WaitingForAnswer`**: a conflict question is a property of a
  running job; a state the card must special-case twice is worse than a `waiting`
  flag.
- **Leaving the part on cancel**: rejected — a cancel is "stop and clean up"; the
  failed transfer is the case the memo allows to keep it, because it is the one a
  retry resumes from.
- **A separate `CopyOp` per direction**: one op trait, one `StreamOp`, one local
  op. The direction is in `OpenRequest`, so an adapter picks reader/writer; the
  queue does not branch on it except for the move's source removal.
