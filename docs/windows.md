# dtr on Windows — first real run, classified

Status: notes, 2026-11-02 · Scope: ten observations from a first Windows session, each
classified against the code in this tree. Not a plan. §7 is the order worth doing them in,
and §8 is what still needs a capture from the machine that produced the list.

The short version: **the DPI story is one bug wearing three hats** (the grid is sized in
points but drawn in pixels), the status bar is the same mistake left unfixed in one place
after it was fixed in another, and most of the rest is either a design gap or unmeasurable
until there is an on-screen counter.

---

## 1. Summary

| # | Observation | Verdict | Where | Scope | Effort |
|---|---|---|---|---|---|
| 1 | Status bar (rx/tx) hides text | **Bug**, cause located | `app.rs:4429` vs `app.rs:3016` | every DPI ≠ 100% | XS |
| 2 | Baud input "needs to exist somewhere" | **UX gap** — the control exists but is buried | `settings.rs:566` | all | S |
| 3 | Autobaud | **Missing**, and needs a safety decision first | — | all | M–L |
| 4 | Tabs should be in the header | **Design change**, decided in [chrome.md](chrome.md) | `app.rs:4396` | all | M |
| 5 | Needs HiDPI support | **Bug**, and the config already promised it | `render.rs:677`, `lib.rs:42` | every DPI ≠ 100% | S–M |
| 6 | Latency feels bad on Windows | **Unverified**; two known floors, one invisible today | `pane.rs:1155`, `app.rs:4639` | Windows worst | measure |
| 7 | Debug UI for fps / frame timing | **Built** — see §5.2 | `crates/dtr/src/stats.rs` | all | S |
| 8 | Icons and Nerd Fonts render badly | **Partly reproduced**: an uncovered glyph draws *nothing*; a glyph wider than its cell overflows | `render.rs:367` | all | S–M |
| 9 | "pi has artifacts" | **Unverified**; one trap ruled out, see §4.2 | — | ? | needs capture |
| 10 | Windows resize is sluggish | **Real, partly inherent**; one avoidable cost found | `app.rs:4329` | Windows | S–M |

**1 and 5 are fixed**, and they were the same bug — the grid sized in points and drawn in
pixels, with the status bar the place it was left unfixed; §2.4 is what the fix touched, and
§2.1's two rows of measurements are the before and the after. The table keeps the verdicts of
the run that found it, because "we thought this was three problems" is the part worth
keeping. Note 9 (§4.2) was fixed too, and was not a rendering bug at all.

---

## 2. The DPI cluster (1 and 5)

### 2.1 The contract that is being broken

`dtr-config` documents the grid font size in **points** — "Grid font size in points"
(`crates/dtr-config/src/lib.rs:42`) — and the preferences menu presents it as a font size. The
renderer used that number as **physical pixels**, and nothing multiplied by the window's
scale factor. So on Windows at 150 % the terminal drew 14 physical pixels where 21 were
asked for, and because the cell was smaller it fitted ~1.5× too many columns while the egui
chrome — tab strip, status bar, settings — grew correctly.

**Measured, not inferred** (2026-11-07, the `winvm` guest, display scale set in Settings →
Display, `--capture` for the window and `mode con` inside the shell for the pane; the guest
reset to `base` between the runs so only the scale differed):

| scale | build | window (`--capture`) | cell | pane (`mode con`) |
|---|---|---|---|---|
| 100 % | either | 900 × 620 | 9 × 17 | 100 × 34 |
| 125 % | before | 1125 × 775 | 9 × 17 | **125 × 43** |
| 125 % | after | 1125 × 775 | 11 × 22 | **102 × 34** |

`winit` honours the requested `LogicalSize(900, 620)`, so the window — and with it the tab
strip, the preferences menu and every inset — grows by the factor. Before the fix the grid did
not: the same 14 pt was still 14 physical pixels, so the cell stayed 9 × 17 and the pane
*gained* 25 columns and 9 rows in the same window. In one line: at 125 % a five-letter word
was the same 39 px wide as at 100 %, while the tab above it was 25 % taller.

**The fix, and what it is not.** `GridRenderer::new` and `Fonts::load` take the DPI factor,
`look.size * scale` is the size every face is rasterised at, and `Metrics`, `Loaded::adjust`
and the atlas key are physical from there on — so the cell, the fallback adjustment and the
sprite canvas all inherit it without a second multiplication anywhere. `Gpu::new` passes
`window.scale_factor()`, `ScaleFactorChanged` sets `fonts_dirty` (which rebuilds the grid)
and `fit_panes` then re-fits the pty. The offscreen `--screenshot` path passes 1.0, because
a committed baseline must compare against the same raster on every machine.

The cell does not come out at exactly 1.25 × 9 × 17: widths and heights are `ceil`ed to whole
pixels so glyphs land on pixel boundaries, and `ceil(1.25 × 8.43) = 11` against
`1.25 × ceil(8.43) = 11.25`. That is the 102 columns instead of 100 in the table above, and
it is the same trade the 1× path makes.

