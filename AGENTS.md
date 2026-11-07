# Working on dtr

Notes for anyone — human or agent — changing this repository. Design and
milestones live in [PLAN.md](PLAN.md); the TTL specification lives in
[crates/dtr-ttl/MANUAL.md](crates/dtr-ttl/MANUAL.md). This file is the stuff that
bites you while you are in there.

## What it is

A serial-first terminal in Rust: `libghostty-vt` for the VT core, egui/wgpu for the
window, one OS thread per pane. `crates/` holds the workspace:

| crate | what |
| --- | --- |
| `dtr` | the binary |
| `dtr-config` | the config file (`toml_edit`) |
| `dtr-complete` | the palette's completion engine |
| `dtr-store` | the redb logbook |
| `dtr-sshconfig` | the ssh config parser |
| `dtr-sftp` | the SFTP client |
| `dtr-vfs` | file backends: local, in-memory, ssh |
| `dtr-ttl` | the Tera Term macro language — manual, corpus, and an interpreter that runs the corpus |

## Building and testing

The toolchain is not on `PATH`; everything goes through the flake.

```sh
nix develop .#default --command bash -c 'cargo build -p dtr'
nix develop .#default --command bash -c 'cargo test --workspace'
nix build .#dtr                              # Linux package
nix build .#windows                          # the one-file Windows exe (cross)
nix build .#checks.x86_64-linux.single-exe   # asserts the one-file property
nix flake check                              # every gate CI runs, locally
```

If `cargo build` cannot find libghostty's pkg-config file (the devShell hook sets
this, not every shell):

```sh
export PKG_CONFIG_PATH=$(nix build .#libghostty-vt.dev --no-link --print-out-paths)/share/pkgconfig
```

A fake serial device: `socat pty,raw,echo=0,link=/tmp/dtr-serial EXEC:'some
script',pty,raw,echo=0` — killable mid-session to exercise reconnect. Kill it with
`pkill -x socat`; `pkill -f "socat pty"` matches the shell running it and kills
your own test.

