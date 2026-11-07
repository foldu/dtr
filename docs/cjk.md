# CJK, Nerd Font and emoji glyphs — what Ghostty does, and the plan

Status: research, 2026-11-07 · Scope: *how* dtr draws text outside its grid face
(Han, kana, fullwidth punctuation, Nerd Font PUA icons, powerline arrows, emoji)
compared with Ghostty, then the order to close the gap. Not a rewrite of the
glyph pipeline.

**Landed so far: M0, M1 and M2** (§5). `dtr --font-report` exists, every glyph is
placed on one grid baseline at its design size, and the cell width comes from the
VT core. §1 keeps the before-numbers because that is the evidence the plan rests
on; the "now" column in that table is what the binary prints. M3–M6 are still
plans.

Question: the grid face covers Latin, so everything else arrives through
`Fonts::face_for` and is placed by `render::prepare_pane`. On this machine that
path produced visibly wrong output — oversized Han, square full stops, icons
that overlap their neighbours. Ghostty ships the same fonts and looks right.
What does Ghostty do that dtr does not, and what is the cheapest order to adopt
it?

Everything in §1 was **measured on this machine** with the real `Fonts` code, now
available as `dtr --font-report` (§7.1). §3 is **read** from Ghostty at `5813c94`
(2026-11-06). §8 says which is which.

---

## 1. What dtr drew before M1–M2

`Look::default()` — 14, DejaVu Sans Mono as the grid face. The cell is
**9 × 17**, baseline 13 px from the cell top (ascent 12.99, descent 3.30, no
line gap). "drawn" is the rectangle `prepare_pane` pushed as an instance;
"natural" is where the glyph sits with its own bearings on that baseline, which
is what M1–M2 made it draw.

| char | face that answered | advance | ink bbox (left, top) | drew before | draws now |
|---|---|---|---|---|---|
| `M` | grid (DejaVu) | 8.43 | 8×10 at (0,10) | 8×10 at (0,3) | 8×10 at (0,3) |
| `啊` | Noto Sans Mono CJK SC | 14.00 | 13×14 at (0,12) | **15.8×17 at (1.1,0)** | 13×14 at (1,1) |
| `漢` | " | 14.00 | 14×14 at (0,12) | **17×17 at (0.5,0)** | 14×14 at (0,1) |
| `日` | " | 14.00 | 10×13 at (2,11) | **13.1×17 at (2.5,0)** | 10×13 at (2,2) |
| `。` | " | 14.00 | 5×5 at (0,3) | **17×17 at (0.5,0)** | 5×5 at (0,10) |
| `、` | " | 14.00 | 5×5 at (0,3) | **17×17 at (0.5,0)** | 5×5 at (0,10) |
| `F015` (Nerd Font) | **Courier New** | 8.40 | 5×2 at (2,13) | 5×2 at (2,2.4) | (wrong face) |
| `E0B0` (powerline) | **Monaspace Radon** | 8.68 | 8×18 at (-1,14) | 8×18 at (-1,-0.2) | (overflows the row) |
| `😀` | **Unifont Upper** | 14.00 | 14×13 at (0,12) | 18×16.7 at (0,0.3) | (monochrome, wrong face) |

The "drew before" column is the old rule applied to the measured ink (the ink
does not change with placement, so the arithmetic is exact); the "draws now"
column is `--font-report`. The first four rows are fixed; the last three are
what M3–M4 are for.

`啊` (`U+554A`) is the repository's CJK test character (`AGENTS.md`), so it is
in this table and in the tests: one glyph, chosen for ink in all four quadrants,
instead of a different character per bug report.

Three rules produced every "before" row, and each was wrong in a different way:

1. **Wide text was scaled, not placed.** `prepare_pane` computed
   `scale = min(2·cell_w / ink_w, cell_h / ink_h)` and rested the result on the
   cell's bottom edge. For `漢` that was ×1.21 — 21 % larger than Latin, and it
   grew with whatever filled first. Because the scale was driven by whichever
   axis filled first, a full-width punctuation glyph (5×5 ink) was blown up to
   17×17: **。 rendered as a solid square**. The same rule is why Han characters
   sat ~2 px lower than Latin: they were bottom-aligned, not baseline-aligned.
