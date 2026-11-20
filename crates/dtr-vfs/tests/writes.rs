//! The write verbs: what the trait promises, and the freedesktop trash `Local`
//! implements for `Removal::Trash`.
//!
//! Every test here uses a `tempfile` directory and, where it deletes, a pinned
//! trash root (`Local::with_trash_root`), so a delete can never reach the real
//! `$HOME`/`$XDG_DATA_HOME` and no `$topdir/.Trash-$uid` is written into the
//! test tree. `docs/trash.md` has the reasoning.

use std::path::{Path, PathBuf};

use dtr_vfs::{
    Backend as _, BackendId, Batches, BoxFuture, Caps, Local, Memory, Meta, Removal, Ssh,
    is_conflict,
};

/// A local backend whose trash lives under the test's own directory.
fn local(dir: &Path) -> Local {
    Local::with_trash_root(dir.join("trash"))
}

fn write(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, bytes).unwrap();
}

// ---- capabilities and the read-only default ---------------------------------

#[test]
fn the_caps_say_what_each_backend_can_write() {
    assert_eq!(
        Local::new().caps(),
        Caps {
            watch: true,
            dir_size: false,
            write: true,
            trash: cfg!(unix),
        },
        "a local filesystem writes, and trashes where the freedesktop spec does"
    );
    assert_eq!(
        Memory::new().caps(),
        Caps {
            watch: false,
            dir_size: false,
            write: true,
            trash: false,
        },
        "the memory backend writes but has no desktop trash"
    );
    assert_eq!(
        Ssh::new("jupyter").caps(),
        Caps {
            watch: false,
            dir_size: false,
            write: true,
            trash: false,
        },
        "the sftp codec writes, and there is never a remote trash"
    );
}

/// A backend that declares `write: false` and implements none of the verbs, the
/// way a read-only machine would: the trait's defaults are the refusal, and they
/// name the verb and the path rather than saying "unsupported".
struct ReadOnly {
    id: BackendId,
}

impl ReadOnly {
    fn new() -> Self {
        Self {
            id: BackendId::Local,
        }
    }
}

impl dtr_vfs::Backend for ReadOnly {
    fn id(&self) -> &BackendId {
        &self.id
    }

    fn caps(&self) -> Caps {
        Caps {
            watch: false,
            dir_size: false,
            write: false,
            trash: false,
        }
    }

    fn stat<'a>(&'a self, _path: &'a Path) -> BoxFuture<'a, anyhow::Result<Meta>> {
        Box::pin(async { anyhow::bail!("a read-only test backend does not stat") })
    }

    fn fetch<'a>(&'a self, _path: &'a Path, _dest: &'a Path) -> BoxFuture<'a, anyhow::Result<u64>> {
        Box::pin(async { anyhow::bail!("a read-only test backend does not fetch") })
    }

    fn list(&self, _path: &Path) -> Batches {
        let (_tx, rx) = async_channel::bounded(1);
        rx
    }
}

#[compio::test]
async fn a_read_only_backend_refuses_each_verb_by_name() {
    let read_only = ReadOnly::new();
    assert!(!read_only.caps().write, "and it declares itself read-only");
    let from = Path::new("/srv/a");
    let to = Path::new("/srv/b");

    let err = read_only.create_dir(from).await.unwrap_err();
    assert!(
        err.to_string().contains("cannot create a directory"),
        "{err}"
    );
    let err = read_only.rename(from, to).await.unwrap_err();
    assert!(err.to_string().contains("cannot rename"), "{err}");
    let err = read_only.copy(from, to).await.unwrap_err();
    assert!(err.to_string().contains("cannot copy"), "{err}");
    let err = read_only.move_to(from, to).await.unwrap_err();
    assert!(err.to_string().contains("cannot move"), "{err}");
    let err = read_only.remove(from, Removal::Trash).await.unwrap_err();
    assert!(err.to_string().contains("cannot trash"), "{err}");
    let err = read_only
        .remove(from, Removal::Permanent)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("cannot delete"), "{err}");
}

// ---- create_dir, rename, copy, move_to -------------------------------------

