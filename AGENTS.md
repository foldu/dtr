# Working on dtr

Notes for anyone — human or agent — changing this repository. The design and
the milestone list live in [PLAN.md](PLAN.md); this file is the stuff that
bites you while you are in there.

## What it is

A serial-first terminal in Rust: `libghostty-vt` for the VT core, egui/wgpu for
the window, one OS thread per pane. `crates/` holds the workspace; `dtr` is the
binary, `dtr-config` the config file, `dtr-complete` the palette's completion
engine, `dtr-store` the redb logbook, `dtr-sshconfig` the ssh config parser.

## Building and testing

Everything goes through the flake; the toolchain is not on `PATH`.

```sh
nix develop .#default --command bash -c 'cargo build -p dtr'
nix develop .#default --command bash -c 'cargo test --workspace'
nix build .#dtr                          # Linux package
nix build .#windows                      # the one-file Windows exe (cross)
nix build .#checks.x86_64-linux.single-exe   # asserts the one-file property
```

If `cargo build` fails to find libghostty's pkg-config file, point it at the
derivation (this is in the devShell hook too, but not in every shell):

```sh
export PKG_CONFIG_PATH=$(nix build .#libghostty-vt.dev --no-link --print-out-paths)/share/pkgconfig
```

Serial work in a sandbox: `socat pty,raw,echo=0,link=/tmp/dtr-serial
EXEC:'some script',pty,raw,echo=0` gives a fake device that can be killed
mid-session to test reconnect. Kill it with `pkill -x socat` — `pkill -f
"socat pty"` matches the shell that is running it and kills your own test.

## The house rules

- **Edit files with the editor tools, not shell redirection.**
- **Never `git add -A`.** Subagents write files (logos, CI, docs) while you are
  working, and `-A` sweeps their work into your commit. Stage explicit paths:
  `git add crates Cargo.toml Cargo.lock flake.nix PLAN.md`. This mistake was
  made twice here.
- **`git add` before `nix build`.** A flake build sees only git-tracked files,
  so a new source file that is untracked produces a confusing "file not found"
  or a phantom compile error about a *different* crate.
- **Screenshots are in-app only**, never a compositor grab. Two paths exist and
they are not the same picture: `--screenshot` renders offscreen from injected
state, while `--capture PATH --capture-after MS` copies the *live window's*
surface (configured `COPY_SRC` for exactly this). When a menu or a font behaves
differently in the window than in a capture, `--capture` is the one that tells
the truth.
- Commits end with the co-author trailer.

## Things that look like bugs and are not

**`O` and `0` look identical.** egui's default monospace draws a capital O and
a zero with almost the same shape at terminal sizes, so a screen showing `OK`
looks like `0K` in a screenshot. The text is fine. Before touching the render
path, run:

```sh
nix develop .#default --command bash -c 'cargo test -p dtr --bin dtr the_vt_core'
```

It feeds `"OK"` through the VT core and asserts the cells come back as `"O"`
and `"K"`. If that passes, the glyphs are right and you are looking at a font
ambiguity — this cost an hour once. It is also a real reason the M2 font stack
matters: a coding font with a slashed or dotted zero removes the ambiguity.

**`reconnecting…` on a transport that cannot disconnect.** `Stats::connected`
starts true and only the serial reader turns it false. If you add a new
transport, do not leave the flag at its `Default`.

**"No bytes" is not "the reader ended".** The pane keeps the byte-channel
sender alive for reconnects, so `bytes_rx.recv()` never returns `None`. A
disconnect is signalled on its own channel (`closed_rx`). Assuming otherwise
made reconnect silently do nothing.

**A clean frame is not a changed frame.** `publish` skips `Dirty::Clean`; the
first publish after a session starts is forced, or a pane with no output would
show nothing at all.

**`egui::Window` renders nothing inside `Context::run_ui`.** The palette is an
overlay drawn into the same `Ui` for this reason, not by preference.

**A widget's background painted after its text hides the text.** The palette's
selected row was invisible until the highlight was drawn first.

**A solid quad that samples the atlas can vanish silently.** Backgrounds, the
cursor and the selection were solid rectangles sampling one white texel; the
`write_texture` for that byte did not take effect, so all three were invisible
with no validation error and no log line. Solids now carry a negative uv and
take their colour from the instance, and the shader branches. If a rectangle
disappears, suspect the texture path before the blend state.

**A key release carries the same `logical_key` as its press**, so encoding both
types every keystroke twice ("typing `ls` shows `lsls`"). That trap belongs to the
encoder now rather than to a guard in the UI: a release encodes to nothing unless
the program asked the kitty protocol for release events.
`a_release_sends_nothing_unless_it_was_asked_for` is what keeps "ls" from being
"lsls".

**Feeding `RedrawRequested` to egui creates an infinite redraw loop.** egui replies
"yes, repaint" to that event, we ask winit for another frame, and the app spins at
the refresh rate while looking idle — 133 frames in 1.2 s of a terminal doing
nothing. Every event *except* `RedrawRequested` goes to `egui_state.on_window_event`.

**`Gpu`'s fields drop in declaration order, and it matters.** Anything holding a
texture, buffer or pipeline must drop before the device; the surface must drop
before the window it views; the window must drop last. Wrong order segfaults on
close instead of failing loudly.

**Panes share one instance buffer, and queue writes land before submission.** A
per-pane draw would rewrite the buffer between panes and the last pane would win;
that is why `render::prepare` takes every visible pane at once. Cells outside a
pane's rectangle are simply not emitted rather than scissored.

**CI runs the tests, not the window.** `.gitlab-ci.yml` has two jobs, both on the
`nix` runner because nothing is installed globally there: `test` runs
`cargo test --workspace` in the devShell, and `build` produces `.#windows` — the
one-file executable is the artifact, and a Linux-only build would not catch a
cross-compilation break. A failed snapshot run uploads
`crates/dtr/tests/snapshots/`, because the log otherwise says only that images
differ.

Snapshots are per-machine sensitive in the usual way: they are generated here and
compared byte-exactly there. If a renderer or font difference ever makes CI
disagree with a laptop, the answer is a threshold, not a regenerated baseline
from whichever machine ran last.

**The egui layer is tested with `egui_kittest`, not with a window.** `cargo test`
drives a `Harness` — real egui layout and AccessKit, no event loop — so clicks,
keys and rendering can be asserted. `crates/dtr/src/ui.rs` has the pattern: query
a widget by its accessible label, `.click()`, `harness.run()`, assert what the
widget reported. `UPDATE_SNAPSHOTS=1 cargo test` writes a baseline PNG under
`crates/dtr/tests/snapshots/`; the plain run compares against it. Each surface has
one snapshot per state worth seeing — `tab_bar` away, `tab_bar_hover` with the
pointer on a chip — because a hand-painted hover fill is invisible to every test
where the pointer is elsewhere.

Two things it taught, both real bugs: a widget painted by hand has **no
accessibility node** unless `Response::widget_info` declares one (the tab chips
were invisible to the harness *and* to a screen reader), and a widget is not a
`Button` just because it is clickable — `Sense::click()` plus a declared label
gives a hit target that Enter cannot activate.

