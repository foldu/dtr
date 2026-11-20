# Files, links, and previews

The account behind the "Files, links, and panes" bullets in
[AGENTS.md](../AGENTS.md): clicking a path a program printed, previewing it, saving it, and
the rules that keep a remote path from being used as a local one. `docs/sftp.md` is the
decision memo for the transport itself; this is what the UI and the VFS layer each own.

The whole section turns on one idea: **the pane is the only thing that knows which machine a
path is on**, because a path arrives as text from a program that may be running anywhere.

## 1. A `Location` comes from the pane, not from a URI

`dtr-vfs` has `Location { backend, path }`, a `Backend` trait with `stat`, `fetch` and
`list`, and local, in-memory and ssh implementations. `BackendId::Ssh` is only ever built
for a session that *is* remote, and `Vfs::backend` answers an ssh alias by opening a backend
for it — never by falling back to local, because that fallback would silently open a
*different file of the same name* on this machine, which is worse than an error.

`file:///etc/hosts` printed in a session on `jupyter` means jupyter's `/etc/hosts`. There is
no such thing as resolving it locally.

Listings are **batches**, because `readdir` answers in chunks and a directory can be huge;
dropping the receiver cancels the walk. Capabilities are **declared** — no remote watch, no
directory size — and the UI hides what is not there rather than greying out a lie.

## 2. The host part decides which machine, and `file://blah` usually has one

The first version accepted only empty and `localhost`, on the theory that a hyperlink without
a host is local. Then `ls --hyperlink=always` turned out to print
`file://<hostname>/path` — the machine's own name, not `localhost` — so the rule refused every
link `ls` has ever printed.

The rule now lives in `crate::uri`, shared with the OSC 7 sniffer, because both answer the
same question: a **local** session resolves a path only when the host is empty, `localhost`
or this machine's own name, and names the other host in the toast otherwise; a **remote**
session resolves every `file:` path on its own host, because that is where it is.

## 3. A hyperlink is asked about, not carried in the frame

OSC 8 lives in the terminal, and the terminal never leaves the pane thread. Resolving it
during publish would be ~8 ns of FFI per cell — for a link nobody is pointing at — so
`Command::HyperlinkAt { x, y }` answers into a slot instead.

The reply carries the cell that was asked about, so an answer that arrives a frame late is
dropped rather than opening the link under the pointer *now*, which is a different one.

Only `file:` is understood. It is percent-decoded, `stat`ed through the VFS, and handed to
`xdg-open`, `open` or `explorer` — **never a shell**, because the path comes from a program's
output. Another scheme is said out loud in a toast; silently ignoring a link reads as a bug in
the click.

A press on a link opens it and starts no drag, because a drag would be a selection and the
press has already been spent.

## 4. A directory link opens a file pane, not a toast

The worker sends `UserEvent::Reveal` and the **UI** decides where a pane may go — layout is
not a worker's business — and focus stays on the terminal, because the person clicked a path
to look at something, not to leave their shell.

A pane already browsing the *same machine* is retargeted; another machine is another pane,
because a pane reading this disk cannot become one reading somebody else's.

## 5. A hovered picture is decoded off the drawing thread and capped before it is read

16 MiB of source, 8192 pixels a side, 256 MiB of decode. The extension only decides whether to
*try*, so a JPEG named `.png` still previews.

Three traps, all of them met:

- `DynamicImage::thumbnail` **enlarges** — a 1×1 icon came back 360×360 — so the resize is a
  cap, not a fill.
- The texture is made at *draw* time, because `load_texture` needs an egui context, so the
  decode result has to travel as pixels and be uploaded later.
- A refusal is cached. Without that, a row of names containing one 2 GB video re-reads it five
  times as the pointer crosses it.

## 6. A remote preview waits, then fetches, and the cap comes before the bytes

The pointer must rest for `PREVIEW_DELAY` (350 ms), or a sweep across a row is one `ssh` fetch
per cell. The wait has its own deadline in `about_to_wait`, because a stopped pointer produces
no events and a deadline checked only on input never fires.

