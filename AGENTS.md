# Working on dtr

Notes for anyone — human or agent — changing this repository. Design and milestones live
in [PLAN.md](PLAN.md); the TTL specification in
[crates/dtr-ttl/MANUAL.md](crates/dtr-ttl/MANUAL.md).

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
| `dtr-widgets` | the UI's look: palettes, style tokens, and painted controls |
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

If `cargo build` cannot find libghostty's pkg-config file (the devShell hook sets it, not
every shell):

```sh
export PKG_CONFIG_PATH=$(nix build .#libghostty-vt.dev --no-link --print-out-paths)/share/pkgconfig
```

A fake serial device:
`socat pty,raw,echo=0,link=/tmp/dtr-serial EXEC:'some script',pty,raw,echo=0` — killable
mid-session to exercise reconnect. Kill with `pkill -x socat`; `pkill -f "socat pty"`
matches the shell running it and kills yours.

- **The CI gates live in the flake**: `nix flake check` is the whole pipeline on both
  forges, and every gate is a `checks.x86_64-linux` entry — `tests` (whole suite, through
  lavapipe and the store's fonts), `bench-payloads` (`scripts/bench.sh --check`), `fmt`
  (`cargo fmt --all -- --check`), `desktop-entry`, `just-fmt`, `deny`, `single-exe` (which
  also cross-compiles `.#windows`). Put a new gate there, not in YAML; snapshots are
  generated here and compared byte-exactly there, so a disagreement takes a threshold,
  never a regenerated baseline. Run `just fmt` before committing.
- **`deny`**: [`deny.toml`](deny.toml) allows a permissive licence set (the Windows exe is
  statically linked and handed out as a download), denies crates.io-only sources other
  than the pinned VT core, and refuses wildcards; the advisory half is not a flake check
  because it fetches RustSec, so GitHub runs `cargo deny check advisories` after
  `nix flake check` and `just deny` runs all four locally. An ignore is a claim the
  advisory cannot be reached; write the reason down.
- `just --list` lists tasks; `just check` is `nix flake check`; `just test` is the suite
  without the sandbox. Both runners run the same command, so a red job means the same
  thing on either.

## Releasing

`just release X.Y.Z` refuses a dirty tree or a non-`master` branch, rewrites the workspace
version in `Cargo.toml` (its only home — `flake.nix` imports it, so package, exe and tag
cannot disagree), refreshes `Cargo.lock` through `cargo metadata`, proves the flake
reports the same number, then commits, tags `vX.Y.Z` and pushes;
`.github/workflows/release.yml` does the rest on the tag: refuses a tag that disagrees
with `Cargo.toml`, runs the full `nix flake check`, builds `dtr.exe`, publishes it with
`--generate-notes` (a `-suffix` makes it a pre-release), and a re-run replaces the assets.

**The tag triggers the release**, and the repository is mirrored GitLab → GitHub: if tags
are not mirrored nothing publishes — push it by hand (`git push github vX.Y.Z`) or turn
tag mirroring on.

## The house rules

- **Edit files with the editor tools, not shell redirection.**
- **Never `git add -A`**: subagents write files while you work and `-A` sweeps them in, so
  stage explicit paths (`git add crates Cargo.toml Cargo.lock flake.nix PLAN.md`).
- **`git add` before `nix build`**: a flake build sees only git-tracked files, so an
  untracked source file gives "file not found" or a phantom error about a *different*
  crate.
- **Do not hand-roll a widget.** A control a screen needs belongs in `dtr-widgets`,
  painted from the skin's palette and style, so two screens cannot disagree about a row
  height, a focused border or what "selected" looks like. A colour, a radius or a stroke
  width written inside a screen is a bug in `dtr-widgets` (§The UI, below). One-offs are
  how this tree got a bespoke baud field, a separate name row and three spellings of
  "segmented switch".
- **Screenshots are in-app only**, never a compositor grab: `--screenshot` renders
  offscreen from injected state, while `--capture PATH --capture-after MS` copies the live
  window's surface and is the truth when a menu or font differs — the offscreen path
  renders at 1× and cannot show a HiDPI bug (`docs/windows.md` §2).
- **The real window runs on Linux without a display, and at any scale**: `--capture` needs
  a `Window` and a surface, Xvfb provides both, and `WINIT_X11_SCALE_FACTOR` forces a
  factor the machine does not have — the only way to see a DPI bug without a HiDPI screen
  (`docs/windows.md` §2.1). This is the ordinary X11 path; nothing in dtr knows Xvfb
  exists:

  ```sh
  nix build nixpkgs#xvfb --no-link --print-out-paths   # Xvfb is not on PATH
  Xvfb :99 -screen 0 1280x800x24 -nolisten tcp -ac &   # a managed background job
  DISPLAY=:99 WINIT_X11_SCALE_FACTOR=2 nix develop .#default --command bash -c \
    './target/debug/dtr --shell "bash --norc --noprofile" \
       --send "bash /tmp/sample.sh\n" \
       --capture /tmp/shot.png --capture-after 3000 --exit-after 5000'
  ```

  `--capture` writes the file and carries on drawing, so `--exit-after` ends it. Read the
  result three ways: the PNG's size is the window in *physical* pixels (900 × 620 at 1×,
  1800 × 1240 at 2×), `magick identify` catches a window that did not scale at all, and
  `stty size` from inside the shell is the pane's cell count — the number that changes
  when the *cell* scales (100 × 34 at 1×, 105 × 35 at 2×, five extra columns being the
  whole-pixel ceiling). Two traps: the capture needs a frame then, so the sample must
  still be printing (a `tick` line every 500 ms), and no window manager does not matter —
  the window maps at its requested size.
- **Capture with a shell that does not read the user's bash rc**: dtr launches `$SHELL`,
  so a broken `~/.bashrc` lands *in the grid* — readline's `\[` and `\]` print literally
  and the prompt grows to half a screen. Use `--shell "bash --norc --noprofile"` for any
  capture, screenshot or font comparison.
- **A captured sample must print `\r\n`, not `\n`**: the pty is raw, so nothing turns a
  line feed into a carriage return for the child, and lines ending in `\n` draw a
  *staircase*. It is the harness, not the renderer.
- Commits end with the co-author trailer.

## The event loop, redraws, and doing work off the UI thread

- **`set_control_flow` is sticky state, not a per-iteration timeout**: `WaitUntil(now)`
  plus nothing on idle iterations leaves the deadline in the past and every `poll` times
  out. Set exactly one value every `about_to_wait`, from the earliest deadline, defaulting
  to `Wait`; a new deadline belongs in the `sooner(...)` list, never its own
  `set_control_flow`.
- **Repaints have to be carried**: popups arrive on the *next* frame, so after each frame
  ask `has_requested_repaint()` and repaint, and on Wayland — where `request_redraw`
  inside a redraw waits for the compositor's frame callback — `App::repaint` sets `wake`,
  which `about_to_wait` turns into `WaitUntil(now)` plus another request. **Never feed
  `RedrawRequested` to egui**: it answers "repaint" and the app spins at refresh rate.