**egui's default fonts have no arrows.** `↑↓` and `←→` in the palette and settings
hints rendered as tofu: the harness panicked with "No glyph for U+2191 in
Proportional", and the window had been drawing empty boxes there all along. Hints are
words now (`up/down`, `left/right`), which reads better in a terminal anyway. Arrows
and other symbols are still missing: see the IME entry for the one fallback face the
UI does have.

**Keys can be typed at the window from a script.** `--keys "ctrl-shift-p;down;down"` applies one
stroke per frame through the same handler a keyboard uses (`App::on_key`), and
`--capture PATH --capture-after MS` photographs the window afterwards. `;`
separates tokens because a chord may be a comma; `+` joins the chords of one
binding; `text:` types characters (only meaningful with a real shell, not
`--demo`). Together they check that a menu opens, a chord is wired, or a font
renders in the window.

**A missing glyph is not a missing character.** The grid face covers Latin; CJK,
emoji and the rest come from another installed family. `Fonts::face_for` asks the
grid face first, then the fallback faces it has already loaded (one Han family
covers most Han), and only then walks every installed family, loading a face and
testing its charmap. The result is cached per character, so a screen of 2000
distinct Han characters costs one extra font load, not 2000.

Which family answers is decided by `fonts::FALLBACK`, not by the order fontconfig
reports: Sarasa Mono first, then the Noto CJK families, monospaced variants ahead
of proportional ones. A terminal wants a monospaced CJK face, or the wide
characters each carry their own advance and the grid stops lining up. A logged
`fallback face` line names the family that won, which is how to tell whether the
preference list is doing anything on a given machine.

Two things that follow from it: the glyph atlas key includes the *face index*,
because glyph ids collide between fonts; and a glyph is placed by the ascent of
the face that produced it, because a fallback rarely shares the grid's ascent.

**A placed offset is not a length.** The wide-glyph fitting scaled `glyph.top` by
the same factor as the bitmap, but `top` is the *distance from the cell's top edge
to the glyph*, not a size proportional to the glyph — multiplying it moved the
glyph by the wrong amount and made CJK pile up on its neighbours. Wide glyphs are
now fitted from the bitmap itself: scaled uniformly into the two-cell box,
centred horizontally, resting on the cell's bottom.

**A wide character is two cells, not one.** The render iterator does not report
cell width, so width comes from `unicode-width`: a character two cells wide is
drawn across both cells, scaled to fit. Without that, the grid shows a one-cell
glyph followed by a one-cell hole, which reads as broken spacing.

**Key bindings are config, and the format is Zed's.** `[keys]` maps an action
name to a chord (`ctrl-shift-t`), a named key (`tab`, `pageup`), or several
chords in a row (`ctrl-k ctrl-s`). `false` hands a chord back to the shell. The
action vocabulary lives in `crates/dtr/src/keys.rs`; the chords live in the
config, so the config crate parses them and the app matches them.

Two rules borrowed from Zed, both of which bite: **shifted punctuation is written
as the character it produces** — pressing ctrl+shift+backslash reports `|`, so
the binding is `ctrl-|`, and `ctrl-shift-\\` would never match (the lookup retries
without shift so both spellings work); and **a chord that starts a longer binding
waits**, which is why a pending sequence flushes after a second to the longest
binding that is already complete.

**A shell has no status bar.** It exists for a serial port's counters and modem lines;
a shell's status is its tab title. The bar is not drawn and its height is not
reserved when the focused session has no counters, so a shell gets the rows the
empty strip used to take.

**The strip is reserved even for one tab.** It is where a shell is named now, so it
is always drawn — and reserving space only when several tabs exist is exactly how
the prompt ended up underneath it on a single-tab window.

**A resize must re-fit the terminal, not just the surface.** `WindowEvent::Resized`
only reconfigured the swapchain; the pty kept the rows and columns of the old
window, so the grid it published was the wrong shape for the new pane rectangle —
with a strip across the top the first row slid underneath it. Resize now calls
`fit_panes` as well. Any future per-size state belongs in that call, not in the
surface reconfiguration.

**A capture needs a frame, and an idle window draws none.** `--capture` happens while
drawing, so a window that has settled produces no picture; the deadline passes.
Believed fixed once by waking from `about_to_wait`, but it still misses on a plain
shell window, so the wake is not reaching the loop the way it looks like it should.

**`inner_size()` is already physical in winit 0.30, so a "scale" divided by it is
always 1.0.** `Gpu::scale()` was `config.width / window.inner_size().width`, which
is physical/physical — the DPI factor never entered it. Every consumer of `scale`
then reserved *half* the space it needed on a HiDPI display: the tab strip's
`top_inset` was 34 physical pixels over a strip that egui draws at 30 **points**
(60 px at 2×), so the first rows sat underneath it. At scale 1 the bug does not
exist, which is why it reproduced on the laptop and not in the captures here.

`scale()` is `window.scale_factor()` now. `-v` logs `strip geometry` once per
scale: `reserved_px` must be at least `strip_px`, both physical.

The units rule that the same area needs: egui works in **points** and applies the
DPI factor itself (`Panel::top(...).exact_size(n)` is `n` points), while `top_inset`
and the grid rects are **physical pixels**. So layout constants are points, grid
sizes are physical, and the scale is applied exactly once — by egui, or by us.

**Two layouts must agree on the top inset.** The terminal is sized by `fit_panes` and
drawn by `pane_views`; when only one of them knew about the tab strip, the pty kept
the rows of a taller area and the first prompt row was drawn underneath the strip.
Both now ask `App::top_inset`, so a strip can appear without moving a row under it —
the same class of bug as the status bar covering the last row.

**8 ms is a vsync, not a slow frame.** The first version of the `slow frame` log
timed the whole draw, `present` included, so every frame at 120 Hz was "8.2 ms"
with `glyphs=0` — the number was the compositor waiting for the next refresh, not
work. The timing now stops at `submit`; `present` is measured by nobody, because
it measures the display, not dtr.

**Control flow is sticky, and a stale deadline is a spin.** `set_control_flow` is
the loop's state until something changes it, not a per-iteration timeout. dtr set
`WaitUntil(now)` for a repaint and then, on iterations where nothing was due, set
nothing — so the deadline stayed in the past and every `poll` timed out instantly:
**47,000 loop iterations a second against 5 frames drawn**. `about_to_wait` now
collects the earliest deadline and sets exactly one value every time, defaulting to
`Wait`. Any new deadline belongs in that `sooner(...)` list, never in its own
`set_control_flow` call.

`-v` logs `loop iterations_per_s= frames_per_s= wakes=` once a second, which is the
measurement that separates a real spin (many iterations, few frames) from drawing
at refresh rate (frames == iterations) and from an idle terminal (both near zero).
Without it, 77% of a profile inside `poll` looked identical in all three cases.

**egui's own repaint request has to be carried too.** Opening the `+` menu draws
nothing until the mouse moves, because a popup appears on the *next* frame and
nothing in dtr called `repaint` for it — the request was egui's. After each frame
the app asks `has_requested_repaint()` and, if so, repaints. Any egui feature that
animates or appears late (popups, tooltips, hover) depends on this.

**`request_redraw` inside a redraw does not draw on Wayland.** Closing a tab left
the strip unchanged until the mouse moved: the redraw request made from inside
the frame waits for the compositor's next frame callback, which arrives with the
next input event. `App::repaint` now also sets a `wake` flag, and `about_to_wait`
turns it into `ControlFlow::WaitUntil(now)` plus another request — an immediate
wake rather than a hopeful one.

