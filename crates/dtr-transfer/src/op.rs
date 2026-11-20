//! One file's transfer, cut into segments, and the transport it runs over.
//!
//! The queue never does I/O itself: it owns a [`Transport`] passed on every
//! call, and the transport hands it an [`Op`] per source file. A segment is the
//! unit of bounded work and of cancellation — the SFTP codec's per-answer
//! deadline supports one 64 KiB packet (`docs/sftp.md` §7), so the core's
//! default segment is the same figure, and the queue's answer to "cancel" lands
//! between two of them.
//!
//! **The futures are boxed, and nothing here polls them.** A real transport is
//! `compio` — `dtr-vfs`'s backends and `dtr-sftp`'s [`Session`] are `!Send`
//! async — and `docs/compio.md` §10.3 is why a segment must be a future the
//! caller's runtime polls on the caller's thread, not a blocking call and not a
//! thread of our own. `BoxFuture` is the same spelling `dtr-vfs` uses for the
//! same reason: no executor dependency, no `async_trait` macro, and object
//! safety for `dyn Op`.

use std::future::Future;
use std::pin::Pin;

use anyhow::{Result, bail};
use dtr_vfs::{Location, Meta};

use crate::job::{Direction, JobId};

/// A future the driving runtime polls. Not `Send`: compio's handles are
/// thread-local, so the queue and its transports live on one thread
/// (`docs/compio.md` §10.3, trap 4).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// The most bytes one segment moves, and the size of the queue's shared buffer.
///
/// 64 KiB is what one SFTP `WRITE` packet may carry inside the codec's 30 s
/// answer deadline (`dtr_sftp::CHUNK`, `docs/sftp.md` §7). The core does not
/// depend on the codec, so it states the figure itself; a transport that can
/// move more per answer is free to, but the buffer is the bound.
pub const SEGMENT: usize = 64 * 1024;

/// What one segment did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Segment {
    /// Bytes written to the partial destination.
    Wrote(u64),
    /// The source is exhausted; the next call is [`Op::finish`].
    Done,
}

/// One file moving from a source to a `<name>.part` in the destination
/// directory.
///
/// The op never publishes and never invents a `.part` convention: it opens
/// exactly the part path [`OpenRequest`] names, and [`Transport::publish`]
/// renames it once [`Op::finish`] has flushed it. That split is what lets a
/// cancelled or failed transfer leave nothing under the real name.
pub trait Op {
    /// Move at most `buf.len()` bytes. `Wrote(0)` is a broken op, not a pause.
    fn step<'a>(&'a mut self, buf: &'a mut [u8]) -> BoxFuture<'a, Result<Segment>>;

    /// Flush and close the partial destination. Called once, after
    /// [`Segment::Done`] and before publishing.
    fn finish<'a>(&'a mut self) -> BoxFuture<'a, Result<()>>;

    /// Release the op's handles after a cancel or a failure.
    ///
    /// Deliberately synchronous and best-effort: an error here is the second
    /// error of a failed transfer, and there is nobody left to tell. It must
    /// **not** delete the part — the queue owns the part's lifecycle, because
    /// a failure leaves it for retry and a cancel discards it.
    fn abort(&mut self);
}

/// What the queue asks a transport to open.
///
/// `part` is the exact destination path (`<name>.part` in the destination
/// directory); the op writes there and nowhere else. `direction` says which way
/// the bytes go, so one adapter can pick a reader and a writer without knowing
/// the queue's phases.
#[derive(Clone, Debug)]
pub struct OpenRequest {
    pub job: JobId,
    pub source: Location,
    pub part: Location,
    pub direction: Direction,
}

/// Everything the queue needs from a machine's filesystem, as futures.
///
/// This is the seam the F5 wiring implements over `dtr-vfs`/`dtr-sftp`: `stat`
/// is `Backend::stat` (mapping "not found" to `None`), `open` builds a segmented
/// copy out of the source reader and the destination's upload handle, `publish`
/// is the rename, and `discard` is the unlink. [`LocalTransport`] in this crate
/// is the first implementation; `tests/queue.rs` has a fault-injecting one.
///
/// [`LocalTransport`]: crate::LocalTransport
pub trait Transport {
    /// One path's metadata, or `None` when it does not exist.
    ///
    /// A missing path is `Ok(None)` and not an error: the queue asks "does the
    /// destination exist?" with the same call it asks "what is the source?".
    fn stat<'a>(&'a mut self, at: &'a Location) -> BoxFuture<'a, Result<Option<Meta>>>;