- `-v` logs `loop iterations_per_s= frames_per_s= wakes=` once a second, separating a spin
  (many iterations, few frames) from refresh-rate drawing (frames == iterations) and idle
  (both near zero); it also logs `slow frame`, timed to `submit`, not `present`, because
  `present` makes every vsynced frame look like work.
- **The UI thread asks for slow work, it does not do it**: serial enumeration hits udev or
  SetupAPI and the ssh config is a file read, and either drops a frame, so
  `App::request_scan` hands work to a one-thread runtime (`App::worker`) and the result
  arrives as a `UserEvent` (`scanning` stops duplicates, `scan_due` is the poll interval);
  files work has its own runtime (`App::files_rt`), and `close_tab` hands teardown to
  `spawn_blocking`.
- **A capture needs a frame, and an idle window draws none**: `--capture` happens while
  drawing, so a settled window writes nothing; waking from `about_to_wait` does not fix it
  on a plain shell window — still open.

## Panes, publishing, and geometry

- **`Terminal` is `!Send`**: one thread per pane, current-thread tokio `LocalSet`,
  commands and slots.
- **A terminal without a scrollback limit is a memory leak with a schedule**: the core
  defaults to *unlimited*, so `pane::new_terminal` sets it from
  `config.terminal.scrollback_limit`; there is no unlimited spelling,
  `dtr_config::SCROLLBACK_LIMIT` (100 MB) is default and ceiling, and the parser refuses
  more.
- **A pty is cooked until somebody says otherwise**: `openpty` returns the kernel's
  default mode, so a second line discipline sat before the VT core (`\n` → `\r\n` per
  character, line-buffered echo, ^S stopping the flow). `pty::set_raw` runs *before* the
  child spawns, `the_session_starts_raw` reads termios back, and `ISIG` is the one flag
  kept so ^C reaches the foreground process.
- **`publish` gates on change, not on arrival**: it skips `Dirty::Clean`, so the first
  publish after a session starts is forced. **A cursor is not a dirty cell** — hiding or
  restyling it changes no cell, so `publish` compares the `Cursor` (position plus DECSCUSR
  style) with the last frame's and a hidden cursor draws nothing; that is the only
  non-cell gate, so do not add a second. **A frame per read chunk is a thousand frames
  nobody sees**: publish on a gate of half a refresh interval, with a timer arm so a
  skipped publish cannot leave the picture stale.
- **`Terminal::resize`'s pixel arguments are the cell's, not the pane's** — the pane size
  put every click in the top-left cell. **A resize must re-fit the terminal, not just the
  surface**: `WindowEvent::Resized` must call `fit_panes`, where per-size state belongs.
  **Two layouts must agree on the top inset**: `fit_panes` sizes and `pane_views` draws,
  both asking `App::top_inset`, or the pty keeps a taller area's rows. **With
  `chrome = "client"` that strip is also the title bar**: the same 30 points hold the drag
  region, eight resize borders and three platform buttons (`crate::chrome`) — the row
  moved, its height did not — and the drag region stops at `buttons_rect`, or presses over
  the chips become "sometimes unresponsive".
- **`inner_size()` is already physical, so a scale divided by it is always 1.0**:
  `Gpu::scale()` must be `window.scale_factor()`, or the physical/physical ratio reserves
  half the needed space on HiDPI (a 34 px inset over a 30 **point** strip); `-v` logs
  `strip geometry`. Units rule: egui works in points and applies the DPI factor,
  `top_inset` and grid rects are physical, layout constants are points, grid sizes
  physical, the scale applied exactly once. **The grid's font size is a point size and the
  rasteriser wants pixels, so the multiply happens in `Fonts::load`**: it and
  `GridRenderer::new` take the factor, `Metrics` and the atlas key are physical from
  there, and the cell, the fallback `adjust` and the sprite canvas inherit it; a scale
  change sets `fonts_dirty`, which rebuilds the grid and lets `fit_panes` re-fit.
  `STATUS_HEIGHT` is drawn in *points* over a reservation in pixels — multiplying it by
  the scale in points grew the bar as `scale²` over the bottom row.
- **A shell has no status bar, but the tab strip is reserved even for one tab**: the bar
  holds a serial port's counters and modem lines and is not drawn (nor reserved) without
  one, while the strip is always drawn. **`Gpu`'s fields drop in declaration order and it
  matters**: textures, buffers and pipelines before the device, the surface before the
  window, the window last — wrong order segfaults on close. **Panes share one instance
  buffer**: a per-pane draw would rewrite it and the last pane would win, so
  `render::prepare` takes every visible pane at once, and cells outside a pane's rectangle
  are not emitted rather than scissored.
- **A pane border is geometry, so the drag lives in `layout.rs`**: `divider_at` (with two
  points of slop either side, because a three-point gap is not a target), `divider_toward`
  (the nearest split at that edge of the focused pane), `set_ratio` (clamped to
  `MIN_RATIO` once, in the tree, so a stored ratio and a dragged one cannot disagree),
  `neighbour` (nearest with a *shared span*, so a pane that only meets a corner is not in
  that direction) and `visible` (zoom) are pure functions with tests; `App` only turns a
  pointer position into a call. **One `pane_area` for three consumers** — the grid pass,
  the pty fit and the divider hit test — because two of them disagreeing puts the divider
  where the picture is not, and `halves` is shared by `rects` and the drag for the same
  reason. **A press on a gap is not a click**: it starts a drag, changes no focus, asks no
  pane about a cell and never reaches the program; letting go copies nothing, and a second
  press in the same place puts the divider back at half. **`ctrl-alt-arrow` moves focus,
  the same chord with shift moves the divider one cell, `ctrl-alt-z` zooms** — a zoom is a
  *view* of the tree rather than an edit, so unzooming restores every divider exactly, and
  a split or a close unzooms first or the change happens off screen. `fit_panes` asks the
  same `visible`, so a hidden pane is not resized behind the one on screen.
- **`O` and `0` look identical in egui's default monospace**, so `OK` looks like `0K` in a
  screenshot; before touching the render path run
  `nix develop .#default --command bash -c 'cargo test -p dtr --bin dtr the_vt_core'` — if
  that passes, the cells are right and you are looking at a font ambiguity.

## Keys, mouse, IME, selection

- **The program decides what a key means, and only the pane knows**: `Command::Key`
  carries a *stroke* and the pane's `key::Encoder` picks bytes after
  `set_options_from_terminal`, because the sequence is terminal state — `DECCKM`
  (`ESC O A`), `DECKPAM`, mode 1036, `modifyOtherKeys`, the kitty protocol
  (`CSI > flags u`, a *stack*; ctrl+a is `CSI 97;5u`, releases included) — so a table in
  the UI cannot be right.
- **A release repeats its press's `logical_key`**, so encoding both sends every keystroke
  twice ("typing `ls` shows `lsls`"); a release encodes to nothing unless the kitty
  protocol asked for releases. It is the encoder's rule, not a UI guard.
