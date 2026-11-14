# A real file manager, and the desktop's file picker

The file pane becomes a file **manager** — a grid of large icons, places, multi-select, and
eventually writes — and the same browser is what the desktop asks for when an application wants a
file: an `org.freedesktop.impl.portal.FileChooser` backend of our own, in a daemon, so that picking a
file is a window that is already warm instead of GTK starting from nothing.

They are one project because they are one widget. The browser in a terminal pane, the browser in the
daemon's picker, and the browser in a manager window of its own are the same `dtr-files` view with a
different chrome and a different footer.

**Nothing in this memo is built.** §11 is the order — stages **F0…F6**, which are this programme's
numbers and not the project's `M` milestones — and `PLAN.md` §3 (`M4`) is the programme in a paragraph.
The rest is the reasoning, the facts it rests on, and the things worth measuring before anyone promises
a number.

## 1. What is there now

| | today |
|---|---|
| `crates/dtr-vfs` | `stat` / `fetch` / `list`, and `Caps { watch, dir_size }`. **Read-only.** Local, in-memory, ssh. |
| `crates/dtr-sftp` | `connect`, `list`, `stat`, `home`, `download`, `close` — no mkdir, remove, rename or upload |
| `crates/dtr/src/explorer.rs` | one selection, keyboard only (arrows, `Enter`, `h`/`l`, `r`, `g`/`G`), 18-point painted rows, name and size, a synthetic `..`, dotfiles filtered with no toggle, no icons, no wheel, no row is a click target |
| the pane | a leaf in the split tree, so splits, focus, zoom, close and "a shell beside it" came free (`docs/panes.md`) |
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

## 3. The grid is the default view

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
  copy path, reveal), a sort menu (name/size/modified, directories first), `Ctrl+H`, a breadcrumb that
  navigates, and a places **popup** — the sidebar is window chrome (§6), a pane is too narrow for one.
- **A local directory is watched** (`Caps::watch` already exists): `notify` on the directory in view,
  coalesced into one re-list per burst. The ssh backend keeps saying it cannot, and the UI keeps hiding
  what a backend cannot do.

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
worker so the window never waits for the disk. Measured cold starts and the comparison are §9 — until
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
- Copy and move inside one machine are streaming (`dtr-vfs` `fetch` writes to a destination already);
  cross-machine drag is a stream with progress and cancel, on the same worker that lists.
- Undo is worth having for trash, rename and move — a stack of the last N operations — and is *not* in
  the milestone that first makes destroying possible.

## 8. What this is not

- **Not GTK, and not a GVfs front end.** No network mounts, no MTP, no smb; browsing another machine is
  the ssh session you already have.
- **Not an indexer.** Search filters the directory in view; a recursive search is a terminal's `find`
  and can wait until someone misses it.
- **No thumbnailer daemons and no tag/star databases.**
- **No tabs inside the browser.** dtr's tab strip is terminal tabs; a browser lives in a pane or a
  window, and a tab that is one thing in one strip and another in the next is worse than no tabs.
- **No portal on Windows.** `rfd`'s `IFileDialog` is the platform's answer and the single-exe story has
  no D-Bus; the manager window can still be built there.

## 9. The measurements to take first

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

## 10. Risks

- **Losing data** is the one that matters: trash-always, ask-on-overwrite, and never a verb whose
  failure is silent. §7 is the rule list.
- **A resident daemon's memory**: a wgpu device and an egui context per window, held while hidden. The
  idle timeout is the answer; it is also the reason the window is hidden rather than destroyed.
- **Multi-window in egui** is where a new class of bug lives (a context per window, DPI per window,
  surface loss per window, focus and IME routing). `dtr-frame` exists so that this is solved once.
- **Grid density in a narrow pane** — the list toggle is the mitigation, and it must ship together with
  the grid rather than after it.
- **Portal compatibility in the corners**: `SaveFiles` (whose arguments changed), `choices`, filters
  whose MIME patterns are not globs, and apps that ask for a directory with `multiple`. The private-bus
  test should cover the wired shapes, including a `Close` while the dialog is up.
- **Apps that never ask** — GTK4 without `GTK_USE_PORTAL`, most Qt — cannot be served by any backend.
  Worth saying in the README before someone measures the wrong thing.

## 11. The order

| | | the gate |
|---|---|---|
| **F0** | `dtr` → lib + bin; `dtr-frame` (multi-window); `dtr-files` skeleton holding today's listing; measurements 1–2 | every existing test and snapshot green, unchanged; two numbers written into this memo |
| **F1** | the grid, read-only, in the pane: `IconTile` + gallery snapshot, grid geometry, icons, thumbnails, multi-select, 2-D keys, breadcrumb, places popup, sort, `Ctrl+H`, watch, list toggle | kittest snapshots of a grid, a list, a hover and a selection; pure-function tests for columns and hit-testing |
| **F2** | pick mode: footer, multiple, directory, save-with-name, filters, overwrite confirmation | a snapshot per mode; the picker's answers unit-tested without a window |
| **F3** | `dtr-portal`: zbus backend, activation, resident hidden window, the three packaging files, the NixOS module, private-bus test | a real `OpenFile` answered over a private bus; measurement 3 |
| **F4** | the manager window: `dtr-files [PATH]`, private `org.dtr.Files.OpenWindow`, toolbar, sidebar with places and ssh hosts, status bar | the same browser in a third window, and a snapshot of the window |
| **F5** | writes on this machine: VFS verbs and caps, trash, mkdir, rename, copy/move, the conflict card | tests that a delete goes to the trash and an overwrite asks |
| **F6** | writes over ssh: `dtr-sftp` mkdir/rmdir/rename/upload, cross-machine copy with progress and cancel | the codec's own tests, against the corpus host |

F0's gate is the important one: if the split cannot leave every test and snapshot untouched, the rest
of this is being built on a refactor nobody can trust.