Then `stat` **before** `materialise`: a remote 400 MB scan must never be downloaded to find out
it is too big. Then the decoder runs on the background thread, never the drawing thread.

The cache key is `{ machine, path }`, not a path — `.bashrc` on three hosts is three files. The
preview is a tooltip-layer `Area` with `interactable(false)`, placed below-right of the pointer
and flipped at the edges with the clamp applied last, and `CursorLeft` clears the hover so a
preview cannot outlive the pointer that asked for it.

**A preview belongs to the grid, so it is suppressed by anything that is not the grid.** Two
surfaces were wrong in the same way. An overlay — a card, the palette, the pager, a menu — has
the window, and the preview is in the *tooltip* layer, above the foreground layer an overlay is
drawn in: a link hovered before `ctrl+?` raised its picture over the keyboard map, of a cell
nobody could see. And the cell mapping clamped a point outside the pane to the nearest one, so
the pointer over the tab strip (or the status bar, or above a split) mapped to row 0 and asked
the pane what link was there — a pointing hand and a preview of a file behind the tabs. The
answers are one list and one rect: `App::overlay_open` is every surface that has taken the
keyboard, `Context::layer_id_at` is egui's own answer for a card, a popup, a file pane or a
scroll bar under the pointer, and `App::cell_at` returns `None` outside the focused pane's
rectangle rather than the nearest cell — `cell_of` is that mapping, and
`a_point_outside_the_pane_is_no_cell` is the test. Reports to a *program* still clamp
(`pane_pixels`), because a drag that leaves the grid is still its drag; the question here is
"what is under the pointer", which has a true answer only inside the grid. The pointing hand
follows the same rule, because a hand over a card points at a link the card covers.

The image preview is suppressed while the link menu is open for the sharper version of the same
reason: the menu opens *at* the pointer and the preview draws 14 points from it, in a layer
above the menu.

## 7. Right-clicking a link is a menu

`Open`, `Save as...`, `Copy path` — and only a link *under the pointer* gets one; a right click
on text or background is not a menu.

A left press outside closes it **and goes nowhere else**. Dismissing a menu that also starts a
selection is the classic version of this bug, and avoiding it needs the menu's rect from the
last frame (`LinkMenu::contains`), because the press is handled before this frame's menu has
been drawn.

Escape is the menu's own key. Any other key closes the menu and *then* does what it was going
to do — a modal that swallows the next keystroke makes typing feel broken.

The image preview is suppressed while the menu is open: the menu opens *at* the pointer and the
preview draws 14 points from it, in a layer above the menu.

## 8. The file dialog is the platform's, and it blocks

`rfd` gives the XDG portal (zenity without one) and the Common Item Dialog on Windows. It runs
on the background thread and answers with `UserEvent::SavePath`, because on the drawing thread it
would freeze the window for as long as somebody browses.

A dialog that never appears returns the same `None` as a cancel, which is why `LogTracer` is
installed: rfd's own `log` line is what `-v` shows. `set_parent` keeps it in front on Windows.

It starts at `~/Downloads/<name>`, shared with the explorer's own download. Saving is the same
`materialise` a click uses, and then a copy — so "Save as" and "Open" cannot disagree about the
bytes. The toast is raised *before* the transfer, because a slow link is otherwise a silence
that reads as a dead button.

## 9. Materialising is where transparency ends, and it says so

A local file is **its own path**. `materialise` must never copy it, or clicking a file forks
the one you are working on and every later edit goes to the wrong copy.

A remote one is fetched under `<cache>/dtr/remote/<alias>/<path>`, with the tree mirrored so a
title bar says where it came from; `..` is *dropped* rather than resolved, the alias is
sanitised because it comes from `~/.ssh/config`, directories are `0700` and files `0600`.

The toast says "opened a copy of …" for a remote file and nothing at all for a local one, which
is the difference a person needs in order to know whether they are editing the original.

## 10. One `ssh` child per alias, and a dead one is killed