The status bar is the other half of this section: it is drawn in points and reserved in
pixels (§2.2), so a serial session at 125 % no longer covers its bottom row. That half is
**reasoned, not observed** — the guest has no serial device, and the bar is only drawn for
one.

**Also measured on Linux**, because the code has no platform in it: Xvfb at 1280 × 800 with
`WINIT_X11_SCALE_FACTOR` forcing the factor, the same `--capture`, and `stty size` inside the
shell for the pane:

| factor | window (`--capture`) | pane (`stty size`) | cell, from the two |
|---|---|---|---|
| 1 | 900 × 620 | 100 × 34 | 9 × 17 |
| 2 | 1800 × 1240 | 105 × 35 | 17 × 34 |

The cell doubles; the five extra columns are the ceiling again (`ceil(2 × 8.43) = 17` where
`2 × ceil(8.43) = 18`), not the terminal growing. That is the harness to use for a scale
question on the host — `Xvfb :99 -screen 0 1280x800x24`, `DISPLAY=:99
WINIT_X11_SCALE_FACTOR=2 dtr … --capture PNG --capture-after MS --exit-after MS` — and it
needs no window manager.

The guest offers only 100 % and 125 % at 1280 × 800 on `virtio-vga`, so 150 % has no
observation on either platform for the *grid*. The caption's 150 % behaviour was settled
without one: the guest's per-DPI metrics say the caption band is 45 px there, and a plain
band is the wrong height for a strip that carries the tabs, so the client caption is a
design constant with the band as a floor (`chrome.md` §5). At an integer factor the `ceil`
costs the least; 1.5 is where a half-pixel cell would show, and it is the number to try
next.

### 2.2 Why the status bar covers the bottom row

egui panels are dimensioned in **points**, and `fit_panes` reserves space in **physical
pixels** (`crates/dtr/src/app.rs:3016`). The tab strip gets this right and says so:
`.exact_size(title_height)` in points — the platform's caption height, `chrome::title_height`
(`crates/dtr/src/app.rs:4396`, `crates/dtr/src/app.rs:4402`) — with a comment recording that
multiplying by the scale there made it grow twice at scale 2. The status panel does exactly
that multiplication:

```rust
.exact_size(crate::ui::STATUS_HEIGHT * self.scale())   // app.rs:4429 — points × scale
```

With `STATUS_HEIGHT = 24` and `GRID_GAP = 4` (`crates/dtr/src/ui.rs:204`,
`crates/dtr/src/ui.rs:216`), at scale `s`:

| scale | reserved (`28s` px) | drawn (`24s²` px) | covered |
|---|---|---|---|
| 1.00 | 28 | 24 | none (4 px slack) |
| 1.25 | 35 | 37.5 | 2.5 px |
| 1.50 | 42 | 54 | 12 px — most of a row |
| 2.00 | 56 | 96 | 40 px — two rows |

That is "Statusbar(rx/tx) hides text": bottom rows disappear under the bar, and only serial
sessions draw it at all (`status_visible`, `crates/dtr/src/app.rs:2982`), which is exactly
where the rx/tx counters are.

**Fixed**: the panel is `.exact_size(STATUS_HEIGHT)` in points now, which is what egui
scales, so the drawn height is `24s` against a `28s` reservation at every scale. The rule
is the same one the tab strip already had, and it is worth stating as a rule: a constant
that is drawn through egui is in points; a reservation `fit_panes` makes is in pixels; the
scale goes on exactly one side of that boundary.

**Why this was not caught:** the offscreen screenshot path uses the correct
`.exact_size(status_height)` (`crates/dtr/src/screenshot.rs:215`) and renders at 1×
(`screen_rect` in physical pixels with `pixels_per_point` left at 1.0,
`crates/dtr/src/screenshot.rs:132`). Every `--screenshot` in the repository is a 1×
renderer, so it cannot show a DPI bug of either kind. `--capture` on a live window is the
only instrument that can.

### 2.3 Moving between monitors

**Fixed**: `ScaleFactorChanged` sets `fonts_dirty`, which is the hook that already rebuilt
the grid after a settings change. The next frame rasterises at the new factor and
`fit_panes` — which runs every frame — re-fits the pty from the new metrics. The window
keeps its *physical* size across the move (Windows suggests a new rect via
`WM_DPICHANGED`; winit does not adopt it for us), so a moved window keeps its pixel size and
changes how many cells it holds, which is the behaviour a terminal should have. The refresh
rate is refreshed on `Moved` — which follows the monitor rather than the factor — and, since
Wayland sends no `Moved` for a monitor change, also by a once-a-second check
(`crates/dtr/src/app.rs`).

### 2.4 What the fix touched

- `GridRenderer::new(device, format, look, scale)` and `Fonts::load(look, scale)`:
  `let size = look.size * scale` is the single place points become pixels. `Metrics`,
  `Loaded::adjust`, the atlas key and the sprite canvas are all derived from it.