- **A shifted character is that character, not the key plus a shift bit**: `input::fill`
  passes `consumed_mods`, or shift+`;` goes out as `CSI 59;2u` and the program applies the
  shift — Neovim uppercases letters only, so `:` and `#` never arrived. winit has no
  `getConsumedModifiers`, so it is derived: shift went into a one-character text differing
  from the key's unshifted one, and a key with no unshifted character keeps its shift
  (`<S-CR>`). The *position* decides, not the character (`physical_key`; `--keys` assumes
  US), and ctrl or super must not pass `text` through while a plain character must, or
  accents and CJK vanish.
- **…except where the position is a legend**: `caps:escape` reports position `CapsLock`
  with logical key `Escape`, and the encoder has no `CapsLock` bytes, so the keystroke
  vanished while menus — which read the logical key — still closed on it. `input::key_of`
  takes the layout's answer for `CapsLock`, `NumLock`, `ScrollLock`, `Fn` and `FnLock`,
  keeping the position elsewhere (`DECKPAM`'s `Numpad8`), and tests drive a pty in both
  encodings (`ESC`, `CSI 27 u`).
- **A click is encoded where the modes are**: `Command::Mouse` carries pane-relative
  position, button, modifiers and held state, and `mouse::Encoder` reads tracking mode per
  event — `?9h` presses, `?1000h` presses and releases, `?1002h`/`?1003h` motion, `?1006h`
  SGR. The mode lives in the `!Send` terminal, so `Stats::mouse()` is an atomic published
  by `note_modes`, without which a click in vim starts a *selection*. Shift hands the
  mouse back, or `set mouse=a` makes copying impossible; a press outside is refused while
  a drag that leaves is clamped (`pane_pixels`); a wheel notch is buttons 4 and 5, press
  *and* release (X10), and shift sends it to scrollback. **A paste is the program's too**:
  `pane::pasted` turns breaks into CR and wraps in `ESC[200~ … ESC[201~` under `?2004h`.
- **IME text is not a keystroke, and egui has no CJK**: the preedit is a picture
  (`crate::ime`, above the caret) and nothing reaches the pty until commit, which makes a
  composition cancelable — `KeyStroke::composing` keeps the romaji out,
  `set_ime_allowed(true)` keeps pinyin latin out, and `set_ime_cursor_area` (per caret
  cell, deduplicated) puts the candidates by the text. egui's faces are latin-only and
  panic on a missing glyph (`No glyph for "に" in Monospace`), so `Look::install` appends
  `fonts::ui_fallback()` to both families, checked for kana *and* Han.
- **A selection is anchored to characters, not to the grid**: the pane's two
  `TrackedGridRef`s follow the page list through scrolling, pruning and reflow and resolve
  on *every* publish, so the frame carries the range. `PointSpace::Viewport` resolves but
  does not bound (`y = 13` below a five-row viewport), so the check is ours, and once
  either end leaves the frame the selection is **dropped, not clamped** — a stretched
  highlight claims cells it does not cover and a copy hands back a fragment. A press that
  has not moved is an anchor, so one click deselects; a word, line or drag is
  `deliberate`.
- **Double- and triple-click are counted, not reported**: `App::count_clicks` is 400 ms
  and four pixels, two takes the word and three the line via `select_word`/`select_line`,
  and a drag inside the press's cell changes nothing — jitter must not turn a word into
  one character. A double-click copies on release.
- **A copy cannot use the published frame, because drawing it is what removes it**: the
  slot is `None` by then (shipped as "copied 0 chars" with a selection on screen), so
  `Command::Copy`/`CopySelection` rebuild from the terminal, the only holder of the cells.
  **A toast that says "copied" is not a clipboard**: `ui.ctx().copy_text` goes through
  `egui-winit`, whose clipboard falls back to an in-app buffer, so dtr also writes it with
  `arboard` and holds the handle on `App` for the process's life (on X11 the owner must
  live or the text goes).
- **OSC 52 writes arrive on the pane thread**: `on_clipboard_write` parks the text in a
  slot and the UI does the work, and `Err` for a non-text MIME tells the program the write
  was refused.

## The grid, fonts, and glyphs

- **A missing glyph is not a missing character**: the grid face covers Latin and CJK/emoji
  come from another family, so `Fonts::face_for` asks the grid face, then the loaded
  faces, then walks every installed family loading one and testing its charmap, caching
  per character — 2000 Han characters cost one extra load, not 2000.
- **Which family answers is a written policy, not fontconfig's order** (`docs/cjk.md` §5,
  M4): loaded faces; `[font] fallback`; `fonts::FALLBACK` (Sarasa Mono, Noto CJK, Nerd
  Font families); the platform's script preference from fontique; then a walk with **Nerd
  Fonts first and monospaced before proportional**. The first four are opinions and win
  even when proportional; the walk is a search, so monospace decides there — a
  proportional face with a stray private-use glyph must not draw an icon. A terminal wants
  a monospaced CJK face, or wide characters carry their own advance; `-v` logs
  `fallback face`.
- **A charmap entry is not ink**: Noto Color Emoji maps every emoji and has no outlines,
  so the walk accepted it and drew nothing — `has_ink` rasterises the candidate and skips
  an empty box. **No grid face is not an empty grid, it is Emoji**: `GRID_CANDIDATES` was
  all Linux families with `"monospace"` last, a *generic* family rather than a name, so
  `family_by_name` never resolved it; on a fresh Windows the grid had no face, `face_for`
  fell through to the first fallback with a glyph, and Latin was drawn by Segoe UI Emoji
  at 1.6× the advance — `Microsoft` came out `M crosoft` and an SGR sequence arrived as
  literal `[0m`. Then the platform is asked
  (`generic_families(GenericFamily::Monospace)`); adding a family to `GRID_CANDIDATES` or
  `fonts::FALLBACK` is how a new OS gets its faces, and `--font-report` in the `winvm`
  guest checks it (`docs/cjk.md` §5, M7). A *pictograph* (U+1F000–1FAFF, no script) also
  skips the already-loaded shortcut, or the symbol face that answered a powerline arrow
  hands it a monochrome `😀`.
- **A fallback is rasterised at its own optical size**: `Loaded::adjust` is the ratio of
  the *ideographic widths* — the advance of `一`, else `min(ascii ink height, two cells)` —
  and it must be that metric, because x-height gave 0.92 and cap height 0.89 on
  DejaVu/Noto CJK and both shrank every Han character. Two consequences: the atlas key
  includes the *face index* (glyph ids collide between fonts), and every glyph is placed
  on the **grid's** baseline — `Metrics::baseline` — not the producing face's ascent
  (`cell_baseline`, `docs/cjk.md` §3.1).
- **`啊` is the CJK test character**: `U+554A` is full-width Han with ink in all four
  quadrants, so a baseline error, a wrong size or a half-drawn cell shows up in its
  numbers and on screen. It is in `FONT_REPORT_SAMPLE` (`dtr --font-report`) and the
  placement tests; use it rather than inventing another character.