This is the general shape of "the UI only updates when I touch it", and it is
worth checking before blaming the work itself.

**`ssh` options go before the destination, and its stderr is the error message.**
`ssh -s host sftp -o BatchMode=yes` looks reasonable and is wrong: after the
destination ssh treats the rest as a command, so the option is handed to the
subsystem instead of parsed. `dtr-sftp::ssh_args` builds the list and a test
asserts every flag precedes the alias.

The second half is what made that failure invisible: the child's stderr was
`Stdio::null()`, so a dead `ssh` reached the UI as `sender dropped` — the
protocol's way of saying "nothing more will arrive", which names no cause at all.
stderr is piped now and quoted in the error with the exit status.

`examples/probe.rs` runs the same path from a shell when a pane refuses to open:
one line from `ssh` beats guessing. `DTR_SSH_CONFIG=/dev/null` gets past a broken
config, and `DTR_SSH_BATCH=0` lets a prompt be seen.

**A session knows its directory only because it says so.** There is no portable way
to ask a running shell where it is: `/proc` is Linux-only, and an `ssh` session has
no local process at all. OSC 7 (`ESC ] 7 ; file://host/path`, BEL or ST terminated)
is the report every modern shell and prompt can emit, so `CwdSniffer` watches the
byte stream before the VT core and keeps the tail across chunks — terminal output
arrives at arbitrary offsets, and a sequence split in two is the normal case, not
a corner.

The **host** in that URI is the part that matters: empty, `localhost` or this
machine's own name is a local directory; any other host is a shell on another
machine, and a path from there must never be used as a local path — "new tab
here" would try to `cd` into a directory that only exists on the wrong machine.
`Session::remote` records the `ssh` case too, so a remote session's directory is
never adopted even if the remote host happens to share this machine's name.

`+` → local shell starts in the focused tab's directory when that tab is local.

**A prompt mark is a reference, not a row — and the core already knows where
the prompts are.** OSC 133 (`A` prompt starts, `B` input starts, `C` output
starts, `D;exit` command ends) is what shell integration emits, and libghostty
parses it: `ScrollViewport::DeltaPrompt(n)` walks the viewport prompt to prompt,
so `previous-prompt` (`ctrl-shift-up`) needs nothing from dtr at all.

What dtr keeps is for *capture*: `PromptMarks` records each `A` as a
`TrackedGridRef`, which follows the page list through scrolling, pruning and
reflow — a row number would go stale the moment the block scrolled. The block
is then `Selection::new(previous_prompt, cursor)`, formatted with trim and
unwrap, so it includes the command line as well as its output.

Two things measured while building it. `select_output` is the tempting API and
is not what a pager wants: it returns `None` unless the grid reference is
*inside* the output, and it covers the output only. And a sequence arrives in
whatever pieces the pty hands over, so the introducer can end a read — a search
for the whole `ESC ] 133 ;` finds nothing there, and writing those bytes loses
the mark. The tail is held back unwritten (prefix included) until the
terminator arrives, or flushed past `CARRY_MAX` so a bogus introducer cannot
stall the session.

`capture-block` (`ctrl-shift-o`) parks the text in the pane's `captured` slot and
`crate::pager` shows it. Without shell integration there are no marks, and the
toast says so instead of inventing a block.

**The pager is a `Label` in a `ScrollArea`, and that is the whole design.** The
text is already in memory, so wrapping, scrolling and mouse selection are egui's
job; a line-oriented pager would have to re-break every line itself and would
still disagree with the font about where the break goes. What that costs is two
things worth knowing: the page height a key must scroll by is only known from the
*last* frame (so it is remembered, not measured), and "go to the end" is
`f32::MAX` with the scroll area clamping it, because only the frame knows how tall
the text turned out to be.

It is **modal**, and the keyboard is the reason: `j`, `k`, `b` and space are a
shell's keys, and a keystroke that leaked through would run a command nobody typed
while the thing that typed it is hidden. `App::on_key` therefore hands every key to
the pager first and returns — including the ones it does not understand. Chords
are not pager keys, so `ctrl+…` falls through to nothing rather than to a binding
that would fire under the pager.

The pointer is modal for the same reason: while it is open, the wheel must not also
scroll the shell's scrollback and a drag must not select text behind the pager, so
`CursorMoved`, `MouseInput` and `MouseWheel` skip the terminal. It draws in the
same foreground `overlay` layer as the palette, because a file pane is an `Area`
and would otherwise paint over it.

Closing drops the text. A captured block can be megabytes, and holding it for the
rest of the process is a leak nobody would ever look for.

**fish already speaks it** — nothing to install. Captured from a real
`fish --no-config -i`, the sequence is
`ESC ] 133 ; A ; click_events=1 ESC \` and the same with `cmdline_url=…` on
`C`: options after a `;`, and ST rather than BEL as the terminator. Both were
already handled, and `the_form_fish_sends_is_a_prompt` pins it so they stay
handled. bash and zsh need the snippet from the shell-integration proposal
sourced from their prompt.

**Clicking a file is a VFS question, and the pane owns the answer.** `crates/dtr-vfs`
has `Location { backend, path }`, a `Backend` trait (`stat`, `open`, `list`) and
two implementations: `Local` and an in-memory one for tests. The rules baked into
it are the ones that were argued about:

- **A `Location` comes from the pane, not from a URI.** `file:///etc/hosts`
  printed on `jupyter` means jupyter's file. `BackendId::Ssh` can only be built
  from a session that *is* remote, and `Vfs::backend` refuses it outright rather
  than falling back to local — a remote path must never be read off this disk.
- **Listings are batches**, because `readdir` answers in chunks and a large
  directory over a slow link is the normal case. Dropping the receiver cancels
  the walk, which is the cancel.
- **Capabilities are declared.** No remote watch, no directory size; the UI hides
  what is not there instead of polling and pretending.

**A hyperlink is asked about, not carried in the frame.** OSC 8 lives in the
terminal, which never leaves the pane thread, so `Command::HyperlinkAt { x, y }`
answers into a slot. It is *not* resolved per cell during publish: one
`hyperlink_uri` call per cell is ~8 ns of FFI, and on a dense 3808-cell grid that
is another 30 µs on a 41 µs publish for a link nobody is pointing at. The reply
carries the cell it was asked about, so a frame-late answer is dropped instead of
opening the wrong link — hover state without that is a race with a real mouse.

Only `file:` is understood, percent-decoded, `stat`ed through the VFS and handed
to `xdg-open`/`open`/`explorer` — never a shell, because the path comes from a
program's output. Another scheme is said out loud in a toast. A press on a link
opens it and starts no drag; everywhere else a press still selects.

**The host part decides which machine, and `file://blah` usually has one.**
`ls --hyperlink=always` prints `file://<hostname>/path` — not `localhost`, not an
empty host — so a rule that accepted only those two refused every link `ls` ever
printed. The rule lives in `crate::uri` now and is shared with the OSC 7 sniffer,
which had always known about the machine's own name while the link opener did not;
that is what "clicking a link says cannot open file://jupiter/…" was.

A local session resolves a path only when the host is empty, `localhost` or this
machine; anything else is named in the toast (`… is on build-host, not this
machine`) rather than guessed, because reading another machine's path off this
disk is the one mistake a link can make that pointing at the right file later does
not undo. A remote session resolves *every* `file:` path on its own host, and that
is correct rather than lax: `file://localhost/x` printed on `jupyter` means
jupyter's `/x`, and the backend it asks can only read that machine.