- `Gpu::new` passes `window.scale_factor()`; `rebuild_grid` passes `self.scale()`.
- `ScaleFactorChanged` → `fonts_dirty` (one line, reusing the existing rebuild hook).
- The status panel is `exact_size(STATUS_HEIGHT)`; the reservation in `fit_panes` keeps its
  `* scale`.
- `screenshot.rs` passes 1.0 twice — the offscreen path is a 1× renderer on purpose.
- `render::font_tests::a_cell_scales_with_the_display`: loads at 1.0 and 2.0 and asserts the
  cell, the baseline and the box thickness double to within the whole-pixel rounding. It is
  the test that would have failed every day since the grid got a font size.

Left alone, deliberately: notes 8 and 9 (icon sizing, the π artefact). The original memo
put this fix in front of them because every glyph was rasterised at 14 physical pixels, so
reproducing a *size* problem at 2× was measuring the wrong frame. That condition is gone;
those two now have a rasterised size that matches the display.

---

## 3. Serial (2 and 3)

### 3.1 Where the baud rate lives now

**Corrected 2026-11-12:** the gap this section diagnosed — the rate was visible but not
editable where it was visible — is closed. `crates/dtr/src/serial_dialog.rs` is a connection
sheet that the `+` menu and the palette's `connect <port>` both open: fifteen rates
(300 … 921600), a typeable field, and the store's per-port memory as the starting value;
`--baud` still exists for a capture. The bullets below are what the build this report ran
against did.

- Rates were a fixed eight-entry list, `9600 … 921600` (`crates/dtr/src/settings.rs:13`), in
  a combo box that only appeared when a serial session had focus
  (`crates/dtr/src/settings.rs:37`, `crates/dtr/src/settings.rs:566`); otherwise the row said
  "no port". The settings row still offers the eight, because it is where a *connected* port
  is changed; the sheet is where a *new* connection is described.
- Changing it does not reconfigure the port: the pane reconnects at the new rate, keeping its
  scrollback (`crates/dtr/src/pane.rs:1627`). That is deliberate — you cannot change an open
  port's rate in place with `serialport`.
- The rate is *displayed* in the pane label, `COM3 115200 8N1` (`crates/dtr/src/pane.rs:365`),
  which is what the tab title and the status bar show.
- The connect paths used to take the remembered rate silently, with `--baud` on the command
  line as the only way to say otherwise.

The gap was therefore discoverability and range, not plumbing: expose the rate in the connect
flow and allow a typed value (the list omitted 300/600/1200/2400/4800/14400/28800/500000/1M/2M).
The sheet is that; the status bar still only shows the number.

### 3.2 Autobaud

Nothing exists. Two honest designs, and they differ in risk:

1. **Listen and score.** Open at a candidate rate, read for a few hundred milliseconds,
   score the bytes (printable ratio, CR/LF pairs, escape-sequence sanity), keep the best.
   Transmits nothing. Works only when the device is already talking.
2. **Probe.** Send `CR` (or `0x55`) at each candidate and look for a sane response. Works
   when the device is silent, but **transmits into an unknown bus** — into a piece of
   embedded hardware that is mid-boot, that is how you feed it a stray command. It needs an
   explicit opt-in and probably a "listen-only" default.

Implementing this means `serial::Config.baud` becomes an enum (`Fixed(u32)` / `Detect`), the
reader has to own the probing loop, and the pane needs a state between "opening" and
"connected" that the status bar can show. Do it after §3.1 so the manual path is usable
first.

---

## 4. Rendering (8 and 9)

### 4.1 What was reproduced

A private-use glyph with no covering face draws **nothing at all** — not tofu, not a box.
`glyph()` returns `None` and the cell is skipped (`crates/dtr/src/render.rs:367`), and
`face_for` only accepts a family whose charmap maps the character
(`crates/dtr/src/render.rs:821`). Reproduced on Linux/lavapipe with `U+E080` between ASCII
cells: the icon cell is blank, and a space next to it looks identical. That is the worst
possible failure for a Nerd Font: silence.

Structural reasons a glyph that *does* render can still look wrong:

- **The advance is never consulted.** The cell is sized from the grid face's `M`; each glyph
  is emitted at its bitmap size and position with no clip, so an icon bitmap wider than a
  cell overlaps its neighbour by construction. That is the "icons are mushy" look at small
  sizes.
- **Wide glyphs are scaled bitmaps**, not re-rasterised at the target size — a CJK glyph is
  rasterised at the font size and then fitted into two cells.
- **Fallback faces are placed by their own ascent**, which is right for the baseline and
  wrong if an icon font's metrics are unusual.

**Corrected 2026-11-12:** all three are fixed, and the list is kept as the diagnosis it was.
`render::Constraint` ports Ghostty's rules, so a symbol is fitted or centred into its cell
instead of overflowing, `constraint_width` lets one borrow the next cell when it is blank, a
uniform scale re-rasterises the outlines rather than the bitmap, every glyph is placed on the
grid's baseline (`render.rs::placement`), and `Fonts::load` multiplies by the display scale
(§2.1). What remains is the blank cell this section opened with: an icon nobody covers is
still nothing, which is a font to install, not a measurement to fix — `--font-report` names
the face that answered.

