# Writes, and the trash

The account behind `crates/dtr-vfs`'s write verbs and its freedesktop trash: what
the API promises, which parts of the specification are implemented and which are
not, and why a failed delete is an error rather than an unlink. `docs/manager.md`
§7 is the requirement ("everything goes to the trash", "an overwrite asks"); §8
says the transfer queue is what copies trees later.

## 1. The API

`Caps` grows two declarations:

```rust
pub struct Caps {
    pub watch: bool,
    pub dir_size: bool,
    /// create_dir, rename, copy, move_to, remove(Permanent).
    pub write: bool,
    /// whether Removal::Trash is real here.
    pub trash: bool,
}
```

`Backend` grows five methods, all `BoxFuture` (the futures are `!Send`, like
`stat`/`fetch`), all defaulting to the same refusal — the backend's id, the verb
and the path — so a read-only backend implements nothing:

```rust
fn create_dir(&self, path: &Path) -> Result<()>;
fn rename(&self, from: &Path, to: &Path) -> Result<()>;
fn copy(&self, from: &Path, to: &Path) -> Result<u64>;   // bytes written
fn move_to(&self, from: &Path, to: &Path) -> Result<()>;
fn remove(&self, path: &Path, how: Removal) -> Result<()>;

pub enum Removal { Trash, Permanent }
```

Semantics the trait promises:

- **A destination that exists is never replaced.** `rename`, `copy` and
  `move_to` return a `Conflict` and change nothing; `create_dir` treats an
  existing name the same way (a name collision, not an overwrite choice).
  `dtr_vfs::is_conflict(&err)` is how the UI recognises it and raises the one
  conflict card (replace / keep both / skip). `from == to` on a rename is a
  no-op.
- `create_dir` is one `mkdir`: the parent must exist.
- `copy` is whole-file. A directory is refused with a message that names the
  transfer queue (§8); a symlink source is followed by `std::fs::copy`, which is
  what a paste of the file's contents means.
- `rename`/`move_to` are the same primitive (on one machine a move is a
  rename); they exist because a UI says "Rename…" and "Move to…" differently. A
  cross-filesystem rename fails with the OS error; that is the queue's job too.
- `remove(Trash)` on a backend whose `Caps::trash` is false must **refuse**,
  never fall through to permanent. `remove(Permanent)` deletes recursively and
  is the one destructive verb: the confirmation belongs to the caller.

Backends: `Local` implements all five (`trash` on Unix, `write` everywhere);
`Memory` implements all five with `trash: false`; `Ssh` keeps both false and
implements nothing (the SFTP write codec is F7, and there is no remote trash ever
— §8 of `docs/manager.md`).

## 2. The specification, as read

[Trash Specification v1.0](https://specifications.freedesktop.org/trash/latest/)
(2014-01-02). The parts that shape the code:

- **§5, the trash directory.** A user's home trash is
  `$XDG_DATA_HOME/Trash`; files on `$XDG_DATA_HOME`'s own filesystem go there.
  For another device, `$topdir` is that filesystem's mount point, and the trash is
  either the administrator's `$topdir/.Trash/$uid` (only if `.Trash` is a real
  directory, not a symlink, with the sticky bit set) or our own
  `$topdir/.Trash-$uid`, created on demand. If `.Trash/$uid` cannot be created,
  fall back to `.Trash-$uid` immediately. If both fail, the implementation may
  copy into the home trash **or refuse** — it may not erase without confirmation.
- **§6, the contents.** `$trash/files/<name>` is the moved entry and
  `$trash/info/<name>.trashinfo` the metadata; the info file is created first,
  atomically (`O_EXCL`), which is what makes two simultaneous deletes of one name
  two entries. The info file is `[Trash Info]`, `Path=` (URL-escaped original
  location; absolute in the home trash, may be relative to `$topdir` in a
  top-directory trash) and `DeletionDate=` (`YYYY-MM-DDThh:mm:ss`, local time).