#[compio::test]
async fn a_created_directory_is_there_and_a_second_is_a_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let backend = local(dir.path());
    let made = dir.path().join("made");

    backend.create_dir(&made).await.unwrap();
    assert!(made.is_dir(), "mkdir must create one directory");

    let err = backend.create_dir(&made).await.unwrap_err();
    assert!(is_conflict(&err), "an existing name is a conflict: {err}");

    // `mkdir`, not `mkdir -p`: the parent has to be there.
    let err = backend
        .create_dir(&dir.path().join("missing/child"))
        .await
        .unwrap_err();
    assert!(
        !is_conflict(&err),
        "a missing parent is not a conflict: {err}"
    );
}

#[compio::test]
async fn a_rename_moves_the_name_and_the_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let backend = local(dir.path());
    let from = dir.path().join("a.txt");
    let to = dir.path().join("b.txt");
    write(&from, b"hello");

    backend.rename(&from, &to).await.unwrap();
    assert!(!from.exists(), "the old name must be gone");
    assert_eq!(std::fs::read(&to).unwrap(), b"hello");
}

/// The trait promises a rename never replaces a destination; the UI asks.
#[compio::test]
async fn a_rename_onto_an_existing_name_is_a_conflict_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let backend = local(dir.path());
    let from = dir.path().join("a.txt");
    let to = dir.path().join("b.txt");
    write(&from, b"a");
    write(&to, b"b");

    let err = backend.rename(&from, &to).await.unwrap_err();
    assert!(
        is_conflict(&err),
        "an occupied destination is a conflict: {err}"
    );
    assert_eq!(std::fs::read(&from).unwrap(), b"a", "the source stays");
    assert_eq!(std::fs::read(&to).unwrap(), b"b", "the destination stays");
}

#[compio::test]
async fn a_copy_returns_its_bytes_and_never_replaces() {
    let dir = tempfile::tempdir().unwrap();
    let backend = local(dir.path());
    let from = dir.path().join("a.txt");
    let to = dir.path().join("copy.txt");
    write(&from, b"hello");

    assert_eq!(backend.copy(&from, &to).await.unwrap(), 5);
    assert_eq!(std::fs::read(&to).unwrap(), b"hello");

    let err = backend.copy(&from, &to).await.unwrap_err();
    assert!(is_conflict(&err), "a second copy is a conflict: {err}");
}

/// A tree copy is the transfer queue's job, not a whole-file copy's: it must say
/// so, and not leave a half copy behind.
#[compio::test]
async fn a_copy_of_a_directory_is_refused_with_a_reason() {
    let dir = tempfile::tempdir().unwrap();
    let backend = local(dir.path());
    let tree = dir.path().join("tree");
    write(&tree.join("leaf.txt"), b"x");

    let err = backend
        .copy(&tree, &dir.path().join("copy"))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("transfer queue"),
        "the refusal must say where tree copies belong: {err}"
    );
    assert!(!dir.path().join("copy").exists());
}

#[compio::test]
async fn a_move_to_another_directory_is_a_rename() {
    let dir = tempfile::tempdir().unwrap();
    let backend = local(dir.path());
    let from = dir.path().join("a.txt");
    let into = dir.path().join("sub");
    std::fs::create_dir(&into).unwrap();
    write(&from, b"hello");

    backend.move_to(&from, &into.join("a.txt")).await.unwrap();
    assert!(!from.exists());
    assert_eq!(std::fs::read(into.join("a.txt")).unwrap(), b"hello");
}

// ---- permanent delete ------------------------------------------------------

#[compio::test]
async fn a_permanent_delete_removes_a_file_dir_a_tree_and_never_trashes() {
    let dir = tempfile::tempdir().unwrap();
    let backend = local(dir.path());
    let file = dir.path().join("file.txt");
    let tree = dir.path().join("tree");
    write(&file, b"x");
    write(&tree.join("sub/leaf.txt"), b"y");

    backend.remove(&file, Removal::Permanent).await.unwrap();
    assert!(!file.exists(), "a permanent delete leaves nothing");

    backend.remove(&tree, Removal::Permanent).await.unwrap();
    assert!(!tree.exists(), "a directory is removed with its contents");

    let trash_files = dir.path().join("trash").join("files");
    assert!(
        !trash_files.exists() || std::fs::read_dir(&trash_files).unwrap().next().is_none(),
        "a permanent delete must not write into the trash"
    );

    let err = backend
        .remove(&dir.path().join("gone"), Removal::Permanent)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("delete"), "{err}");
}

// ---- the trash -------------------------------------------------------------

#[cfg(unix)]
mod trash {
    use super::*;