Two things worth knowing before chasing this:

- `dtr --fonts` is the command that answers "does dtr see my font". In the devShell used here
  it listed six families and no Nerd Font even though `fc-list` knows them — that is a
  sandbox fontconfig, not a Windows conclusion, but it is the first check on the target
  machine.
- On a HiDPI display every glyph was rasterised at the unscaled size when this was written;
  the DPI fix in §2 makes the rasteriser take the factor, so re-judge "bad icons" only on a
  build with that fix (§2.1).

### 4.2 The trap: LF-only output on a raw pty staircases

**Corrected 2026-11-08**: the raw pty was the bug, not the harness. dtr now calls
`pty::set_sane` and leaves the kernel's line discipline in place, so a bare `\n` renders
as a new line and this section describes what *used* to happen. It is kept because the
diagnosis below is how the mode was found, and because the same picture still means "check
the tty mode" rather than "check the glyphs".

While reproducing the Nerd Font case I wrote a `--shell` script that printed with `\n` only.
The result looked like a catastrophic renderer bug: each row began where the previous line
ended, so the screen staircased by the length of every line.

It is not a renderer bug. dtr set the pty raw before spawning
(`pty::set_raw`, once documented in `AGENTS.md`), so `OPOST`/`ONLCR` was off and a bare
line feed moved the cursor down **without returning the carriage** — correct VT behaviour.
The same script with `\r\n` rendered perfectly aligned.

That mode also left `ECHO` and `ICANON` off, and `ssh` copies the local tty's modes to the
far end in its `pty-req` — so an ssh session in dtr came up with a raw *remote* tty: no
echo (typing invisible) and the same staircase on Ubuntu's motd, fixed only by `reset`,
which is `tcsetattr(sane)` at the other end. That is what settled it; the raw mode is gone.

Consequence for the "pi has artifacts" report: if the π came from a script, a `printf`, or
any program that writes bare `\n`, the artifact may be a staircase and not a glyph at all.
Rule it out before looking at the renderer.

### 4.3 "pi has artifacts" — what would settle it

No reproduction here: π renders cleanly, and the atlas is `R8Unorm` with `Nearest` filtering
and a one-pixel shelf gap (`crates/dtr/src/render.rs:134`,
`crates/dtr/src/render.rs:501`), which makes texture bleed unlikely. Candidates, in order:

1. **DPI** — a glyph rasterised at 14 px on a 2× display is aliased and half-size; this is
   the cheapest explanation and the first to rule out.
2. **Glyph overflow** — the glyph comes from a fallback face and its bitmap is wider than the
   cell, so it collides with the next character (§4.1).
3. **Driver/backend** — DX12 vs Vulkan differences are plausible and are exactly what a
   Linux run cannot see.
4. **A staircase or partial redraw** — see §4.2 and the resize path (§5.3).

The instrument: `--capture` on the Windows window (not `--screenshot`), the same payload
rendered on Linux, and the font family from settings. Without those, this item should stay
open rather than guessed at.

---

## 5. Performance (6, 7, 10)

### 5.1 The latency chain, and its two floors

Keystroke → pty → parse → publish → repaint → draw → present:

- **Present.** `AutoVsync` (`crates/dtr/src/app.rs`), one frame in the
  compositor's queue: `desired_maximum_frame_latency` is set to 1. On DX12 the
  wait for a free swapchain buffer happens in `get_current_texture` — wgpu's DX12
  backend waits on the frame-latency waitable object there, on the UI thread —
  so the default depth of 2 could delay a keystroke's echo by a whole refresh
  before the frame was even drawn. `wgpu-hal`'s Vulkan backend ignores the
  field, so this changes nothing on Linux.
- **Measurement gap the overlay has to close.** `cpu_ms` is taken *before* `present`,
  but on DX12 the vblank wait is in `acquire`, which is *inside* `cpu_ms`: the
  number already contains the compositor's stall and so cannot be read as "our
  work". A frame-phase instrument must start the clock after
  `get_current_texture` and keep acquire and present as separate maxima.
- **Publish gate.** Half a refresh interval, floored at 2 ms
  (`crates/dtr/src/pane.rs:59`); when the pane is hot it waits out the gate before publishing
  (`crates/dtr/src/pane.rs:1155`), so sustained typing pays 8.3 ms at 60 Hz, 4.2 ms at
  120 Hz, and **16.7 ms in a 30 Hz mode** — a 4K laptop panel is the place a 30 Hz mode
  actually happens, and there the gate alone is more than a refresh of echo latency.
  Idle sessions publish immediately (the deadline is already in the past), so a prompt is
  not delayed. Whatever counter reads the frame budget (an overlay, or the `publish gate`
  debug line when the panel changes) is where to check which mode the machine is in.