**A hovered picture is decoded off the drawing thread and capped before it is
read.** `crate::thumb` previews a link whose path ends in `.png`, `.jpg` or
`.jpeg`: 16 MiB of source, 8192 pixels a side, 256 MiB of decode — a preview of a
400 MB scan is not a service to anyone, and a 20000x20000 PNG is 1.6 GB of pixels
whether or not the pointer stays. The extension only decides whether to *try*: the
reader sniffs the bytes, so a JPEG named `.png` still works.

Three things that are easy to get wrong here:

- **`DynamicImage::thumbnail` enlarges.** It fits the picture into the box, so a
  1x1 icon came back 360x360 — invented pixels, and the test says `(1, 1)` now. Only
  an image larger than the box is downscaled; anything smaller is used as it is.
- **The texture is made at draw time, not when the decode lands.** `load_texture`
  needs an egui context, and the worker has no business knowing about textures or
  owning one. So the cache holds decoded pixels and the first frame that draws the
  entry uploads them.
- **A refusal is cached too.** Moving the pointer along a row of names must not
  re-read a 2 GB file five times; `Entry::Failed` is the answer "no preview" and it
  is as good as a picture.

**A remote preview waits, then fetches, and the cap comes before the bytes.** The
pointer has to rest on a remote link for `PREVIEW_DELAY` (350 ms) before anything is
transferred: a sweep across a row of names passes over every one of them, each is a
different link, and without the wait that is one `ssh` fetch per cell. Any new hover
drops the pending one, and the wait has its own deadline in `about_to_wait` — a
pointer that has stopped produces no events, so a request nobody wakes for would
never be made. A local file is read immediately, because there is nothing to spend
but a read.

Once the wait is over the path goes through the same VFS the click does: `stat`
first — a remote 400 MB scan must never be downloaded to find out it is too big to
preview — then `materialise` (a local file is its own path and is never copied, a
remote one is fetched into the cache), then the decoder on the files runtime.

The cache key is **`{ machine, path }`**, not a path: the same `/tmp/shot.png` on
`jupyter` and on this disk are two files, and a picture from one must never be
handed back for the other. `a_remote_file_is_not_the_local_one_with_its_name` pins
that.

The preview is a tooltip-layer `Area` and `interactable(false)`, because a file pane
is an `Area` too and a picture that slid underneath one would be a picture nobody
could see. It is placed below-right of the pointer and flipped at the edges, with a
clamp last: flipping a picture wider than the window pushes it off the opposite
edge, which is the case `place` is tested for. `CursorLeft` clears the hover — there
was no handler at all, so leaving the window left the pointing hand (and would have
left the preview) standing on a file the pointer is nowhere near.

`the_preview_is_painted_next_to_the_pointer` renders a frame in the harness and
checks the pixel at the computed position is the texture's colour and the far corner
is not — a mesh that never reached the screen fails there, which no amount of
geometry testing would catch.

**Right-clicking a link is a menu, and "Save as" is a path field.** Three things
can be done with a link that names a file: `Open` (exactly what a left click does —
it goes through `open_link` with the URI, so a remote one still opens a copy),
`Save as...`, and `Copy path`. Only a link *under the pointer* gets a menu: a
right-click on ordinary output has no target, and a menu of nothing is worse than
no menu. The cell decides, as it does for the press that opens a link — the hover
is one frame behind the pointer, so a very fast right-click after moving misses.

Dismissal is the part with a trap. A left press outside the menu closes it *and
goes nowhere else*, because a click that both dismisses a menu and starts a
selection is how text gets selected by accident. That needs the menu's rect, which
only the frame that drew it knows, so `LinkMenu::contains` answers from the last
frame — hit-testing must not depend on this frame's egui pass, or the press would
dismiss the menu before its own button could see the click. Escape is the menu's
own (the one key that must not reach the shell); any *other* key closes it and then
does what it was going to do.

The dialog is a text field, not a native picker: `rfd` would mean GTK on Linux or a
portal dependency for a one-file Windows build, and a terminal's user can type a
path. The default is `~/Downloads/<name>` — shared with the explorer's download, so
two ways of taking a remote file agree — and `~` is expanded, because a field
showing `~/x` that writes a literal `~` is discovered after the file is gone.

Saving is the same `materialise` a click uses, then `tokio::fs::copy`: local files
are their own path, remote ones are fetched into the cache first, and the copy is
the last step, so "Save as" and "Open" cannot disagree about what the bytes are.
The toast is raised *before* the transfer ("saving x...") and replaced by the
result: a file over a slow link is otherwise a silence that reads as a dead button.
There is no progress and no cancel yet.

The image preview is suppressed while either is open, and that is not cosmetic: the
menu opens *at* the pointer and the preview is drawn 14 points from it, in the
tooltip layer — which is above a foreground menu. Without the guard the picture
covers the first two items of the menu that was opened to click them.

**A dialog with a text field owns the keyboard.** `on_key` returns early while the
save dialog is open; egui reads the same events, so the field still types — without
the guard, the filename is also sent to the shell as a command. The menu is the
milder version: it closes on the first key and lets it through.

**A directory link opens a file pane, not a toast.** `xdg-open` on a directory
starts the desktop's file manager, which is a second window to close again; dtr has
a file pane, so the click shows the directory there, beside the terminal. The
worker that resolved the link sends `UserEvent::Reveal { path, alias }` and the UI
decides where a pane may go — layout is not a worker's business. Focus stays on
the terminal: the click happened in the shell and the next keystroke belongs there.
A pane already browsing the *same machine* is retargeted instead of a second one
growing; another machine is another pane, because a pane reading this disk cannot
become one reading somebody else's. A remote directory opens a pane over its host
(`Explorer::remote_at`), which is the only thing that can list it.

**A reply has to outlive its request, and a pane nobody is in still has to list.**
The SFTP pump used to ask only the *focused* pane and deliver only to the focused
pane, so the pane a link opens — deliberately unfocused — sat on `loading…` for
ever, and a split pane with the terminal focused never listed at all. Both loops
walk every leaf now, and the reply is routed by host *and* by the directory the
pane asked for. That needed a second field: `take_request` consumes `pending`, so
matching a reply against it matched nothing, and `awaiting` is what survives the
send. `a_pane_waits_for_the_listing_it_sent` fails if the two are ever merged
again.

`--click x,y` exists because the hover has to happen before the press for a link
to be known: it moves, waits one frame, then presses. That is what a hand does,
and it means the scripted path exercises the real one.