**The CI gates live in the flake.** `nix flake check` is the whole pipeline, on
both forges: `.gitlab-ci.yml` has two jobs on the `nix` runner (`check` runs it,
`build` produces `.#windows` for download), and `.github/workflows/ci.yml` runs
the same command on the GitHub mirror. Everything worth failing a branch over is a
`checks.x86_64-linux` entry — `tests` (the whole suite, through lavapipe and the
store's fonts), `bench-payloads` (`scripts/bench.sh --check`), `fmt` (`cargo fmt
--all -- --check`, with the toolchain's own rustfmt), `desktop-entry`, `just-fmt`,
`deny` and `single-exe` — so a local `nix flake check` runs the same gates
as either pipeline. `single-exe` also cross-compiles `.#windows`, because a Linux-only
build would not catch a cross-compilation break. Put a new gate there, not in YAML.
Snapshots are generated here and compared byte-exactly there; if a font or renderer
ever disagrees, the answer is a threshold, not a baseline regenerated from
whichever machine ran last.

`fmt` exists because the tree had never been `cargo fmt`ed: the only way to find out
was `just fmt`, which rewrote two thousand lines in the middle of a change, and the
first commit after the gate was 54 files of pure layout (`f46bf48`). Run `just fmt`
before committing; if you forget, this is what tells you, seconds after the build
starts rather than after the suite.

`deny` is the supply-chain half of that: [`deny.toml`](deny.toml) allows a permissive
licence set (the Windows exe is statically linked and handed out as a download),
denies crates.io-only sources other than the pinned VT core, and refuses wildcard
dependencies. The one check that is *not* a flake check is the advisory half:
it fetches the RustSec database, so GitHub runs `cargo deny check advisories` after
`nix flake check`, and `just deny` runs all four locally. An ignore in `deny.toml` is
a claim that the advisory cannot be reached; write the reason down.

The lab runner is the fast one — its store is already warm — and GitHub is the
public one; both run the same command, so a red job means the same thing on
either. `just --list` is the short list of tasks; `just check` is `nix flake
check`, and `just test` is the suite without the sandbox.

## Releasing

`just release X.Y.Z` is the whole thing: it refuses a dirty tree or a branch other
than `master`, rewrites the workspace version in `Cargo.toml` (the only place it
lives — `flake.nix` imports it, so the package, the exe and the tag cannot
disagree), refreshes `Cargo.lock` through `cargo metadata`, proves the flake
reports the same number, then commits, tags `vX.Y.Z` and pushes.

`.github/workflows/release.yml` does the rest on the tag: it refuses a tag that
disagrees with `Cargo.toml`, runs the full `nix flake check`, builds `dtr.exe`,
and publishes it with `--generate-notes` (a `-suffix` in the version makes it a
pre-release). Re-running it replaces the assets rather than failing on names that
already exist.

**The tag is what triggers the release**, and the repository is mirrored GitLab →
GitHub: if the mirror does not copy tags, nothing publishes — push it by hand
(`git push github vX.Y.Z`) or turn tag mirroring on.

## The house rules

- **Edit files with the editor tools, not shell redirection.**
- **Never `git add -A`.** Subagents write files while you work and `-A` sweeps them
  into your commit. Stage explicit paths: `git add crates Cargo.toml Cargo.lock
  flake.nix PLAN.md`.
- **`git add` before `nix build`.** A flake build sees only git-tracked files, so an
  untracked source file produces a "file not found" or a phantom error about a
  *different* crate. Two `#[cfg(test)]` mistakes were caught only this way.
- **Screenshots are in-app only**, never a compositor grab. `--screenshot` renders
  offscreen from injected state; `--capture PATH --capture-after MS` copies the live
  window's surface. When a menu or a font differs between them, `--capture` is the
  one that tells the truth — and the offscreen path renders at 1×, so it cannot show a
  HiDPI bug at all (`docs/windows.md` §2).
- **Capture with a shell that does not read the user's bash rc.** dtr launches
  `$SHELL`, so a broken `~/.bashrc` ends up *in the grid*: readline's `\[` and `\]`
  prompt markers are printed literally, the prompt grows to half a screen, and it
  reads like a renderer bug. Use `--shell "bash --norc --noprofile"` (the flag
  splits on whitespace) for any capture, screenshot or font comparison, and the
  same for a payload that a shell rc would otherwise decorate.
- **A captured sample must print `\r\n`, not `\n`.** The pty is raw, so nothing turns
  a line feed into a carriage return for the child: a script that ends its lines with
  `\n` draws a *staircase*, each line starting where the last one ended, and the
  glyphs look misplaced. It is the harness, not the renderer.
- Commits end with the co-author trailer.

## The event loop, redraws, and doing work off the UI thread

**`set_control_flow` is sticky state, not a per-iteration timeout.** Setting
`WaitUntil(now)` and then setting nothing on iterations where nothing was due left
the deadline in the past and every `poll` timed out instantly: 47,000 loop
iterations a second against 5 frames drawn. `about_to_wait` collects the earliest
deadline and sets exactly one value every time, defaulting to `Wait`. Any new
deadline belongs in that `sooner(...)` list, never in its own `set_control_flow`.

**A repaint can be requested by egui, and has to be carried.** Opening the `+` menu
drew nothing until the mouse moved, because a popup appears on the *next* frame and
nothing in dtr called repaint — the request was egui's. After each frame the app
asks `has_requested_repaint()` and repaints. Popups, tooltips and hover all depend
on it.

**`request_redraw` inside a redraw does not draw on Wayland.** It waits for the
compositor's next frame callback, which arrives with the next input event. So
`App::repaint` also sets a `wake` flag that `about_to_wait` turns into
`WaitUntil(now)` plus another request. This is the shape of "the UI only updates
when I touch it": check the loop before blaming the work.

**Never feed `RedrawRequested` to egui.** egui replies "yes, repaint", we ask winit
for another frame, and the app spins at refresh rate while looking idle. Every
event *except* `RedrawRequested` goes to `egui_state.on_window_event`.

`-v` logs `loop iterations_per_s= frames_per_s= wakes=` once a second, which is what
separates a real spin (many iterations, few frames) from drawing at refresh rate
(frames == iterations) and an idle terminal (both near zero) — a profile that is 77%
inside `poll` looks identical in all three. It also logs a `slow frame` when the
draw exceeds a threshold, timed to `submit`: the first version included `present`,
so every frame at 120 Hz was "8.2 ms" — that is a vsync, not work.

**The UI thread asks for slow work, it does not do it.** Serial enumeration touches
udev or SetupAPI and the ssh config is a file read; on the drawing thread either is
a dropped frame. `App::request_scan` hands work to a one-thread runtime
(`App::worker`) and the result arrives as a `UserEvent`; `scanning` stops duplicates
and `scan_due` is the poll interval. Files work has its own runtime (`App::files_rt`)
so a directory walk cannot queue behind port enumeration. Closing a tab is teardown —
a dropped pty thread has to be noticed by the child — so `close_tab` hands the dead
tab to `spawn_blocking` and logs how long it took.

**A capture needs a frame, and an idle window draws none.** `--capture` happens
while drawing, so a settled window produces no picture and the deadline passes.
Waking from `about_to_wait` was believed to fix it and does not on a plain shell
window — still open.

## Panes, publishing, and geometry

**`Terminal` is `!Send`.** Each pane owns its own thread with a current-thread tokio
runtime and a `LocalSet`; everything that touches the terminal happens there and the
UI talks to it with commands and slots.

**A pty is cooked until somebody says otherwise.** `openpty` hands back a terminal in
the kernel's default mode, so a second line discipline sits in front of the VT core:
output post-processing, `\n` rewritten to `\r\n` a character at a time, input
line-buffered and echoed by the kernel, ^S stopping the flow. dtr never set raw and
the cost was extreme — a 32 MiB flood of two-byte lines drained in **3.9 s cooked
against 37 ms raw**; the control that proved where the time went was the same payload
through `script`, which parses nothing at all and still took 4.06 s, so it was the
session's syscalls and not dtr's. `pty::set_raw` runs *before* the child is spawned,
and `the_session_starts_raw` reads termios back from the master. `ISIG` is the one
flag kept, because ^C has to reach the foreground process.

**A clean frame is not a changed frame.** `publish` skips `Dirty::Clean`, so the
first publish after a session starts is forced — otherwise a pane with no output
would show nothing at all.

**A cursor is not a dirty cell.** Hiding it, showing it, or asking for nvim's insert
bar changes no cell, and the render state counts only cells. `publish` compares the
`Cursor` it is about to publish (position plus DECSCUSR's style) against the one the
last frame carried, and a hidden cursor is nothing to draw. That one comparison is
the only gate for anything non-cell — do not add a second.

**A frame per read chunk is a thousand frames nobody sees.** The pane publishes on a
gate of half a refresh interval, with a timer arm so a skipped publish cannot leave
the picture stale. (1.1 million publishes and 57 s of CPU became 484 and 31 ms.)

**`Terminal::resize`'s pixel arguments are the cell's, not the pane's.** Passing the
pane size put every click in the top-left cell.

**A resize must re-fit the terminal, not just the surface.** `WindowEvent::Resized`
reconfigured the swapchain only, so the pty kept the old rows and columns and the
published grid was the wrong shape. Resize now calls `fit_panes`; any future per-size
state belongs there.

**Two layouts must agree on the top inset.** `fit_panes` sizes the terminal and
`pane_views` draws it; when only one knew about the tab strip, the pty kept the rows
of a taller area and the first prompt row was drawn underneath it. Both ask
`App::top_inset`.

**With `chrome = "client"` that strip is also the window's title bar.** The same 30 points
hold the drag region, the eight resize borders and the platform's three buttons, and the
reservation is unchanged — the row moved, its height did not (`crate::chrome`). The drag
region is keyed off what `ui::tab_bar` allocated and stops at `buttons_rect`, because a
drag area laid over the chips is how a press becomes "sometimes unresponsive".

**`inner_size()` is already physical, so a "scale" divided by it is always 1.0.**
`Gpu::scale()` was physical/physical and the DPI factor never entered it; every
consumer then reserved half the space it needed on a HiDPI display (a 34 px inset
over a 30 **point** strip), which is why it reproduced on the laptop and not in the
captures here. `scale()` is `window.scale_factor()` now, and `-v` logs `strip
geometry` once per scale. The units rule: egui works in points and applies the DPI
factor itself; `top_inset` and grid rects are physical pixels. Layout constants are
points, grid sizes are physical, and the scale is applied exactly once.

**The grid's font size is a point size and the rasteriser wants pixels, so the
multiply happens in `Fonts::load`.** That was the last exception to the rule above,
and it was a visible one: at 125 % the cell stayed 9 × 17 while the tab strip grew,
so the text was smaller than the UI and the pane gained 25 columns
(`docs/windows.md` §2.1). `GridRenderer::new` and `Fonts::load` take the factor,
`Metrics` and the atlas key are physical from there on, and everything downstream —
the cell, the fallback `adjust`, the sprite canvas — inherits it for free. A scale
change sets `fonts_dirty`, which rebuilds the grid and lets `fit_panes` re-fit the
pty; nothing else may multiply by the scale. `STATUS_HEIGHT` is drawn in *points*
(egui multiplies) over a reservation in pixels — multiplying it by the scale in
points is how the bar grew as `scale²` and covered the bottom row.

**A shell has no status bar; the strip is reserved even for one tab.** The bar exists
for a serial port's counters and modem lines and is not drawn (nor reserved) when the
focused session has none. The tab strip is always drawn, because it is where a shell
is named — reserving it only when several tabs exist is how the prompt ended up
underneath it.

**`Gpu`'s fields drop in declaration order and it matters.** Anything holding a
texture, buffer or pipeline must drop before the device; the surface before the
window it views; the window last. Wrong order segfaults on close instead of failing
loudly.

**Panes share one instance buffer, and queue writes land before submission.** A
per-pane draw would rewrite the buffer between panes and the last pane would win,
which is why `render::prepare` takes every visible pane at once. Cells outside a
pane's rectangle are not emitted rather than scissored.

**`O` and `0` look identical in egui's default monospace**, so a screen showing `OK`
looks like `0K` in a screenshot. Before touching the render path:

```sh
nix develop .#default --command bash -c 'cargo test -p dtr --bin dtr the_vt_core'
```

If that passes, the cells are right and you are looking at a font ambiguity.

## Keys, mouse, IME, selection

**The program decides what a key means, and only the pane knows.** `Command::Key`
carries a *stroke*, not bytes; the pane's `key::Encoder` picks the sequence after
`set_options_from_terminal`, because what a key sends is the terminal's state:
`DECCKM` makes an arrow `ESC O A`; `DECKPAM` moves the keypad; mode 1036 prefixes alt
with `ESC`; `modifyOtherKeys` respells ctrl and alt; the kitty protocol (`CSI > flags
u`, a *stack*) turns ctrl+a into `CSI 97;5u` and can report releases. A table in the
UI cannot be right.

**A key release carries the same `logical_key` as its press**, so encoding both types
every keystroke twice ("typing `ls` shows `lsls`"). A release encodes to nothing
unless the program asked the kitty protocol for releases — that is the encoder's
rule, not a guard in the UI.

**A shifted character the platform resolved is that character, not the key plus a
shift bit.** `input::fill` has to tell the encoder which modifiers the *text* already
accounts for (`consumed_mods`), or shift+`;` goes out as `CSI 59;2u` and the program is
left to apply the shift itself. Neovim uppercases letters and nothing else, so `:` and
`#` never arrived while `A` did. winit has no such notion — GTK's
`getConsumedModifiers` is what ghostty uses — so it is derived: shift went into the
character when the text is one character that is not the key's unshifted one. A key
with no unshifted character keeps its shift, because `<S-CR>` is a key a program can
ask for.

Two more: the *position* decides, not the character (a real event has `physical_key`;
`--keys` implies one from a US layout), and a ctrl or super combination must not pass
its `text` through while a plain character must, or accents and CJK never arrive.

**…except where the position is a legend and not a key.** A layout exists to rewrite
keys, and `caps:escape` is the one everybody uses: winit then reports the position
`CapsLock` with the logical key `Escape`, and the encoder has no bytes for `CapsLock`
at all — so the keystroke vanished. It read as "Caps Lock does not work in dtr" while
every dtr menu still closed on it, because those look at the logical key. `input::key_of`
takes the layout's answer for the keys that only ever carry a legend (`CapsLock`,
`NumLock`, `ScrollLock`, `Fn`, `FnLock`) and keeps the position for everything else; a
keypad key is the counter-example, because `DECKPAM` spells `Numpad8` differently from
`ArrowUp`. The test drives a pty in *both* encodings (`ESC` and kitty's `CSI 27 u`),
because the bug was silence and a test on the mapping alone would have called it fixed.

**A click is encoded where the modes are.** `Command::Mouse` carries a pane-relative
position, the button, modifiers and whether one is held; the pane's `mouse::Encoder`
reads tracking mode and output format per event — `?9h` presses only, `?1000h`
presses and releases, `?1002h`/`?1003h` motion, `?1006h` SGR. The UI cannot know what
a press means without knowing whether the program tracks the mouse, and that mode
lives in a `!Send` terminal, so `Stats::mouse()` is an atomic published by
`note_modes` after each parsed burst. Without it a click in vim starts a *selection*.
Shift hands the mouse back, or `set mouse=a` would make text impossible to copy.

A press outside the pane is not the program's while a drag that leaves it still is,
so `pane_pixels` clamps and only presses are refused. A wheel notch is buttons 4 and
5 as a press *and* a release (X10 has no wheel event); shift sends it to the
scrollback.

**A paste is the program's too.** `pane::pasted` turns line breaks into CR and wraps
the text in `ESC[200~ … ESC[201~` when the program asked with `?2004h`.

**IME text is not a keystroke, and egui has no CJK.** The preedit is only a picture —
`crate::ime` draws it in a box above the caret — and nothing reaches the pty until
the commit, which is what makes a composition cancelable. Keys typed *while composing*
must not send their text either, hence `KeyStroke::composing`; without it a
composition types the romaji as well as the kanji. `set_ime_allowed(true)` stops a
pinyin keyboard typing latin at the shell, and `set_ime_cursor_area` (per caret cell,
deduplicated) puts the candidate window next to the text.

**egui's bundled faces are latin-only and it panics on a missing glyph** (`No glyph
for "に" in Monospace`) — a terminal is exactly where CJK is typed. `Look::install`
appends `fonts::ui_fallback()` to both families, checked at startup to have kana
*and* Han.

**A selection is anchored to characters, not to the grid.** Cell indices name
whichever character is at a row later; a scroll moves the block and the highlight
stays. The pane holds two `TrackedGridRef`s, which follow the page list through
scrolling, pruning and reflow, and resolves them on *every* publish — which is also
why the frame carries the range rather than the UI holding one.

`PointSpace::Viewport` resolves but does not bound (a cell thirteen rows below a
five-row viewport comes back as `y = 13`), so the range check is ours. Once either end
leaves the frame the selection is **dropped, not clamped**: a highlight stretched to
the border claims cells it does not cover and a copy hands back a fragment that looks
like the whole thing. A press that has not moved is an anchor and not a selection, so
a single click deselects; a word, a line or a drag is `deliberate`, so its one-cell
case still paints.

**Double- and triple-click are counted, not reported.** `App::count_clicks` counts a
press within 400 ms and four pixels. Two takes the word, three the line, both via the
core's `select_word`/`select_line` so boundaries break where they do in every other
terminal. A drag that stays inside the press's own cell changes nothing — a pixel of
jitter must not turn a word into a one-character range — and a double-click copies on
release without having moved.

**A copy cannot use the published frame, because drawing it is what removes it.** The
UI takes the frame out of the pane's slot to render, so a copy reading that slot gets
`None` and copies the empty string (shipped as "copied 0 chars" with a selection on
screen). `Command::Copy`/`CopySelection` rebuild the text from the terminal at copy
time; the pane is the only place that still holds the cells.

**A toast that says "copied" is not a clipboard.** `ui.ctx().copy_text` goes through
`egui-winit`, whose clipboard falls back to an *in-app buffer* when it cannot reach
the display. dtr also writes the clipboard itself with `arboard`, keeping the handle
on `App` for the process's life (on X11 the owner must stay alive or the text
disappears). Failures are logged; silence is not success.

**OSC 52 writes arrive on the pane thread.** The core calls `on_clipboard_write`
synchronously while parsing, so the handler only parks the text in a slot and the UI
does the clipboard work; replying `Err` for a non-text MIME is what tells the program
the write was refused.

## The grid, fonts, and glyphs

**A missing glyph is not a missing character.** The grid face covers Latin; CJK and
emoji come from another family. `Fonts::face_for` asks the grid face, then the
fallback faces already loaded, then walks every installed family loading one and
testing its charmap, caching per character — 2000 distinct Han characters cost one
extra font load, not 2000.

Which family answers is a written policy, not fontconfig's order — that order used to
change between runs, which is the bug (`docs/cjk.md` §5, M4). `Fonts::face_for` asks:
the faces already loaded, then `[font] fallback` from the config, then
`fonts::FALLBACK` (Sarasa Mono, then the Noto CJK families, then the Nerd Font
families), then the platform's script preference from fontique, then a walk over every
installed family with **Nerd Fonts first and monospaced before proportional**. The
first four are opinions, so they win even when proportional; the walk is a search, so
monospace decides there — a proportional face with a stray private-use glyph must not
draw an icon. A terminal wants a monospaced CJK face or the wide characters carry
their own advance and the grid stops lining up. `-v` logs `fallback face` with the
family, its adjustment and where it came from.

**A charmap entry is not ink.** Noto Color Emoji maps every emoji and has no outlines,
so the walk accepted it and drew nothing; `has_ink` rasterises the candidate glyph and
skips a face whose box is empty. A terminal that draws nothing is worse than one that
draws the wrong glyph.

**No grid face is not an empty grid, it is Emoji.** `load_faces` tries a preference
list (`GRID_CANDIDATES`), and every entry in it used to be a Linux family, with
`"monospace"` last — which is a *generic* family, not a name, so `family_by_name` never
resolved it. On a fresh Windows the grid therefore had no face at all, `face_for` fell
through to the first fallback with a glyph, and every Latin character was drawn by Segoe
UI Emoji at 1.6× the cell advance: `Microsoft` came out `M crosoft` and an SGR sequence
arrived as literal `[0m`. After the list runs out the platform is asked
(`generic_families(GenericFamily::Monospace)`), which is the part that works on a machine
this repository has never run on; adding a family to `GRID_CANDIDATES` or
`fonts::FALLBACK` is how a new OS gets its faces. `--font-report` in the `winvm` guest is
how the answer is checked (`docs/cjk.md` §5, M7). A *pictograph* (U+1F000–1FAFF, no
script) also skips the already-loaded shortcut, or the symbol face that answered a
powerline arrow hands it a monochrome `😀`.

**A fallback is rasterised at its own optical size.** `Loaded::adjust` is the ratio of
the *ideographic widths* — the advance of `一`, else `min(ascii ink height, two
cells)` — and it has to be that metric: x-height gave 0.92 and cap height 0.89 on
DejaVu/Noto CJK, both of which shrink every Han character, because a CJK face has a
small x-height by design. The bearings still land on the grid's baseline, so only the
ink changes.

Two consequences: the glyph atlas key includes the *face index* (glyph ids collide
between fonts), and every glyph is placed on the **grid's** baseline — `Metrics::baseline`,
from the grid face — not on the ascent of the face that produced it. A fallback's own
ascent only decides where its ink sits relative to that baseline (Ghostty's
`cell_baseline`, `docs/cjk.md` §3.1).

**`啊` is the CJK test character.** `U+554A`: full-width Han with ink in all four
quadrants, so a baseline error, a wrong size or a half-drawn cell shows up in its
numbers and on screen. It is in `FONT_REPORT_SAMPLE` (`dtr --font-report`) and in the
placement tests; use it for a new CJK test or capture rather than inventing another
character, so two people comparing two builds are looking at the same glyph.

**Box drawing, blocks and braille are drawn, not borrowed.** `crates/dtr/src/sprite.rs`
is a port of Ghostty's sprite face (`src/font/sprite/draw/*.zig`): box drawing (the
light/heavy/double arms and every junction, from a table of edges per codepoint, plus
the dashes), block elements (fractions, quadrants, shades as flat alpha) and braille
(eight dots with Ghostty's order of concessions when the cell is too small). The cell
*is* the canvas, so a line reaches the cell's edge and a block fills it at any cell
size — which is the whole point, because a font draws these at its own thickness and
side bearings (`─` in DejaVu is 19 px of ink in a 17 px cell, and two of them
overlap). Not ported, and still from a font: the arcs `╭╮╯╰`, the diagonals `╱╲╳`, the
legacy-computing blocks and the geometric shapes.

Two traps. A sprite's mask is the *whole cell*, so its ink box says nothing about how
much was drawn — assert on the mask's pixels, not on the rectangle. And the sprite is
keyed in the atlas by codepoint in place of a glyph id: with one shared id every box
character draws the first one it cached.

**A wide character is two cells, not one, and neither is a box to fill.** The width
is the terminal's (`Cell::wide()` → `Narrow`/`Wide`/`SpacerTail`/`SpacerHead`), taken
in `fill_cells`; a spacer is not drawn. The glyph is drawn at its design size across
the pair — the old code scaled the *ink* to fill the box, which made a full-width full
stop a 17×17 square (`docs/cjk.md` §1).

**A placed offset is not a length.** `glyph.top` is the distance from the cell's top
edge to the glyph, not a size: the bearings come from the rasteriser relative to the
baseline and were never proportional to the bitmap. One placement rule, in
`render::placement`, shared with `--font-report`; do not add a second copy of it here.

**A glyph is drawn as its font drew it unless a rule says otherwise, and the rules
are Ghostty's.** `render::Constraint` is a port of `RenderOptions.Constraint`
(`font/Glyph.zig`): five size rules, five alignments, pads in thousandths of the face
box, a maximum number of cells, and two height metrics. `constraint_for(codepoint)` is
the only policy: private use and the symbol blocks get `fit_cover1` centred on the
first cell, powerline and braille `stretch`, everything else nothing. Two traps. A
uniform scale belongs on the **outlines** — re-rasterise at `size × factor`, or an
enlarged icon is grey mush, and only `stretch` (which changes the aspect ratio) may
scale the bitmap. And `stretch` is only right where the ink *is* the shape: `─` is
three pixels of ink and stretching it draws a solid block, so box drawing and block
elements are left alone until there is a sprite face (`docs/cjk.md` §5, M6).

**How many cells a glyph may use is the neighbours' answer.** `constraint_width` is
Ghostty's `renderer/cell.zig` rule: a symbol may borrow the cell to its right when
that cell is blank, the glyph to its left was not also a symbol, and it is not the
last column of its row. It changes the *glyph's* room, never the grid — and without
it every other icon in a row drifts right.

**A colour glyph keeps its own pixels, and a mask is tinted.** There are two atlases —
`R8Unorm` for masks and `Rgba8Unorm` for colour bitmaps — and an `f32` flag on the
instance picks which the fragment shader samples. One draw call is the point of this
renderer, so it is a flag and not a second pipeline. `swash`'s source list is
`[ColorOutline(0), ColorBitmap(BestFit), Outline]` and `Image::content` says which one
answered; `has_ink` uses the same list, which is why an outline-only renderer saw Noto
Color Emoji as "draws nothing" and an emoji came out blank. A colour glyph must not be
tinted with the cell's foreground, and a letter must never reach the colour atlas or it
stops being tinted. Presentation selectors (VS15/VS16) do not work: each codepoint of a
cell is drawn on its own, so `❤️` is `❤` (`docs/cjk.md` §4, gap 9).

**egui's default fonts have no arrows.** `↑↓` and `←→` panicked the harness and had
been drawing empty boxes in the window; hints are words now (`up/down`).

**`faint` arrives as a flag, and dropping it is how a ghost disappears.** The core
parses SGR 2 into `style.faint` and `fill_cells` used to read only bold, italic and
inverse, so a program's dimmed suggestion — fish's autosuggestion, an editor's inline
completion — came out exactly as bright as the text it suggested against. Ghostty does
not draw faint either: its renderer uses the flag as the foreground's alpha,
`faint-opacity` (0.5 by default), over the cell's background, and that is what `faint()`
now blends. There is no palette entry called "faint", so a blend is the only version
that can be right on every theme; a fixed grey would not be grey on all of them.

**`invisible` is the same bug with the opposite fix.** A program that conceals input —
a password field, a `read -s` prompt — sends `\e[8m`, and `fill_cells` used to ignore
that flag too. `Cell::concealed` now carries it to `render::prepare`, which skips the
glyph and still draws the background: xterm's rule, and Ghostty's. The characters stay
in the cell because a copy and a capture are built from the frame's text — clearing the
string would hide the text on screen and quietly lose it everywhere else.

## Sessions: shell, ssh, serial

**A shell that exits closes its tab.** The pane cannot — the UI owns the tab — so the
pty reader sets `Pane::ended` on EOF and wakes; `App::reap_ended` closes those tabs
highest index first, and the last one takes the window with it.

**A dropped ssh connection is not a shell that exited, and the exit status is the only
thing that tells them apart.** `ssh` exits 255 when ssh itself failed; anything else
is the remote command's status, which means somebody typed `exit`. `pane::ending` is
that rule. A drop keeps the tab, says so, and reconnects; an `exit` closes it.

**A client that never speaks is worse than one that fails.** A refused connect fails
in milliseconds; a blackhole sits in `connect` for two minutes; and a connection that
goes away without a reset leaves an `ssh` that still looks alive, so the reconnect
never fires. `pty::Config::ssh` therefore passes three options, all **before the
destination**: `ConnectTimeout=10`, `ServerAliveInterval=20`,
`ServerAliveCountMax=3`. ssh's own default is no keepalive at all, which is why this
is not optional. They override `~/.ssh/config`; `ssh_args` builds the list and a test
asserts every flag precedes the alias, because an option after the destination becomes
a command for the far end.

**An ssh failure's cause is its stderr.** With `Stdio::null()` a dead `ssh` reached
the UI as `sender dropped` — "nothing more will arrive", naming no cause. stderr is
piped and quoted with the exit status. `examples/probe.rs` runs the same path from a
shell; `DTR_SSH_CONFIG=/dev/null` gets past a broken config and `DTR_SSH_BATCH=0` lets
a prompt be seen.

**Retries are bounded in rate, not in number.** Waits go 1, 2, 4, 8, 16, 30, 30, …
seconds, forever; closing the tab is how a person gives up. A laptop can be shut for a
day and its session is still worth reopening, which a cap would turn into "closed
while you were not looking". The reset needs ten seconds of *uptime*, not just a
successful connect, so a flapping host keeps the long wait.

Two consequences: the child belongs to the reader thread (`wait` blocks and the status
only exists after EOF), and the first connection is opened inside the event loop by
the same code as every reopen — a host that is down at startup should be a pane that
waits, not an app that exits. That is why `--send` is held in `typed_early` until the
first pty exists instead of being dropped into a `sink` of `None`.

**A reconnected session is put back where it was by the remote command.** `ssh -t
host` starts in `$HOME`, so the reopen is `ssh -t host 'cd <dir> ; exec
"${SHELL:-/bin/sh}" -l'`: the directory quoted by `shlex` for the shell at the other
end (a path can hold anything but `/` and NUL, and NUL is refused), `;` rather than
`&&` so a deleted directory still leaves a usable shell, and `exec`ed so the login
shell *is* the session. `a_hostile_directory_is_still_one_word` round-trips a dozen
hostile paths through `shlex::split`.

**The modes outlive the program that set them.** A drop can happen while vim holds the
alternate screen with mouse modes on. `reset_modes` clears them through the core
before the reopen — including the kitty stack (`CSI < 999 u`) and DECSCUSR's shape and
visibility — while leaving the text, because the last screen before the drop is the
first thing one wants to read. The notice (`-- reconnecting to jupyter in /srv/app
--`) goes into the terminal, not the pty: nothing at the other end would read it, and
a pane that simply stops looks like one waiting for a key.

**A session knows its directory only because it says so.** `/proc` is Linux-only and
an ssh session has no local process, so `CwdSniffer` watches for OSC 7
(`ESC ] 7 ; file://host/path`) in the byte stream before the VT core, keeping the tail
across chunks — a sequence split in two is the normal case, and holding the
introducer back (prefix included, or flushed past `CARRY_MAX` so a bogus introducer
cannot stall the session) is what keeps it parseable.

The **host** in that URI decides the machine: empty, `localhost` or this machine's own
name is local; anything else is another machine, and a path from there must never be
used as a local path or "new tab here" tries to `cd` into a directory that only exists
on the wrong machine. `Session::remote` records the ssh case too. `+` → local shell
starts in the focused tab's directory when that tab is local.

**A prompt mark is a reference, not a row.** OSC 133 is parsed by the core, so
`previous-prompt` (`ctrl-shift-up`) walks `ScrollViewport::DeltaPrompt`. What dtr keeps
is for *capture*: `PromptMarks` records each `A` as a `TrackedGridRef`, because a row
number goes stale the moment the block scrolls. `select_output` is the tempting API
and is not what a pager wants — it returns `None` unless the reference is *inside* the
output, and covers the output only. `capture-block` (`ctrl-shift-o`) parks the text
for `crate::pager`; without shell integration there are no marks and the toast says so.

**fish already speaks OSC 133** — nothing to install. It sends
`ESC ] 133 ; A ; click_events=1 ESC \`, options after a `;` and ST rather than BEL;
bash and zsh need the snippet from the shell-integration proposal sourced from their
prompt.

**The pager is a `Label` in a `ScrollArea`, and that is the whole design.** The text is
in memory, so wrapping, scrolling and mouse selection are egui's job; a line-oriented
pager would re-break every line itself and still disagree with the font. Two costs: the
page height a key scrolls by is only known from the *last* frame (remembered, not
measured), and "go to the end" is `f32::MAX` with the scroll area clamping.

It is **modal** because the keyboard is: `j`, `k`, `b` and space are a shell's keys, and
a leaked keystroke would run a command nobody typed. `App::on_key` hands every key to
the pager first and returns, including ones it does not understand; chords are not pager
keys, so `ctrl+…` falls through to nothing. The pointer is modal for the same reason
(wheel, drag and move skip the terminal), and it draws in the foreground `overlay`
layer because a file pane is an `Area`. Closing drops the text — a captured block can
be megabytes.

**Devices are polled, not watched.** `serialport::available_ports` every couple of
seconds: udev is Linux-only and Windows enumerates COM ports by another route, and one
mechanism that works on both beats a fast one that only exists here. Files are
different — `~/.ssh/config` is watched with `notify`, because that mechanism *is*
portable.

**"No bytes" is not "the reader ended".** The pane keeps the byte sender alive for
reconnects, so `bytes_rx.recv()` never returns `None`; a disconnect arrives on
`closed_rx`. **`reconnecting…` on a transport that cannot disconnect** means
`Stats::connected` was left at its `Default` — only the serial reader turns it false.

## Files, links, and panes

**Clicking a file is a VFS question, and the pane owns the answer.** `dtr-vfs` has
`Location { backend, path }`, a `Backend` trait (`stat`, `open`, `list`) and local,
in-memory and ssh implementations. Three rules:

- **A `Location` comes from the pane, not from a URI.** `file:///etc/hosts` printed on
  `jupyter` means jupyter's file. `BackendId::Ssh` can only be built from a session
  that *is* remote, and `Vfs::backend` refuses it rather than falling back to local.
- **Listings are batches**, because `readdir` answers in chunks and a large directory
  over a slow link is normal. Dropping the receiver cancels the walk.
- **Capabilities are declared.** No remote watch, no directory size; the UI hides what
  is not there.

**A hyperlink is asked about, not carried in the frame.** OSC 8 lives in the terminal,
which never leaves the pane thread, so `Command::HyperlinkAt { x, y }` answers into a
slot. Resolving per cell during publish would be ~8 ns of FFI per cell — 30 µs on a
41 µs publish for a link nobody is pointing at. The reply carries the cell it was
asked about, so a frame-late answer is dropped instead of opening the wrong link.

Only `file:` is understood, percent-decoded, `stat`ed through the VFS and handed to
`xdg-open`/`open`/`explorer` — never a shell, because the path comes from a program's
output. Another scheme is said out loud in a toast. A press on a link opens it and
starts no drag; everywhere else a press still selects.

**The host part decides which machine, and `file://blah` usually has one.**
`ls --hyperlink=always` prints `file://<hostname>/path` — not `localhost` — so a rule
accepting only empty and `localhost` refused every link `ls` ever printed. The rule
lives in `crate::uri`, shared with the OSC 7 sniffer. A local session resolves a path
only for empty/`localhost`/this machine and names the other host in the toast;
a remote session resolves every `file:` path on its own host, which is correct rather
than lax.

**A directory link opens a file pane, not a toast.** The worker sends
`UserEvent::Reveal` and the UI decides where a pane may go — layout is not a worker's
business. Focus stays on the terminal. A pane browsing the *same machine* is
retargeted; another machine is another pane, because a pane reading this disk cannot
become one reading somebody else's.

**A hovered picture is decoded off the drawing thread and capped before it is read.**
16 MiB of source, 8192 pixels a side, 256 MiB of decode; the extension only decides
whether to *try*, so a JPEG named `.png` still works. Three traps:
`DynamicImage::thumbnail` *enlarges* (a 1x1 icon came back 360x360); the texture is
made at draw time because `load_texture` needs an egui context and the worker has no
business owning one; and a refusal is cached, or a row of names re-reads a 2 GB file
five times.

**A remote preview waits, then fetches, and the cap comes before the bytes.** The
pointer must rest for `PREVIEW_DELAY` (350 ms), or a sweep across a row is one `ssh`
fetch per cell; the wait has its own deadline in `about_to_wait`, because a stopped
pointer produces no events. Then `stat` before `materialise` — a remote 400 MB scan
must never be downloaded to find out it is too big — then the decoder on the files
runtime. The cache key is **`{ machine, path }`**, not a path.

The preview is a tooltip-layer `Area`, `interactable(false)` (a file pane is an `Area`
too), placed below-right and flipped at the edges with a clamp last. `CursorLeft`
clears the hover.

**Right-clicking a link is a menu** with `Open`, `Save as...`, `Copy path` — and only a
link *under the pointer* gets one. A left press outside closes it *and goes nowhere
else*, or a click that dismisses a menu also starts a selection. That needs the menu's
rect from the last frame (`LinkMenu::contains`), not this frame's egui pass. Escape is
the menu's own key; any other key closes it and then does what it was going to do. The
image preview is suppressed while the menu is open: the menu opens *at* the pointer and
the preview draws 14 points from it in a layer above.

**The file dialog is the platform's and it blocks.** `rfd` gives the XDG portal (zenity
without one) and the Common Item Dialog on Windows; it runs on the files runtime and
answers with `UserEvent::SavePath`, because on the drawing thread it would freeze the
window. A dialog that never appears returns the same `None` as a cancel, which is why
`LogTracer` is installed — rfd's own `log` line is what `-v` shows. `set_parent` is
what keeps it in front on Windows. It starts at `~/Downloads/<name>`, shared with the
explorer's download. Saving is the same `materialise` a click uses and
then a copy, so "Save as" and "Open" cannot disagree about the bytes; the toast is
raised *before* the transfer, because a slow link is otherwise a silence that reads as
a dead button.

**Materialising is where transparency ends, and it says so.** A local file is its own
path — `materialise` must never copy, or clicking a file forks the one you are working
on. A remote one is fetched under `<cache>/dtr/remote/<alias>/<path>`: the tree is
mirrored so a title bar says where it came from, `..` is *dropped* rather than resolved,
the alias is sanitised because it comes from `~/.ssh/config`, and directories are `0700`
with files `0600`. The toast says "opened a copy of …" for a remote file and nothing for
a local one.

**One `ssh` child per alias, and a dead one is killed.** The session is shared behind an
`Arc` because a listing runs in its own task and still needs *this* connection; a fresh
one per `read_dir` would spawn an `ssh` per directory. Every operation has a 30 s
deadline and one reconnect, and a failure or timeout **takes the child with it** — a
dead session left in place makes every later call fail the same way, which reads as
"this host is down" long after it came back.

**A reply has to outlive its request, and a pane nobody is in still has to list.** The
SFTP pump asked only the focused pane and delivered only to it, so the pane a link opens
sat on `loading…` forever and a split pane never listed. Both loops walk every leaf now,
and the reply is routed by host *and* by the directory asked for. `take_request` consumes
`pending`, so matching a reply against it matched nothing; `awaiting` survives the send.

**The new-tab button is a menu, not a chord.** The `+` opens `egui::Popup::menu`: local
shell, then serial ports, then `~/.ssh/config` hosts, each group behind a separator and
each row saying which it is. The answer is `TabHit::New(Option<Destination>)` — typed,
because the menu used to guess "serial" from a leading `/` while the app opened *every*
entry as `ssh`, so picking a port ran `ssh /dev/ttyUSB0`. A port goes through
`Action::Connect`, which remembers its baud rate and CR translation; a host is
`ssh -t <alias>`.

## The UI, its widgets, and its tests

**The egui layer is tested with `egui_kittest`, not a window.** `cargo test` drives a
`Harness` with real layout and AccessKit, so clicks, keys and rendering are assertable;
query a widget by accessible label, `.click()`, `harness.run()`, assert what it
reported. `UPDATE_SNAPSHOTS=1 cargo test` writes a baseline PNG under
`crates/dtr/tests/snapshots/` and the plain run compares. Each surface has one snapshot
per state worth seeing — `tab_bar` away and `tab_bar_hover` with the pointer on a chip —
because a hand-painted hover fill is invisible to every test where the pointer is
elsewhere.

**A baseline is an input, so it is committed — through LFS.** `.gitignore` ignores
`*.png` (a `--screenshot` must never be committed) with an exception for this directory,
and `.gitattributes` sends it through LFS like every other image. A checkout whose
`git-lfs` did not smudge them holds 130-byte pointers instead, which `egui_kittest`
reports as a decode error that reads like a rendering bug; `tests/snapshots.rs` checks
the signature and the size first, and also fails when a `snapshot("…")` has no baseline
or a baseline has no test.

**The rasteriser that writes a baseline has to be the one that reads it.** The devShell
selects lavapipe — Vulkan in software — when the machine has no `/dev/dri`, and CI has
no DRM node, so regenerate with `UPDATE_SNAPSHOTS=1 cargo test` *inside the devShell*
(never with the variable unset on a GPU machine) or the next pipeline fails on a diff
that is two rasterisers disagreeing rather than a rendering change.

**A widget painted by hand has no accessibility node** unless `Response::widget_info`
declares one; the tab chips were invisible to the harness *and* to a screen reader. And
`Sense::click()` is **not pointer-only**: egui turns Enter into a click for any focused
widget that senses clicks, so a clicked chip stayed focused and every Enter typed into
the shell switched tabs. A hit is accepted only when `pointer.any_click()` says the
pointer actually clicked, and the chip surrenders focus after being drawn.

**Overlapping click targets read as "sometimes unresponsive".** egui gives the press to
the topmost widget that senses clicks, so a chip, its close cross and the icon on top of
both made the winner depend on allocation order — the cross closed sometimes, and the
`+` lost its press to its own icon. Disjoint rects, and `Image::paint_at` for anything
that must not sense anything.

**A chip has two hit targets**: the label switches tabs, a cross on the right closes
that tab — whichever is active. Both declare accessibility names (`2 shell`, `close tab
2`). The cross is *drawn* with two lines, not `×`: egui's font has no multiplication
sign. The active tab's title follows its focused session and the others keep theirs.
Every tab switch goes through `App::set_active`, which logs
`active tab from=… to=… why=…` — five call sites can do it and only one is the mouse.

**`egui::Window` renders nothing inside `Context::run_ui`.** The palette is an overlay
drawn into the same `Ui` for that reason, not by preference.

**A widget's background painted after its text hides the text.** The palette's selected
row was invisible until the highlight was drawn first. **A solid quad that samples the
atlas can vanish silently**: solids now carry a negative uv, take their colour from the
instance, and the shader branches. If a rectangle disappears, suspect the texture path
before the blend state.

**The settings menu is mouse-first.** Rows are click targets, font size is a drag value,
family and theme are combo boxes, toggles are checkboxes. `Settings::ui` takes `&self`
(the renderer holds a borrow), so a control *reports* its edit as a `Change` and
`Settings::apply` writes the draft. Every control declares an accessible name of the
form `<row> value`, not the row name — the row's label already carries the bare name,
and two nodes with one name cannot be addressed from a test. Port rows exist only when
a port is focused: baud and CR translation belong to one device, so the rows are built
per open.

**One theme is compiled in, and a file of the same name beats it.** `Kanagawa Wave` is
Ghostty's own file under a three-line provenance header, `include_str!`-ed from
`crates/dtr/assets/themes/`. Not because a theme corpus belongs in a terminal — the
licence finding in `docs/themes.md` §9.1 is why there is exactly one — but because the
places dtr runs have nothing to read: Ghostty's themes on NixOS are a store path, and
the one-file Windows exe has no `share/` at all. `theme::load` asks the search paths
first (exact name, then ignoring case: Ghostty capitalises its file names and people do
not) and falls back to the compiled-in copy, so `~/.config/dtr/themes/Kanagawa Wave` is
how you change it and `theme = "kanagawa wave"` is how you select it. `names()` merges
both lists, which is why the palette and the settings combo offer it on a machine with
no themes installed.

**A menu that opens slowly the first time is doing its work in the frame.**
`usable_families` builds a fontconfig collection and probes every family (the same
`FcFontSort` cost that showed up in a profile at 16%); it is cached in a `OnceLock`, so
the second open is instant. The app warms both lists on a `dtr-warm` thread at startup
and `-v` logs `warmed the settings lists` and `settings populated`, so a regression is a
number rather than a feeling.

**Key bindings are config, and the format is Zed's.** `[keys]` maps an action name to a
chord (`ctrl-shift-t`), a named key (`tab`), or several chords in a row (`ctrl-k
ctrl-s`); `false` hands a chord back to the shell. The vocabulary is in
`crates/dtr/src/keys.rs`. Two rules bite: **shifted punctuation is written as the
character it produces** — ctrl+shift+backslash reports `|`, so the binding is `ctrl-|`
(the lookup retries without shift so both spellings work) — and **a chord that starts a
longer binding waits**, which is why a pending sequence flushes after a second.

**A trailing space is a word break, not whitespace to trim.** The palette trimmed it
before deciding which word was being completed, so `ssh ` looked like the command `ssh`
and offered commands where hosts belong; `Parsed::of` keeps it. The consequence: after a
space there is nothing to complete for a command with no arguments, so the list is empty
rather than offering the command back.

**A command's first letter is its accelerator, so it is unique.** `s` is `ssh` and
nothing else may start with `s`: rename the new command rather than share the letter
(`shell` became `exec`). `crates/dtr-complete` fails the build when two commands collide,
and holds the ranking to it — one letter must *complete* to its command, not just rank
it first. A command with no `palette::action_for` arm does not belong in the list: `log`
and `logbook` were offered and inert, and are gone.

**`C-n` and `C-p` are consumed like the arrows** — taken from `input_mut` before the text
field sees them, so navigating cannot land an `n` in the query. **Reopening the palette
starts a fresh sentence**: `toggle` clears query, selection and completions. **A modal
must ignore key releases** — the release of the keystroke that opened it arrives while
it is open, which made the settings menu flash and vanish. Overlays check
`ElementState::Pressed` first.

**There was no new-tab key.** `Ctrl+Tab` cycled tabs that only the palette could create,
so a terminal had one tab and no discoverable way to get a second. `Ctrl+Shift+T` opens
one, and the palette footer is the one place bindings are written down.

**Icons are Lucide SVGs, fetched by a skill in this repository.** `.dsh/skills/lucide-icons`
and `scripts/fetch.sh` pull `https://lucide.dev/api/icons/<name>` into
`crates/dtr/assets/icons/<name>.svg`, refusing to overwrite one already committed. They
are embedded with `egui::include_image!` and rasterised by `egui_extras`, so the one-file
build carries them. Lucide's `currentColor` rasterises to *black* and `Image::tint` only
multiplies, so `fetch.sh` rewrites it to `#ffffff` and each file becomes a white mask the
tint colours. `install_image_loaders` is idempotent and `tab_bar` calls it rather than
trusting startup — a kittest harness makes its own `Context` and would otherwise draw
egui's missing-image glyph, a `⚠` that egui's own fonts cannot render either.

**Keys can be typed at the window from a script.** `--keys "ctrl-shift-p;down;down"`
applies one stroke per frame through `App::on_key`; `;` separates tokens because a chord
may be a comma, `+` joins the chords of one binding, and `text:` types characters.
`--click x,y` moves, waits a frame, then presses — what a hand does, which is what makes
it exercise the real hover-then-press path. `--drag x1,y1,x2,y2` calls the same
`pointer_down/move/up`. Those are only verifiable where a display is reachable; a
headless shell has no clipboard and winit refuses to open.

## Profiling and benchmarking

`./scripts/flamegraph.sh --demo` builds the `profiling` profile (release + debug info)
and samples a real window, through `perf` when the kernel allows recording and otherwise
`dtr --cpu-profile <path>`, which samples itself. `--cpu-profile /tmp/x.svg` always prints
a text self-time report. `--exit-after MS` closes the window by itself, which exercises
shutdown without a window manager.

**`PowerPreference::LowPower` is the integrated GPU**, which is wrong for a terminal
someone stares at. `--gpu <substring>` (or `DTR_GPU`) picks by adapter name; `-v` logs the
adapter and every adapter seen.

`./scripts/bench.sh` runs vtebench — Alacritty's PTY-read benchmark — against dtr with the
delta from the previous run. It writes the payload to its own stdout, so it must run
*inside* the terminal being measured: the script starts dtr with vtebench as the shell and
the numbers arrive in a `.dat` file. Reading the table:

- Absolute numbers only mean anything on one machine against itself.
- The previous run is the baseline, so bench one change at a time and rebuild first. The
  delta is a *rate*, and the sample size sits next to each `.dat` because comparing two
  runs that measured different amounts of work is how a 4× payload change looks like a 4×
  regression.
- vtebench records whole milliseconds, so a fast terminal quantises to 2 or 3 ms; the
  default is 32 MiB per sample and a median of `<1 ms` is a failed measurement.
- A sample is bounded by whichever is slower, reading or drawing, so a renderer that
  cannot keep up shows up here too. `--profile` writes `target/bench/profile.svg`.

Two payloads are ours: `wide_cells` (the double-width path) and `cwd_reports` (the OSC 7
sniffer, which sits on every byte). `scripts/bench.sh --check` validates them without a
window, a GPU or a network, and is what CI runs.

**The reader can be measured without a window.** `--screenshot PATH --screenshot-delay MS`
spawns the session and pumps it with nothing on screen, so a payload can be timed on a
machine with no display — `--shell` points at it and it writes the `.dat`. Only the final
image needs an adapter, and a software Vulkan ICD (`WGPU_BACKEND=vulkan`, lavapipe) is
enough.

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
| `a pty with sh on it: No such file or directory` | portable-pty passes `$HOME` to `current_dir` without checking it exists; `pty::start_dir` always hands it a real directory, because a stale home made the whole spawn fail with a bare `ENOENT` |
| `cargo test` does not refresh the binary | build first; `target/debug/dtr` is what you run, tests do not rebuild it |
| A removed pane takes two panes with it | the recursive remove returned "delete me" and the grandparent deleted the collapsed subtree; only the direct parent may collapse |
| Line height is far too small | swash reports `descent` as a *positive* distance below the baseline; negating it halves the cell |
| Caps Lock remapped to Escape does nothing, while every menu still closes on it | the physical key is `CapsLock` and the encoder has no bytes for it; `input::key_of` trusts the layout for legend keys, and menus read the logical key so they never noticed |
| A custom caption's maximize button does nothing even though `WM_NCHITTEST` answers `HTMAXBUTTON` | winit does not forward `WM_NCLBUTTONDOWN` to `DefWindowProc`; the subclass sends `SC_MAXIMIZE`/`SC_RESTORE` itself (`crates/dtr/src/chrome/windows.rs`) |
| Windows 11 snap layouts never appear over a custom maximize button | the hit test is necessary, not sufficient: `HTMAXBUTTON` is answered (verified with `SendMessage`), and the flyout still does not show in the guest while Edge's does — `docs/chrome.md` §11 |
| A stray console window next to dtr on Windows | the exe is console-subsystem; a process started without a console gets one allocated. Launching from `cmd` or Windows Terminal reuses theirs |
| The exe shows the generic blank in Explorer and in the taskbar's pin menu, while the running window has an icon | those read a resource out of the *file*; `WM_SETICON` is only a window. `crates/dtr/build.rs` compiles `assets/logo/dtr.ico` with `zig rc` — regenerate the artwork with `./scripts/icon.sh` |
| `include_image!` paints a small red ⚠ where the picture should be | egui_extras has no decoder enabled for that format: PNG needs its `image` feature, and only `svg` was on. `ui::every_embedded_image_decodes` is the test that fails instead |

## Where to look

- `PLAN.md` — design, milestones, spike findings, and the reasoning behind
  decisions that were reversed.
- `crates/dtr-ttl/MANUAL.md` — the Tera Term macro language, distilled.
- `crates/dtr-ttl/tests/corpus/` — Tera Term's own 53 macro tests, vendored under
  their BSD-3 notice; its README says what they pin and what they cannot. The
  totals asserted in `tests/corpus.rs` move only when the corpus is re-vendored.
  `tests/run_corpus.rs` runs them against a recording host and
  `tests/corpus/corpus-dialogs.txt` is what they said — regenerate it with
  `UPDATE_CORPUS_DIALOGS=1` and read the diff.
- `docs/ttl-gaps.md` — what the TTL interpreter still does not do, and the order
  worth doing it in. `crates/dtr-ttl/tests/field/` is where real macros go: the
  corpus is upstream's bug reports, and nobody's job depends on them.
- `docs/sftp.md` — measured decision memo for the SFTP idea.
- `docs/cjk.md` — measured memo and plan for everything outside the grid face:
  Han/kana, fullwidth punctuation, Nerd Font icons, emoji. Read it before
  changing `prepare_pane`'s placement rules. **§9 is the list of what is left**,
  in the order a person would notice it.
- `docs/themes.md` — which terminal theme formats dtr could read.
- `docs/windows.md` — the first Windows run, classified: the DPI cluster (grid font size,
  status bar), serial baud and autobaud, glyph and icon rendering, resize cost, and the
  instrument each open item still needs. §2.1 now carries the measurement at 125 %.
- `docs/chrome.md` — how the tabs get into the title bar: client chrome on Windows and
  Linux, the winit/egui mechanics that make it mostly one implementation, and the
  per-platform metrics and glyphs that keep the buttons looking native.
- `crates/dtr/src/pane.rs` — transports, publish, reconnect, modem, paste.
- `crates/dtr/src/app.rs` — the event loop, keys, and the palette glue.
- `crates/dtr/src/layout.rs` — the split tree. It knows nothing about terminals;
  a leaf is an id, and the tab's map decides what that id is.
- `crates/dtr/src/render.rs` — glyph atlas and the instanced grid pass.