Expected worst case before the compositor is gate + one refresh; the latency
hint above removes one frame of queueing, which is the only part of that chain
this tree controls.

### 5.2 The debug counter (note 7)

**Built** (`crates/dtr/src/stats.rs`), drawn in the same egui pass as the rest of the chrome.
It exists because the numbers were only in `tracing` — loop iterations / frames / wakes per
second, per-frame CPU ms and glyph count, cell metrics, scale and adapter — and a person
looking at a laggy window should not have to run `-v` on the machine they are looking at.

- **On** with `--stats` at startup, or the palette's `overlay` command. That command is not
  called `stats` because a command's first letter is its accelerator and `s` is `ssh`'s
  (`crates/dtr-complete` fails the build on a collision). Off by default; there is no config
  key.
- **Shows** five lines: `frame last/avg/p99` ms, `present last/avg/p99` ms, fps with loop
  iterations and wakes per second, glyphs (rasterised this frame — 0 is steady state, a high
  count is font work), grid columns × rows, cell pixels and scale, and the GPU adapter. The
  two timings are separate on purpose: `cpu_ms` stops before `present` (§5.1), so the present
  wait — the vblank this box exists to make visible — cannot be inferred from it. The
  percentiles are over the last 120 frames (two seconds at 60 Hz). On an idle window the
  box reads about ten fps (`-v` beside it says `frames_per_s=10..12`); feeding a shell a
  line every 20 ms pushed the same run to `frames_per_s≈100`. Its own ten-a-second refresh is
  counted as a frame, and `--capture` forces every frame while it waits, so a PNG of an idle
  window shows the capture's rate rather than the idle one.
- **Where**: the bottom-right of the pane area, above the status row, as an `egui::Area` in
  `Order::Middle` — over the panes and under every card, so a preferences sheet still covers
  it. It is `interactable(false)` with selectable labels off, so a click or a drag over it
  belongs to the terminal underneath, and its colours and font are the skin's
  (`palette.card`/`text`/`border`, `style.value_font`): readable at 4K/150 % for the same
  reason the rest of the chrome is.
- **Cost, and what idle looks like**: off, no sample is taken, no `Instant` is read around
  `present`, and nothing is drawn — the only per-frame work is a bool. It reuses
  `LoopReport`'s rates rather than keeping a second set of counters that could disagree with
  `-v`. On, it refreshes its readout every 100 ms (`stats::REFRESH_INTERVAL`), **not every
  frame**: ten frames a second, and its own refresh is one of them, so a quiet window reads
  about ten fps rather than zero. Typing or an animation pushes the number to whatever dtr
  really draws — and the two timings are the cost of a real frame, not of a forced one. A
  counter that asked for a frame per refresh would measure itself: fps would pin to the
  display's rate whether the app was idle or busy, and the fans would run for a number.
  (While the window is occluded it still wakes ten times a second, finds no surface and draws
  nothing; that is the cadence, not a spin.)
- **One frame stale.** A frame's own cost is not known until it has been submitted and
  presented, so the box shows the ring as it stood when the frame began.
- **Not in `--screenshot`**: the offscreen path renders from injected state, at 1×, with no
  window, no `present` and no loop — there are no numbers to show. `--capture` photographs
  the live window and does show it (under lavapipe, the numbers are the software
  rasteriser's, not the machine's GPU).

### 5.3 Resize (note 10)

**Done: configure once per frame, not once per event.** `WindowEvent::Resized` now only records
the size (`Gpu::resize`); the frame that draws it calls `Gpu::configure_pending_resize` first,
and the pty is re-fitted by the same frame (`fit_panes` there, not in the handler). Measured on
Xvfb/llvmpipe, `-v` counting `surface resized` lines:

| how the sizes arrive | events | `surface.configure` calls |
|---|---|---|
| 120 resizes, 16 ms apart (a slow drag) | 120 | 120 before the change — one per event |
| 300 resizes back to back (a fast drag, or a maximise animation) | 300 | **1** after the change — one per drawn frame |

The second row is the point. Windows delivers one `WM_SIZE` per mouse move, so a fast drag on a
1000 Hz mouse can put a dozen between two frames — and every `surface.configure` waits for the
present queue to go idle in wgpu's DX12 backend (`device.wait_for_present_queue_idle`, then
`ResizeBuffers`), so that was up to a dozen swapchain rebuilds per drawn frame. Each configure
measured 0.6–1.3 ms on llvmpipe Vulkan; a DX12 `ResizeBuffers` is not cheaper.

The redundant per-event `fit_panes` is gone with it: it sent a pty resize whenever the cell grid
changed, which during a drag is most events, and on Windows that is a ConPTY resize per mouse
move. The frame path still re-fits every pane (`fit_panes` is called once per drawn frame and
sends nothing when the cell grid is unchanged), and the burst above was checked end to end:
after it, `stty size` inside the shell reported `30 92` for a 1300×900 window at 150 %.