2. **Every face had its own baseline.** `glyph()` stored
   `top = ascent(face) − placement.top`, using *that face's* ascent. Noto CJK's
   ascent is 16.24 against the grid's 13.00, so its glyphs were pushed 3.24 px
   down; Monaspace Radon's 15.82 pushed the powerline arrow down 2.82 px;
   Unifont Upper's 12.25 lifted the emoji 0.75 px. A fallback glyph never shared
   the baseline of the text beside it.
3. **The width was guessed, and icons were not constrained.** `cell.wide` came
   from `unicode_width::UnicodeWidthChar` rather than the terminal, so VS16 and
   ZWJ clusters disagreed with the core and spacer cells were invisible to the
   renderer. And a PUA icon whose design advance is 1 em (14 px) was drawn at
   that size inside a 9 px cell — the powerline arrow above is 8 px wide but
   18 px tall in a 17 px row, so a powerline prompt overlapped the row above.
   Nothing fits a symbol to its cell and nothing gives a wide icon the second
   cell; **still true, M3**.

The width half of rule 3 is fixed (`fill_cells` reads `Cell::wide()`); the symbol
half is M3.

**The fallback face is not a policy, it is whatever comes first** — still true,
M4. Running the same probe twice against the same config produced
`0xProto Nerd Font Mono` first and then `Courier New`; `face_for` walks
`collection.family_ids()` and takes the first family whose charmap maps the
character. A proportional Latin face with a stray PUA mapping wins over the
installed Nerd Font whenever fontconfig happens to list it first. Emoji come from
`Unifont Upper` (or DejaVu Sans) for the same reason — the first family with the
codepoint, not the terminal-appropriate one.

Also still true and cheap to fix (M6): a block element (`█`, from the grid face)
is 10×18 at (-1,-1) in a 9×17 cell, so full blocks overlap their neighbours and
every TUI box leaks by a pixel.

---

## 2. What dtr does, in code