**Materialising is where transparency ends, and it says so.** A local file is its
own path — `materialise` must never copy, or clicking a file forks the one you
are working on. A remote one is fetched first, because `xdg-open` and every
viewer it starts can only open paths. The copy lands under
`<cache>/dtr/remote/<alias>/<path>`: the remote tree is mirrored so the title bar
of whatever opens it says where it came from, `..` is *dropped* rather than
resolved (nothing a remote program prints can address the cache root's parent),
the alias is sanitised because it comes from `~/.ssh/config`, and directories are
`0700` with files `0600` so a 0022 umask cannot make somebody's remote file
readable. The toast says "opened a copy of …" for a remote file and nothing for
a local one, because one is a copy and the other is not.

**One `ssh` child per alias, and a dead one is killed.** `Ssh` holds
`Arc<Mutex<Option<dtr_sftp::Session>>>` — shared rather than owned, because a
listing runs in its own task and still has to use *this* connection; a fresh one
per `read_dir` would spawn an `ssh` per directory, which is the thing pooling
avoids. `Vfs::backend` keeps one `Arc` per alias and there is a test (`Arc::ptr_eq`)
that it is the same one twice. Every operation has a 30 s deadline and one
reconnect; a failure or a timeout **takes the child with it**, because a dead
session left in the slot makes every later call fail the same way — which reads
as "this host is down" long after it came back.

`list` over ssh is one `read_dir` round trip delivered in batches. The batch
shape is what the UI will keep when the protocol starts paging; today the
protocol hands back the whole directory, and pretending otherwise would be a lie
in the wrong layer.

**Files work has its own runtime.** `App::files_rt` is separate from `App::worker`
(which keeps scans): a directory walk must not queue behind serial-port
enumeration, and the SFTP work that follows will need it.

**A shell that exits closes its tab.** The pane cannot do it — the UI owns the
tab — so the pty reader sets `Pane::ended` on EOF (the child closed the pty, so
it has exited) and calls `wake`; `App::reap_ended` closes those tabs, highest
index first so the shifts do not invalidate the rest. The last tab closing takes
the window with it, which is what `exit` does in any terminal.

`ended` is a flag rather than a message because the reader is a plain thread and
the UI polls the frames anyway; a channel would need its own arm in the loop for
something that happens once.

**A dropped ssh connection is not a shell that exited, and the exit status is the
only thing that tells them apart.** `ssh` exits 255 when ssh itself failed — the
host is unreachable, the key was rejected, the connection died — and anything else
is the remote command's own status, which means somebody typed `exit`.
`pane::ending` is that rule, and `ending_tests` is the test that keeps it. A drop
keeps the tab, says so on the screen and starts the client again after a backoff
(1 s doubling to 30 s, reset once a connection has been up for 10 s); an `exit`
closes the tab as it always did.

**A client that never speaks is worse than one that fails.** Two different
silences, and only one of them ends on its own. A connect that is *refused* or
unresolvable fails in milliseconds; a connect into a blackhole sits in the
kernel's `connect` for two minutes. And a connection that goes away without a
reset — a lid closed, a route dropped — leaves an `ssh` that is still running and
still looks alive, so the pane sits there showing a session that is not there and
the reconnect never fires, because the child never exits. `pty::Config::ssh`
therefore passes three options, all of them before the destination:
`ConnectTimeout=10`, `ServerAliveInterval=20`, `ServerAliveCountMax=3`. Ten
seconds to reach the host; a minute of unanswered keepalives before ssh calls the
connection dead and exits 255. **ssh's own default is no keepalive at all**, which
is why this is not optional. The options override `~/.ssh/config`, so a
`ServerAliveInterval` there is ignored; the numbers live in one place in `pty.rs`
and `the_timeouts_come_before_the_destination_and_survive_a_reopen` pins their
position, because an option after the destination becomes a command for the far
end.

**Retries are bounded in rate, not in number.** There is no give-up count and no
retry button: waits go 1, 2, 4, 8, 16, 30, 30, … seconds, so a host that is down
is asked about twice a minute for as long as the pane exists, and closing the tab
is how a person gives up. That is deliberate — a laptop can be shut for a day and
the session it was holding is still worth reopening when the network returns,
which a cap would turn into "closed while you were not looking". A flapping host
keeps the long wait, because the reset needs ten seconds of *uptime*, not just a
successful connect.

Two consequences that are easy to get wrong. The child now belongs to the reader
thread, not the pane: `wait` blocks, and the status is only available after EOF —
so `start_shell` hands the child over and the pane hears how it ended on its own
channel. And the first connection is opened *inside* the event loop by the same
code as every reopen, because a host that is down at startup should be a pane that
waits rather than an app that exits; that is why `--send` is held in
`typed_early` until the first pty exists instead of being dropped at a `sink` of
`None`.

**A reconnected session is put back where it was by the remote command.** `ssh -t
host` starts in `$HOME`, so the reopen is
`ssh -t host 'cd <dir> ; exec "${SHELL:-/bin/sh}" -l'`: the directory quoted by
`shlex` for the shell at the *other* end — a path can hold anything but `/` and
NUL, and NUL is refused rather than quoted into something a different shell would
split — `;` rather than `&&` so a directory that has been deleted still leaves a
usable shell, and `exec`ed so the login shell is the session rather than a child of
a wrapper. Quoting is not a place for hand-rolling:
`a_hostile_directory_is_still_one_word` round-trips a dozen paths (`$(…)`,
backticks, `;`, quotes, a newline) through `shlex::split` and asserts each arrives
as one word.
The directory is the session's own OSC 7 report, which is why the sniffer returns a
report instead of a path: `file://build-host/srv/app` is not a local directory
(nothing here may `cd` into it) but it is exactly where a session on `build-host`
belongs. `pty::Config` carries the alias as `remote` so this stays a decision about
a connection rather than a guess from the program string.

**The modes outlive the program that set them.** A connection can drop while vim
holds the alternate screen with the mouse modes on, and the shell that replaces it
would start inside them. `reset_modes` clears them through the core before the
reopen; the text is left alone, because the last screen before the drop is the
first thing one wants to read. The notice (`-- reconnecting to jupyter in
/srv/app --`) goes into the terminal, not the pty: nothing is running at the other
end to read it, and a pane that simply stops looks like one waiting for a key.

**Closing a tab is teardown, and teardown is slow.** Dropping a `Tab` drops a pty
and the thread that owns it; the child has to notice and exit, which is not
instant. `close_tab` therefore hands the dead tab to the worker runtime
(`spawn_blocking`) and logs how long the teardown took, so a slow close is visible
in `-v` instead of feeling like a stuck button.

**The UI thread asks for slow work, it does not do it.** Enumerating serial ports
touches udev or SetupAPI and reading the ssh config is a file read; both are
milliseconds-to-tens-of-milliseconds, which on the drawing thread is a dropped
frame and a button that feels dead. `App::request_scan` hands the work to a
one-thread tokio runtime (`App::worker`) with `spawn_blocking`, and the result
arrives as `UserEvent::Destinations`. `scanning` stops duplicate scans, and
`scan_due` is the poll interval, not a busy loop.

The panes' runtimes are single-threaded and per-session by design; this one is
the UI's own, and `rt-multi-thread` had to be added to tokio's features for it.

**udev would be better on Linux and wrong overall.** Devices are *polled* through
`serialport::available_ports` every couple of seconds, not watched through `/dev`:
udev is Linux-only, and Windows enumerates COM ports by an entirely different
route. One mechanism that works on both beats a fast one that only exists here.
Files are different — `~/.ssh/config` is watched with `notify` (inotify on Linux,
`ReadDirectoryChangesW` on Windows), because that mechanism *is* portable.



**The new-tab button is a menu, not a chord.** The `+` at the end of the strip opens
`egui::Popup::menu`: the local shell first, then every alias in `~/.ssh/config`
with the detail line the palette shows. It returns `TabHit::New(Option<String>)` —
`None` is local, `Some(alias)` is `ssh -t <alias>` through the system client, so
jump hosts and agent forwarding keep working. The destination list is read once at
startup into `App::destinations`; reading `~/.ssh/config` per frame would be a
syscall per redraw.

**Lucide's `currentColor` is black, and a tint multiplies.** An icon fetched
verbatim disappeared on a dark chip: the rasteriser resolves `currentColor` to
black and `Image::tint` only multiplies, so black × colour is black. `fetch.sh`
rewrites it to `#ffffff`, which makes each file a white mask that the tint colours.
The geometry is still upstream's.

**Icons are Lucide SVGs, fetched by a skill in this repository.** `.dsh/skills/lucide-icons`
holds the instructions and `scripts/fetch.sh`, which pulls `https://lucide.dev/api/icons/<name>`
into `crates/dtr/assets/icons/<name>.svg` and refuses to overwrite one that is
already committed. They are embedded with `egui::include_image!` and rasterised by
`egui_extras`' SVG loader, so the one-file build still carries them.

The loader must be installed before the first image, and `install_image_loaders` is
idempotent, so `tab_bar` calls it rather than trusting startup — a kittest harness
creates its own `Context` and would otherwise draw egui's missing-image glyph,
which is a `⚠` that egui's own fonts cannot render either.

**Overlapping click targets read as "sometimes unresponsive".** The chip and its
close cross were two sensors over the same rectangle, with the icon added as a
third `ui.put` widget on top. egui gives the press to the topmost widget that
senses clicks, so which one won depended on allocation order — the cross closed
sometimes and selected the tab otherwise, and the `+` lost its press to its own
icon. The fix is disjoint rects and `Image::paint_at`, which cannot sense anything.

**A switch that appears on the next keystroke is a stale frame.** `Ctrl+Tab` was
reported as "switch to the other tab, press Enter, and it goes back" — the log
shows the switch happening on the chord and nothing moving it afterwards. The
switch was always correct; the *picture* was late, and the next keypress was what
made a frame. Same root cause as the closed tab that only vanished when the mouse
moved, which is why fixing the loop's control flow made both symptoms disappear.

Every tab switch now goes through `App::set_active`, which logs `active tab
from=… to=… why=…`. Five call sites can do it and only one is the mouse; the log
names the one that did.

**Hover is worth a redraw.** Skipping the repaint for pointer motion was tried and
rejected: the chips and the `+` lighten under the pointer, and a chip that looks
identical there reads as decoration. So a terminal redraws while the mouse moves —
expected, not a leak.

**A chip has two hit targets.** The label switches tabs, a cross on the right closes
that tab — whichever tab is active. Both are `Sense::click` regions with
declared accessibility names (`2 shell`, `close tab 2`), and both are gated on a
real pointer click — see the `Sense::click()` entry, which is the trap that
`Sense::click` alone does not close. The cross is *drawn* with two lines, not a `×` glyph: egui's default
font has no multiplication sign and would draw an empty box.

Closing the last tab closes the window, the same rule `close_active` follows: a
terminal with no sessions has nothing to show.

**Tabs were invisible until the strip existed.** The top strip is reserved in the grid
layout only when there is more than one tab, so a single session loses no rows;
the active tab's title follows its focused session, the others keep the title they
had. A click on a chip switches tabs through the same `self.active` the chords use.

**`Sense::click()` is not pointer-only.** egui turns Enter into a click for *any*
focused widget that senses clicks (`context.rs`: `has_focus(id) && key_pressed(Enter)`
sets `FAKE_PRIMARY_CLICKED`). A chip that had been clicked once stayed focused for
the rest of the session, so every Enter typed into the shell switched tabs — with
no mouse involved, which is why it looked like the tab switched itself. The log
named it: `active tab why="chip clicked"` one millisecond after `key Named(Enter)`.

Both are needed: the chip surrenders focus after being drawn, and a hit is accepted
only when the pointer actually clicked (`pointer.any_click()`), because the keyboard
"click" arrives with the pointer somewhere else entirely. The `+` button clears the
same flag before handing its response to `Popup::menu`, or Enter would reopen the
menu. `enter_after_a_click_does_not_press_a_chip_again` puts focus on a chip by hand
and fails without the fix — kittest's `.click()` alone does not leave focus behind.

**There was no new-tab key.** `Ctrl+Tab` cycled tabs that only the palette could
create, so a terminal had one tab and no discoverable way to get a second — the
cycling looked broken rather than unused. `Ctrl+Shift+T` opens a tab, and the
palette footer is the one place the bindings are written down.

**`cargo build` can pass while `nix build` fails: the sandbox compiles the committed
tree, not the working one.** Two `#[cfg(test)]` mistakes were caught this way — a
`Row::key` helper used by production code, and a `default_family` whose body read
a list that had just been made test-only. Before believing a local build, run
`git add` and `nix build .#dtr`; a dirty-tree `cargo build` can compile a stale
file set in ways the release build will not.

**A menu that opens slowly the first time is doing its work in the frame.**
`Settings::show` fills two lists: the installed font families and the theme files.
The families are the expensive one — `usable_families` builds a fontconfig
collection and probes every family, which is the same `FcFontSort` cost that showed
up in a profile at 16%. It is cached in a `OnceLock`, so the *second* open is
instant, which is exactly the shape of "slow the first time, fine afterwards".

The app now warms both on a `dtr-warm` thread at startup, and `-v` logs
`warmed the settings lists` with the cost, plus `settings populated` on each open so
a regression is a number rather than a feeling. `the_family_list_is_computed_once`
fails if the cache is ever removed.

**The settings menu is mouse-first.** Rows are click targets, the font size is a
drag value, the family and the theme are combo boxes, the toggles are checkboxes;
the keyboard still works but nothing needs it. `Settings::ui` takes `&self` (the
caller cannot lend a mutable borrow while the renderer holds one), so a control
*reports* its edit as a `Change` and `Settings::apply` writes it into the draft.

Every control declares an accessible name through `Response::widget_info`, and the
name is `<row> value`, not the row name: the row's label already carries the bare
name, and two nodes with one name are impossible to address from a test.

**The settings menu shows port rows only when a port is focused.** Baud and CR
translation belong to one serial device, so with a shell in front there is
nothing to show: the rows are built per open, not baked into a fixed list.

**A trailing space is a word break, not whitespace to trim.** The palette trimmed
the end of the query before deciding which word was being completed, so `ssh `
looked like the command `ssh` and offered *commands* where the hosts should be.
The space is the signal that the argument has started; `Parsed::of` keeps it, and
`ssh ` now lists the hosts while `ssh jup` narrows to one.

The consequence for commands with no arguments: after the space there is nothing
to complete, so the list is empty rather than offering the command back.

**`C-n` and `C-p` are consumed like the arrows.** The palette takes them from
`input_mut` before the text field sees them, so navigating with Emacs keys cannot
land an `n` in the query. The footer says both spellings, because a binding nobody
can discover is a binding nobody uses.

**Reopening the palette starts a fresh sentence.** `toggle` clears the query, the
selection and the completions; a palette that remembers the last command turns the
next one into a correction.

**A modal must ignore key releases.** The release of the keystroke that opened a
modal arrives while it is open, so handling releases as well as presses closed
the settings menu the instant `Ctrl+,` was let go — it flashed and vanished and
read as "does not open". Key handling for overlays checks `ElementState::Pressed`
first; the same trap as the doubled keystrokes, in a different disguise.

**A toast that says "copied" is not a clipboard.** `ui.ctx().copy_text` goes through
`egui-winit`, whose clipboard *falls back to an in-app buffer* when it cannot reach
the display — so the copy looks successful and the desktop never sees it. dtr now
also writes the clipboard itself with `arboard`, keeping the handle on `App` for
the life of the process (on X11 the owner has to stay alive or the text disappears
with it). Failures are logged; silence is not success.

**Selecting with the mouse copies.** The press sends `Command::SelectStart`, the
drag `SelectHead`, and the release `CopySelection`; the pane builds the text from
the terminal, parks it, and the next frame hands it to `ui.ctx().copy_text`, which
`egui_state.handle_platform_output` puts on the system clipboard. The release
handler used to only clear `dragging`, so a mouse selection produced text on
screen and nothing in the clipboard.

**A selection is anchored to characters, not to the grid.** Cell indices name
whichever character happens to be at a row later, which is fine until the first
scroll: the block you dragged over has moved and the highlight stays. So the
pane holds two `TrackedGridRef`s — the kind that follow the page list through
scrolling, pruning and reflow — and resolves them to viewport cells on *every*
publish. That is also why the frame carries the resolved range rather than the
UI holding one: a scroll republishes, so the picture and the copy cannot
disagree about what is selected.

`PointSpace::Viewport` does the resolving but not the bounding: a cell thirteen
rows below a five-row viewport comes back as `y = 13`, so the range check is
ours. Once either end has left the frame the selection is **dropped**, not
clamped — a highlight stretched to the border would claim cells the selection
does not cover, and a copy would hand back a fragment that looks like the whole
thing. `a_selection_that_leaves_the_frame_is_dropped` covers both directions.

A press that has not moved is an anchor and not a selection: nothing is
highlighted, so a single click deselects rather than selecting one cell. The
anchor has to exist at the press or a drag could not grow from it, which is why
the rule lives in `selection_span` and not in the commands — and why the anchors
carry a `deliberate` flag. A word, a line or a drag is deliberate, so its
one-cell case still paints: double-clicking `x` is a real selection, and only a
press that went nowhere is silent.

**Double- and triple-click are counted, not reported.** winit has no click
count, so `App::count_clicks` counts a press within 400 ms and four pixels of
the last one. Two takes the word under the pointer and three the line, both via
the core's `select_word`/`select_line` — its idea of a boundary, so
`--flag=value` and quotes break where they do in every other terminal — and
then converted to the same anchors, which is why a word or a line scrolls with
its text too.

Two details that are the difference between working and feeling broken: a drag
that stays inside the press's own cell changes nothing (a pixel of jitter while
clicking a word must not turn it into a one-character range), and a
double-click copies on release *without* having moved, because the two-click
was the point.