The session is shared behind an `Arc`, because a listing runs in its own task and still needs
*this* connection; a fresh one per `read_dir` would spawn an `ssh` per directory.

Every operation has a 30 s deadline and one reconnect, and a failure or timeout **takes the
child with it**. A dead session left in place makes every later call fail the same way, which
reads as "this host is down" long after it came back.

## 11. A reply has to outlive its request

The SFTP pump asked only the *focused* pane and delivered only to it, so the pane a link opened
sat on `loading…` forever and a split pane never listed. Both loops walk every leaf now.

The reply is routed by host **and** by the directory that was asked for. Two details made that
hard to see: `take_request` consumes `pending`, so matching an incoming reply against it
matched nothing, and `awaiting` has to survive the send.

## 12. The new-tab button is a menu, not a chord

The `+` opens `egui::Popup::menu`: the shells this machine has, then serial ports, then
`~/.ssh/config` hosts, each group behind a separator and each row saying which it is. The shells
are discovered rather than listed — `/etc/shells` on Unix, the well-known installs and `PATH` on
Windows — one row per name with the login shell first (`docs/sessions.md` §13).

Two things about the body were settled by looking at it. **It is as wide as its widest row**,
not as wide as the window: a row fills the width it is offered (that is what makes the hover fill
span the menu rather than the text), so a body left to decide for itself took everything from the
`+` to the window's edge — 600 points of menu with the words bunched at the left.
`dtr_widgets::menu_width` measures the rows it is about to draw, the scroll bar's width included,
and the popup is given that width. And **a shell's row is its name**, `shell bash`, not where
bash lives: one row per name means a path could not tell two rows apart, and fifty characters of
store path is a menu nobody can read. The program is in `-v`'s `shell started program=…`.

**A shell's row leads with the machine's picture of that program, and with the shell's logo where
the machine has none.** Windows answers with the executable's own icon (`system::app_icon`), the
one Explorer shows: on the guest, `powershell` is the blue PowerShell mark and `cmd` the console
window. Linux has no icon inside the executable, so it asks the two files a launcher asks — the
program's `share/applications` entry, whose `Icon=` names a picture, and the icon theme the package
installs beside it, which holds that picture under the name (`baash` ships both). A shell no
package gave an entry leads with GNU Bash's, Zsh's, fish's or Nushell's own logo, and a shell with
no logo (`sh`, `dash`, `ksh`) keeps the column and draws nothing, so the group's labels line up. A
Windows executable with no icon of its
own (an App Execution Alias is one) gets whatever Windows draws for it. A serial port leads with
a cable and a host with a rack — Lucide glyphs in the skin's text colour, because neither has a
picture: no icon theme knows `/dev/ttyUSB0`, and an alias is a name in a config file. `menu_width`
is told which rows reserve the column, so it never widens a menu that has no marks in it.

**The hosts come in the order they are used.** A row that opens an ssh tab writes two keys into
the store's `settings` namespace — `ssh␟<alias>␟uses` and `␟last` — and the menu sorts by the
count, then by when the host was last used, then by the order `~/.ssh/config` was written, which
a stable sort leaves untouched on a machine that has never opened one. The count is taken where
the *row is chosen*, not where a connection succeeds: a host that is down this minute is still
the one somebody reached for, and an ssh server on a laptop is away half the time. The logbook
cannot answer this — it records a run, and a tab opened from this menu never reaches it — so the
count lives where the choice does. `--no-remember`, or a store that will not open, is the config
file's order again.

**The count never adds a row.** `~/.ssh/config` decides which hosts exist and the store only
decides their order, so a host deleted from the file stops being offered however many times it was
opened — the next two-second scan takes the row away, even with the menu open, and a config file
that has gone leaves no hosts behind. The count is kept rather than pruned: a host that comes back
to the file comes back with its history instead of starting at zero, which is the difference
between "this host is gone" and "this file is being edited".