| what | where |
|---|---|
| cell metrics and the one baseline | `render.rs` `Metrics`, computed in `Fonts::load` |
| which face has a character | `render.rs` `Fonts::face_for` — preferred list (`fonts::FALLBACK`), then every family |
| rasterise | `render.rs` `Fonts::rasterise` — swash, `Source::Outline` only, `.hint(true)` |
| place it | `render.rs` `placement` — the bearings on `Metrics::baseline`, shared with `--font-report` |
| constrain it | `render.rs` `Constraint` / `lay_out` — a port of Ghostty's rule set; `constraint_for` is the per-codepoint policy |
| how many cells it may use | `render.rs` `constraint_width` — Ghostty's `renderer/cell.zig` rule, from the frame's neighbours |
| draw it | `render.rs` `prepare_pane` — no scaling, whatever the cell's width |
| cell text and width | `pane.rs` `fill_cells` — `Cell::wide()`, spacers carry no text |
| cursor | `pane.rs` `Cursor` (`wide_tail`) + `render.rs` `cursor_cell` |
| report | `main.rs` `--font-report` → `render::font_report` |
| UI text fallback | `fonts.rs` `ui_fallback` (egui's own faces are Latin-only) |

The atlas key is `(face, glyph id, size × 4, constraint, cells)`; the size is
the same for every face, so a fallback is rasterised at the grid's point size
with no adjustment (M4 changes that), and a constrained glyph is a different
raster because it is rasterised at a different size.

---

## 3. What Ghostty does

The whole model is one sentence: **a glyph is drawn into a cell-shaped canvas
whose baseline is the grid's baseline; nothing is scaled unless a rule says
so.** The rest is the rules.

### 3.1 One cell, one baseline

`font/Metrics.zig` `calc` derives, from the *primary* face only:

* `cell_width = round(advance)`, `cell_height = round(ascent − descent + line_gap)`;
* the line gap is split in half and put above and below the face;
* `cell_baseline` is the distance from the **bottom** of the cell to the
  baseline, so the face sits optically centred in the rounded cell
  (`cell_baseline = round(half_line_gap − descent − (cell_height − face_height)/2)`).

`font/face/freetype.zig` `renderGlyph` then places every glyph on that
baseline, for every face: the glyph's own bearings are added to
`y − cell_baseline`, so a face whose ascent differs from the primary's draws
higher or lower *without moving its baseline*. A face narrower than the cell is
centred by `round((cell_width − face_width)/2)`. Nothing about the placement
depends on the face's ascent.

### 3.2 Sizing is opt-in

`font/Glyph.zig` defines a `Constraint`: `size` ∈ `{none, fit, cover,
fit_cover1, stretch}`, `align_horizontal/vertical` ∈ `{none, start, end,
center, center1}`, padding, and `constraint_width` in cells. A codepoint with
no constraint is rendered at its design size:
`renderer/generic.zig` `addGlyph` passes `.constraint = getConstraint(cp) orelse
if (isSymbol(cp)) .{.size = .fit} else .none`. So Han, kana and fullwidth
punctuation are *never* scaled — they are placed, which is the difference
between `漢` at 14×14 and dtr's 17×17, and between `。` at 5×5 and dtr's 17×17.

### 3.3 Symbols get a second cell when there is room

`renderer/cell.zig` `constraintWidth(cell, x, cols)` decides how many cells a
glyph may use:

* a grid-wide cell (Han) → 2;
* a non-symbol → the grid width (1);
* a *symbol* (PUA, symbols-for-legacy-computing, emoji-ish) → 2 cells **if** it
  is not in the last column, the previous cell is not also a symbol (so a row
  of icons stays aligned), and the next cell is blank; otherwise 1.

That is why a Nerd Font icon in Ghostty is allowed to be ~18 px wide in a
9 px cell — it borrows the empty cell to its right — while a solid row of icons
stays on the grid.

### 3.4 Nerd Font constraints are data

`font/nerd_font_attributes.zig` + `nerd_font_tables.zig` are a generated
3-level lookup from codepoint to `Constraint`, produced by
`nerd_font_codegen.py` from the Nerd Fonts patcher's own patch-set attributes
(`SymbolsNerdFont`). The table is ~2 400 lines and mostly
`fit_cover1` + `align_* = .center` + `max_constraint_width = 1`, with
`stretch` for the sets that are meant to tile (powerline, block-ish sets) and
`cover` for a few. This is why patched glyphs line up across Nerd Font
versions: the terminal, not the font, decides how they sit.

### 3.5 Graphics elements are drawn, not borrowed

`font/sprite/Face.zig` has a built-in "sprite font" that draws box drawing,
block elements, braille, powerline, geometric shapes, branch glyphs and
Symbols-for-Legacy-Computing itself, geometrically, into the same cell canvas.
Boxes and blocks therefore tile exactly at any cell size, instead of inheriting
whatever the font's em grid produced (dtr's `█` is 10×18 in a 9×17 cell).

### 3.6 Fallback discovery asks for the codepoint

`font/CodepointResolver.zig` `getIndex` is per (codepoint, style,
presentation) and cached. On a miss it asks discovery for the *codepoint*
(`discovery.zig` `discoverFallback`), and the fontconfig backend builds a
pattern with `charset = {cp}` and — importantly — `spacing = mono`, then takes
the first `FcFontSort` result. The comment is honest about it: spacing does not
exclude proportional faces, it only *prefers* monospace, which is enough for a
Nerd Font to beat Courier New.

### 3.7 Fallback faces are size-adjusted to the primary

`Collection.SizeAdjustment` — added with `default_fallback_adjustment =
.ic_width` — scales a fallback face so a chosen metric matches the primary's:
ideographic width when the face declares one, else x-height, else cap height,
else line height (`Collection.scaleFactor`). This is CSS's `font-size-adjust`.
A CJK fallback is therefore the *optically matching* size for the Latin grid,
computed, instead of "same point size, hope the design sizes agree".

### 3.8 Emoji are colour, from a second atlas

Presentation comes from the UCD (`uucode.is_emoji_presentation`) unless the
codepoint map overrides it; a glyph the face reports as colour (`isColorGlyph`)
goes to a BGRA atlas instead of the grayscale one, and emoji get
`size = .cover`, centred, 2.5 % padding. Text-presentation emoji stay in the
text face.

### 3.9 Combining marks are shaped

Cells are shaped as runs (`font/shaper/harfbuzz.zig`), and the per-cell x/y
offset from the shaper is added to the bearings. dtr draws each `char` at the
same pen position, so a decomposed kana dakuten or a combining mark lands on
top of its base instead of after it.

