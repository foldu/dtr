# Files, links, and previews

The account behind the "Files, links, and panes" bullets in
[AGENTS.md](../AGENTS.md): clicking a path a program printed, previewing it, saving it, and
the rules that keep a remote path from being used as a local one. `docs/sftp.md` is the
decision memo for the transport itself; this is what the UI and the VFS layer each own.

The whole section turns on one idea: **the pane is the only thing that knows which machine a
path is on**, because a path arrives as text from a program that may be running anywhere.

## 1. A `Location` comes from the pane, not from a URI

`dtr-vfs` has `Location { backend, path }`, a `Backend` trait with `stat`, `open` and `list`,
and local, in-memory and ssh implementations. `BackendId::Ssh` is only ever built from a
session that *is* remote, and `Vfs::backend` refuses one it does not have rather than falling
back to local — the fallback would silently open a *different file of the same name* on this
machine, which is worse than an error.

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
it is too big. Then the decoder runs on the files runtime, never the drawing thread.

The cache key is `{ machine, path }`, not a path — `.bashrc` on three hosts is three files. The
preview is a tooltip-layer `Area` with `interactable(false)`, placed below-right of the pointer
and flipped at the edges with the clamp applied last, and `CursorLeft` clears the hover so a
preview cannot outlive the pointer that asked for it.

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
on the files runtime and answers with `UserEvent::SavePath`, because on the drawing thread it
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

The `+` opens `egui::Popup::menu`: local shell, then serial ports, then `~/.ssh/config` hosts,
each group behind a separator and each row saying which it is.

The answer is `TabHit::New(Option<Destination>)`, and it is a *typed* destination because the
menu used to infer "serial" from a leading `/` while the app opened every entry as `ssh` — so
picking a port ran `ssh /dev/ttyUSB0`. A port goes through `Action::Connect`, which remembers
its baud rate and CR translation; a host is `ssh -t <alias>`.
