# A real file manager, and the desktop's file picker

The file pane becomes a file **manager** — a grid of large icons, places, multi-select, and
eventually writes — and the same browser is what the desktop asks for when an application wants a
file: an `org.freedesktop.impl.portal.FileChooser` backend of our own, in a daemon, so that picking a
file is a window that is already warm instead of GTK starting from nothing.

They are one project because they are one widget. The browser in a terminal pane, the browser in the
daemon's picker, and the browser in a manager window of its own are the same `dtr-files` view with a
different chrome and a different footer.

**Nothing in this memo is built.** §12 is the order — stages **F0…F7**, with F1 split as F1a and F1b —
and these are this programme's numbers, not the project's `M` milestones. `PLAN.md` §3 (`M4`) is the
same programme in a paragraph; the rest of this is the reasoning, the facts it rests on, and the things
worth measuring before anyone promises a number.

## 1. What is there now

| | today |
|---|---|
| `crates/dtr-vfs` | `stat` / `fetch` / `list`, and `Caps { watch, dir_size }`. **Read-only.** Local, in-memory, ssh. |
| `crates/dtr-sftp` | `connect`, `list`, `stat`, `home`, `download`, `close` — no mkdir, remove, rename or upload |
| `crates/dtr/src/explorer.rs` | one selection, keyboard only (arrows, `Enter`, `h`/`l`, `r`, `g`/`G`), 18-point painted rows, name and size, a synthetic `..`, dotfiles filtered with no toggle, no icons, no wheel, no row is a click target |
| the pane | a leaf in the split tree, so splits, focus, zoom, close and "a shell beside it" came free (`docs/panes.md`) |
| the local listing | **synchronous on the UI thread and does not use `dtr-vfs`**: `Explorer::reload` calls `std::fs::read_dir`, `metadata()` per entry, then sorts, in one frame. `dtr-vfs`'s batched `list` exists and only the remote path uses it |
| what a listing carries | `{ name, kind, len }`. The mtime is **dropped in both backends** even though both have it in hand — `Metadata::modified()` locally, `dtr_sftp::Entry.modified` remotely |
| the first click target | the close cross in the header, `f24ea1f` — and it needed an `Area` of its own *because* the pane is painted rather than clicked (`docs/files.md` §13) |
| `zbus` | already a dependency (`crates/dtr/src/system/linux.rs`), and `notify.rs`'s test already **serves** a D-Bus interface over a private bus |
| `rfd` | the workspace builds it with the `xdg-portal` feature, so dtr's own "Save as" is already a portal call |

Two numbers that shape the plan:

- **A cold dtr window is ~570 ms** to its first captured frame here, release build, on software Vulkan
  (lavapipe), doing terminal startup the picker would not do: config, store, shell, pty. Warm, a frame
  is a frame.
- **A theme icon lookup costs 11 ms cold and 0.5 ms warm** for a *miss* over every `applications/`
  directory (`70b9603`), so a grid must reuse that cache and add a size ladder rather than asking per
  cell.

## 2. The shape: the browser is a crate, so it can be three windows

Everything lives in the `dtr` **binary** crate, so no second executable can reach the listing, the skin
or the window, and a second copy of the window plumbing would be a second copy of the traps.

- **`crates/dtr` becomes lib + bin.** `main.rs` keeps `fn main` and the argument parsing; the modules
  move behind `pub mod`. Tests, snapshots and behaviour do not change.
- **`crates/dtr-files`** (new lib): the browser — model, grid and list views, places, icons,
  thumbnails, operations, and `Mode::{Manage, Pick}`. No winit, no libghostty, no terminal.
- **`crates/dtr-frame`** (new lib): the window, wgpu surface, egui context and skin install, lifted out
  of `app.rs`, and **multi-window from the start** — a `Vec` of windows over one event loop, one
  `egui::Context` each. This is where the traps go so that no window repeats them: DPI, surface loss
  and reconfigure, `process_viewport_commands`, repaint carrying.
- **`crates/dtr-portal`** (new bin): the daemon. It is *also* the manager window's process (§6), which
  is why it needs no second binary.

