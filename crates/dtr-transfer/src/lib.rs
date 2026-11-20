//! The transfer queue, without a window.
//!
//! `docs/manager.md` §8 decides that dtr does not mount a remote filesystem and
//! that the copy queue is the layer a mount would have been. This crate is that
//! layer's model: a [`Queue`] of [`Job`]s, each advanced by bounded
//! [`Segment`](op::Segment)s through a [`Transport`] the app provides, with the
//! `.part`-then-publish rule, per-machine serialisation, conflict answers,
//! cancel and retry.
//!
//! # What is here, and what is not
//!
//! - **Here**: the job model ([`Job`], [`JobView`]), the scheduler and its
//!   state machine ([`Queue`], [`Event`]), the op/transport seam ([`Op`],
//!   [`Transport`], [`StreamOp`]), the local copy ([`LocalCopyOp`],
//!   [`LocalTransport`]), and the keep-both naming rule ([`naming`]).
//! - **Not here**: any runtime (no `compio`, no `tokio`, no thread); egui or any
//!   drawing; the SFTP/ssh adapter; recursive directory copies, resume, trash,
//!   permissions, or a `statvfs` implementation. The adapter implements
//!   [`Transport`] over `dtr-vfs`/`dtr-sftp`; the F5 wiring task drives
//!   [`Queue::step`] on the worker thread and paints [`Queue::snapshot`] and
//!   [`Event`]s. `docs/transfers.md` is the account.
//!
//! # The three rules a caller has to know
//!
//! 1. **The destination never sees a partial byte under its real name.** The op
//!    writes `<name>.part` in the destination directory; publishing is one
//!    rename after the part is flushed. A cancel removes the part; a failure
//!    leaves it (retry restarts the file from byte zero, truncating it) and
//!    never publishes.
//! 2. **One machine, one stream.** A job holds every machine it touches
//!    (sources and destination) for its whole run; two jobs on the same machine
//!    run one after the other, two jobs on different machines run together.
//! 3. **A conflict is asked, not guessed.** When the destination exists the
//!    queue raises [`Event::NeedsAnswer`] and stops that job until
//!    [`Queue::answer`] is called; the answer is consumed by that one file.
//!
//! # Driving it
//!
//! ```no_run
//! # async fn drive(queue: &mut dtr_transfer::Queue, transport: &mut dyn dtr_transfer::Transport) {
//! // One bounded action per call; keep going while it can make progress.
//! while queue.may_progress() {
//!     for event in queue.step(transport).await {
//!         // paint a row, open the conflict card, raise a toast
//!         let _ = event;
//!     }
//!     let rows = queue.snapshot(); // what the card draws
//!     let _ = rows;
//! }
//! # }
//! ```

pub mod job;
pub mod local;
pub mod naming;
pub mod op;
pub mod queue;

pub use job::{Answer, Conflict, Direction, EnqueueError, Job, JobId, JobView, State};
pub use local::{LocalCopyOp, LocalTransport};
pub use naming::{free_name, numbered_name};
pub use op::{BoxFuture, Op, OpenRequest, Reader, SEGMENT, Segment, StreamOp, Transport, Writer};
pub use queue::{AnswerError, ControlError, Event, Queue};