The pointer now goes through `App::pointer_down/move/up`, which `--drag
x1,y1,x2,y2` also calls: the copy path can be exercised without a mouse. That
option is only verifiable where an X display is reachable — a headless shell has
no clipboard and winit refuses to open.

**A copy cannot use the published frame, because drawing it is what removes it.**
The UI takes the frame out of the pane's slot to render it, so a copy that reads
that slot gets `None` and copies the empty string — which shipped as "copied 0
chars" with a selection on screen. `Command::Copy`/`CopySelection` rebuild the text
from the terminal at copy time instead; the pane is the only place that still
holds the cells.

**The program decides what a key and a paste mean, and only the pane knows.**
`Command::Key` carries a *stroke*, not bytes: `input::fill` maps winit's vocabulary
onto `libghostty_vt::key`, and the pane's `key::Encoder` picks the sequence after
`set_options_from_terminal`. A table in the UI cannot be right, because what a key
sends is the terminal's state: `DECCKM` (`?1h`, ncurses' `smkx`, so htop, less, man
and mc) makes an unmodified arrow `ESC O A`; `DECKPAM` moves the keypad; mode 1036
makes alt an `ESC` prefix; `modifyOtherKeys` respells ctrl and alt; and the kitty
protocol (`CSI > flags u`, a *stack*, not a mode) turns ctrl+a into `CSI 97;5u`
and can report releases. fish asks for `kitty=5`, nvim for `kitty=3`, and the
terminal answers their `CSI ? u`, which is why they switch over.