Two alternatives were considered and rejected. *One binary that grows a `--pick` mode* keeps the
daemon warm but means the picker's process is the terminal — 100 MB of scrollback, a shell, and a
dependency on a VT core for a dialog. *A fresh process per window* is the simplest thing that works and
throws away the only advantage the daemon has: a warm window.

## 3. The listing, and the grid

### The grid is the default view

The decision is Nautilus's default: a grid of large icons. That makes the view a **control**, not a
tweak to the row renderer, and the control belongs in `dtr-widgets` with a gallery snapshot, like every
other painted thing (`AGENTS.md`, "Do not hand-roll a widget").

- **`IconTile`** in `crates/dtr-widgets/src/widgets/`: a cell is an icon, a label of up to two elided
  lines, hover, selected and dimmed states, painted from `Palette` and `Style`.
- **Geometry as pure functions**, tested the way `arrange` and `first_visible` are:
  `columns(width, cell)`, `first_row(selected, scroll, rows)`, `index_at(point)`, and the cell width as
  a function of the icon size. A grid that computes its own columns in two places is the `pane_area`
  bug again — two calculations put the picture where the target is not (`AGENTS.md`, panes).
- **Keys change meaning.** In a grid `Left` is a movement, so "go to the parent" becomes `Backspace`,
  `Alt+Up` and the breadcrumb; `explorer::Key` grows `Right`, `PageUp`, `PageDown`, and
  `App::explorer_key` translates accordingly. `Home`/`End` stay ends, type-ahead still works.
- **Icons are three questions, in this order.** What the entry *is* (`inode-directory` → `folder`,
  MIME → a theme name: `text-x-generic`, `image-x-generic`, `application-pdf`, `text-x-script`, …); what
  the *packaging* says (`a.desktop` file's own `Icon=`, an executable's application icon, both already
  implemented for the new-tab menu in `system::linux`); and whether the desktop has decorated it (a
  directory listed in `user-dirs.dirs` is `folder-download` / `folder-documents` / `folder-pictures`,
  which is why `~/downloads` in Nautilus carries an arrow).
- **Thumbnails, not just icons.** `crates/dtr/src/thumb.rs` already decodes a picture off the drawing
  thread with the caps that matter (16 MiB source, 8192 px a side, 256 MiB decode) and caches by
  `{ machine, path }`. A grid cell is where that cache finally pays for itself: images and video get
  real previews, everything else gets its type icon.
- **The size ladder.** `in_theme` currently asks for 64 px first because a menu row is 18 points. A
  grid wants 96 or 128, so the ladder becomes a parameter with the same first-match rule.
- **The list view ships in the same milestone, not later.** A pane is often 450 points wide, where a
  96-point tile gives two or three columns — six items where ten rows fit. Grid is the default; one
  toggle (`Ctrl+1`/`Ctrl+2`) is the escape hatch, and it is the existing renderer, kept rather than
  rewritten.
- **Also in F1, because a grid without them is a picture**: click to select, double-click to enter or
  open, wheel to scroll, drag to select, `Ctrl`/`Shift` to extend, a right-click menu (open, open with,
  copy path, reveal), `Ctrl+H`, a breadcrumb that navigates, and a places **popup** — the sidebar is
  window chrome (§6), a pane is too narrow for one. Sorting and the selector are below.
- **A local directory is watched** (`Caps::watch` already exists): `notify` on the directory in view,
  coalesced into one re-list per burst. The ssh backend keeps saying it cannot, and the UI keeps hiding
  what a backend cannot do.

### Streamed

**A listing arrives in batches and the browser shows the first batch immediately.** `Backend::list`
already returns `Batches = Receiver<Result<Vec<Entry>>>` and both backends send 64 at a time, so the
protocol for this exists and only the pane's local path ignores it (it reads the whole directory and
sorts inside one frame). Three rules make streaming survivable:

- **The view is a prefix of the arrival order until the walk ends**, and the footer says so
  (`reading…` with a count) rather than `empty directory`. A streamed listing must never render its
  emptiness as a fact; the current footer does.
