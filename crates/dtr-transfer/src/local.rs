//! Local files: the first concrete operation, and the first concrete transport.
//!
//! A local copy is the queue's simplest user and its reference implementation:
//! open the source, create `<name>.part` next to the destination, move 64 KiB
//! (or whatever segment the queue was built with) at a time, flush, and let the
//! queue publish with a rename. It is deliberately written against `std::fs`, a
//! blocking API, because this crate has no runtime: the driver runs on a worker
//! thread, not the UI thread (`docs/compio.md` §10.3), and one 64 KiB read and
//! write is the bounded work a segment promised.
//!
//! The [`Transport`] half is what the app's adapter delegates to for local
//! paths; remote paths are the adapter's business (the SFTP codec's segmented
//! upload, `docs/sftp.md` §7).

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use dtr_vfs::{BackendId, Kind, Location, Meta};

use crate::op::{BoxFuture, Op, OpenRequest, Segment, Transport};

/// One local file to one local part.
pub struct LocalCopyOp {
    source: File,
    part: File,
    finished: bool,
}

impl LocalCopyOp {
    /// Open `source` for reading and create `part` (truncating a leftover).
    ///
    /// The queue has already stat'ed the source and refused a directory, so
    /// this is the normal path plus the races: a source that vanished and a
    /// destination directory that is not writable both surface here.
    pub fn open(source: &Path, part: &Path) -> io::Result<Self> {
        let metadata = fs::metadata(source)?;
        if metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{} is a directory; recursive copy is not supported yet",
                    source.display()
                ),
            ));
        }
        let source = File::open(source)?;
        let part = File::create(part)?;
        Ok(Self {
            source,
            part,
            finished: false,
        })
    }
}

impl Op for LocalCopyOp {
    fn step<'a>(&'a mut self, buf: &'a mut [u8]) -> BoxFuture<'a, Result<Segment>> {
        Box::pin(async move {
            let mut filled = 0;
            while filled < buf.len() {
                match self.source.read(&mut buf[filled..]) {
                    Ok(0) => break,
                    Ok(read) => filled += read,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error).context("read the source"),
                }
            }
            if filled == 0 {
                return Ok(Segment::Done);
            }
            self.part.write_all(&buf[..filled])?;
            Ok(Segment::Wrote(filled as u64))
        })
    }

    fn finish<'a>(&'a mut self) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if self.finished {
                return Ok(());
            }
            self.finished = true;
            self.part.flush()?;
            // Durability before the rename: a published name whose bytes are
            // still in the page cache is fine after a crash on a journaled
            // filesystem, not fine on removable media.
            self.part.sync_all()?;
            Ok(())
        })
    }

    fn abort(&mut self) {
        self.finished = true;
        // Dropping the handles closes them. The part stays: the queue discards
        // it on a cancel and leaves it for a retry on a failure, so deleting it
        // here would make both decisions for the caller.
    }
}

/// The local filesystem as a [`Transport`].
///
/// [`open`](Transport::open) only accepts local-to-local requests; an adapter
/// that composes this with an SFTP transport routes by `Location::backend` and
/// hands the local pairs here. `publish` and `discard` are local-only as well,
/// because a part and the name it becomes are always in the destination
/// directory — the same machine, always.
#[derive(Default)]
pub struct LocalTransport;

impl LocalTransport {
    pub fn new() -> Self {
        Self
    }
}

fn meta_of(metadata: &fs::Metadata) -> Meta {
    Meta {
        kind: if metadata.is_dir() {
            Kind::Dir
        } else if metadata.is_file() {
            Kind::File
        } else {
            Kind::Other
        },
        len: metadata.len(),
        modified: metadata.modified().ok(),
    }
}

impl Transport for LocalTransport {
    fn stat<'a>(&'a mut self, at: &'a Location) -> BoxFuture<'a, Result<Option<Meta>>> {
        Box::pin(async move {
            if at.backend != BackendId::Local {
                bail!(
                    "the local transport cannot look at {}; the adapter routes by machine",
                    at.describe()
                );
            }
            match fs::metadata(&at.path) {
                Ok(metadata) => Ok(Some(meta_of(&metadata))),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error).with_context(|| format!("stat {}", at.path.display())),
            }
        })
    }

    fn open<'a>(&'a mut self, request: OpenRequest) -> BoxFuture<'a, Result<Box<dyn Op>>> {
        Box::pin(async move {
            if request.source.backend != BackendId::Local
                || request.part.backend != BackendId::Local
            {
                bail!(
                    "the local transport only copies between local paths ({} -> {})",
                    request.source.describe(),
                    request.part.describe()
                );
            }
            let op = LocalCopyOp::open(&request.source.path, &request.part.path)
                .with_context(|| format!("copy {}", request.source.describe()))?;
            Ok(Box::new(op) as Box<dyn Op>)
        })
    }

    fn publish<'a>(
        &'a mut self,
        part: &'a Location,
        destination: &'a Location,
        overwrite: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if part.backend != BackendId::Local || destination.backend != BackendId::Local {
                bail!(
                    "the local transport cannot publish {} as {}",
                    part.describe(),
                    destination.describe()
                );
            }
            publish_local(&part.path, &destination.path, overwrite)
        })
    }

    fn discard<'a>(&'a mut self, part: &'a Location) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            match fs::remove_file(&part.path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error).with_context(|| format!("remove {}", part.path.display())),
            }
        })
    }

    fn remove<'a>(&'a mut self, at: &'a Location) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            fs::remove_file(&at.path).with_context(|| format!("remove {}", at.path.display()))
        })
    }
}

/// Rename a finished part to its real name.
///
/// `overwrite = false` must fail rather than replace, so it is a hard link (the
/// one no-clobber rename `std` offers on one filesystem) followed by the part's
/// removal. A filesystem without hard links falls back to check-then-rename,
/// which is a race the fallback admits rather than a rule the transfer breaks;
/// the SFTP adapter has the better answer for a race — plain `SSH_FXP_RENAME`
/// fails if the name appeared — and uses it.
fn publish_local(part: &Path, destination: &Path, overwrite: bool) -> Result<()> {
    if overwrite {
        match fs::rename(part, destination) {
            Ok(()) => return Ok(()),
            Err(first) => {
                // Unix replaces in `rename`; Windows does not, and says so with
                // an error. The replace answer was the user's, so removing the
                // old name is what they asked for — not a clobber.
                if destination.exists() {
                    fs::remove_file(destination).with_context(|| {
                        format!("remove {} to replace it", destination.display())
                    })?;
                    fs::rename(part, destination)
                        .with_context(|| format!("publish {}", destination.display()))?;
                    return Ok(());
                }
                return Err(first).with_context(|| {
                    format!("rename {} to {}", part.display(), destination.display())
                });
            }
        }
    }
    match fs::hard_link(part, destination) {
        Ok(()) => {
            fs::remove_file(part).with_context(|| format!("remove the part {}", part.display()))
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => bail!(
            "{} appeared while the transfer was running, so it was not overwritten",
            destination.display()
        ),
        Err(_unsupported) => {
            // No hard links here (some removable-media filesystems). Check and
            // rename: the only no-clobber option `std` leaves on those media.
            if fs::symlink_metadata(destination).is_ok() {
                bail!(
                    "{} appeared while the transfer was running, so it was not overwritten",
                    destination.display()
                );
            }
            fs::rename(part, destination)
                .with_context(|| format!("publish {}", destination.display()))
        }
    }
}
