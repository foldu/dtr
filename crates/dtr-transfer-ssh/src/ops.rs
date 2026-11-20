//! The readers and writers one segmented copy is built from.
//!
//! `dtr_transfer::StreamOp` is the queue's generic operation: read a segment,
//! write a segment, one progress figure. That is exactly the shape of an
//! upload, a download and a cross-host copy — the difference is only which end
//! is a local `File` and which is an alias's session — so this module supplies
//! the four ends and lets the queue's op own the loop. Nothing here publishes,
//! invents a `.part` name, or deletes anything: those are the queue's decisions
//! (`docs/transfers.md` §5).
//!
//! The local halves are `std::fs`, deliberately: a 64 KiB read or write on the
//! driver's thread is the bounded work a segment promised, and the queue's own
//! [`LocalCopyOp`](dtr_transfer::LocalCopyOp) set that precedent. The remote
//! halves own a [`Slot`] and the codec's handle; each `read`/`write` borrows the
//! alias for one packet and gives it back.

use std::fs::File;
use std::io::{self, Read as _, Write as _};

use anyhow::{Context as _, Result};
use dtr_transfer::{BoxFuture, Reader, Writer};

use crate::session::{RemoteRead, RemoteWrite};

/// A local source file as a stream, for an upload.
pub(crate) struct LocalReader {
    file: File,
}

impl LocalReader {
    pub(crate) fn new(file: File) -> Self {
        Self { file }
    }
}

impl Reader for LocalReader {
    fn read<'a>(&'a mut self, buf: &'a mut [u8]) -> BoxFuture<'a, Result<usize>> {
        Box::pin(async move {
            let mut filled = 0;
            while filled < buf.len() {
                match self.file.read(&mut buf[filled..]) {
                    Ok(0) => break,
                    Ok(read) => filled += read,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error).context("read the source"),
                }
            }
            Ok(filled)
        })
    }
}

/// The local part of a download as a stream.
pub(crate) struct LocalWriter {
    file: Option<File>,
}

impl LocalWriter {
    pub(crate) fn new(file: File) -> Self {
        Self { file: Some(file) }
    }
}

impl Writer for LocalWriter {
    fn write<'a>(&'a mut self, data: &'a [u8]) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.file
                .as_mut()
                .context("writing to a part that was already closed")?
                .write_all(data)
                .context("write the part")
        })
    }

    fn finish<'a>(&'a mut self) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let Some(file) = self.file.take() else {
                return Ok(());
            };
            file.sync_all().context("flush the part")
        })
    }

    fn abort(&mut self) {
        // Dropping the handle closes it. The part stays: the queue discards it on
        // a cancel and leaves it for a retry on a failure.
        self.file = None;
    }
}

/// A remote source as a stream, one codec chunk at a time.
///
/// The codec's `read_chunk` always asks for [`dtr_sftp::CHUNK`] bytes, and the
/// queue's segment buffer may be smaller (its tests build queues with a handful
/// of bytes), so what does not fit in the caller's slice waits in `pending` for
/// the next call. A segment therefore never claims to have written more than the
/// buffer it was given.
pub(crate) struct RemoteReader {
    read: RemoteRead,
    pending: Vec<u8>,
    at: usize,
}

impl RemoteReader {
    pub(crate) fn new(read: RemoteRead) -> Self {
        Self {
            read,
            pending: Vec::new(),
            at: 0,
        }
    }
}

impl Reader for RemoteReader {
    fn read<'a>(&'a mut self, buf: &'a mut [u8]) -> BoxFuture<'a, Result<usize>> {
        Box::pin(async move {
            if self.at == self.pending.len() {
                self.pending.clear();
                self.at = 0;
                let Some(bytes) = self.read.next_chunk().await? else {
                    return Ok(0);
                };
                self.pending = bytes;
            }
            let take = buf.len().min(self.pending.len() - self.at);
            buf[..take].copy_from_slice(&self.pending[self.at..self.at + take]);
            self.at += take;
            Ok(take)
        })
    }
}

/// A remote part as a stream: the codec's segmented upload.
pub(crate) struct RemoteWriter {
    write: RemoteWrite,
}

impl RemoteWriter {
    pub(crate) fn new(write: RemoteWrite) -> Self {
        Self { write }
    }
}

impl Writer for RemoteWriter {
    fn write<'a>(&'a mut self, data: &'a [u8]) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { self.write.write(data).await })
    }

    fn finish<'a>(&'a mut self) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { self.write.close().await })
    }

    fn abort(&mut self) {
        self.write.abort();
    }
}