    fn trash_root(dir: &Path) -> PathBuf {
        dir.join("trash")
    }

    /// A file deleted with `Removal::Trash` is gone from where it was and whole
    /// in `$trash/files`, with the info file §6 requires beside it.
    #[compio::test]
    async fn a_trashed_file_leaves_the_original_and_lands_in_the_trash() {
        let dir = tempfile::tempdir().unwrap();
        let backend = local(dir.path());
        let victim = dir.path().join("notes.txt");
        write(&victim, b"hello");

        backend.remove(&victim, Removal::Trash).await.unwrap();

        assert!(!victim.exists(), "trashing must remove the original");
        let root = trash_root(dir.path());
        assert_eq!(
            std::fs::read(root.join("files/notes.txt")).unwrap(),
            b"hello",
            "the file itself goes under files/"
        );
        assert!(
            root.join("info/notes.txt.trashinfo").is_file(),
            "every entry needs its info file"
        );
    }

    /// The info file is the spec's format exactly: `[Trash Info]`, an absolute
    /// percent-encoded `Path`, and a local `DeletionDate` in RFC 3339's shape.
    #[compio::test]
    async fn the_trashinfo_names_the_original_path_and_deletion_time() {
        let dir = tempfile::tempdir().unwrap();
        let backend = local(dir.path());
        let victim = dir.path().join("a file 100%.txt");
        write(&victim, b"x");

        backend.remove(&victim, Removal::Trash).await.unwrap();

        let info =
            std::fs::read_to_string(trash_root(dir.path()).join("info/a file 100%.txt.trashinfo"))
                .unwrap();
        let mut lines = info.lines();
        assert_eq!(lines.next(), Some("[Trash Info]"));
        assert_eq!(
            lines.next(),
            Some(format!("Path={}", encoded(&victim)).as_str()),
            "the original path is absolute and URL-escaped"
        );
        let date = lines.next().expect("a DeletionDate line");
        let value = date
            .strip_prefix("DeletionDate=")
            .unwrap_or_else(|| panic!("not a DeletionDate line: {date}"));
        assert_local_date(value);
    }

    /// Deleting two files with the same basename must not overwrite the first
    /// entry: the second gets a new name, and each info file names its own
    /// original.
    #[compio::test]
    async fn a_second_delete_of_one_name_does_not_overwrite_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let backend = local(dir.path());
        let first = dir.path().join("one/dup.txt");
        let second = dir.path().join("two/dup.txt");
        write(&first, b"one");
        write(&second, b"two");

        backend.remove(&first, Removal::Trash).await.unwrap();
        backend.remove(&second, Removal::Trash).await.unwrap();

        let root = trash_root(dir.path());
        assert_eq!(std::fs::read(root.join("files/dup.txt")).unwrap(), b"one");
        assert_eq!(std::fs::read(root.join("files/dup.txt.1")).unwrap(), b"two");