- **The selection is keyed by name, never by index.** Batches arriving and rows re-sorting while
  somebody holds an arrow key make `selected: usize` a reference to whatever is at that position
  *now*. `Explorer::selected` becomes a `Selection` over entry ids (name, and the backend, for two
  panes on one host) and the same applies to the scroll anchor: scrolling is anchored to the row the
  reader was looking at, so a batch landing above it cannot scroll the page under their eyes.
- **One repaint per burst.** Batches coalesce behind a 16 ms gate before the view is published — 157
  batches of a 10k directory must not be 157 repaints, which is the same rule `publish` already
  applies to read chunks (`AGENTS.md`, panes).

### Lazy

Lazy here means *lazy metadata*, and it is the difference between one syscall per file and none:

- **A directory read yields names and kinds for free.** `DirEntry::file_type()` reads `d_type` out of
  the directory entry, with no `stat` — *where the filesystem provides it*: some (network
  filesystems, notably) return `DT_UNKNOWN` and std then falls back to an `lstat` itself, so the
  saving is real on ext4/xfs/btrfs and absent elsewhere. `crates/dtr-vfs/src/local.rs`'s `walk`
  calls `entry.metadata()` for *every* entry today, which is one `stat` per file — 10 000 of them for
  a 10 000-file directory, and on a cold cache that is the listing's whole cost.
- **A symlink is resolved in the first batch, not the pass.** `d_type` says "symlink", not what it
  points at, so a link to a directory would arrive as a file and *change groups* when the metadata
  pass resolved it — a row jumping from the files group to the folders group is exactly the visible
  glitch the rest of this section is trying to avoid. One `lstat` per symlink (they are rare) in the
  first batch, and the kind a row is given is the kind it keeps.
- **A size and a time are `Option`s, filled later.** `vfs::Entry` grows
  `len: Option<u64>, modified: Option<SystemTime>` (from `u64` and nothing today), and a backend may
  send a name it already sent, refined: **a later batch patches a known name rather than duplicating
  it**, keyed by name. One channel, one type, and the model merges.
- **The metadata pass runs only when something needs it** — and that is the load-bearing rule. A
  **name** sort needs no metadata at all, so the common case is a `readdir` and a sort, with stats
  taken only for rows on screen (about forty) to draw the size and date columns. A modified or size
  sort needs the whole directory's stats, so it starts a full pass, prioritized to the visible
  window so the first screen is right immediately.
- **Remote is the opposite case and worth stating**: SFTP's `READDIR` already carries size and mtime
  in the same packet (`dtr_sftp::Entry.modified`), so a remote listing is complete as it streams and
  a remote sort costs no extra round trips. Lazy metadata is a *local* problem, and the code must not
  grow a second pass that a server already answered.

### Sorted

Four keys — **name, modified, size, type** — and two independent switches: **reverse**, and **folders
first** (on by default, as in Nautilus). Arrival order is not one of them: it is what the view shows
while a walk is still running, and a directory whose `readdir` order matters is what a shell is for.

- **Sorting is in the model, not in the view.** `arrange(entries, cwd, &View)` where
  `View { sort, reverse, folders_first }`; the grid and the list both order from the same `Vec`, so a
  toggle cannot make them disagree, and it is a pure function the existing tests extend.
- **Reversing flips the key, not the grouping.** Directories stay first under `modified ↓`; only the
  order *within* each group reverses. A test for exactly that, because "reverse" that also moves
  folders to the bottom is the bug everyone ships once.
- **Unknowns sink, in both directions.** An entry with no mtime — a permission denied, a server that
  reports none — sorts **last** whether ascending or descending. Sorted descending by date with the
  unknowns first is a list of the files you cannot read, presented as the newest. Same for a size
  sort's directories: `Caps::dir_size` is false, so a directory's size key is `None` and it sinks
  rather than claiming its inode size.
- **`..` is always first.** It is synthetic and inserted after sorting, which is what the function
  does today; a reverse sort must not sink it.