### 3.10 The escape hatches

`adjust-cell-width`, `adjust-cell-height`, `adjust-font-baseline`,
`adjust-icon-height`, and `font-codepoint-map` (force a codepoint to a family).
The cell-height adjustment re-centres the baseline; the icon-height one exists
because a patched 1 em icon in a 0.6 em cell is the common case.

---

## 4. The gap, ordered by how much it hurts

| # | gap | symptom | fix |
|---|---|---|---|
| 1 | wide text is scaled to fill the box | Han 21 % too big and low; **。 is a square** | **done**, M1–M2: design size on the grid baseline |
| 2 | baseline is per face | every fallback glyph floats or sinks by (face ascent − grid ascent) | **done**, M1: one baseline from the grid metrics |
| 3 | symbols are unconstrained and 1 cell | powerline/block/Braille overlap their neighbours and the row above | **done**, M3: `fit_cover1` + centring for icons, `stretch` for powerline and braille, the 2-cell rule |
| 4 | fallback face is "first in fontconfig order" | icons from Courier New, emoji from Unifont | M4: preferred list, monospace preference, per-codepoint policy |
| 5 | no fallback size adjustment | CJK optically larger/smaller than Latin even when placed correctly | M4: scale by x-height/ic-width at load |
| 6 | colour glyphs never requested | monochrome emoji, and boxes for bitmap-only faces | M5: second atlas, swash colour sources |
| 7 | graphics elements come from a font | `█`/box drawing leak a pixel, seams between blocks | M6: a sprite face — **not** a stretch, which turns a 3 px line into a block |
| 8 | cell width guessed by `unicode-width` | wrong for VS16/ZWJ clusters; spacer cells unhandled | **done**, M2: `Cell::wide()` from the core |
| 9 | no per-cell shaper | combining marks stack on the base | M5-ish: shape a cell's cluster before placing |
| 10 | cursor is one cell | a block cursor covers half a Han character | **done**, M2: `wide_tail`, 2-cell block |

---

## 5. Plan

Ordered so each step is independently visible and testable. M0 is the
instrument every later step needs; M1–M2 are the CJK fix proper; M3–M4 the
icons; M5–M6 the remaining families. Estimates are for a tested slice.

**Done: M0, M1, M2, M3.** Their acceptance lines are the tests named in §7.2,
and `dtr --font-report` prints the "draws now" column of §1.

### M0 — make it measurable (`--font-report`) — done

Add a diagnostic mode that prints what the glyph pipeline actually chose:
cell metrics, baseline, the grid family, and for a fixed sample (Latin, Han,
kana, fullwidth punctuation, PUA, powerline, emoji) the face, the size
adjustment, the ink box and the final rectangle. The probe in §7.1 is exactly
this; it should not stay a throw-away.

Also extend the existing `-v` `fallback face` line to include the face's
ascent, size adjustment and how it was found (preferred list / script /
walk), so a bug report has the answer without a rebuild.

Files: `main.rs` (flag), `render.rs` (a `report()` on `Fonts`). Effort: half a day.
Acceptance: `dtr --font-report` on this machine prints the §1 table.
Risk: none. Do this first or the later steps are judged by eye.

*Landed:* `--font-report` (not a subcommand — the existing non-interactive flags
are flat, and this one wants the same `Look` a window would use, so it sits after
the config and theme are read). The `-v` line now carries the fallback face's
ascended size; "how it was found" is still the walk, so it says nothing yet.

### M1 — one cell canvas, one baseline — done

* `Metrics` gains `baseline` (from the cell top) computed the Ghostty way:
  `cell_height = ceil(ascent + descent + line_gap)` with half the gap on each
  side, `baseline = round(ascent + half_line_gap + (cell_height − face_height)/2)`.
  For DejaVu this is unchanged (9×17, 13); it matters for faces with a real
  line gap, and it fixes the asymmetric inset dtr had.
* `Renderer::glyph` stores `top = metrics.baseline − placement.top` for **every**
  face. `Fonts::ascent(face, size)` is gone; a fallback's own ascent cannot move
  the baseline.
* Rasterise each face at its own size (see M4) and let the bearings place it.