Three consequences that are easy to get wrong. The *position* decides, not the
character: a real event has `physical_key`, `--keys` only implies one from a US
layout. A ctrl or super combination must not pass its `text` through — the encoder
sent a literal `a` for ctrl+a — while a plain character must, or accents and CJK
never arrive. And `reset_modes` clears the kitty stack as well (`CSI < 999 u` then
`CSI = 0 u`): a dead program's push would have the next shell typed `CSI 97;5u`
for ctrl+a.

A paste is the same story: `pane::pasted` turns line breaks into CR and wraps the
text in `ESC[200~ … ESC[201~` when the program asked with `?2004h`.

**IME text is not a keystroke, and egui has no CJK.** `winit::event::Ime` carries
the composition: the preedit is only a picture — `crate::ime` draws it in a box
above the caret, clamped into the pane — and nothing reaches the pty until the
commit, which is what makes a composition cancelable. Keys typed *while composing*
must not send their text either: the commit carries it, so `KeyStroke::composing`
reaches the encoder, and without it a composition types the romaji as well as the
kanji. `set_ime_allowed(true)` is what makes a pinyin keyboard stop typing latin
letters at the shell, and `set_ime_cursor_area` (per caret cell, deduplicated) is
what puts the candidate window next to the text instead of in a corner.

The other half is a trap that costs a panic: **egui's bundled faces are latin-only
and it panics on a missing glyph** (`No glyph for "に" in Monospace`). A terminal
is exactly where CJK is typed — the preedit box, and any palette query — so
`Look::install` appends `fonts::ui_fallback()` (the grid's own preference list,
read once at startup, `swift`-checked to have kana *and* Han) to both families.

**A click is encoded where the modes are, and the UI has to ask first.**
`Command::Mouse` carries a pane-relative pixel position, the button, the modifiers and
whether one is held; the pane's `mouse::Encoder` reads tracking mode and output format
from the terminal per event, as the key encoder does — `?9h` presses only, `?1000h`
presses and releases, `?1002h`/`?1003h` motion with or without a button held, `?1006h`
the SGR form. nvim asks `?1002h?1006h`, and `a_click_moves_a_real_editor_cursor` puts a
click through it.

The UI cannot know what a press means without knowing whether the program tracked the
mouse, and that mode lives in a `!Send` terminal, so `Stats::mouse()` is an atomic
published by `note_modes` after each parsed burst. Without it a click in vim starts a
*selection*, and the gesture the program wanted is the one it never gets. Shift hands the
mouse back (`mouse_goes_to_program`), or `set mouse=a` would make text impossible to copy.

Two traps. **`Terminal::resize`'s pixel arguments are the cell's, not the pane's** — the
pane size put every click in the top-left cell, which is how that was found. And a press
outside the pane is not the program's, while a drag that leaves it still is, so
`pane_pixels` clamps and only presses are refused. A wheel notch is buttons 4 and 5 as a
press *and* a release, because X10 has no wheel event; shift sends it to the scrollback.