- **The comparator allocates on every comparison.** The current one calls `to_lowercase()` on **both**
  names per comparison — two `String`s, `O(n log n)` times. On 10 000 generated names that measured
  **2.77 ms against 1.10 ms** for a precomputed key: real, and gone with `sort_by_cached_key` or a
  lowercased key computed once per entry. A stable sort still decides the equal-ignoring-case tie.
- **A metadata sort says it is working.** Choosing *modified* starts the pass and the control reads
  `modified ↓ (reading…)` until the directory's stats are all in, then the order is applied **once**.
  Re-sorting as each batch lands would shuffle rows under the pointer continuously, which is worse
  than a second of the previous order. If the pass was already done for this directory it is cached
  until a watch event invalidates it.
- **Show the key you sort by.** A *modified* sort with no visible dates is a mystery: the list view
  gains a Modified column, the grid shows it in the hover tooltip and the footer, and the footer
  names the selection's date the way it names its path today.
- **The selector is a control, twice.** In the manager window it is a toolbar dropdown: the four keys
  with a tick on the active one, then *Reverse* and *Folders first*. In a pane it cannot be a
  dropdown in the flow of a painted header — every pointer target there is its own `Area`
  (`docs/files.md` §13) — so the pane gets **one** sort area in the header (a glyph that opens the
  same menu as a popup) and the same actions on the keyboard, offered in the palette. One menu
  builder, two windows.
- **The order is remembered.** `[files] sort = "modified"`, `reverse = true`, `folders-first = true`,
  `view = "grid"` in the config file, applied on load and on reload like every other key
  (`AGENTS.md`, "a new key is a key a reload applies"). Per **folder** memory — Nautilus remembers
  that you sort `~/Downloads` by date and `~/src` by name — is a store schema and a second writer, so
  it is deliberately not in F1.

## 4. Pick mode

The same browser with a different footer, and it is the *reason* the read-only milestone is worth
shipping on its own: a picker only ever reads.

- `Mode::Pick(Pick)` where `Pick` carries what the caller asked for: files or a directory, one or many,
  a suggested name for save, the filters, the accept label, the starting folder.
- The footer is `Cancel` / `<accept_label>`, with the name field in save mode and an overwrite
  confirmation — not a silent replace.
- Filters are the portal's `a(sa(us))` (`*.[ch]`, `*.png`, MIME patterns) matched against the name, and
  the chosen filter decides the extension appended in save mode.
- Refusals are said out loud in the footer: a directory that cannot be read, a save whose parent is not
  writable. A picker whose button does nothing is the one outcome that must not happen (`docs/files.md`
  §8 is the same rule for the platform dialog).

## 5. The daemon

The interface, read from the installed `xdg-desktop-portal` 1.22.1 XML
(`$datadir/dbus-1/interfaces/org.freedesktop.impl.portal.FileChooser.xml`) rather than remembered:

```
org.freedesktop.impl.portal.FileChooser
  OpenFile (o handle, s app_id, s parent_window, s title, a{sv} options) → (u response, a{sv} results)
  SaveFile (…same…)   SaveFiles (…same…)   [1.22.1: the file list is in options, not an argument]
org.freedesktop.impl.portal.Request at `handle`:  Close ()
```

`response` 0 is a choice and 1 is a cancel; `results` carries `uris` as `file://` URIs, percent-encoded
from **bytes** because a path need not be UTF-8, plus `current_filter` and `writable` where they apply.
The options to honour are `multiple`, `directory`, `filters`, `current_filter`, `accept_label`,
`current_folder` (a NUL-terminated `ay`), `current_name`, `current_file`, `modal`.

- **Transport: `zbus` with the `async-io` backend**, which is already in the tree for the settings
  portal and is runtime-agnostic — no tokio, no GTK. The method is `async` and returns when the user
  answers; the request object at `handle` is what lets the caller `Close` it.
- **Threads**: a D-Bus thread owns the connection, the UI thread owns the windows, the two exchange
  messages over a channel. Nothing in the bus path may block on a `read_dir`.
