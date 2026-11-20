//! Running a write request through a machine's VFS verbs.
//!
//! The browser reports a [`WriteRequest`] and does no I/O (`dtr-files`'
//! module doc says why); somebody has to turn it into the five `Backend` calls,
//! and every frontend that does must do it the same way. The two copies this
//! replaces — the terminal app's and the portal worker's — disagreed about one
//! thing already: the terminal only recognised a missing destination as an
//! `io::ErrorKind::NotFound`, while the portal also read SFTP's own words, so a
//! remote replace whose destination had vanished failed in one frontend and was
//! tolerated in the other.
//!
//! Two rules live here and nowhere else:
//!
//! - **A backend never replaces a destination.** `replace` names how to clear
//!   the destination, and this removes it *first*, then runs the verb. A
//!   destination that vanished between the conflict card and the answer is not a
//!   failure — the answer was "write over it", and it is no longer there. Any
//!   other removal failure stops the operation before the verb runs.
//! - **One verb, one call.** `CreateDir`, `Rename`, `Copy`, `Move` and `Remove`
//!   are the trait's methods, in the order the plan names; nothing here
//!   re-implements a rename.

use std::sync::Arc;

use dtr_files::{WriteOp, WriteRequest};
use dtr_vfs::Backend;

/// Execute one file operation through `backend`'s verbs.
///
/// The caller resolves the `Arc<dyn Backend>` from its own `Vfs` (the connection
/// pool belongs to the frontend) and awaits this off the drawing thread. Every
/// verb is the backend's, so a slow mount or an `ssh` round trip is the worker's
/// wait and not a frame's.
pub async fn execute(backend: Arc<dyn Backend>, request: WriteRequest) -> anyhow::Result<()> {
    let WriteRequest { op, replace, .. } = request;
    if let (Some(how), Some(destination)) = (replace, op.destination()) {
        match backend.remove(destination, how).await {
            Ok(()) => {}
            Err(err) if is_not_found(&err) => {}
            Err(err) => return Err(err),
        }
    }
    match &op {
        WriteOp::CreateDir { at } => backend.create_dir(at).await,
        WriteOp::Rename { from, to } => backend.rename(from, to).await,
        WriteOp::Copy { from, to } => backend.copy(from, to).await.map(|_bytes| ()),
        WriteOp::Move { from, to } => backend.move_to(from, to).await,
        WriteOp::Remove { at, how } => backend.remove(at, *how).await,
    }
}

/// Whether a failed write was "it is not there", which the replace path ignores.
///
/// The VFS verbs wrap the OS error with the path they were working on, so the
/// `io::Error` is in the chain rather than at the top. A remote backend answers
/// in SFTP's words instead (`no such file`), so the message is matched too: a
/// false negative leaves the write failed, which is the safe direction, but a
/// false positive would run a verb over a destination nobody cleared — which is
/// why this is only ever asked about the *removal* of a destination, never about
/// a source.
pub fn is_not_found(err: &anyhow::Error) -> bool {
    if err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|err| err.kind() == std::io::ErrorKind::NotFound)
    }) {
        return true;
    }
    let text = format!("{err:#}").to_ascii_lowercase();
    text.contains("no such file") || text.contains("not found") || text.contains("no file")
}

#[cfg(test)]
mod tests {
    use super::*;
    use dtr_files::WriteOp;
    use dtr_vfs::Removal;

    fn local_backend(dir: &std::path::Path) -> Arc<dyn Backend> {
        Arc::new(dtr_vfs::Local::with_trash_root(dir.join("trash")))
    }

    fn request(op: WriteOp, replace: Option<Removal>, backend: dtr_vfs::BackendId) -> WriteRequest {
        WriteRequest {
            backend,
            op,
            replace,
        }
    }

    fn local(op: WriteOp, replace: Option<Removal>) -> WriteRequest {
        request(op, replace, dtr_vfs::BackendId::Local)
    }

    /// Every verb the plan can hand the worker reaches the trait method the verb
    /// is named for, and the ones that change the listing really change the disk:
    /// this is the half that `dtr-files` deliberately does not test, because its
    /// state machine owns no I/O.
    #[compio::test]
    async fn a_write_request_creates_renames_copies_and_deletes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let backend = local_backend(root);

        execute(
            backend.clone(),
            local(
                WriteOp::CreateDir {
                    at: root.join("created"),
                },
                None,
            ),
        )
        .await
        .expect("mkdir");
        assert!(root.join("created").is_dir());

        std::fs::write(root.join("before.txt"), b"bytes").expect("a source");
        execute(
            backend.clone(),
            local(
                WriteOp::Rename {
                    from: root.join("before.txt"),
                    to: root.join("after.txt"),
                },
                None,
            ),
        )
        .await
        .expect("rename");
        assert!(!root.join("before.txt").exists());
        assert_eq!(
            std::fs::read(root.join("after.txt")).expect("renamed"),
            b"bytes"
        );