    /// Open one source and create its part, ready for segments.
    fn open<'a>(&'a mut self, request: OpenRequest) -> BoxFuture<'a, Result<Box<dyn Op>>>;

    /// Rename the finished part to the real destination name.
    ///
    /// `overwrite` is the caller's answer to the conflict question, never a
    /// guess: `true` only after the user said "replace". `false` must fail
    /// rather than clobber, because a "keep both" that lost a race must not
    /// overwrite what it kept.
    fn publish<'a>(
        &'a mut self,
        part: &'a Location,
        destination: &'a Location,
        overwrite: bool,
    ) -> BoxFuture<'a, Result<()>>;

    /// Remove a partial destination after a cancel. Best effort: a part that
    /// cannot be removed is a warning on the cancelled job, not a second error.
    fn discard<'a>(&'a mut self, part: &'a Location) -> BoxFuture<'a, Result<()>>;

    /// Remove a source file, after a move has published its copy.
    fn remove<'a>(&'a mut self, at: &'a Location) -> BoxFuture<'a, Result<()>>;

    /// Refuse a transfer before any byte moves, if the adapter can tell.
    ///
    /// This is the `statvfs@openssh.com` hook (`docs/manager.md` §8): the SFTP
    /// adapter answers "not enough room" here so a 4 GB upload fails in one
    /// round trip rather than on the last segment. The default cannot tell and
    /// does not pretend to.
    fn preflight<'a>(
        &'a mut self,
        destination: &'a Location,
        bytes: u64,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let _ = (destination, bytes);
            Ok(())
        })
    }
}

/// A source as a byte stream, for the cross-host copy.
///
/// A real implementation is one backend's read: `dtr-vfs`'s `fetch` is
/// whole-file today, so the SFTP adapter reads through the codec's `read_at`
/// and a local source reads a `File`. The queue never sees either.
pub trait Reader {
    /// Fill up to `buf.len()` bytes; `0` means end of file.
    fn read<'a>(&'a mut self, buf: &'a mut [u8]) -> BoxFuture<'a, Result<usize>>;
}

/// A destination as a byte stream, for the cross-host copy.
///
/// A real implementation is `begin_upload`/`write_upload_chunk`/
/// `finish_upload`/`abort_upload` for SFTP and a `File` for a local copy.
/// [`abort`](Writer::abort) closes without publishing; the `.part` is the
/// queue's business, as with [`Op::abort`].
pub trait Writer {
    /// Append `data` to the part. Bounded by the queue's segment.
    fn write<'a>(&'a mut self, data: &'a [u8]) -> BoxFuture<'a, Result<()>>;

    /// Flush and close the part.
    fn finish<'a>(&'a mut self) -> BoxFuture<'a, Result<()>>;

    /// Release the writer after a cancel or a failure.
    fn abort(&mut self);
}

/// The generic operation: read A, write B, one segment at a time.
///
/// This is the cross-host copy §8 asks for — SFTP v3 has no server-to-server
/// copy and OpenSSH's `copy-data` stays inside one server, so A → B streams
/// through dtr — and it is also how an upload or a download can be built when
/// the adapter prefers streams over a whole-file API. One op, one progress
/// figure: the queue adds each [`Segment::Wrote`] to the job.
pub struct StreamOp {
    reader: Box<dyn Reader>,
    writer: Box<dyn Writer>,
    /// Set once `finish` has run, so a second call is a no-op rather than a
    /// second close on a handle that is gone.
    finished: bool,
}

impl StreamOp {
    pub fn new(reader: Box<dyn Reader>, writer: Box<dyn Writer>) -> Self {
        Self {
            reader,
            writer,
            finished: false,
        }
    }
}