- **Lifecycle**: D-Bus activated, **one window per request, kept hidden between requests**, idle-exit
  after a timeout when it owns no visible window. The first picker pays for a process; every later one
  is a frame.
- **Packaging** is three text files, all searched in `$XDG_DATA_DIRS`, so a `~/.local/share` trial needs
  no system change:

  ```
  share/xdg-desktop-portal/portals/dtr.portal
      [portal]
      DBusName=org.freedesktop.impl.portal.desktop.dtr
      Interfaces=org.freedesktop.impl.portal.FileChooser
      UseIn=gnome

  share/dbus-1/services/org.freedesktop.impl.portal.desktop.dtr.service
      [D-BUS Service]
      Name=org.freedesktop.impl.portal.desktop.dtr
      Exec=…/bin/dtr-portal

  ~/.config/xdg-desktop-portal/portals.conf
      [preferred]
      default=gnome
      org.freedesktop.impl.portal.FileChooser=dtr
  ```

  The `portals.conf` resolution order is `$XDG_CONFIG_HOME`, `$XDG_CONFIG_DIRS`, the build-time
  `sysconfdir`, then the data dirs (`portals.conf(5)`), and a key may name a semicolon-separated
  fallback list, `none` and `*` being special. The flake grows a `nixosModules.dtr-portal` that sets
  `xdg.portal.config` and `xdg.portal.extraPortals` instead of asking anyone to copy files by hand.
- **`parent_window`**: `x11:0x…` becomes transient-for and modal. On Wayland the handle is opaque and
  winit exposes no xdg-foreign, so the window is **not** transient there. That is a limitation written
  down, not a lie told by a flag.

### Why it is not slow

The GNOME backend is `xdg-desktop-portal-gnome` (51.0 here), and it links GTK: GDK, the accessibility
bridge, the theme and settings machinery, and its own file-chooser widget, which it builds for each
request. The frontend D-Bus-activates that process, so the first picker of a session pays for all of
it and the ones after it pay for the dialog. Ours does none of that: no GTK, no GVfs, no Nautilus
model, no thumbnailer daemons, no chooser widget tree built per request, and the listing is read on a
worker so the window never waits for the disk. Measured cold starts and the comparison are §10 — until
they exist, "faster" is a hypothesis.

## 6. The manager window

`dtr-files [PATH]` is the same binary as the daemon, and the same process: it asks the daemon (a
private `org.dtr.Files.OpenWindow`) to open a manager window, and starts the daemon if it is not
running. So a picker and a manager window share a process, a warm wgpu device and the icon cache, and
`dtr-files` costs nothing extra in packaging.

What the window adds over the pane is chrome, and only chrome: a toolbar (back/forward/up, breadcrumb
or an editable location, search/filter, view toggle, menu), a **sidebar** of places — the XDG user
directories, the root, real mounts from `/proc/mounts`, and **this session's ssh hosts** with their
home directories (the SFTP client already answers `home()`), and a status bar with counts and the
selection's size. The pane keeps the breadcrumb and a places popup instead of a sidebar.

Nautilus's window is the reference for behaviour, not for pixels: `dtr-widgets` paints from the skin,
and `Preset::Native` already reads the desktop's palette without linking GTK.

## 7. Writes

The half that can lose data, so it comes last and with rules rather than defaults:

- `Backend` grows `create_dir`, `rename`, `remove`, `copy`, `move`, and `Caps` grows
  `write`/`trash`. `Caps` is what the UI reads, so a backend without them hides the verbs.
- **Everything goes to the trash.** Trash, not `unlink`: the freedesktop trash spec for local files (a
  `.trashinfo` file and the right trash directory per mount); `Shift+Delete` is a permanent delete with
  a confirmation, and nothing else is permanent.
- **An overwrite asks.** A paste that would replace, a rename onto an existing name, and a copy whose
  destination exists are one `Card` with the same three answers (replace, keep both, skip), not three
  dialogs.
- **`dtr-sftp` grows mkdir/rmdir/rename/setstat/upload** before any remote write appears; a remote
  "delete" that cannot be undone is refused with a toast instead of emulated.