The answer is `TabHit::New(Option<Destination>)`, and it is a *typed* destination because the
menu used to infer "serial" from a leading `/` while the app opened every entry as `ssh` — so
picking a port ran `ssh /dev/ttyUSB0`. A port goes through `Action::Connect`, which remembers
its baud rate and CR translation; a host is `ssh -t <alias>`; a shell is that program, opened in
the focused tab's directory when that tab is local.

## 13. A file pane is painted, and its one button is an area of its own

A file pane draws a listing and senses nothing: it is an `Area` with `interactable(false)`,
because a press anywhere in it is the app's business. `App::pointer_down` turns that press into
a focus move, which is what makes the arrow keys walk the listing, and an area egui hit-tests
would take the press before the app ever saw it. The rows are painted, not widgets, for the
same reason.

So the cross in the header is **an `Area` of its own**, and that is not tidiness — it is the
only version that works. egui *skips a non-interactable area in its hit test*, so a
`dtr_widgets::cross_at` drawn inside the pane was a picture of a button that no press could
reach; the first version of it was exactly that, and it looked right in a snapshot.
`prime_area(…, interactable = true)` puts the target in place for the pane's first frame as
well, or a pane that appeared this frame would have a button in the picture and nowhere to
press.

One 20-point square over the header's right corner is the whole of the pane's click surface. A
press on it closes the pane and does *not* focus it first, and a press anywhere else in the
listing still moves the focus exactly as it did before. Its rectangle comes from
`explorer::close_rect`, which the painter, the widget and the test all call: the entry count
sits against its left edge, and two calculations would drift apart the first time the inset
changed.

The answer travels the way a clicked tab's does — the drawing code knows what was pressed, the
app knows what to do about it. `pane_ui` returns "the close was pressed", `Gpu::render` hands
back the leaf id, and `App::close_pane` takes it out of the layout. `close_active` is that same
call with the focused pane as its argument, and `close_pane` moves the focus **only when the
pane that went held it**: otherwise dismissing a listing beside a shell nobody touched would
move the keyboard out of that shell.

Checked in the live window at 1× and at 2×: the cross is where the header puts it, a click on
it closes the pane (`request redraw why="file pane closed"`), the terminal takes the width and
keeps the focus, and a press on the pane's body still moves the focus to the listing (the
terminal's cursor stops being drawn, which is how the screenshot says who has it).

What the pane *is* — a grid of icons, places, writes, and the same browser serving as the
desktop's file picker — is planned in `docs/manager.md`, and `PLAN.md` §3 (M4) is the order.

## 14. The app executes the writes, and the pane's field owns the keyboard

`docs/manager.md` §7 is the rules; `crates/dtr-files/src/write.rs` is the state machine. This
section is the third piece that makes F6 real: the terminal app takes the requests, runs them
off the UI thread, and answers the pane — the menu rows and `dtr-files`' tests stopped at
"reports a `WriteRequest`", which is exactly one request short of a file on disk.