- **§7, the directory-size cache** (`$trash/directorysizes`) is a SHOULD for
  listing sizes. Not implemented: dtr neither lists nor sizes the trash yet, and
  a stale cache is worse than a `stat` per entry when it is read.
- **§9, implementation notes.** A trashed directory is moved whole; its nested
  names are not altered. Permissions/owners are preserved by the move (it is a
  `rename`).

## 3. Decisions

**The device rule is `$XDG_DATA_HOME`, not the trash directory.** The spec's
footnote is exact: home trash is for files "on the same filesystem as
`$XDG_DATA_HOME`". Comparing against the `Trash` directory would also work once it
exists, but the data directory's device is the question and it needs no
directory created to answer it. `device_of` walks up to the nearest existing
ancestor, so an uncreated `~/.local/share` is not a problem.

**`$XDG_DATA_HOME` must be absolute, and unset means `~/.local/share`.** The
base-directory specification says a relative value is to be ignored; a path
resolved against the process's working directory would put the trash wherever
dtr happened to be started. A broken value falls back rather than erroring.

**The top directory is the highest ancestor on the same device**, found by
walking `parent()` until the device changes. That is a mount point without
reading `/proc/mounts` or `statfs`, and it is the same answer for the common
case (`/home`, `/media/USB`, `/tmp`).

**Both top-directory methods are implemented, in the spec's order.** If
`$topdir/.Trash` passes the checks, its `$uid` subdirectory is used; a failure to
create that subdirectory falls back to `$topdir/.Trash-$uid` at once, without a
warning. The checks are the spec's: symlink → not usable even if a `$uid` copy
already sits inside; no sticky bit → not usable. A `.Trash-$uid` that is not a
directory is a failure, not a "already there".

**If both top-directory methods fail, trashing errors; it does not copy into the
home trash.** The spec permits either. Copying is a silent cross-device transfer
of an arbitrarily large tree with no progress and no cancel, and it can fill the
home partition when the user asked to free a USB stick — the UI can offer a
permanent delete instead, which is the confirmation the spec requires before
erasing. And no failure path ever unlinks.

**`Path=` is absolute for the home trash and relative to `$topdir` for a
top-directory trash.** §6 allows absolute paths only in the home trash and wants
relative ones from the directory the trash lives in; for `.Trash/$uid` and
`.Trash-$uid` "the directory containing the trash directory" is read as `$topdir`
(GIO and KDE resolve it that way, and any other base would need `..`, which the
spec forbids). A path that does not strip from `$topdir` (a bind-mount oddity) is
written absolutely: a reader can use it, and a refusal would lose the entry.

**The filename collision scheme is `name`, `name.1`, `name.2`, …** on the whole
original name — the info file's name is what a reader uses for the original
name, so the extension is not special. The pair is claimed by opening
`info/<name>.trashinfo` with `O_CREAT|O_EXCL`; a name is skipped if its
`files/<name>` already exists too (a broken trash can have one without the
other). Only after the info file is written is the entry `rename`d in; if that
move fails, the info file is removed again, because an info entry pointing at
nothing is the spec's "emergency case" shown in a desktop's trash view.

**`DeletionDate` is local time, formatted from `libc::localtime_r`.** The spec
asks for the user's local time, and the value is what a trash view shows the
person who deleted the file. `libc` is already a workspace dependency; the
alternative was a date crate for two calls.

**`Path=` escaping leaves URL path characters alone.** Alphanumerics and
`-._~/` stay literal; every other byte (space, `%`, newline, every byte of a
non-UTF-8 name) becomes `%XX` with uppercase hex. The spec's minimum is newline
and `%`; encoding more is valid, and it matches what GIO writes.

**Symlinks are handled as links.** `remove(Permanent)` and the trash both read
`symlink_metadata`, so a link is moved or unlinked as a link, never followed into
whatever it names (a `remove_dir_all` on a link's target would be the worst
possible bug). Trashing a directory moves the tree in one rename, so only the
top-level name gets an info file (§6).