Files: `render.rs`. Effort: half a day.
Acceptance (unit tests, skip-if-absent like the existing font tests):
`啊` is drawn at its ink size with `top == baseline − placement.top`;
`。` is not a square.
Risk: glyphs with deep descenders now extend below the cell — Ghostty allows
that too (it is why `adjust-cell-height` exists); visual check with a font that
has a large descent.

*Landed:* `placement(metrics, image)` is the one rule, used by the renderer and
the report. DejaVu has no line gap, so the cell metrics on this machine did not
move; the baseline is no longer per face.

### M2 — draw wide text, do not fit it; take the width from the core — done

* Delete the wide-scale branch. A 2-cell glyph is placed at
  `x + placement.left` inside the 2-cell box (18 px here), on the cell's
  baseline; the glyph's own bearings decide the rest.
* Take the width from the terminal: `pane.rs::fill_cells` reads
  `cell.raw_cell()?.wide()?` → `Narrow`/`Wide`/`SpacerTail`/`SpacerHead`.
  `Wide` draws across two cells; the spacers carry no text, so they draw
  nothing (the background stays per cell, which it already was). This replaces
  the `unicode-width` guess and makes VS16/ZWJ clusters agree with the cursor,
  selection and mouse.
* Cursor: carry `at_wide_tail` (`CursorViewport` has it) and, when set, move the
  mark back one cell and — for a block — double its width.

Files: `pane.rs`, `render.rs`. Effort: half a day.
Acceptance: `。` is drawn 5×5 at (0,10), not 17×17; `啊` at (1,1) not (1.1,0);
a block cursor on a wide character covers both cells; a wide character is `Wide`
and its second cell carries no text.
Risk: an 18 px pair for a 14 px glyph leaves a 4 px right gap between Han
characters. That is Ghostty's rendering too (its cell is narrower, so the gap
is smaller), and it is what keeps the advance honest. If it reads as loose,
the answer is the M4 size adjustment, not a per-glyph stretch.