- Copy and move — inside one machine or across two — go through the queue in §8 rather than through
  a second mechanism that happens to know a byte count.
- Undo is worth having for trash, rename and move — a stack of the last N operations — and is *not* in
  the milestone that first makes destroying possible.

## 8. The transfer queue, and no mount

**Decided: dtr does not mount a remote filesystem, and the transfer queue is the layer that
replaces what a mount would have given.** No FUSE, no WinFsp, no macFUSE. Four reasons, in the order
they matter:

- A mount is a **per-OS kernel dependency**: FUSE is in the Linux kernel and free, Windows needs
  WinFsp (a driver install) plus SSHFS-Win, and macOS needs macFUSE as a system extension the user
  approves. dtr's single-`.exe` rule (PLAN §7) exists to avoid exactly that; the file manager must not
  be the thing that breaks it.
- A mount is **stateful in the file manager's face**: a stale mount blocks `stat` for seconds, the
  path (`$XDG_RUNTIME_DIR/gvfs/sftp:host=…,user=…/path`) leaks into anything handed to another app,
  and there is now a second connection manager beside the user's ssh.
- **The owner's experience of GVfs is that it is buggy**, which is a reason to own the layer rather
  than delegate it, not a reason to rebuild it.
- **A terminal already has the other program at the far end.** The mount's headline benefit is "other
  apps can open the remote file as a path"; `ssh host vim /srv/x.conf` edits it where it is local,
  while a mount downloads it, lets a local editor write a snapshot and uploads it back.

What GVfs actually is, since the comparison is worth being exact about: a `GFile` D-Bus layer, one
`gvfsd-<scheme>` daemon per protocol, and `gvfsd-fuse`, which exposes the result as a path for
programs that are not GIO. **We already have the middle layer.** The installed backend (`gvfs`
1.62) is not a libssh2 client either — it spawns the user's `ssh` with the `sftp` subsystem and
`-oControlMaster`/`-oControlPath`, i.e. the same transport decision as `docs/sftp.md` §1 and the same
multiplexing trick as its §3.4. The only layer we lack is the FUSE one, and it is the one that does
not port.

### The queue

One queue **per window** — the window a pane is in, or the manager window — owned by the app rather
than by a pane: a job outlives the pane that started it, and closing a pane must not silently abandon
a transfer. What Nautilus calls its file-operations
window is not a mount feature; it is a copy/retry/cancel feature, and local copies need it just as
much as remote ones.

- **A job** is: the sources (`Location`), the destination (`Location`), the direction
  (upload/download/local copy/move), the byte total where `stat` knows it, the bytes done, the state,
  and the answers it already has (replace / keep both / skip, from the one conflict `Card` §7 names).
- **Per-alias serialisation is by construction**: `crates/dtr-vfs/src/ssh.rs` keeps one thread, one
  `ssh` child and one `async_lock::Mutex<Option<Session>>` per alias, and an operation holds that
  mutex for its whole duration. So a whole-file `fetch` of a big file **stalls browsing that host**
  for as long as it runs, and a cancel can only land when the operation ends. The transfer therefore
  has to be **a sequence of segments** — open, read or write one chunk, close or keep — each taking
  the mutex, so a listing interleaves and a cancel is answered between chunks. Chunk size is bounded
  by the codec's 30 s per-answer deadline, not by taste.
- **Across aliases it is parallel, within one it is serial.** Two hosts in one queue run at once; ten
  files to one host are one stream, which is what the protocol is.
- **A cross-host copy streams through dtr**: SFTP v3 has no server-to-server copy, and OpenSSH's
  `copy-data` extension copies within one server only, so A → B is read A, write B, in one job with
  one progress figure.
- **A cancelled or failed transfer leaves no half file under the real name.** Write to
  `<name>.part` (or a temp in the destination directory), then publish with
  `posix-rename@openssh.com` where the server has it and a plain rename otherwise; on cancel, remove
  the part. A file that *looks* complete but is a truncated copy is worse than a failure.