What is left is inherent: the frame that draws a new size still reconfigures once, and a
ConPTY resize still happens when the cell count changes. The honest expectation stays "modest",
but the per-event multiplier is gone.

### 5.4 The 4K grid (a full screen of glyphs)

A 3840×2160 window at 150 % is a ~21 000-cell pane (~8 400 painted glyphs). Measured with
`--cpu-profile` and temporary phase timers on the dev host (release build, llvmpipe, so the GPU
side is software and only the CPU-side numbers are worth quoting):

- **Idle is quiet**: 6–10 loop iterations/s, 0–2 frames/s, the two frames being the new-tab
  destination scan finishing twice a second. There is no redraw spin to fix.
- **`GridRenderer::prepare` was 1.9 ms per frame of a 4K grid, and 1.2 ms after the cache.** The
  cost was re-resolving every glyph's face and glyph id, which parses the font
  (`FontRef::from_index` and its cmap table): one parse per glyph per frame. `Fonts::resolve`
  now caches `(style, char)` → `(face, glyph id)` and the same frame costs 1.2 ms; at 1350×930
  the same measurement is 0.22–0.27 ms. The gain is per-pixel work; the software renderer's
  `submit`/`present` dominated the rest of the 4K frame, so that part needs the Windows machine
  to judge.
- The egui chrome is ~0.2 ms on a frame that redraws nothing new, with occasional 1–5 ms spikes
  not yet explained (not per-pixel: it does not appear at small windows). The overlay is the
  instrument for that on Windows.

Not changed, and why: `COPY_SRC` on the window surface (needed for `--capture`, no evidence it
costs anything on DX12); adapter selection (`HighPerformance` on an Intel-only laptop is still
the Intel iGPU, and `WGPU_BACKEND=dx12|vulkan` already lets the machine A/B the two backends
without a rebuild); and the grid/egui render passes, which could be one pass — that saves a
full-surface `Load` on a tile-based GPU, but merging them measured no gain at all on llvmpipe,
so it is a Windows A/B candidate rather than a change to make blind.

---

## 6. Tabs in the header (note 4)

**Done**: client-side chrome on both platforms, tabs in dtr's style, window buttons in
the platform's — the design, the per-platform levers and the build order are in
[chrome.md](chrome.md), and its §11 has what the Windows guest verified (including the one
thing it did not: the snap-layout flyout). That work also gave the exe an icon, which is
where the taskbar and Explorer had been drawing the generic blank. The rest of this
section is the reasoning that led there, kept because it still explains what the merge
costs.

Before that work there were two rows above the grid: the OS title bar (default
decorations) and a 30-point tab strip inside the client area (`ui::TAB_HEIGHT`). "Tabs in
the header" meant merging them, which meant client-side decorations:

- a drag region (`window.drag_window()` on press, double-click to maximise);
- window buttons drawn by us, and on Windows 11 the snap-layout hover on the maximise button,
  which needs `WM_NCHITTEST` returning `HTMAXBUTTON` — i.e. a platform module;
- a Wayland and macOS story (server-side decorations, traffic lights);
- the tab strip's hit targets still have to work inside the drag region.

Options, cheapest first: (a) leave it, spend the row on something else; (b) merge only on
Windows, keeping decorations elsewhere; (c) custom decorations everywhere. (c) won: winit
keeps the Windows frame styles that Aero Snap needs and hides the frame in `WM_NCCALCSIZE`,
and egui already forwards drag, resize, maximise and title, so the platform work was the
snap-layout flyout and the Linux edge zones — both built (`chrome.md` §2), with the flyout
still not appearing. It was the one item on the list that blocked nothing, which is why it
was last in §7.

---

## 7. Order, and what each step unlocks

1. **Debug counter (§5.2) — done.** `--stats`, or the palette's `overlay`: the instrument for
   6 and 10 is now on the screen it measures.
2. **DPI: grid size + status bar (§2) — done.** `Fonts::load` takes the factor and the guest
   measures 102 × 34 cells at 125 % (§2.1); 8 and 9 were re-judged afterwards.
3. **Glyph fitting (§4.1) — done.** `render::Constraint` decides the policy (clamp every
   glyph to its cell, `fit_cover1` for symbols, `stretch` only where the ink is the shape)
   and re-rasterises wide/oversized glyphs at cell size instead of scaling a bitmap;
   `constraint_width` covers the neighbour-cell rule. A screenshot case with a Nerd Font is
   still worth adding to the guest run.
4. **Baud in the connect flow (§3.1) — done.** The serial connection sheet asks, with fifteen
   rates and a typeable field.
5. **Autobaud (§3.2)** — after the safety decision in §9.
6. **Resize (§5.3) — done.** One swapchain configure per drawn frame instead of one per
   `WM_SIZE`, and the pty re-fitted once per frame instead of once per event. The counter
   (§5.2) is still the thing that says how much of the rest is ConPTY.
7. **Tabs in the header (§6)** — implemented; [chrome.md](chrome.md) §11 is the record,
   including the flyout that did not reproduce in the guest.

