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

**A key release carries the same `logical_key` as its press.** Only `text` differs,
and `encode` falls back to the character when `text` is empty — so encoding every
`KeyboardInput` sends each keystroke twice ("typing `ls` shows `lsls`"). `input::encode`
drops anything whose state is not `Pressed`, and after the key handler moved into
`App::on_key` the guard lives there instead — the send-to-shell branch returns
early on any release. This regressed once already during that move: "ls" typed as
"lsls". A scripted keystroke now sends press *and* release, so the harness can
catch it.

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

**The egui layer is tested with `egui_kittest`, not with a window.** `cargo test`
drives a `Harness` — real egui layout and AccessKit, no event loop — so clicks,
keys and rendering can be asserted. `crates/dtr/src/ui.rs` has the pattern: query
a widget by its accessible label, `.click()`, `harness.run()`, assert what the
widget reported. `UPDATE_SNAPSHOTS=1 cargo test` writes a baseline PNG under
`crates/dtr/tests/snapshots/`; the plain run compares against it.

Two things it taught, both real bugs: a widget painted by hand has **no
accessibility node** unless `Response::widget_info` declares one (the tab chips
were invisible to the harness *and* to a screen reader), and a widget is not a
`Button` just because it is clickable — `Sense::click()` plus a declared label
gives a hit target that Enter cannot activate.

**egui's default fonts have no arrows.** `↑↓` and `←→` in the palette and settings
hints rendered as tofu: the harness panicked with "No glyph for U+2191 in
Proportional", and the window had been drawing empty boxes in those hints all
along. Hints are words now (`up/down`, `left/right`), which also reads better in a
terminal. Any new UI text should assume Latin-1 only until dtr ships its own UI
font.

**Keys can be typed at the window from a script.** `--keys "ctrl-shift-p;down;down"` applies one
stroke per frame through the same handler a keyboard uses (`App::on_key`), and
`--capture PATH --capture-after MS` photographs the window afterwards. `;`
separates tokens because a chord may be a comma; `+` joins the chords of one
binding; `text:` types characters (only meaningful with a real shell, not
`--demo`). Together those two flags are how a human-free run checks that a menu
opens, that a chord is wired, or that a font renders in the window.

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
icon. The fix is disjoint rects: the chip only reserves space (`Sense::hover`),
the label and the cross are separate `interact` regions that do not overlap, and
icons are drawn with `Image::paint_at`, which cannot sense anything.

**A switch that appears on the next keystroke is a stale frame.** `Ctrl+Tab` was
reported as "switch to the other tab, press Enter, and it goes back" — the log
shows the switch happening on the chord and nothing moving it afterwards. The
switch was always correct; the *picture* was late, and the next keypress was what
made a frame. Same root cause as the closed tab that only vanished when the mouse
moved, which is why fixing the loop's control flow made both symptoms disappear.

Every tab switch now goes through `App::set_active`, which logs `active tab
from=… to=… why=…`. Five call sites can do it and only one is the mouse; the log
names the one that did.

**Hover is worth a redraw.** Skipping the repaint for pointer motion that cannot
change anything was tried and rejected: the strip lights up under the pointer, and
that is worth a frame per refresh. So a terminal redraws while the mouse moves over
it — expected, not a leak. The chips and the `+` lighten on hover for exactly this
reason; a chip that looks identical under the pointer reads as decoration.

**A chip has two hit targets.** The label switches tabs, a cross on the right closes
that tab — whichever tab is active. Both are pointer-only `Sense::click` regions
with declared accessibility names (`2 shell`, `close tab 2`), so Enter cannot
touch either. The cross is *drawn* with two lines, not a `×` glyph: egui's default
font has no multiplication sign and would draw an empty box.

Closing the last tab closes the window, the same rule `close_active` follows: a
terminal with no sessions has nothing to show.

**Tabs were invisible until the strip existed.** The top strip is reserved in the grid
layout only when there is more than one tab, so a single session loses no rows;
the active tab's title follows its focused session, the others keep the title they
had. A click on a chip switches tabs through the same `self.active` the chords use.

**A tab chip is a hit target, not a control.** egui buttons take keyboard focus, so
Enter (and Space) activated the focused chip — pressing Enter in the shell switched
tabs. The chips are painted and hit-tested by hand with `Sense::click()`, which takes
the pointer and nothing else.

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

**Selecting with the mouse copies.** The drag's release sends
`Command::CopyRange` for any selection wider than one cell; the pane builds the
text from the terminal, parks it, and the next frame hands it to
`ui.ctx().copy_text`, which `egui_state.handle_platform_output` puts on the system
clipboard. -The release handler used to only clear `dragging`, so a mouse
selection produced text on screen and nothing in the clipboard.

The pointer now goes through `App::pointer_down/move/up`, which `--drag
x1,y1,x2,y2` also calls: the copy path can be exercised without a mouse. That
option is only verifiable where an X display is reachable — a headless shell has
no clipboard and winit refuses to open.

**A copy cannot use the published frame, because drawing it is what removes it.**
The UI takes the frame out of the pane's slot to render it, so a copy that reads
that slot gets `None` and copies the empty string — which shipped as "copied 0
chars" with a selection on screen. `Command::Copy`/`CopyRange` rebuild the text
from the terminal at copy time instead; the pane is the only place that still
holds the cells.

**OSC 52 writes arrive on the pane thread, not the UI.** The VT core calls
`Terminal::on_clipboard_write` synchronously while parsing bytes, so the handler
only parks the text in the pane's `copied` slot; the UI does the clipboard work
and raises the toast. Replying `Err` for a non-text MIME is what tells the program
the write was refused — returning without replying denies it silently.

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