*Landed:* the wide branch is gone; `fill_cells` asks the core; the cursor has
`wide_tail`. Not done: a wide glyph in the last fitting column can still emit an
instance up to one cell past the pane's right edge — the renderer has no scissor
and all panes share one draw call (`AGENTS.md`, "Panes share one instance
buffer"). It is a one-cell overdraw in a resize or an off-grid pane width, not a
wrong glyph; a per-pane scissor is the fix if it ever shows.

### M3 — symbols are constrained — done

* Port Ghostty's `Constraint` (size/align/pad/`constraint_width`) into
  `render.rs`, applied at rasterise time. The atlas key becomes
  `(face, glyph, size, constraint)`.
* `is_symbol(cp)`: PUA (`E000–F8FF`, `F0000–FFFFD`, `100000–10FFFD`),
  powerline (`E0B0–E0D7`), and Symbols-for-Legacy-Computing. Non-symbols get
  `.none`; symbols get `.fit` by default.
* Vendor the Nerd Font constraint table. It is generated from the patcher's
  attributes; taking Ghostty's generated table (MIT) with a provenance header
  is the house pattern (the Kanagawa theme does the same) and is more accurate
  than re-deriving the rules. If we want to avoid the data, the fallback rule
  is `fit_cover1` + centre for all PUA, `stretch` for powerline and blocks —
  worse, but 95 % of the visual win.
* The 2-cell rule from `cell.zig`, evaluated in `prepare_pane` where the
  neighbouring cells are known (dtr has the whole frame there, which is more
  than Ghostty's per-cell view).

Files: `render.rs` (and a generated `assets/nerd_fonts.rs` if vendored).
Effort: a day with the vendored table, a day and a half deriving rules by hand.
Acceptance: `F015` is drawn inside 18×17 centred on the cell pair when the
neighbour is blank, and inside 9×17 when it is not; `E0B0` fills the cell
height exactly (|height − cell_height| ≤ 1); a row of icons keeps one advance.
Risk: the constraint data is a new vendored artifact; record its provenance and
the generator's upstream commit.

*Landed:* the rule set is ported in full — `Fit` (`none`, `fit`, `cover`,
`fit_cover1`, `stretch`), `Align` (`none`, `start`, `end`, `center`, `center1`),
pads, `max_width`, and both height metrics (`cell`, and the patcher's `icon`
with its one-cell form) — but the *table* is not. `constraint_for` decides from
the codepoint: private use and the symbol blocks get `fit_cover1` + `center1` +
the icon height, powerline (`E0B0–E0D7`) gets `stretch` one cell wide, braille
(`U+2800–28FF`) gets `stretch`, and everything else is left alone.

That is this plan's own fallback rule, minus the part it got wrong. Box drawing
and block elements are **not** stretched: `─` is three pixels of ink and
stretching it draws a solid block, and `▀` would lose the half that makes it a
half. There is no rule that fixes both of those from the ink box alone, which is
why Ghostty draws them itself — they wait for M6.

A uniform scale is applied to the *outlines*, not the bitmap: the glyph is
re-rasterised at `size × factor`, so an icon at 1.6× is crisp instead of grey.
Only `stretch` changes the aspect ratio, so it is the one drawn into a wider
rectangle.

Measured here at 28 pt, before → after, same faces:

| glyph | before | after |
|---|---|---|
| `E0B0` (DejaVu grid) | 15×36 at (−1,−1), overlapping the row above | **17×33 at (0,0)**, exactly the cell |
| `F015` (from Monaspace Radon NF) | 19×15 at (−1,+8) | 19×15 at (0,+9) |
| `E0B0` (0xProto Nerd Font grid) | 19×45 at (−1,0) | 18×44 at (0,0), exactly the cell |
| `F015` (0xProto Nerd Font grid) | 18×16 at (0,14) | unchanged — it already fitted, and `fit_cover1` does not enlarge a glyph that fits |

Not done: the generated table. Every icon gets one rule instead of the patcher's
per-glyph one, which is right for the bulk of the PUA and wrong for the few
icons drawn to two cells — those get the second cell when the neighbour is
blank, which is the same compromise Ghostty's `constraintWidth` makes. `😀` has
no ink at all: Unifont Upper draws it monochrome and Noto Color Emoji has no
outline. That is M5.

### M4 — fallback choice becomes a policy

* `[font] fallback = ["Noto Sans Mono CJK SC", "0xProto Nerd Font Mono", ...]`
  in the config, prepended to `fonts::FALLBACK`; extend the built-in list with
  the Nerd Font families (`Symbols Nerd Font Mono`, the `* Nerd Font Mono`
  variants) and the emoji families.
* Two-pass walk: monospaced families first (reuse `fonts::works`, which already
  loads the face for the charmap test), then everything else. This is the
  portable version of `spacing = mono`, and it is what stops Courier New from
  answering for a private-use codepoint.
* Ask fontique's script fallback first for text scripts (`Han`, `Kana`,
  `Hangul`, `Arabic`, …) so the platform's own preference wins for CJK; the
  family walk is the last resort.
* Size-adjust a fallback face at load: `scale = primary_metric / face_metric`
  using ideographic width when the face declares it, else x-height. Rasterise
  that face at `size * scale`. Log the factor.

Files: `render.rs`, `fonts.rs`, `dtr-config`. Effort: a day.
Acceptance: on this machine PUA resolves to a `* Nerd Font` family and emoji to
a colour emoji family, deterministically, with the same answer across runs and
after a fontconfig cache rebuild; Noto CJK is rasterised at a scale within 5 %
of 1.0 for DejaVu, and the `--font-report` CJK size matches Latin cap height
visually.
Risk: the walk is still O(families) on the first new character; the preference
list is what keeps it to one load in practice. Measure with the `fallback
face` log.

### M5 — colour emoji

* A second atlas in BGRA (or RGBA) beside the grayscale one, plus an instance
  flag and a shader branch (the shader already branches for solid quads).
* Rasterise with `Source::[ColorOutline(0), ColorBitmap(BestFit), Outline]` and
  record whether the result is colour; emoji get `cover` + centre + padding,
  as Ghostty does.
* Presentation: default emoji presentation from the UCD; without a Unicode
  table in the tree, ask the core (`ghostty_unicode_*` is already linked) or
  treat a colour-capable glyph as emoji. Prefer the emoji family in M4 so the
  question is rare.

Files: `render.rs`, the WGSL shader. Effort: two days.
Acceptance: `😀` is 2 cells wide, in colour, centred and inside its cell;
`❤︎` with VS15 stays in the text face.
Risk: a second texture binds into the one pipeline; `Gpu`'s drop order must
keep carrying it (see AGENTS.md).

### M6 — graphics elements tile

* Cheap version: give box drawing, block elements, braille and powerline a
  `stretch` constraint so they exactly fill their cell(s). Kills the 10×18 in
  9×17 overlap and the seams between blocks.
* Accurate version (only if the cheap one is not enough): a sprite face like
  Ghostty's, drawing those ranges geometrically. That is the only way box
  corners are pixel-exact at fractional cell sizes.

Files: `render.rs` (cheap), or a new `crates/dtr/src/sprite.rs` (accurate).
Effort: a day stretched, or four days for the sprite face.
Acceptance: a full block covers its cell exactly at 3×, 9×17 and 10×19 cells;
`─` joins its neighbours with no gap; `┌` sits on the cell's left and top
edges.
Risk: stretching distorts any glyph that is not meant to tile — keep the range
list narrow and do not apply it to general symbols.

---

## 6. Config surface

Not implemented yet; M1–M2 needed none of it. Keep it small and name it after
Ghostty when it lands, so the mental model transfers:

```toml
[font]
fallback = ["Noto Sans Mono CJK SC", "0xProto Nerd Font Mono"]  # M4
adjust_cell_height = 0      # pixels; re-centres the baseline like Ghostty
adjust_baseline = 0
cjk_scale = 1.0             # opt-in; the old stretch-the-box behaviour
icon_scale = 1.0            # M3
```

`cjk_scale > 1` is the escape hatch for people who prefer Han filling its two
cells; it must not be the default, because it is the bug this plan removed.

---

## 7. Tests and measurement

### 7.1 `dtr --font-report`

The probe that produced §1 is a flag now:

```sh
dtr --font-report              # honours [font] size/family/path and the theme
dtr --font-report --font-family 'Noto Sans Mono CJK SC'
```

It loads the real `Fonts`, walks the same sample (`M`, `g`, `啊`, `あ`, `。`, `─`,
`█`, a Nerd Font icon, a powerline arrow, an emoji), and prints for each: the
face and family that answered, that face's own ascent, the advance, the ink box,
and the rectangle `prepare_pane` would emit. No GPU, no atlas, no window — so it
runs on a CI box with no `/dev/dri`.

`啊` is first among the CJK samples because it is the repository's CJK test
character (`AGENTS.md`); the report and the tests look at the same glyph.

### 7.2 Assertions, not pixels

Placement is deterministic arithmetic on a font, so the tests assert the numbers
with a 1 px tolerance rather than adding grid goldens (a golden would duplicate
the rasteriser-disagreement problem the UI snapshots already have). Landed:

* `a_wide_glyph_keeps_its_design_size_and_baseline` — `啊` is drawn at its ink
  size, inside its two cells, with `top + placement.top == metrics.baseline`.
* `a_fullwidth_stop_is_not_a_square` — `。` fits inside one cell.
* `a_block_cursor_on_a_wide_tail_covers_both_cells` — and a bar stays one cell.
* `a_wide_character_is_two_cells_and_a_spacer` (pane) — `啊` is `Wide`, the cell
  after it carries no text and is not wide, the next character is column 2.
* `the_constraint_table_says_what_each_family_gets` — the codepoint policy, with
  no font at all: text untouched, icons fitted, powerline and braille stretched,
  box drawing and blocks left alone.
* `a_symbol_borrows_a_cell_only_when_its_neighbour_is_blank` — the two-cell
  rule, driven through real `Frame`s: blank either side is two cells, an icon to
  the left is one, text to the right is one, the end of a row never borrows the
  next row's first cell.
* `an_icon_fits_and_is_centred` — a PUA glyph is inside `2 × cell_w` and
  `cell_height`, centred on its own cell (flush left when it is wider than one),
  and one cell is a smaller box than two.
* `a_powerline_arrow_fills_the_cell_height` — `E0B0` is the cell's height.
* `a_braille_cell_fills_its_cell` — `U+28FF` is the cell exactly, and at its
  corner.

Still to write, with the milestone that makes them mean something:

* `a_block_fills_its_cell` — `█` size equals the cell (M6, if the sprite face
  lands; a block element is deliberately untouched until then).
* `a_monospace_family_wins_over_a_proportional_one` — the M4 policy, with a
  synthetic candidate list.

All of them skip when the font is absent, like the existing CJK tests, because
the devShell pins `FONTCONFIG_FILE` to `fontPackages` — see §7.3.

### 7.3 The devShell pins the font set, and that is a trap for this work

`FONTCONFIG_FILE` in the devShell and in `checks.tests` points at
`fontPackages = [dejavu_fonts noto-fonts-cjk-sans]`, i.e. **17 families** and no
Nerd Font. Outside the shell this machine sees 351 families, three of them Nerd
Font families (`0xProto Nerd Font*`). Consequences:

* add a Nerd Font to `fontPackages` (e.g. `nerd-fonts.0xproto`, the one
  installed here) so the M3 PUA tests run in CI instead of skipping — **not
  done yet**;
* every §1-style measurement must say whether it was taken inside or outside
  the devShell — the numbers differ (inside, PUA has *no face at all*, which the
  report now says out loud);
* a bug report "CJK looks wrong" from someone running `nix develop` then `dtr`
  is reporting the *pinned* font set, not their system's.

### 7.4 What the screenshots can and cannot show

`--screenshot` renders offscreen at 1× and `--capture` copies the live window,
so a HiDPI check of CJK needs `--capture` (docs/windows.md §2). Neither is a
test gate; the placement assertions are. Two traps in the harness, both of which
read like renderer bugs:

* Capture with a shell that does not read the user's bash rc
  (`--shell "bash --norc --noprofile"`): a bad `PS1` prints readline's `\[`
  markers into the grid.
* A script that prints `\n` renders a staircase. The pty is raw (that is the
  whole `pty::set_raw` story in AGENTS.md), so nothing turns LF into CRLF for
  the child: every line starts where the previous one ended. Print `\r\n` from
  the sample, or the picture is unreadable and the glyphs look misplaced.

---

## 8. Provenance and unknowns

Measured (§1): Linux, `Look::default()` (14), DejaVu Sans Mono → Noto Sans Mono
CJK SC → 0xProto Nerd Font Mono / Monaspace Radon / Unifont Upper, with the
probe on the real `Fonts` code. The two runs that disagreed about the PUA face
were the same config and package set, so the order fontconfig reports families
(and the state of its cache) is enough to change the answer — which is exactly
why a policy is needed.

Measured again after M1–M2, through `--font-report`, at 14 and at 28 (cell
17 × 33, baseline 26): `啊` 25 × 27 ink at (+2, +2), `。` 9 × 10 at (+1, +18),
`M` 15 × 20 at (+1, +6). A side-by-side `--screenshot` of the two builds at 28
(sharing `XDG_CONFIG_HOME` so they read the same config) is the sanity check for
the numbers: the old build's Han are ~21 % larger, sit low, and its full stop is
a blob; the new ones share the Latin baseline.

Read (§3): Ghostty `5813c94`, `src/font/Metrics.zig` (`calc`, `apply`),
`src/font/face/freetype.zig` (`renderGlyph`), `src/font/Glyph.zig`
(`Constraint`), `src/renderer/cell.zig` (`constraintWidth`, `isSymbol`),
`src/renderer/generic.zig` (`addGlyph`), `src/font/CodepointResolver.zig`
(`getIndex`), `src/font/Collection.zig` (`scaleFactor`,
`default_fallback_adjustment`), `src/font/nerd_font_attributes.zig` and
`nerd_font_codegen.py`, `src/font/sprite/Face.zig`, `src/font/discovery.zig`.

Unknown / not decided:

* Whether a per-cell shaper (HarfBuzz through swash's `shape` module) is worth
  it before M5. dtr only draws a cell's cluster, and the clusters that matter
  for CJK (base + combining dakuten in NFD) are rare in practice; the other
  scripts are the real audience.
* Whether `cjk_scale > 1` has any honest constituency. Ghostty's answer is
  `adjust-cell-width`, which changes the whole grid instead.
* Cell width: Ghostty rounds the advance (8 px for DejaVu), dtr ceilings it
  (9 px) to stop `llll` smearing. Ceiling costs 0.57 px of right margin per
  cell and is part of why a full block does not fit. Revisit only with a
  measurement of the overlap it was protecting against.
* The `--screenshot`-only render path draws at 1× and would not show a HiDPI
  regression here; `docs/windows.md` §2 has the wider story.