**`Local::with_trash_root` is the test seam.** Tests pin the trash under their own
`tempfile` directory, so nothing touches `$HOME` or the real `$XDG_DATA_HOME`.
It also bypasses the mount choice on purpose: a `tempfile` directory is usually
on a different device from `$XDG_DATA_HOME`, and the spec's fallback would then
create `$topdir/.Trash-$uid` in the test tree (for `/tmp`, the real one). The
pin is documented on the constructor; production uses `Local::new`.

## 4. What the UI task must know

- Gate every verb on `Caps::write`; gate the trash button (rather than the
  permanent alternative) on `Caps::trash`. On Windows `Local::caps().trash` is
  false, so the delete affordance there is the permanent one plus its
  confirmation.
- Check `dtr_vfs::is_conflict(&err)` before showing any error toast: that answer
  means the destination exists and **nothing changed**, so the conflict card
  (replace / keep both / skip) is the next thing. Any other error is a failure
  (permissions, missing parent, cross-device rename, a backend that refused).
  `Conflict { path }` is public if the card wants the path out of the error.
- A permanent delete (Shift+Delete) must ask first; the trait will do it without
  a question if asked.
- A directory copy errors deliberately; route that to the transfer queue when
  §8 exists, and until then show the message it carries.
- Moving across machines is not `move_to`: it is read + write, and the queue's
  job (§8). `move_to` fails on another machine's `BackendId` by construction
  because only one backend is called.

## 5. Tests

`crates/dtr-vfs/tests/writes.rs` (plus pure-function tests in
`crates/dtr-vfs/src/trash.rs`):

- capabilities: `the_caps_say_what_each_backend_can_write`;
- the read-only default: `a_read_only_backend_refuses_each_verb_by_name`;
- `a_created_directory_is_there_and_a_second_is_a_conflict`,
  `a_rename_moves_the_name_and_the_bytes`,
  `a_rename_onto_an_existing_name_is_a_conflict_and_changes_nothing`,
  `a_copy_returns_its_bytes_and_never_replaces`,
  `a_copy_of_a_directory_is_refused_with_a_reason`,
  `a_move_to_another_directory_is_a_rename`,
  `a_permanent_delete_removes_a_file_dir_a_tree_and_never_trashes`;
- the trash: `a_trashed_file_leaves_the_original_and_lands_in_the_trash`,
  `the_trashinfo_names_the_original_path_and_deletion_time`,
  `a_second_delete_of_one_name_does_not_overwrite_the_first`,
  `a_trashed_directory_keeps_its_contents`,
  `a_failed_trash_does_not_delete_the_file`;
- the in-memory backend: `the_memory_backend_can_write_rename_copy_and_remove`;
- encoding, collision names and the date format:
  `a_path_is_percent_encoded_for_the_info_file`,
  `a_collision_gets_the_next_suffix_on_the_whole_name`,
  `the_deletion_date_has_the_spec_shape`.

The mount-selection branches (`.Trash` vs `.Trash-$uid`, the sticky-bit and
symlink checks) are not tested: a test cannot create a second filesystem, and
injecting a fake device would test the fake. They are the spec's checks, spelled
in `admin_trash` and `choose`; a future test could factor the choice over an
injected `(device, topdir)` if a real removable mount ever matters.

## 6. Gaps

- **No trash listing or restore.** This is the writer half; a browser over
  `$trash/info` is a later feature.
- **No Windows Recycle Bin.** The freedesktop spec is Unix's; on Windows
  `Caps::trash` is false and only the confirmed permanent delete exists. The
  shell's recycle-bin API is the Windows implementation when someone wants it.
- **No tree copy, no cross-device move.** Whole files here; `docs/manager.md`
  §8's queue is where a segmented copy, progress and cancel live.
- **No trash for `Ssh`.** `Caps::trash` is false forever (§8: "No trash over
  ssh"); remote writes are F7 and reuse these verbs once the codec has them.
- **No `directorysizes` cache, no undo stack.** Undo is §7's "worth having", not
  this milestone; the trash is what makes it possible.