- **UI**: a card listing jobs with a progress stripe, cancel and retry, and the tab's own stripe
  driven by the same figure — `dtr_widgets::Stripe` and `ActivityState.percent` already do exactly
  this for a tab's build, so the queue reuses the control instead of painting a second kind of bar.
- **What the codec still needs**, none of it v4: `SSH_FXP_WRITE`, `OPEN` with
  `FXF_WRITE|FXF_CREAT|FXF_TRUNC`, `MKDIR`, `RMDIR`, `REMOVE`, `RENAME`, `SETSTAT`, and optionally
  `FXF_APPEND` for resuming a `.part`; `posix-rename@openssh.com` and `statvfs@openssh.com` (free
  space, for the "not enough room" that a mount would have failed on by itself) as extensions.
- **No trash over ssh.** `Caps::trash` stays false for a remote backend and the UI hides the verb:
  a delete that cannot be undone needs the permanent-delete confirmation, not a fake trash.

## 9. What this is not

- **Not GTK, and not a GVfs front end.** No mounts (§8), no MTP, no smb; browsing another machine is
  the ssh session you already have.
- **Not an indexer.** Search filters the directory in view; a recursive search is a terminal's `find`
  and can wait until someone misses it.
- **No thumbnailer daemons and no tag/star databases.**
- **No tabs inside the browser.** dtr's tab strip is terminal tabs; a browser lives in a pane or a
  window, and a tab that is one thing in one strip and another in the next is worse than no tabs.
- **No portal on Windows.** `rfd`'s `IFileDialog` is the platform's answer and the single-exe story has
  no D-Bus; the manager window can still be built there.

## 10. The measurements to take first

1. **The GNOME picker's time-to-dialog.** A fake "app" calls
   `org.freedesktop.portal.FileChooser.OpenFile` on the real desktop and times the moment a window
   appears. This is the number the daemon exists to beat and nobody has taken it.
2. **Ours, cold and warm**, and split into its parts: process and window up, first frame, directory
   read. The 570 ms in §1 is a release build on software Vulkan doing terminal startup; the daemon does
   none of that, and the number should say so rather than inherit it.
3. **A 10 000-entry directory**, local and over SFTP: does the grid batch, and does the drawer need to.
   `docs/bench.md`'s harness is the place.
4. **A grid frame's icon cost**: how many theme lookups a screenful asks for, and what the cache does to
   the second frame. The 11 ms / 0.5 ms figures in §1 are the order of magnitude to expect.
5. **A transfer against browsing**: how long a `stat` on the same alias waits while a 100 MB copy is
   running, before and after the transfer is segmented (§8). That number is what decides the segment
   size, and whether a second connection per transfer is worth having on Windows, where `ControlMaster`
   does not exist.
6. **The lazy pass, measured**: a 10 000-entry directory with `readdir` and `d_type` alone, then with
   one `stat` per entry — warm and cold — because that is the difference §3's *Lazy* section claims.
   Then the same directory over SFTP, where the attributes are already in the `READDIR` replies and
   the pass should cost nothing. Half-taken: the comparator alone was **2.77 ms against 1.10 ms** on
   10 000 generated names (`to_lowercase` per comparison vs a cached key), so the sort itself is not
   the cost — the pass is.
7. **Rows drawn per frame against rows streamed in**: that batches coalesce to one repaint each
   (§3 *Streamed*), i.e. a 157-batch listing produces about a frame's worth of repaints and not 157.

## 11. Risks

- **Losing data** is the one that matters: trash-always, ask-on-overwrite, never a verb whose failure
  is silent, and a cancelled transfer that leaves `.part` rather than a short file with the real name.
  §7 and §8 are the rule lists.
- **A transfer that starves its own host**: one session and one mutex per alias means a whole-file
  copy blocks every `stat` and listing on that machine for its duration if the queue is not
  segmented (§8, measurement 5). This is the design detail most likely to be skipped and most likely
  to be felt.
- **A resident daemon's memory**: a wgpu device and an egui context per window, held while hidden. The
  idle timeout is the answer; it is also the reason the window is hidden rather than destroyed.