- **Box drawing, blocks and braille are drawn, not borrowed**: `crates/dtr/src/sprite.rs`
  ports Ghostty's sprite face (`src/font/sprite/draw/*.zig`) — box drawing
  (light/heavy/double arms and every junction, from a table of edges per codepoint, plus
  the dashes), block elements (fractions, quadrants, shades as flat alpha) and braille
  (eight dots, Ghostty's order of concessions when the cell is too small). The cell *is*
  the canvas, so a line reaches the edge at any cell size, where a font draws these at its
  own thickness and side bearings (`─` in DejaVu is 19 px of ink in a 17 px cell, and two
  overlap). Not ported, still from a font: the arcs `╭╮╯╰`, the diagonals `╱╲╳`, the
  legacy-computing blocks and geometric shapes. Two traps: the mask is the *whole cell*,
  so assert on its pixels, not the rectangle; and the sprite is keyed by codepoint in
  place of a glyph id — with one shared id every box character draws the first one cached.
- **A wide character is two cells, not one, and neither is a box to fill**: the width is
  the terminal's (`Cell::wide()` → `Narrow`/`Wide`/`SpacerTail`/`SpacerHead`), taken in
  `fill_cells`, and a spacer is not drawn. The glyph is drawn at design size across the
  pair — scaling the *ink* to fill the box made a full-width full stop a 17×17 square
  (`docs/cjk.md` §1). **A placed offset is not a length**: `glyph.top` is the distance
  from the cell's top edge. One placement rule, in `render::placement`, shared with
  `--font-report`; do not add a second.
- **A glyph is drawn as its font drew it unless a rule says otherwise, and the rules are
  Ghostty's**: `render::Constraint` ports `RenderOptions.Constraint` (`font/Glyph.zig`) —
  five size rules, five alignments, pads in thousandths of the face box, a maximum cell
  count, two height metrics — and `constraint_for(codepoint)` is the only policy: private
  use and the symbol blocks get `fit_cover1` centred on the first cell, powerline and
  braille `stretch`, everything else nothing. Two traps: a uniform scale belongs on the
  **outlines** (re-rasterise at `size × factor`, or an enlarged icon is grey mush) and
  only `stretch` may scale the bitmap; and `stretch` is only right where the ink *is* the
  shape — `─` is three pixels of ink and stretching draws a solid block, so box drawing
  and block elements wait for a sprite face (`docs/cjk.md` §5, M6).
- **How many cells a glyph may use is the neighbours' answer**: `constraint_width` is
  Ghostty's `renderer/cell.zig` rule — a symbol may borrow the right-hand cell when it is
  blank, the glyph to its left was not also a symbol, and it is not the last column. It
  changes the *glyph's* room, never the grid, without it every other icon in a row drifts
  right.
- **A colour glyph keeps its own pixels, and a mask is tinted**: two atlases (`R8Unorm`
  for masks, `Rgba8Unorm` for colour) and an `f32` flag on the instance pick which the
  shader samples, because one draw call is the point; so a colour glyph must not be tinted
  with the cell's foreground, and a letter must never reach the colour atlas or it stops
  being tinted. `swash`'s source list is
  `[ColorOutline(0), ColorBitmap(BestFit), Outline]`, `Image::content` says which
  answered, and `has_ink` uses the same list. Presentation selectors (VS15/VS16) do not
  work: each codepoint is drawn on its own, so `❤️` is `❤` (`docs/cjk.md` §4, gap 9).
- **egui's default fonts have no arrows**: `↑↓` and `←→` panicked the harness and had been
  drawing empty boxes; hints are words now (`up/down`).
- **`faint` arrives as a flag, and dropping it is how a ghost disappears**: the core
  parses SGR 2 into `style.faint` and `fill_cells` must read it, or fish's autosuggestion
  and inline completions come out as bright as the text they suggest against. Ghostty uses
  the flag as the foreground's alpha, `faint-opacity` (0.5 by default), over the cell's
  background, which is what `faint()` blends; no palette entry is called "faint", so a
  blend is the only version right on every theme.
- **`invisible` is the same bug with the opposite fix**: a concealed input — a password
  field, a `read -s` prompt — sends `\e[8m`, and `Cell::concealed` carries it to
  `render::prepare`, which skips the glyph and still draws the background (xterm's rule,
  and Ghostty's). The characters stay in the cell, because a copy and a capture are built
  from the frame's text.

## Sessions: shell, ssh, serial

- **A tab is named by the program, not by the transport**: OSC 0/2 set the title,
  `Frame::title` carries it to the strip and the window, and `1 shell` is only the
  fallback (`tab_name`). It is not a dirty cell and not trustworthy bytes: it is compared
  with the last published title in the same `Displayed` record as the cursor, so a program
  that renames itself and draws nothing still publishes, and `clean_title` decodes
  lossily, drops control characters and caps at 512. **A shell that exits closes its tab**
  — the pane cannot, the UI owns the tab — so the pty reader sets `Pane::ended` on EOF,
  `App::reap_ended` closes highest index first, and the last takes the window.
- **A dropped ssh connection is not a shell that exited; the exit status is the only thing
  that tells them apart**: `ssh` exits 255 when ssh itself failed, and anything else is
  the remote command's status, meaning somebody typed `exit`. `pane::ending` is that rule:
  a drop keeps the tab, says so and reconnects, an `exit` closes it.
- **A client that never speaks is worse than one that fails**: a refused connect fails in
  milliseconds, a blackhole sits in `connect` for two minutes, and a connection that goes
  away without a reset leaves an `ssh` that looks alive, so reconnect never fires.
  `pty::Config::ssh` passes three options, all **before the destination**:
  `ConnectTimeout=10`, `ServerAliveInterval=20`, `ServerAliveCountMax=3` (ssh's own
  default is no keepalive). They override `~/.ssh/config`; `ssh_args` builds the list and
  a test asserts every flag precedes the alias, because an option after the destination
  becomes a command for the far end. **An ssh failure's cause is its stderr**: with
  `Stdio::null()` a dead `ssh` reached the UI as `sender dropped`, so stderr is piped and
  quoted with the exit status. `examples/probe.rs` runs the same path from a shell;
  `DTR_SSH_CONFIG=/dev/null` gets past a broken config and `DTR_SSH_BATCH=0` lets a prompt
  be seen.
- **Retries are bounded in rate, not in number**: waits go 1, 2, 4, 8, 16, 30, 30, …
  seconds, forever, because closing the tab is how a person gives up and a cap would turn
  a laptop shut for a day into "closed while you were not looking". The reset needs ten
  seconds of *uptime*, not just a connect, so a flapping host keeps the long wait. The
  child belongs to the reader thread (`wait` blocks, status only after EOF), and the first
  connection opens inside the event loop by the same code as every reopen — a host down at
  startup should be a pane that waits, which is why `--send` is held in `typed_early`
  until the first pty exists.
- **A reconnected session is put back where it was by the remote command**: `ssh -t host`
  starts in `$HOME`, so the reopen is
  `ssh -t host 'cd <dir> ; exec "${SHELL:-/bin/sh}" -l'` — the directory quoted by `shlex`
  for the shell at the other end (any path may hold anything but `/` and NUL, and NUL is
  refused), `;` rather than `&&` so a deleted directory still leaves a usable shell, and
  `exec`ed so the login shell *is* the session. `a_hostile_directory_is_still_one_word`
  round-trips a dozen hostile paths through `shlex::split`.
- **The modes outlive the program that set them**: a drop can happen while vim holds the
  alternate screen with mouse modes on, so `reset_modes` clears them through the core
  before the reopen — including the kitty stack (`CSI < 999 u`) and DECSCUSR's shape and
  visibility — while leaving the text, because the last screen before the drop is the
  first thing one wants to read. The notice (`-- reconnecting to jupyter in /srv/app --`)
  goes into the terminal, not the pty, because nothing at the other end would read it.
- **A session knows its directory only because it says so**: `/proc` is Linux-only and an
  ssh session has no local process, so `CwdSniffer` watches for OSC 7
  (`ESC ] 7 ; file://host/path`) before the VT core, keeping the tail across chunks —
  holding the introducer back (prefix included, or flushed past `CARRY_MAX` so a bogus
  introducer cannot stall the session) keeps a split sequence parseable. The **host**
  decides the machine: empty, `localhost` or this machine's name is local, and anything
  else is another machine whose path must never be used as a local path, or "new tab here"
  tries to `cd` into a directory on the wrong machine. `Session::remote` records the ssh
  case too, and `+` → local shell starts in the focused tab's directory when that tab is
  local.
- **A prompt mark is a reference, not a row**: OSC 133 is parsed by the core, so
  `previous-prompt` (`ctrl-shift-up`) walks `ScrollViewport::DeltaPrompt`, and what dtr
  keeps is for *capture* — `PromptMarks` records each `A` as a `TrackedGridRef`, because a
  row number goes stale when the block scrolls. `select_output` is the tempting API and is
  not what a pager wants: it returns `None` unless the reference is *inside* the output.
  `capture-block` (`ctrl-shift-o`) parks the text for `crate::pager`; without shell
  integration there are no marks and the toast says so. **fish already speaks OSC 133** —
  nothing to install: it sends `ESC ] 133 ; A ; click_events=1 ESC \`, with options after
  a `;` and ST rather than BEL, while bash and zsh need the snippet from the
  shell-integration proposal sourced from their prompt.
- **The pager is a `Label` in a `ScrollArea`, and that is the whole design**: the text is
  in memory, so wrapping, scrolling and mouse selection are egui's job. Two costs: the
  page height a key scrolls by is only known from the *last* frame (remembered, not
  measured), and "go to the end" is `f32::MAX` with the scroll area clamping. It is
  **modal** because the keyboard is — `j`, `k`, `b` and space are a shell's keys — so
  `App::on_key` hands every key to the pager first and returns, including ones it does not
  understand, while chords are not pager keys and `ctrl+…` falls through to nothing. The
  pointer is modal too, and it draws in the foreground `overlay` layer because a file pane
  is an `Area`; closing drops the text.
- **Devices are polled, not watched**: `serialport::available_ports` every couple of
  seconds, because udev is Linux-only and Windows enumerates COM ports by another route,
  and one mechanism that works on both beats a fast one that only exists here; files
  differ, since `~/.ssh/config` is watched with `notify` because that mechanism *is*
  portable. **"No bytes" is not "the reader ended"**: the pane keeps the byte sender alive
  for reconnects, so `bytes_rx.recv()` never returns `None` and a disconnect arrives on
  `closed_rx`. **`reconnecting…` on a transport that cannot disconnect** means
  `Stats::connected` was left at its `Default` — only the serial reader turns it false.

## Files, links, and panes

- **Clicking a file is a VFS question, and the pane owns the answer**: `dtr-vfs` has
  `Location { backend, path }`, a `Backend` trait (`stat`, `open`, `list`) and local,
  in-memory and ssh implementations. **A `Location` comes from the pane, not from a URI**:
  `file:///etc/hosts` printed on `jupyter` means jupyter's file, and `BackendId::Ssh` is
  only built from a session that *is* remote, with `Vfs::backend` refusing it rather than
  falling back to local. **Listings are batches** because `readdir` answers in chunks, and
  dropping the receiver cancels the walk; **capabilities are declared** — no remote watch,
  no directory size — and the UI hides what is not there.
- **A hyperlink is asked about, not carried in the frame**: OSC 8 lives in the terminal,
  which never leaves the pane thread, so `Command::HyperlinkAt { x, y }` answers into a
  slot, where resolving per cell during publish would be ~8 ns of FFI per cell for a link
  nobody points at; the reply carries the cell asked about, so a frame-late answer is
  dropped instead of opening the wrong link. Only `file:` is understood, percent-decoded,
  `stat`ed through the VFS and handed to `xdg-open`/`open`/`explorer` — never a shell,
  because the path comes from a program's output — and another scheme is said out loud in
  a toast. A press on a link opens it and starts no drag.
- **The host part decides which machine, and `file://blah` usually has one**:
  `ls --hyperlink=always` prints `file://<hostname>/path`, not `localhost`, so a rule
  accepting only empty and `localhost` refused every link `ls` ever printed. The rule
  lives in `crate::uri`, shared with the OSC 7 sniffer: a local session resolves only for
  empty/`localhost`/this machine and names the other host in the toast, while a remote
  session resolves every `file:` path on its own host.
- **A directory link opens a file pane, not a toast**: the worker sends
  `UserEvent::Reveal` and the UI decides where a pane may go — layout is not a worker's
  business — and focus stays on the terminal. A pane browsing the *same machine* is
  retargeted; another machine is another pane, because a pane reading this disk cannot
  become one reading somebody else's.
- **A hovered picture is decoded off the drawing thread and capped before it is read**: 16
  MiB of source, 8192 pixels a side, 256 MiB of decode, and the extension only decides
  whether to *try*, so a JPEG named `.png` still works. Three traps:
  `DynamicImage::thumbnail` *enlarges* (a 1x1 icon came back 360x360), the texture is made
  at draw time because `load_texture` needs an egui context, and a refusal is cached or a
  row of names re-reads a 2 GB file five times.
- **A remote preview waits, then fetches, and the cap comes before the bytes**: the
  pointer must rest for `PREVIEW_DELAY` (350 ms), or a sweep across a row is one `ssh`
  fetch per cell, and the wait has its own deadline in `about_to_wait` because a stopped
  pointer produces no events. Then `stat` before `materialise` — a remote 400 MB scan must
  never be downloaded to find out it is too big — then the decoder on the files runtime.
  The cache key is **`{ machine, path }`**, not a path; the preview is a tooltip-layer
  `Area`, `interactable(false)`, placed below-right and flipped at the edges with a clamp
  last, and `CursorLeft` clears the hover.
- **Right-clicking a link is a menu** with `Open`, `Save as...`, `Copy path`, and only a
  link *under the pointer* gets one. A left press outside closes it *and goes nowhere
  else*, or dismissing a menu also starts a selection, which needs the menu's rect from
  the last frame (`LinkMenu::contains`). Escape is the menu's own key; any other key
  closes it and then does what it was going to do. The image preview is suppressed while
  the menu is open, because the menu opens *at* the pointer and the preview draws 14
  points from it in a layer above.
- **The file dialog is the platform's and it blocks**: `rfd` gives the XDG portal (zenity
  without one) and the Common Item Dialog on Windows, and it runs on the files runtime and
  answers with `UserEvent::SavePath` because on the drawing thread it would freeze the
  window. A dialog that never appears returns the same `None` as a cancel, which is why
  `LogTracer` is installed — rfd's own `log` line is what `-v` shows — and `set_parent`
  keeps it in front on Windows. It starts at `~/Downloads/<name>`, shared with the
  explorer's download; saving is the same `materialise` a click uses and then a copy, so
  "Save as" and "Open" cannot disagree about the bytes, and the toast is raised *before*
  the transfer because a slow link is otherwise a silence that reads as a dead button.
- **Materialising is where transparency ends, and it says so**: a local file is its own
  path — `materialise` must never copy, or clicking a file forks the one you are working
  on — while a remote one is fetched under `<cache>/dtr/remote/<alias>/<path>`, with the
  tree mirrored so a title bar says where it came from, `..` *dropped* rather than
  resolved, the alias sanitised because it comes from `~/.ssh/config`, and directories
  `0700` with files `0600`. The toast says "opened a copy of …" for a remote file and
  nothing for a local one.
- **One `ssh` child per alias, and a dead one is killed**: the session is shared behind an
  `Arc` because a listing runs in its own task and still needs *this* connection, and a
  fresh one per `read_dir` would spawn an `ssh` per directory. Every operation has a 30 s
  deadline and one reconnect, and a failure or timeout **takes the child with it** — a
  dead session left in place makes every later call fail the same way, which reads as
  "this host is down" long after it came back.
- **A reply has to outlive its request, and a pane nobody is in still has to list**: the
  SFTP pump asked only the focused pane and delivered only to it, so the pane a link opens
  sat on `loading…` forever and a split pane never listed. Both loops walk every leaf now,
  and the reply is routed by host *and* by the directory asked for; `take_request`
  consumes `pending`, so matching a reply against it matched nothing, and `awaiting`
  survives the send.
- **The new-tab button is a menu, not a chord**: the `+` opens `egui::Popup::menu` — local
  shell, then serial ports, then `~/.ssh/config` hosts, each group behind a separator and
  each row saying which it is. The answer is `TabHit::New(Option<Destination>)`, typed
  because the menu used to guess "serial" from a leading `/` while the app opened *every*
  entry as `ssh`, so picking a port ran `ssh /dev/ttyUSB0`. A port goes through
  `Action::Connect`, which remembers its baud rate and CR translation; a host is
  `ssh -t <alias>`.

## The UI, its widgets, and its tests

- **The look is a skin, and a surface asks it rather than deciding.** `dtr-widgets` holds
  `Palette` (semantic slots — `card`, `control`, `border`, `accent`, `dim` …), `Style`
  (row height, gaps, radii, fonts, and the `Title`/`Section`/`Footer`/`Focus`
  behaviours) and `Skin` (the pair), plus `Skin::egui_style`, which derives an
  `egui::Style` so stock egui widgets inherit the same look. `Preset::{Dtr, Native}`
  picks a skin; `[ui] widgets` (or `--widgets`) says which, and `native` is the default.
  **A preset is a request**: `Skin::resolve` takes the platform's skin as an argument and
  falls back to dtr, because reading the system palette is platform code and lives in
  `crates/dtr/src/system.rs`, where both platforms are written to one trait: a reader
  answers "dark or light, which accent, which ladder" and the crate turns that into a
  skin. Windows reads `AppsUseLightTheme`, DWM's `AccentColor` and the **accent ladder**
  (`AccentPalette` under `...\Explorer\Accent`) so a hover lands on the variant
  Windows' own controls use; Linux asks the XDG portal for the scheme and the accent and
  refuses a theme that is not Adwaita rather than painting Adwaita's surfaces on Breeze
  (`docs/gtk-palette.md` §6). **It is live**: the answer sits in a cell a frame reads, and
  a watcher replaces it when the platform says it changed — `WM_SETTINGCHANGE`,
  `WM_DWMCOLORIZATIONCOLORCHANGED` or `WM_THEMECHANGED` through the window procedure that
  already subclasses the window (winit forwards only the first, and only when light/dark
  differs), `SettingChanged` from the portal — then asks for a repaint. The Windows half is
  verified on the guest and takes two messages per change, which is why it re-reads both
  registry values rather than deciding which message meant what. `-v` logs
  `chrome skin=… requested=… accent=… source=…`, which is the difference between a
  preference and an outcome, and `source=` names the reader that answered. **Painted controls belong in
  `crates/dtr-widgets/src/widgets/`**: `Editable` (a text field with a dropdown arrow,
  reporting `Edit::Typed` and `Edit::Picked` as different answers), the layout primitives
  `label_cell`/`row`/`section`, and `check`, `Button`, `segmented`, `menu_row` and `cross`.
  The serial sheet is drawn entirely with them — `Look::skin()` derives the skin from the
  theme's colours, so `serial_dialog.rs` contains no colour, radius or stroke width — and a
  surface that needs a control the crate does not have waits for it there instead of
  growing a second copy. **A new primitive arrives with a gallery snapshot** in
  `crates/dtr-widgets/tests/`. **A tick is drawn, not lent**: `check` and `menu_row` are
  strokes, because egui's faces have no check mark and an icon would make every harness
  install an image loader first.
- **The egui layer is tested with `egui_kittest`, not a window**: `cargo test` drives a
  `Harness` with real layout and AccessKit, so clicks, keys and rendering are assertable —
  query a widget by accessible label, `.click()`, `harness.run()`, assert what it
  reported. `UPDATE_SNAPSHOTS=1 cargo test` writes a baseline PNG under
  `crates/dtr/tests/snapshots/` and the plain run compares, with one snapshot per state
  worth seeing — `tab_bar` and `tab_bar_hover` — because a hand-painted hover fill is
  invisible to every test where the pointer is elsewhere.
- **A baseline is an input, so it is committed — through LFS**: `.gitignore` ignores
  `*.png` (a `--screenshot` must never be committed) with an exception for this directory,
  and `.gitattributes` sends it through LFS. A checkout whose `git-lfs` did not smudge
  them holds 130-byte pointers, which `egui_kittest` reports as a decode error that reads
  like a rendering bug, so `tests/snapshots.rs` checks signature and size first and fails
  when a `snapshot("…")` has no baseline or a baseline has no test. **The rasteriser that
  writes a baseline must be the one that reads it**: the devShell selects lavapipe —
  Vulkan in software — when the machine has no `/dev/dri`, and CI has no DRM node, so
  regenerate with `UPDATE_SNAPSHOTS=1 cargo test` *inside the devShell*, never with the
  variable unset on a GPU machine, or the next pipeline fails on two rasterisers
  disagreeing.
- **A widget painted by hand has no accessibility node** unless `Response::widget_info`
  declares one, and the tab chips were invisible to the harness *and* to a screen reader.
  `Sense::click()` is **not pointer-only**: egui turns Enter into a click for any focused
  widget that senses clicks, so a clicked chip stayed focused and every Enter typed into
  the shell switched tabs — accept a hit only when `pointer.any_click()` says the pointer
  clicked, and have the chip surrender focus after being drawn. **Overlapping click
  targets read as "sometimes unresponsive"**: egui gives the press to the topmost widget
  that senses clicks, so a chip, its close cross and the icon on top made the winner
  depend on allocation order — disjoint rects, and `Image::paint_at` for anything that
  must not sense anything.
- **A chip is as wide as its title, until the title is longer than a strip**: the label is
  elided at `TAB_LABEL_MAX_WIDTH` (180 points) with `…`, one row, broken anywhere, and the
  full name stays in the tooltip and the accessibility node — unbounded, one deep
  directory pushes every tab and the `+` off the strip. **A chip has two hit targets**:
  the label switches tabs, a cross on the right closes that tab, whichever is active, and
  both declare accessibility names (`2 shell`, `close tab 2`); the cross is
  `dtr_widgets::cross_at`, two strokes in the chip's own reserved rect, not `×`, because
  egui's font has no multiplication sign. The active tab's title follows its focused
  session, and every switch goes through `App::set_active`, which logs
  `active tab from=… to=… why=…`. The chip's colours and radii are the skin's; its
  elision and its two rects are the strip's, and `New tab`'s menu is `menu_row` rows.
  **The strip's own menu is a burger at the right-hand end, next to the caption
  buttons**, on both platforms — `crate::ui::app_menu`, a Lucide `menu.svg` icon, a
  `Popup::menu` of `menu_row`s: settings, keyboard map, about. The right end because
  the left is where tabs start and a button there would move every tab; the *same*
  place on Linux, where a client frame draws the same caption buttons. Its rect is
  part of the caption's obstacle list, not just painted over it: the drag region stops
  at the menu's left edge (`chrome::drag_area`), or a press on the menu starts a
  window drag. **About is a card, not a dialog** (`crates/dtr/src/about.rs`): every
  fact on it is a compile-time constant (`env!("CARGO_PKG_VERSION")`), so it cannot
  disagree with the binary, and it is opened by the menu and by `about` in the
  palette.
- **`egui::Window` renders nothing inside `Context::run_ui`**, which is why the palette is
  an overlay drawn into the same `Ui`. **A widget's background painted after its text
  hides the text** — the palette's selected row was invisible until the highlight was
  drawn first — and **a solid quad that samples the atlas can vanish silently**: solids
  carry a negative uv, take their colour from the instance, and the shader branches.
- **The settings menu is mouse-first**: rows are click targets, font size is a drag value,
  family and theme are combo boxes, toggles are checkboxes. `Settings::ui` takes `&self`
  (the renderer holds a borrow), so a control *reports* its edit as a `Change` and
  `Settings::apply` writes the draft. Every control declares an accessible name of the
  form `<row> value`, not the row name, because the row's label already carries the bare
  name and two nodes with one name cannot be addressed from a test. Port rows exist only
  when a port is focused: baud and CR translation belong to one device.
- **One theme is compiled in, and a file of the same name beats it**: `Kanagawa Wave` is
  Ghostty's own file under a three-line provenance header, `include_str!`-ed from
  `crates/dtr/assets/themes/` — not because a theme corpus belongs in a terminal (the
  licence finding in `docs/themes.md` §9.1 is why there is exactly one) but because the
  places dtr runs have nothing to read: Ghostty's themes on NixOS are a store path, and
  the one-file Windows exe has no `share/`. `theme::load` asks the search paths first
  (exact name, then ignoring case, because Ghostty capitalises its file names and people
  do not) and falls back to the compiled-in copy, so `~/.config/dtr/themes/Kanagawa Wave`
  is how you change it and `theme = "kanagawa wave"` is how you select it; `names()`
  merges both lists.
- **A menu that opens slowly the first time is doing its work in the frame**:
  `usable_families` builds a fontconfig collection and probes every family (the same
  `FcFontSort` cost that showed up in a profile at 16%), and it is cached in a `OnceLock`
  so the second open is instant. The app warms both lists on a `dtr-warm` thread at
  startup and `-v` logs `warmed the settings lists` and `settings populated`.
- **Key bindings are config, and the format is Zed's**: `[keys]` maps an action name to a
  chord (`ctrl-shift-t`), a named key (`tab`), or several chords in a row
  (`ctrl-k ctrl-s`), and `false` hands a chord back to the shell; the vocabulary is in
  `crates/dtr/src/keys.rs`. Two rules bite: **shifted punctuation is written as the
  character it produces** — ctrl+shift+backslash reports `|`, so the binding is `ctrl-|`
  and the lookup retries without shift so both spellings work — and **a chord that starts
  a longer binding waits**, which is why a pending sequence flushes after a second.
- **A trailing space is a word break, not whitespace to trim**: the palette trimmed it
  before deciding which word was being completed, so `ssh ` looked like the command `ssh`
  and offered commands where hosts belong; `Parsed::of` keeps it, and after a space there
  is nothing to complete for a command with no arguments.
- **A command's first letter is its accelerator, so it is unique**: `s` is `ssh` and
  nothing else may start with `s`, so rename the new command rather than share the letter
  (`shell` became `exec`). `crates/dtr-complete` fails the build when two commands collide
  and holds the ranking to it — one letter must *complete* to its command, not just rank
  it first. A command with no `palette::action_for` arm does not belong in the list: `log`
  and `logbook` were offered and inert, and are gone.
- **`C-n` and `C-p` are consumed like the arrows**, taken from `input_mut` before the text
  field sees them so navigating cannot land an `n` in the query. **Reopening the palette
  starts a fresh sentence**: `toggle` clears query, selection and completions. **A modal
  must ignore key releases** — the release of the keystroke that opened it arrives while
  it is open, which made the settings menu flash and vanish — so overlays check
  `ElementState::Pressed` first. **`Ctrl+Shift+T` opens a tab** (`Ctrl+Tab` only cycles
  ones that exist).
- **A binding nobody can find is a binding nobody has**: `ctrl-?` opens the keyboard map
  ([keymap.rs](crates/dtr/src/keymap.rs)), and `keys` in the palette opens it too, because
  the chord itself is the thing a new person does not know. The card is built from the
  live `keys::Keymap`, so it shows the rebindings `[keys]` in the config has applied and
  says `unbound` where a binding was switched off — "does nothing" and "is not bound" are
  the same silence at the keyboard. Each row also names the action as `[keys]` spells it
  (`new-tab`, `split-horizontal`), because the card is where a person finds out what to
  *write*, not only what to press. **A row is a button**, because rebinding has to start
  somewhere a person can find: pressing one opens a popup that waits for a key, with a
  `reset`. The key is captured by `App::on_key` rather than by the card — the card draws
  and reports, and dispatch is the only thing that knows what a keystroke means — and a
  replacement is `dtr_config::set_value("keys.<name>", …)` followed by rebuilding the map
  from `config.keys`, so the card cannot show a chord a restart would not have. Reset is
  `dtr_config::remove_value`, not "write the default back": deleting the override is what
  makes a future default apply. `keys::refuse_reason` is the one policy for a chord that
  cannot be taken — no modifier, already another action's, or the start of a longer
  binding — and the popup says which, because a refused key and a dead card look alike.
  The columns, rows and fonts come from the skin, so the native style gets a taller card
  whose body scrolls inside the same 588 points. **The chord column is measured, not a
  constant**: each column asks the widest chord it holds, because `ctrl-alt-shift-right`
  and `shift-pageup` are in different columns and one width that fits the first wastes a
  third of the second. The scroll bar's width is reserved for the same reason — a
  floating bar paints over the last characters of the `[keys]` names, which are the point
  of that column — and `Group::COLUMNS` is the index the two columns divide at, with a
  test that keeps the halves about as tall as each other.
- **Icons are Lucide SVGs, fetched by a skill in this repository**:
  `.dsh/skills/lucide-icons` and `scripts/fetch.sh` pull
  `https://lucide.dev/api/icons/<name>` into `crates/dtr/assets/icons/<name>.svg`,
  refusing to overwrite one already committed; they are embedded with
  `egui::include_image!` and rasterised by `egui_extras`, so the one-file build carries
  them. Lucide's `currentColor` rasterises to *black* and `Image::tint` only multiplies,
  so `fetch.sh` rewrites it to `#ffffff` and each file becomes a white mask the tint
  colours. `install_image_loaders` is idempotent and `tab_bar` calls it rather than
  trusting startup, because a kittest harness makes its own `Context` and would otherwise
  draw egui's missing-image glyph, a `⚠` egui's own fonts cannot render.
- **Keys can be typed at the window from a script**: `--keys "ctrl-shift-p;down;down"`
  applies one stroke per frame through `App::on_key`, `;` separates tokens because a chord
  may be a comma, `+` joins the chords of one binding, and `text:` types characters.
  `--click x,y` moves, waits a frame, then presses — what a hand does, which is what makes
  it exercise the real hover-then-press path — and `--drag x1,y1,x2,y2` calls the same
  `pointer_down/move/up`.

## Profiling and benchmarking

`./scripts/flamegraph.sh --demo` builds the `profiling` profile (release + debug info) and
samples a real window, through `perf` when the kernel allows recording and otherwise
`dtr --cpu-profile <path>`, which samples itself. `--cpu-profile /tmp/x.svg` always prints
a text self-time report, and `--exit-after MS` closes the window by itself, which
exercises shutdown without a window manager.

**`PowerPreference::LowPower` is the integrated GPU**, which is wrong for a terminal
someone stares at: `--gpu <substring>` (or `DTR_GPU`) picks by adapter name, and `-v` logs
the adapter and every adapter seen.

`./scripts/bench.sh` runs vtebench against dtr and prints the delta from the previous run.
**`docs/bench.md` is how to read it** — what a sample is bounded by, why a delta across a
different window means nothing, what each `.dat`, `.dat.size` and `.dat.meta` holds, and
the two payloads that are ours. Three things bite without it: the numbers only compare on
one machine against itself with the same window; a run writes `<dat>.meta` (rev, adapter,
window, pane cells, sample size) so a table can be identified afterwards, and
`--meta DAT [LOG]` rebuilds one; and a column that stops partway is usually an OOM kill,
not a slow terminal.

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
| A dtr tab grows to tens of GB and the kernel OOM-kills it | the VT core defaults to unlimited scrollback; `pane::new_terminal` sets a 100 MB limit (`dtr_config::SCROLLBACK_LIMIT`, also the ceiling), so a hit here means a *new* terminal escaped that helper |
| A stray console window next to dtr on Windows | the exe is console-subsystem; a process started without a console gets one allocated. Launching from `cmd` or Windows Terminal reuses theirs |
| The exe shows the generic blank in Explorer and in the taskbar's pin menu, while the running window has an icon | those read a resource out of the *file*; `WM_SETICON` is only a window. `crates/dtr/build.rs` compiles `assets/logo/dtr.ico` with `zig rc` — regenerate the artwork with `./scripts/icon.sh` |
| `include_image!` paints a small red ⚠ where the picture should be | egui_extras has no decoder enabled for that format: PNG needs its `image` feature, and only `svg` was on. `ui::every_embedded_image_decodes` is the test that fails instead |

## Where to look

- `PLAN.md` — design, milestones, spike findings, and reversed decisions.
- `crates/dtr-ttl/MANUAL.md` — the Tera Term macro language, distilled.
- `crates/dtr-ttl/tests/corpus/` — Tera Term's own 53 macro tests, vendored under their
  BSD-3 notice; its README says what they pin and what they cannot. Totals in
  `tests/corpus.rs` move only when the corpus is re-vendored, `tests/run_corpus.rs` runs
  them against a recording host, and `tests/corpus/corpus-dialogs.txt` is what they said —
  regenerate with `UPDATE_CORPUS_DIALOGS=1`.
- `docs/ttl-gaps.md` — what the TTL interpreter still does not do, and the order worth
  doing it in; `crates/dtr-ttl/tests/field/` is where real macros go.
- `docs/sftp.md` — measured decision memo for the SFTP idea.
- `docs/cjk.md` — measured memo and plan for everything outside the grid face: Han/kana,
  fullwidth punctuation, Nerd Font icons, emoji. Read it before changing `prepare_pane`'s
  placement rules; **§9 is the list of what is left**.
- `docs/bench.md` — what a vtebench number means, what a `.dat` is worth, and the `.meta`
  sidecar.
- `docs/themes.md` — which terminal theme formats dtr could read.
- `docs/gtk-palette.md` — how `native` reads the GTK4/libadwaita palette on Linux
  without linking GLib: the XDG portal for scheme and accent, constants measured
  out of libadwaita's gresource, the metrics that come with them, the live signal,
  and why v1 does not parse user CSS.
- `docs/ui.md` — why the chrome has a skin at all, what a token is, and what
  `native` means on each platform.
- `docs/windows.md` — the first Windows run, classified: the DPI cluster (grid font size,
  status bar), serial baud and autobaud, glyph and icon rendering, resize cost; §2.1
  carries the measurement at 125 %.
- `docs/chrome.md` — how the tabs get into the title bar: client chrome on Windows and
  Linux, and the per-platform metrics and glyphs that keep the buttons looking native.
- `crates/dtr/src/pane.rs` — transports, publish, reconnect, modem, paste.
- `crates/dtr/src/app.rs` — the event loop, keys, and the palette glue.
- `crates/dtr/src/layout.rs` — the split tree. It knows nothing about terminals; a leaf is
  an id, and the tab's map decides what that id is.
- `crates/dtr/src/render.rs` — glyph atlas and the instanced grid pass.