        execute(
            backend.clone(),
            local(
                WriteOp::Copy {
                    from: root.join("after.txt"),
                    to: root.join("copy.txt"),
                },
                None,
            ),
        )
        .await
        .expect("copy");
        assert_eq!(
            std::fs::read(root.join("copy.txt")).expect("copied"),
            b"bytes"
        );

        // A delete with a trash is a move into `$trash/files`, and the info file
        // records where it came from: the disk proves the trash was used rather
        // than unlink.
        std::fs::write(root.join("gone.txt"), b"gone").expect("a file to trash");
        execute(
            backend.clone(),
            local(
                WriteOp::Remove {
                    at: root.join("gone.txt"),
                    how: Removal::Trash,
                },
                None,
            ),
        )
        .await
        .expect("trash");
        assert!(!root.join("gone.txt").exists());
        assert!(root.join("trash/files/gone.txt").exists(), "into the trash");
        assert!(
            root.join("trash/info/gone.txt.trashinfo").exists(),
            "the info pair"
        );

        // The permanent delete is the one that does not leave a trash entry.
        std::fs::write(root.join("permanent.txt"), b"gone").expect("a file to delete");
        execute(
            backend.clone(),
            local(
                WriteOp::Remove {
                    at: root.join("permanent.txt"),
                    how: Removal::Permanent,
                },
                None,
            ),
        )
        .await
        .expect("delete");
        assert!(!root.join("permanent.txt").exists());
        assert!(!root.join("trash/files/permanent.txt").exists());

        execute(
            backend,
            local(
                WriteOp::Move {
                    from: root.join("copy.txt"),
                    to: root.join("moved.txt"),
                },
                None,
            ),
        )
        .await
        .expect("move");
        assert!(root.join("moved.txt").exists());
        assert!(!root.join("copy.txt").exists());
    }

    /// The overwrite answer is spelled by the executor: remove the destination
    /// with the answer's removal, **then** run the verb. A backend never replaces
    /// anything itself, so this is the only place the destructive half of
    /// "replace" happens — and the trash is where the old file goes.
    #[compio::test]
    async fn replacing_a_conflict_trashes_the_destination_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let backend = local_backend(root);
        std::fs::write(root.join("new.txt"), b"new").expect("the source");
        std::fs::write(root.join("existing.txt"), b"old").expect("the destination");

        execute(
            backend,
            local(
                WriteOp::Rename {
                    from: root.join("new.txt"),
                    to: root.join("existing.txt"),
                },
                Some(Removal::Trash),
            ),
        )
        .await
        .expect("replace");

        assert_eq!(
            std::fs::read(root.join("existing.txt")).expect("the replacement"),
            b"new"
        );
        assert!(!root.join("new.txt").exists(), "a rename moves the source");
        assert_eq!(
            std::fs::read(root.join("trash/files/existing.txt")).expect("the old destination"),
            b"old",
            "replace is the trash's, not an unlink"
        );
    }

    /// A destination that disappeared between the question and the answer is not
    /// a failure: the answer was "write over it" and it is no longer there. The
    /// same code path covers a copy whose destination never existed, which is
    /// every ordinary paste.
    #[compio::test]
    async fn replacing_a_destination_that_is_already_gone_still_writes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let backend = local_backend(root);
        std::fs::write(root.join("source.txt"), b"bytes").expect("the source");

        execute(
            backend,
            local(
                WriteOp::Copy {
                    from: root.join("source.txt"),
                    to: root.join("dest.txt"),
                },
                Some(Removal::Trash),
            ),
        )
        .await
        .expect("a missing destination is not an error");

        assert_eq!(
            std::fs::read(root.join("dest.txt")).expect("the copy"),
            b"bytes"
        );
    }

    /// A *missing source* is a real failure and must stay one: only the removal
    /// of a destination tolerates "not found".
    #[compio::test]
    async fn a_missing_source_is_not_swallowed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let backend = local_backend(root);
        let result = execute(
            backend,
            local(
                WriteOp::Copy {
                    from: root.join("absent.txt"),
                    to: root.join("dest.txt"),
                },
                None,
            ),
        )
        .await;
        assert!(result.is_err(), "a copy of nothing is an error");
    }

    #[test]
    fn a_missing_path_is_recognised_through_the_anyhow_chain() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        let err = anyhow::Error::new(io).context("while removing");
        assert!(is_not_found(&err));
        let err = anyhow::anyhow!("SFTP: no such file");
        assert!(is_not_found(&err));
        let err = anyhow::anyhow!("permission denied");
        assert!(!is_not_found(&err));
    }
}