Note 9 is not in the list because it is not actionable until §8's capture exists; if it turns
out to be DPI, step 2 closes it.

---

## 8. What is still unverified

These came from a Windows run and cannot be judged from this tree alone:

| Question | Instrument |
|---|---|
| Which of 8/9 is DPI, overflow, or driver | `--capture` at native DPI + the same payload on Linux |
| Whether the Nerd Font is even visible to dtr | `dtr --fonts` on that machine, and the configured family |
| Is latency the gate, vsync, or the panel | `-v` (`loop`, `publish gate`, `slow frame`) and the counter from §5.2 |
| How much of resize is ConPTY | the same drag on Linux under a pty, plus timing from §5.2 |
| Whether the 40-point client caption looks right beside Explorer/Settings | `-v` `strip geometry strip_points=40.0 chip_points=26.0 reserved_px=… strip_px=…` and a screenshot at 150 % — `chrome.md` §5 has the heights it is matched against |

Answered: **Windows DPI is reported as expected** — `winit` hands dtr 1.25, and since §2.1
the grid uses it too (102 × 34 cells at 125 %, against 125 × 43 before). The scale itself is
a *setting*, so a run can no longer be ambiguous about which one it happened at; write the
number down.

Still open on this machine: 150 % and 200 % have no observation, because the guest offers
only 100 % and 125 % at 1280 × 800. A bigger `virtio-vga` mode (or a second head) would be
where a fix at a *non-integer* factor is checked before anyone trusts it at 2×, and where
the status bar (§2.2, drawn in points over a reservation in pixels) can finally be seen at a
scale above 1 — a serial session is required for that and the guest has no serial device.

There is also no Windows test coverage at all: CI runs the suite on Linux and only
cross-compiles `.#windows`. ConPTY resize, DX12 presentation and DPI are unexercised by
construction. A smoke job that launches the exe and captures one frame would have caught the
status-bar bug's class, but not the status-bar bug itself (that needs a non-1.0 scale).

---

## 9. Open questions for the person who ran it

("Tabs in the header" is no longer one of them — [chrome.md](chrome.md) answered it.)

1. Autobaud — may it transmit a probe, or must it be listen-only? On what hardware will it be
   used?
2. Icons — is the Nerd Font configured as the grid family, or is dtr falling back to whatever
   it finds? (`dtr --fonts` answers half of this.)
3. The π artifact — from an interactive program or a script? In the grid or in egui chrome?
   Does it change when the window moves to a 100% display?

---

## 10. The file pane, and the two things only Windows showed

Measured 2026-11-14 on the `winvm` guest at 100 %, from `base`, with the release that shipped
F1b and F2: `dtr.exe --files C:\fixture --capture … --capture-after 2500`, and a second run
with `--keys ctrl-alt-p` for the picker. The window is a screenshot of a real D3D12 surface
(`Microsoft Basic Render Driver`), not the offscreen raster, so what is in it is what a person
sees.

**It presents only in the console session.** Over ssh, `dtr` dies in `Surface::configure`:
`DX12 SwapChain creation error 0x887A0022` (DXGI_ERROR_NOT_CURRENTLY_AVAILABLE), because a
process started over ssh runs in session 0 and has no display to present to. The capture above
comes from a task registered with `schtasks /create … /it` and run, which puts the process in
the logged-on console session. That is now the way to run any dtr screenshot here.

What looked right: the Windows skin in light mode (white cards, the accent on the focused pane
and on the primary button), the Windows caption buttons in the tab strip, Segoe UI everywhere,
the tiles with their drawn file glyphs — there is no icon theme on this machine, so the crate's
own glyphs are what the grid draws — the picker's footer (`the selected file`, `cancel`,
`insert`) and a split with two panes of the same listing.

What did not, and was fixed in one commit:

- **The breadcrumb read `// fixture`.** `crumbs` started at a literal `/` and kept only
  `Component::Normal`, so on Windows the drive was dropped, the separator doubled, and every
  target was a Unix path — a press on `fixture` would have navigated to `/fixture`. It now walks
  the components generally: the volume is the first target (`/`, `C:\`, `\\server\share\`), and
  a root completes its label (`C:` becomes `C:\`).
- **The footer showed `\\?\C:` clipped.** `Browser::new` canonicalises, and Windows'
  `canonicalize` answers with a *verbatim* path. That is right for the API and wrong for a
  person: `home` is `C:\Users\win`, so `~` never appeared, the places menu never ticked the
  folder being shown, and the status strip said `\\?\`. `plain()` strips the prefix (and
  `\\?\UNC\` → `\\`) at the one place a path is canonicalised, and again on the way to a label.

Both are invisible on Linux, which is why the Linux captures of F1b and F2 never showed them:
a path with a drive letter is not a shape that code had ever been given.

---

## 11. What the shell knows: icons, places, hidden

Measured 2026-11-15 on the same guest at 100 %, from `base`, with F2 plus a new `dtr-shell`:
`dtr.exe --files C:\fixture --capture … --capture-after 2500`, another run with `--keys ctrl-h`,
another with `--keys ctrl-alt-p`, one at `--files C:\`, and one with `AppsUseLightTheme=0`. The
places popup is the one thing here that needs a press **and** a release — `--click` is a press alone,
which is what the pane's own targets want and not what an egui `Popup::menu` toggles on — so that
capture comes from the guest's own pointer over QMP (`references/gui.md`), which is also the proof
that a hand can open it.

Three questions a Windows desktop answers differently, and the pane answered none of them:

**The icon.** There is no `share/icons` here. The answer is the shell's icon cache — the colour page
the registry associates with `.pdf`, the bitmap an `.exe` carries inside it, the folder the shell
draws for `Downloads` — so `dtr-shell` asks the shell for an `HICON` and turns it into RGBA, and
`dtr-files` encodes that as the same `Picture` (bytes and a URI) the icon theme would have produced.
Before it the grid drew its own glyphs: one page for a PDF, the same page for a video, the same page
for `notepad.exe`. After it, `notepad.exe` has Notepad's own icon, `.dll` the DLL-with-gear,
`.zip` a zipper, `photo.jpg` the JPEG picture, and `main.rs` a blank page — because Windows has no
association for it and Explorer shows exactly that.

Three details, each of which is a way this goes wrong:

- **Ask the extension, not the file.** `SHGFI_USEFILEATTRIBUTES` answers from the association without
  opening anything, so a hundred-name listing is a hundred registry lookups and no disk. An `.exe`,
  `.lnk` or `.ico` *carries* its picture, so those (and five more) are asked for for real — which is
  what puts the program's own icon on the tile rather than a generic application.
- **The system image list, not `SHGFI_ICON`.** The latter answers 32 px and nothing else, and 32 px in
  a 48-point tile is visibly soft. `SHGetImageList` has 16/32/48/256; `windows-sys` names the
  function but not the interface it returns, so `IImageList::GetIcon` is called through its vtable at
  index 10. A wrong index calls a different method with these arguments, so the answer is verified
  before it is used — a null handle is refused, and a bitmap that is not icon-sized is refused — and
  the 32 px path is the fallback for a process the shell will not give the list to.
- **The mask only when there is no alpha.** A modern icon has an alpha channel; a legacy one has an
  AND mask. Using the mask on a modern icon turns its rounded corner into a hard one and loses its
  shadow, so the mask is consulted only when every alpha byte is zero.

**The places.** `user-dirs.dirs` is a freedesktop file, and `Downloads` on Windows is a *known
folder* — one that OneDrive or a redirected profile is free to move — while the roots are *drives*.
The popup was `home` and `filesystem` (which on Windows meant `\`, the current drive's root) and
nothing else. It is now `home`, `desktop`, `downloads`, `documents`, `pictures`, `music`, `videos`,
then `Windows (C:)` and `WINVM (E:)`: the volume's own label, or Explorer's noun for the kind when
there is none (`Local Disk (C:)`, `DVD Drive (D:)`, `Removable Disk (E:)`), with the letter always
bracketed. The drives replace `filesystem`, because there is no single root to name.

**Hidden.** A leading dot is Unix's hidden file; Windows hides `FILE_ATTRIBUTE_HIDDEN`, and Explorer
hides `FILE_ATTRIBUTE_SYSTEM` with it. The view asked the *name*, which is right on one platform and
wrong on the other, so `Ctrl+H` was a lie: `C:\fixture` showed 9 entries with `secret.txt` and
`desktop.ini` among them, and `C:\` would have offered `pagefile.sys`, `$Recycle.Bin` and
`System Volume Information`. Hidden is now a fact of the listing (`dtr_vfs::Entry::hidden`), answered
by the machine that walked the directory; `C:\fixture` shows 9 and `Ctrl+H` shows 11, and `C:\` shows
its 13 ordinary entries.

**The walk came with it.** `std::fs::DirEntry` does not expose the attributes, and a `stat` per name
to get them is the second pass that can move a row between the visible and the hidden half *after* it
was drawn. `FindFirstFileW` returns the attributes, the size and the write time in the call that
returns the name, so on Windows the listing is one call per entry and the `facts` pass does not exist
at all.

**Dark mode needs no second icon path.** With `AppsUseLightTheme=0` the same window came up in the
Windows dark skin; the shell's icons are background-independent pictures, so nothing about them
changes with the scheme.

Deliberately missing: **overlay icons** (a shortcut's arrow, OneDrive's cloud) come from
`SHGetIconOverlayIndex` and want a second `HICON` composed per tile; **a menu row has no picture**, so
the places popup is words where Explorer's has icons beside them; and none of this has a *test* — the
pure halves (drive labels, mask-to-alpha, `FILETIME` to `SystemTime`) are unit-tested on any machine,
and the rest is this capture. That is also the answer to "is it native now": the pictures and the
places are Explorer's own, and the windows, the buttons, the footer and the tiles are still dtr's.