        let first_info = std::fs::read_to_string(root.join("info/dup.txt.trashinfo")).unwrap();
        let second_info = std::fs::read_to_string(root.join("info/dup.txt.1.trashinfo")).unwrap();
        assert!(
            first_info.contains(&format!("Path={}", encoded(&first))),
            "{first_info}"
        );
        assert!(
            second_info.contains(&format!("Path={}", encoded(&second))),
            "{second_info}"
        );
    }

    /// A directory is moved whole: its own info file, and everything beneath it
    /// unchanged.
    #[compio::test]
    async fn a_trashed_directory_keeps_its_contents() {
        let dir = tempfile::tempdir().unwrap();
        let backend = local(dir.path());
        let tree = dir.path().join("tree");
        write(&tree.join("sub/leaf.txt"), b"leaf");

        backend.remove(&tree, Removal::Trash).await.unwrap();

        assert!(!tree.exists(), "the tree leaves its old place");
        let root = trash_root(dir.path());
        assert_eq!(
            std::fs::read(root.join("files/tree/sub/leaf.txt")).unwrap(),
            b"leaf"
        );
        let info = std::fs::read_to_string(root.join("info/tree.trashinfo")).unwrap();
        assert!(info.contains(&format!("Path={}", encoded(&tree))), "{info}");
    }

    /// A trash that cannot be done is an error, never an unlink: this is the one
    /// failure the trait must not have a fallback for.
    #[compio::test]
    async fn a_failed_trash_does_not_delete_the_file() {
        let dir = tempfile::tempdir().unwrap();
        // A *file* where the trash directory should be: making files/info under
        // it fails, so the trash is impossible.
        let not_a_trash = dir.path().join("trash-is-a-file");
        write(&not_a_trash, b"in the way");
        let backend = Local::with_trash_root(&not_a_trash);
        let victim = dir.path().join("keep.txt");
        write(&victim, b"precious");

        let err = backend.remove(&victim, Removal::Trash).await.unwrap_err();
        assert!(!err.to_string().is_empty());
        assert!(
            victim.exists(),
            "a refused trash must not fall back to an unlink"
        );
        assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
    }

    /// A trash entry's `Path=` records where the file was; a relative input could
    /// only record where the process happened to be.
    #[compio::test]
    async fn a_relative_path_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let backend = local(dir.path());

        let err = backend
            .remove(Path::new("relative.txt"), Removal::Trash)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not absolute"), "{err}");
    }

    /// §6's `Path=` is an URL escape of the path's bytes: `/` stays literal,
    /// everything else that is not URL-safe becomes `%XX`.
    fn encoded(path: &Path) -> String {
        let mut out = String::new();
        for byte in path.as_os_str().to_string_lossy().bytes() {
            let bare =
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/');
            if bare {
                out.push(byte as char);
            } else {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
        out
    }

    /// `YYYY-MM-DDThh:mm:ss`: the length, the separators' places, and digits
    /// everywhere else. The time zone is local by design, so the value is not
    /// pinned to a clock.
    fn assert_local_date(value: &str) {
        assert_eq!(value.len(), 19, "{value} is not YYYY-MM-DDThh:mm:ss");
        for (index, separator) in [(4, '-'), (7, '-'), (10, 'T'), (13, ':'), (16, ':')] {
            assert_eq!(value.as_bytes()[index] as char, separator, "{value}");
        }
        for index in [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18] {
            assert!(value.as_bytes()[index].is_ascii_digit(), "{value}");
        }
    }
}

// ---- the in-memory backend -------------------------------------------------

/// Memory is the backend a write test or a preview can use without a disk: it
/// must do all of create/rename/copy/move/remove, and refuse a trash it cannot
/// have.
#[compio::test]
async fn the_memory_backend_can_write_rename_copy_and_remove() {
    let mem = Memory::new();
    mem.write("/home/lab/notes.txt", b"hello");
    mem.dir("/home/lab");

    mem.create_dir(Path::new("/home/lab/work")).await.unwrap();
    assert!(mem.stat(Path::new("/home/lab/work")).await.is_ok());

    let copied = mem
        .copy(
            Path::new("/home/lab/notes.txt"),
            Path::new("/home/lab/work/notes.txt"),
        )
        .await
        .unwrap();
    assert_eq!(copied, 5);

    mem.rename(
        Path::new("/home/lab/work/notes.txt"),
        Path::new("/home/lab/work/renamed.txt"),
    )
    .await
    .unwrap();
    assert!(
        mem.stat(Path::new("/home/lab/work/notes.txt"))
            .await
            .is_err()
    );
    assert_eq!(
        mem.stat(Path::new("/home/lab/work/renamed.txt"))
            .await
            .unwrap()
            .len,
        5
    );

    mem.move_to(
        Path::new("/home/lab/work/renamed.txt"),
        Path::new("/home/lab/moved.txt"),
    )
    .await
    .unwrap();
    assert_eq!(
        mem.stat(Path::new("/home/lab/moved.txt"))
            .await
            .unwrap()
            .len,
        5
    );

    // A conflict, and then a permanent delete.
    let err = mem
        .copy(
            Path::new("/home/lab/moved.txt"),
            Path::new("/home/lab/moved.txt"),
        )
        .await
        .unwrap_err();
    assert!(is_conflict(&err), "{err}");

    mem.remove(Path::new("/home/lab/work"), Removal::Permanent)
        .await
        .unwrap();
    assert!(mem.stat(Path::new("/home/lab/work")).await.is_err());

    // No trash: asked for one, it says so rather than deleting for good.
    let err = mem
        .remove(Path::new("/home/lab/moved.txt"), Removal::Trash)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("trash"), "{err}");
    assert!(
        mem.stat(Path::new("/home/lab/moved.txt")).await.is_ok(),
        "a refused trash must not delete"
    );
}