**The worker is a `UserEvent`, and the machine is resolved on the UI thread.** After every
frame, `App::pump_files` walks *every file pane in every tab* (not the focused one: a pane a
link opened is deliberately unfocused, and the write is the pane's) and asks it for a request
with `Browser::take_write`. `Vfs::backend(&request.backend)` is called there and then — it
owns the connection pool, and a lookup is not I/O — and the `Arc<dyn Backend>` travels to the
background worker with the op and the replace policy. `App::bg_spawn` builds the future on the
worker's own compio runtime (a `JoinHandle` dropped there cancels a task, so it is detached),
`execute_write` awaits the verb, and the answer comes back as
`UserEvent::Write { leaf, result }`. The UI thread routes it by leaf id (unique for the window's
life, `next_leaf` only counts up), so a pane closed while the operation ran drops its answer
instead of answering somebody else's plan. `write_succeeded()` re-lists when it says the batch
is done; `write_failed` re-lists on `Stopped`/`Idle` (what was written before the failure is on
disk) and leaves the listing alone on `Asking` (the card is the frame) and `Answered` (the next
request is pumped next frame). One request per pane per pump is `take_write`'s contract, and
that sequence is what makes a conflict answerable at all: the question is about *the* request in
flight, and its answer must land before the next one starts.

**Replace is spelled in the worker, never in a backend.** `dtr-vfs` promises a backend never
replaces an existing destination; the card's "replace" arrives as `replace: Some(how)`, and
`execute_write` removes the destination with that removal *first* — the trash where the machine
has one — ignores a destination that vanished between the question and the answer (the answer
was "write over it"), and only then runs the verb. The conflict card's three answers therefore
have three implementations and one meaning: skip writes nothing, keep both rebases the name
(inside `dtr-files`), replace does the two steps above.

**Caps come from the backend, at pane construction.** `file_browser` asks
`Local::new().caps()` — `write: true`, `trash: cfg!(unix)` — so a local pane offers mkdir,
rename, copy, move and delete on Unix, and only the confirmed permanent delete on Windows.
`remote_file_browser` states read-only explicitly (`watch/dir_size/write/trash` all false)
rather than inheriting it: **remote panes stay read-only in this milestone**. The ssh *VFS
backend* is merged, but the app's remote pane still fills its listing through the older SFTP
worker, so a write answered by the VFS would run on a connection the pane does not share. F7
is the piece that switches the remote pane to `Vfs::backend` and flips those four flags; until
then the four words are a statement, and the menu hides the verbs because the caps say they do
not exist.

**The chords are `[keys]` actions, and the six are terminal-safe.** `files-new-folder`
(`ctrl-shift-n`), `files-rename` (`f2`), `files-delete` (`delete`),
`files-delete-permanent` (`shift-delete`), `files-copy` (`ctrl-c`) and `files-paste`
(`ctrl-v`) are the file manager's own chords and are in `DEFAULTS`, so the keyboard map lists
them, `[keys]` can move or disable them, and the palette is a third way in. Dispatch answers
them **only when a file pane is focused and returns `false` otherwise**, which is the
difference from `files-grid`: `^C`, `Delete` and `F2` belong to the program in a terminal pane,
and an action that swallowed them would take `^C` away. `App::browser_key` carries the same six
as the pane's own path (the mapping that also has `djklr` and `Alt+Up`), so the listing keeps
its chords even with the keymap rebuilt or absent; the `[keys]` path is the discoverable one and
is checked by a unit test of the default map, the pane path by a pure-function test of
`browser_key`.

Two traps were measured here, not reasoned about:

- **A key has two edges, and the listing was moving on both.** The terminal side encodes a
  release to nothing inside `pane`'s encoder; a file pane has no encoder, and routing
  `browser_key` on the release as well meant every `Down` moved *two* rows — the listing
  "skated" under the fingers. The Xvfb run made it visible: `Home; Down` landed on the second
  file, and `--capture` showed the row two below the intended one. The fix is the `pressed`
  test on the listing's key path, which is also why `--keys` scripts (which emit presses only)
  behave the same as before.
- **A field is not a set of chords.** While `Browser::write_modal()` is true (rename field,
  new-folder field, overwrite card, permanent-delete confirmation) `App::on_key` returns before
  the alt-digit switch, the keymap resolution, the menus and the listing. egui has already been
  fed the same event (`Frame::feed_event` runs before `on_key`), so the field types and the card
  answers Enter/Escape; what the gate stops is the character also being an `Action` — `ctrl-c`
  copying the selection under the field, `r` re-listing under the text, `ctrl-shift-p` opening
  the palette over a half-typed name, or Escape closing a menu the field was not in.

**The palette names obey the accelerator rule, so three of them are not the obvious words.**
A command's first letter is its accelerator and `crates/dtr-complete` fails the build on a
collision: `connect` owns `c`, `demo` owns `d`, `preferences` owns `p`, `replay` owns `r`,
`theme` owns `t`. The five commands are therefore `mkdir` (NewFolder), `name` (Rename),
`unlink` (Delete), `yank` (Copy) and `insert` (Paste) — the shell's and vim's words for the same
verbs. Each maps in `palette::action_for` to a `palette::Action`, and each palette arm calls
`App::dispatch` with the same `keys::Action` the chord uses, which calls the same `Browser`
method the right-press menu calls. A command with no arm in `action_for` does not belong in the
list, and the count test (`empty_input_ranks_every_command`) moved with them.

The kittest baselines moved with the command list (`palette.png`, `palette_adwaita.png`) and
with the two extra rows the keyboard map's left column gained before the rebind popup's row
(`keymap_rebind.png`); they were regenerated through lavapipe in the devShell, which is the
rasteriser that reads them back.

### The live proof

Xvfb (`Xvfb :124 -screen 0 1280x800x24`), a 900 × 620 window, and **real X11 input through
`xdotool`** — `--keys` cannot type into the rename field because scripted strokes go to
`App::on_key` only, while the field is egui's. `HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME` and
`XDG_CONFIG_HOME` all pointed into a temp tree, so the trash was the temp tree's.

- `--files /tmp/…/scratchA`, in list view: `ctrl-shift-n` **created `created-dir`**; `F2`,
  `ctrl+a`, `renamed.txt`, `Enter` **renamed `alpha.txt` to `renamed.txt`**; `Home`,
  `Down`×2, `Delete` **moved `beta.txt` into `$XDG_DATA_HOME/Trash`** (`Trash/files/beta.txt`
  plus an `info/beta.txt.trashinfo` whose `Path=` is the original). Final directory:
  `created-dir/`, `gamma.txt`, `renamed.txt`; `alpha.txt` is now `renamed.txt`; only `beta.txt`
  is in the trash. This is exactly the sequence a person types; the first attempt failed on
  the release bug above, which is how it was found.
- `--files /tmp/…/scratchB --capture …/rename-field.png --capture-after 8000`: the rename
  field open on `alpha.txt`, holding `renamed-in-the-field.txt`, footer *Enter renames ·
  Escape cancels*. `ctrl-shift+p` was pressed while the field was open and the capture shows
  **no palette** — the gate, in the picture. The run wrote nothing.
- `--files /tmp/…/scratchC --capture …/overwrite-card.png --capture-after 8500`: `F2`,
  `beta.txt`, `Enter` opened the one card — *replace existing file?*, source
  `…/alpha.txt`, destination `…/beta.txt`, answers `skip` / `keep both` / `replace`.
- The same scenario with a **real pointer click on `replace`** (`xdotool mousemove 608 324`,
  `click 1`): the request came back as `write_succeeded`, the destination was trashed first and
  then the rename ran. Final directory has only `beta.txt`, and its bytes are now
  `alpha content`; the trash holds `beta.txt` with its old `beta content`. The capture after
  the answer shows the listing refreshed to one row and no card.

PNGs: `/tmp/dtr-evidence/rename-field.png` and `/tmp/dtr-evidence/overwrite-card.png`,
`/tmp/dtr-evidence/overwrite-answered.png`, each 900 × 620 (the window's physical pixels at
1×).

### What F7 still needs for remote panes

The ssh `Backend` is merged with `create_dir`/`rename`/`copy`/`move_to`/`remove(Permanent)`
and `caps.write = true, caps.trash = false`. The app's remote pane is the missing piece:

1. `remote_file_browser` asks `self.vfs.backend(&BackendId::Ssh(alias))?.caps()` instead of
   the read-only literal — which needs the browser to be built where the `Vfs` is, i.e. the
   three `remote_file_browser` call sites become `App` methods.
2. The remote listing moves from the SFTP worker to `Vfs::backend(alias).list()` (streamed
   batches, `take_request`/`apply_listing` already exist and route by host *and* directory),
   because a write through the VFS connection while the listing is a separate SFTP session is
   the disagreement this milestone refused to ship.
3. `pump_writes` then needs nothing: it already resolves the request's backend through `Vfs`,
   so the ssh backend executes `Rename`/`Remove` on the alias its pane declared.
4. The remote menu then hides only what `trash` decides: the delete is permanent and asks, and
   `replace` clears the destination with `Removal::Permanent` (a trash-less backend's rule),
   which the worker already honours through `Writes::new`'s replace value.