impl Op for StreamOp {
    fn step<'a>(&'a mut self, buf: &'a mut [u8]) -> BoxFuture<'a, Result<Segment>> {
        Box::pin(async move {
            let read = self.reader.read(&mut buf[..]).await?;
            if read == 0 {
                return Ok(Segment::Done);
            }
            if read > buf.len() {
                bail!(
                    "the source returned {read} bytes for a {}-byte segment",
                    buf.len()
                );
            }
            self.writer.write(&buf[..read]).await?;
            Ok(Segment::Wrote(read as u64))
        })
    }

    fn finish<'a>(&'a mut self) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if self.finished {
                return Ok(());
            }
            self.finished = true;
            self.writer.finish().await
        })
    }

    fn abort(&mut self) {
        if !self.finished {
            self.finished = true;
            self.writer.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    struct Bytes {
        left: Vec<u8>,
    }

    impl Reader for Bytes {
        fn read<'a>(&'a mut self, buf: &'a mut [u8]) -> BoxFuture<'a, Result<usize>> {
            Box::pin(async move {
                let take = buf.len().min(self.left.len());
                buf[..take].copy_from_slice(&self.left[..take]);
                self.left.drain(..take);
                Ok(take)
            })
        }
    }

    struct Sink {
        written: Rc<RefCell<Vec<u8>>>,
        finished: Rc<RefCell<bool>>,
        aborted: Rc<RefCell<bool>>,
    }

    impl Writer for Sink {
        fn write<'a>(&'a mut self, data: &'a [u8]) -> BoxFuture<'a, Result<()>> {
            Box::pin(async move {
                self.written.borrow_mut().extend_from_slice(data);
                Ok(())
            })
        }

        fn finish<'a>(&'a mut self) -> BoxFuture<'a, Result<()>> {
            Box::pin(async move {
                *self.finished.borrow_mut() = true;
                Ok(())
            })
        }

        fn abort(&mut self) {
            *self.aborted.borrow_mut() = true;
        }
    }

    #[test]
    fn the_stream_op_moves_bytes_in_segments_and_finishes_once() {
        let written = Rc::new(RefCell::new(Vec::new()));
        let finished = Rc::new(RefCell::new(false));
        let aborted = Rc::new(RefCell::new(false));
        let reader = Bytes {
            left: b"hello, segmented world".to_vec(),
        };
        let writer = Sink {
            written: written.clone(),
            finished: finished.clone(),
            aborted: aborted.clone(),
        };
        let mut op = StreamOp::new(Box::new(reader), Box::new(writer));

        let mut buf = [0u8; 7];
        let mut segments = Vec::new();
        loop {
            let segment = pollster::block_on(op.step(&mut buf)).expect("a segment");
            segments.push(segment);
            if segment == Segment::Done {
                break;
            }
        }
        assert_eq!(
            segments,
            vec![
                Segment::Wrote(7),
                Segment::Wrote(7),
                Segment::Wrote(7),
                Segment::Wrote(1),
                Segment::Done,
            ]
        );
        assert_eq!(&*written.borrow(), b"hello, segmented world");
        assert!(!*finished.borrow(), "step must not finish the writer");
        pollster::block_on(op.finish()).expect("finish");
        assert!(*finished.borrow());
        // A second finish is a no-op, not a second close.
        pollster::block_on(op.finish()).expect("finish again");
        assert!(!*aborted.borrow());
    }

    #[test]
    fn an_empty_source_is_done_without_writing() {
        let written = Rc::new(RefCell::new(Vec::new()));
        let mut op = StreamOp::new(
            Box::new(Bytes { left: Vec::new() }),
            Box::new(Sink {
                written: written.clone(),
                finished: Rc::new(RefCell::new(false)),
                aborted: Rc::new(RefCell::new(false)),
            }),
        );
        let mut buf = [0u8; 4];
        assert_eq!(
            pollster::block_on(op.step(&mut buf)).expect("a segment"),
            Segment::Done
        );
        assert!(written.borrow().is_empty());
    }
}