**OSC 52 writes arrive on the pane thread, not the UI.** The VT core calls
`Terminal::on_clipboard_write` synchronously while parsing bytes, so the handler
only parks the text in the pane's `copied` slot; the UI does the clipboard work
and raises the toast. Replying `Err` for a non-text MIME is what tells the program
the write was refused — returning without replying denies it silently.

**A pty is cooked until somebody says otherwise.** `openpty` hands back a terminal
in the kernel's default mode, which means a second line discipline sits in front of
the VT core: every byte the session writes is scanned for output post-processing,
`\n` becomes `\r\n` one character at a time, input is line-buffered and echoed by
the kernel, and ^S stops the flow. dtr never set raw, and the cost was extreme — a
32 MiB flood of two-byte lines drained in **3.9 s cooked against 37 ms raw**. What
proved where the time went was the control: the same payload through `script`,
which parses nothing at all, took 4.06 s, so the time was the *session's* syscalls
and not dtr's. `pty::set_raw` runs before the child is spawned now, and
`the_session_starts_raw` reads the termios back from the master and fails if any of
it comes back cooked.

`ISIG` is the one flag kept: ^C has to reach the foreground process, and a program
that wants the byte (readline) turns it off itself for as long as it wants it.

**A frame per read chunk is a thousand frames a second nobody sees.** The pane
publishes on a gate of half a refresh interval, with a timer arm so a skipped
publish cannot leave the picture stale; the measurements behind that (1.1 million
publishes and 57 s of CPU became 484 and 31 ms) are in PLAN.

## Profiling

`./scripts/flamegraph.sh --demo` builds the `profiling` profile (release + debug
info) and samples a real window. It uses `perf` through `cargo flamegraph` when
the kernel allows recording, and otherwise `dtr --cpu-profile <path>`, which
samples itself and needs no privileges. `--cpu-profile /tmp/x.svg` always prints
a text self-time report; the `.svg` suffix adds the graph. `--exit-after MS`
closes the window by itself, which is how shutdown gets exercised without a
window manager.

**The low-power adapter is the integrated GPU.** `PowerPreference::LowPower` hands
you the iGPU on a machine with a discrete card, which is the wrong answer for a
terminal someone stares at. `--gpu <substring>` (or `DTR_GPU`) picks by adapter
name; `-v` logs the adapter and every adapter seen.

## Benchmarking

`./scripts/bench.sh` runs vtebench — Alacritty's PTY-read benchmark, the one every
terminal comparison quotes — against dtr, and prints a table with the delta from
the previous run. The runner is fetched at a pinned revision into the cache; the
`benchmarks/` directory is ours and its payloads check with `--check`.

vtebench measures exactly one thing, and its README says so: how fast the
terminal reads its PTY. It writes the payload to *its own* stdout, so it has to
run inside the terminal being measured — the script starts dtr with vtebench as
the shell (`--shell`) and, when vtebench exits, the tab and the window go with
it. That is why the numbers arrive in a `.dat` file instead of on the screen.

Reading the table:

- **Absolute numbers only mean anything on one machine against itself.** A CI
  runner and a laptop have different fonts, GPUs and compositors.
- **The previous run is the baseline**, so bench one change at a time, and
  rebuild first: a stale binary makes the number describe something else. The
  delta is a *rate*, not a time, and the sample size is recorded next to each
  `.dat` for exactly that reason: the table reads milliseconds, but comparing
  two runs that measured different amounts of work is how a 4× payload change
  looks like a 4× regression.
- **vtebench records whole milliseconds**, so a 1 MiB sample from a fast
  terminal quantises to 2 or 3 ms, and dtr now drains about a gigabyte a
  second. The script's default is 32 MiB per sample, near 30 ms; a median of
  `<1 ms` is a failed measurement and it says so.
- **A sample is bounded by whichever is slower, reading or drawing**, so a
  renderer that cannot keep up shows up here as well. `--profile` writes
  `target/bench/profile.svg` for the same flood, and that is where to look once
  a delta says something regressed.

Two payloads are ours: `wide_cells` walks the double-width path (fallback face,
bitmap fitted into two cells) and `cwd_reports` floods the OSC 7 sniffer, which
sits in front of the VT core on every byte. The rest are upstream's.

`scripts/bench.sh --check` validates the payloads without a window, a GPU or a
network, and is what CI runs — a benchmark that silently produces nothing is
worse than no benchmark.

**The reader can be measured without a window.** `--screenshot PATH --screenshot-delay MS`
spawns the session and pumps it with nothing on screen, so a payload can be timed
and profiled on a machine with no display: point `--shell` at it and read the `.dat`
it writes. Only the final image needs an adapter — a software Vulkan ICD
(`WGPU_BACKEND=vulkan`, lavapipe) is enough when there is no GPU. That is how the
pty's cooked mode and the per-chunk publishing were found.

## Gotchas that are real

| Symptom | Cause |
| --- | --- |
| `could not find native static library 'ghostty-vt-static.lib'` | `PKG_CONFIG_PATH` from the devShell wins over `PKG_CONFIG_LIBDIR`; set `PKG_CONFIG_PATH=""` in the Windows build |
| `unable to find dynamic system library "synchronization"` | Windows needs an empty `libsynchronization.a` stub on the link path |
| `libudev-sys` build failure | `serialport` enumerates through libudev on Linux; udev's `.pc` must be on `PKG_CONFIG_PATH` in both the package and the devShell |
| `The wayland library could not be loaded` | winit/wgpu `dlopen` their backends, so the wrapper needs `LD_LIBRARY_PATH`, not rpath |
| `-Werror=date-time` from mimalloc | zig's cc enables it; `CFLAGS_x86_64_pc_windows_gnu="-Wno-error=date-time"` |
| Wine: FXC shader error building egui | Wine finds dx12 first and cannot compile it; `WGPU_BACKEND=vulkan` |
| `Dropped TexturesDelta with 1 unapplied deltas` | egui panics unless `textures_delta.clear()` is called |
| Undefined `ghostty_terminal_mode_get` | crates.io `libghostty-vt 0.2.2` is broken; the git rev in `Cargo.toml` is the pinned one |
| `Terminal` cannot cross threads | it is `!Send`; each pane owns its own thread and `LocalSet` |
| fish renders nothing, or writes nothing | it needs writable `XDG_*` dirs in the sandbox, and the terminal must reply on `on_pty_write` (a shell waits for its answer) |
| `cargo test` does not refresh the binary | build first; `target/debug/dtr` is what you run, tests do not rebuild it |
| A removed pane takes two panes with it | the recursive remove returned "delete me" and the grandparent deleted the collapsed subtree; only the direct parent may collapse |
| Line height is far too small | swash reports `descent` as a *positive* distance below the baseline; negating it halves the cell |

## Where to look

- `PLAN.md` — design, milestones, spike findings, and the reasoning behind
  decisions that were reversed.
- `docs/sftp.md` — measured decision memo for the SFTP idea.
- `docs/themes.md` — which terminal theme formats dtr could read.
- `crates/dtr/src/pane.rs` — transports, publish, reconnect, modem, paste.
- `crates/dtr/src/app.rs` — the event loop, keys, and the palette glue.
- `crates/dtr/src/layout.rs` — the split tree. It knows nothing about terminals;
  a leaf is an id, and the tab's map decides what that id is.
- `crates/dtr/src/render.rs` — glyph atlas and the instanced grid pass.