- **Multi-window in egui** is where a new class of bug lives (a context per window, DPI per window,
  surface loss per window, focus and IME routing). `dtr-frame` exists so that this is solved once.
- **Grid density in a narrow pane** — the list toggle is the mitigation, and it must ship together with
  the grid rather than after it.
- **Index-based selection under a re-sorted stream** is the bug that streaming and sorting combine to
  create: an index means "whatever is at that position now", so a batch landing or a key changing
  moves the highlight to a different file, and a rename or a delete acts on the wrong one. The
  selection must be keyed by name before either ships (§3 *Streamed*).
- **The metadata pass is the listing's real cost**, not the sort (2.77 → 1.10 ms measured on 10 000
  names, against 10 000 `stat`s). A design that stats eagerly "because sorting might need it" pays
  that on every directory, which is why §3 *Lazy* makes the pass conditional on the sort in use.
- **Portal compatibility in the corners**: `SaveFiles` (whose arguments changed), `choices`, filters
  whose MIME patterns are not globs, and apps that ask for a directory with `multiple`. The private-bus
  test should cover the wired shapes, including a `Close` while the dialog is up.
- **Apps that never ask** — GTK4 without `GTK_USE_PORTAL`, most Qt — cannot be served by any backend.
  Worth saying in the README before someone measures the wrong thing.

## 12. The order

| | | the gate |
|---|---|---|
| **F0** | `dtr` → lib + bin; `dtr-frame` (multi-window); `dtr-files` skeleton holding today's listing; measurements 1–2 | every existing test and snapshot green, unchanged; two numbers written into this memo |
| **F1a** | the listing: streamed batches from `dtr-vfs` for local too (off the UI thread), a name-keyed selection and scroll anchor, `Option`al `len`/`modified` with patch-on-redirect, the conditional metadata pass, and the four-key sort with reverse and folders-first in `arrange` | pure-function tests: reverse keeps folders first, unknowns sink both ways, `..` stays first, equal-ignoring-case is stable; a test that a second batch patches rather than duplicates; measurement 6 |
| **F1b** | the grid, read-only, in the pane: `IconTile` + gallery snapshot, grid geometry, icons, thumbnails, multi-select, 2-D keys, breadcrumb, places popup, sort selector, `Ctrl+H`, watch, list toggle | kittest snapshots of a grid, a list, a hover and a selection; pure-function tests for columns and hit-testing; measurement 7 |
| **F2** | pick mode: footer, multiple, directory, save-with-name, filters, overwrite confirmation | a snapshot per mode; the picker's answers unit-tested without a window |
| **F3** | `dtr-portal`: zbus backend, activation, resident hidden window, the three packaging files, the NixOS module, private-bus test | a real `OpenFile` answered over a private bus; measurement 3 |
| **F4** | the manager window: `dtr-files [PATH]`, private `org.dtr.Files.OpenWindow`, toolbar, sidebar with places and ssh hosts, status bar | the same browser in a third window, and a snapshot of the window |
| **F5** | the transfer queue (§8): jobs, per-alias segments, progress, cancel, retry, `.part` then publish, the tab stripe, the queue card — with local copies as its first user | a cancelled copy leaves nothing under the real name; a listing on the same alias answers while a transfer runs (measurement 5) |
| **F6** | writes on this machine: VFS verbs and caps, trash, mkdir, rename, the conflict card | tests that a delete goes to the trash and an overwrite asks |
| **F7** | writes over ssh: `dtr-sftp` writing packets and the rename/statvfs extensions, upload, a cross-host copy streamed through dtr | the codec's own tests, against the corpus host |

F0's gate is the important one: if the split cannot leave every test and snapshot untouched, the rest
of this is being built on a refactor nobody can trust.

F1 is split in two because the listing is where the correctness lives and the grid is where the look
does: streaming plus sorting makes an index-based selection wrong, so **F1a changes the model with
nothing new on screen**, which is the only way to test it honestly. F1b may then assume a listing that
is already complete, ordered and keyed by name.
