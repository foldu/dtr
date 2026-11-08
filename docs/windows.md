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
| 7 | Debug UI for fps / frame timing | **Missing**, cheap, unblocks 6 and 10 | `app.rs:3235`, `app.rs:4069` | all | S |
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
(`crates/dtr-config/src/lib.rs:42`) — and the settings menu presents it as a font size. The
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
strip, the settings menu and every inset — grows by the factor. Before the fix the grid did
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
observation on either platform. At an integer factor the `ceil` costs the least; 1.5 is
where a half-pixel cell would show, and it is the number to try next.

### 2.2 Why the status bar covers the bottom row

egui panels are dimensioned in **points**, and `fit_panes` reserves space in **physical
pixels** (`crates/dtr/src/app.rs:3016`). The tab strip gets this right and says so:
`.exact_size(crate::ui::TAB_HEIGHT)` in points (`crates/dtr/src/app.rs:4396`,
`crates/dtr/src/app.rs:4402`), with a comment recording that multiplying by the scale there
made it grow twice at scale 2. The status panel does exactly that multiplication:

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
rate is still refreshed on `Moved` only, because that follows the monitor rather than the
factor.

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

- Rates are a fixed eight-entry list, `9600 … 921600` (`crates/dtr/src/settings.rs:13`), in a
  combo box that only appears when a serial session has focus
  (`crates/dtr/src/settings.rs:37`, `crates/dtr/src/settings.rs:566`); otherwise the row says
  "no port".
- Changing it does not reconfigure the port: the pane reconnects at the new rate, keeping its
  scrollback (`crates/dtr/src/pane.rs:1627`). That is deliberate — you cannot change an open
  port's rate in place with `serialport`.
- The rate is *displayed* in the pane label, `COM3 115200 8N1` (`crates/dtr/src/pane.rs:365`),
  which is what the tab title and the status bar show. So it is visible but not editable
  where it is visible.
- The connect paths never ask: the `+` menu and the palette's `connect <port>` take the
  remembered rate from the store, or the default (`crates/dtr/src/app.rs:3096`); `--baud`
  exists on the command line only.

The gap is therefore discoverability and range, not plumbing: expose the rate in the connect
flow, allow a typed value (the list omits 300/600/1200/2400/4800/14400/28800/500000/1M/2M),
and consider the status bar as the place to show *and* change it, since that is where the
number already is.

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

Two things worth knowing before chasing this:

- `dtr --fonts` is the command that answers "does dtr see my font". In the devShell used here
  it listed six families and no Nerd Font even though `fc-list` knows them — that is a
  sandbox fontconfig, not a Windows conclusion, but it is the first check on the target
  machine.
- On a HiDPI display every glyph is currently rasterised at the unscaled size (§2.1), so
  re-judge "bad icons" only after the DPI fix.

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

- **Publish gate.** Half a refresh interval, floored at 2 ms
  (`crates/dtr/src/pane.rs:59`); when the pane is hot it waits out the gate before publishing
  (`crates/dtr/src/pane.rs:1155`). Idle sessions publish immediately (the deadline is in the
  past), so a prompt is not delayed — but sustained typing pays up to 8.3 ms at 60 Hz.
- **Present.** `AutoVsync` (`crates/dtr/src/app.rs:4237`) with `present` on the same thread
  as event handling (`crates/dtr/src/app.rs:4639`), so it can block for up to a vblank and
  delay the next input event.
- **Measurement gap.** `cpu_ms` is taken *before* `present` (`crates/dtr/src/app.rs:4638`), so
  the existing `slow frame` log cannot show a vsync wait. Anything felt as latency is
  currently invisible.

Expected worst case before the compositor is ~25 ms (gate + vblank); whether that is what is
being felt on Windows is a measurement, not an argument. Windows adds DWM composition on top,
and the display's actual refresh mode matters (Windows will happily run a panel at 30 Hz).

### 5.2 The debug counter (note 7)

Everything needed already exists and is thrown away into `tracing`:

- loop iterations / frames / wakes per second (`crates/dtr/src/app.rs:3235`);
- per-frame CPU ms and glyph count, logged when over 8 ms (`crates/dtr/src/app.rs:4069`);
- publish gate in microseconds, whenever the panel changes;
- rx/tx rates (`Meter`), scrollback length, atlas size, cell metrics, scale factor, adapter.

A small egui `Area` in a corner, off by default, toggled by a palette command (and optional
config key), showing fps, frame ms, present wait, publish→frame latency, glyphs/frame, grid,
scale and adapter would turn notes 6 and 10 from feelings into numbers. It should reuse the
`LoopReport` rather than add a second set of counters, and it must cost nothing when closed.

### 5.3 Resize (note 10)

- Every `WindowEvent::Resized` calls `surface.configure` and requests a redraw
  (`crates/dtr/src/app.rs:4329`), and the handler also calls `fit_panes`, which the frame loop
  runs anyway (`crates/dtr/src/app.rs:3571`, `crates/dtr/src/app.rs:3774`).
- Windows delivers a `Resized` per `WM_SIZE` during a live drag, so that is a swapchain
  reconfigure plus a blocking present per event.
- `fit_panes` sends a pty resize whenever the cell grid changes, and on Windows that is a
  ConPTY resize — inference, not measured here, but it is the same cost Windows Terminal pays,
  which is consistent with "not really much better there".

Cheap wins to try, in order: configure once per drawn frame (or only when the size has been
stable), drop the redundant `fit_panes` from the handler, and only then consider a present
mode other than vsync during a drag. Measure with §5.2 before and after; the honest
expectation is a modest improvement, not a transformation.

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

1. **Debug counter (§5.2).** Small, and it is the instrument for 6 and 10.
2. **DPI: grid size + status bar (§2).** One theme, largest visible payoff; re-judge 8 and 9
   afterwards.
3. **Glyph fitting (§4.1).** Decide the policy — clamp every glyph to its cell, and
   re-rasterise wide/oversized glyphs at cell size instead of scaling a bitmap; add a
   screenshot case with a Nerd Font.
4. **Baud in the connect flow (§3.1).**
5. **Autobaud (§3.2)** — after the safety decision in §9.
6. **Resize (§5.3)** — after the counter says where the milliseconds are.
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
