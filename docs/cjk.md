# CJK, Nerd Font and emoji glyphs — what Ghostty does, and the plan

Status: research, 2026-11-07 · Scope: *how* dtr draws text outside its grid face
(Han, kana, fullwidth punctuation, Nerd Font PUA icons, powerline arrows, emoji)
compared with Ghostty, then the order to close the gap. Not a rewrite of the
glyph pipeline.

Question: the grid face covers Latin, so everything else arrives through
`Fonts::face_for` and is placed by `render::prepare_pane`. On this machine that
path produces visibly wrong output — oversized Han, square full stops, icons
that overlap their neighbours. Ghostty ships the same fonts and looks right.
What does Ghostty do that dtr does not, and what is the cheapest order to adopt
it?

Everything in §1 was **measured on this machine** with a throw-away test that
called the real `Fonts`/`rasterise` code (the probe is described in §7.1).
§3 is **read** from Ghostty at `5813c94` (2026-11-06). §8 says which is which.

---

## 1. What dtr draws today

`Look::default()` — 14, DejaVu Sans Mono as the grid face. The cell is
**9 × 17**, baseline 13 px from the cell top (ascent 12.99, descent 3.30, no
line gap). "drawn" is the rectangle `prepare_pane` pushes as an instance;
"natural" is where the glyph would sit with its own bearings on that baseline.

| char | face that answered | advance | ink bbox (left, top) | dtr draws | natural |
|---|---|---|---|---|---|
| `M` | grid (DejaVu) | 8.43 | 8×10 at (0,10) | 8×10 at (0,3) | same |
| `漢` | Noto Sans Mono CJK SC | 14.00 | 14×14 at (0,12) | **17×17 at (0.5,0)** | 14×14 at (0,1) |
| `日` | " | 14.00 | 10×13 at (2,11) | **13.1×17 at (2.5,0)** | 10×13 at (2,2) |
| `。` | " | 14.00 | 5×5 at (0,3) | **17×17 at (0.5,0)** | 5×5 at (0,10) |
| `、` | " | 14.00 | 5×5 at (0,3) | **17×17 at (0.5,0)** | 5×5 at (0,10) |
| `F015` (Nerd Font) | **Courier New** | 8.40 | 5×2 at (2,13) | 5×2 at (2,2.4) | (wrong face) |
| `E0B0` (powerline) | **Monaspace Radon** | 8.68 | 8×18 at (-1,14) | 8×18 at (-1,-0.2) | (overflows the row) |
| `😀` | **Unifont Upper** | 14.00 | 14×13 at (0,12) | 18×16.7 at (0,0.3) | (monochrome, wrong face) |

Three rules produce every row, and each one is wrong in a different way:

1. **Wide text is scaled, not placed.** `prepare_pane` computes
   `scale = min(2·cell_w / ink_w, cell_h / ink_h)` and rests the result on the
   cell's bottom edge. For `漢` that is ×1.21 — 21 % larger than Latin, and it
   grows with whatever fills first. Because the scale is driven by whichever
   axis fills first, a full-width punctuation glyph (5×5 ink) is blown up to
   17×17: **。 renders as a solid square**. The same rule is why Han characters
   sit ~2 px lower than Latin: they are bottom-aligned, not baseline-aligned.
2. **Every face has its own baseline.** `glyph()` stores
   `top = ascent(face) − placement.top`, using *that face's* ascent. Noto CJK's
   ascent is 16.24 against the grid's 13.00, so its glyphs are pushed 3.24 px
   down; Monaspace Radon's 15.82 pushes the powerline arrow down 2.82 px;
   Unifont Upper's 12.25 lifts the emoji 0.75 px. A fallback glyph never shares
   the baseline of the text it sits next to.
3. **Width is guessed and icons are not constrained.** `cell.wide` comes from
   `unicode_width::UnicodeWidthChar` rather than the terminal, and a PUA icon
   whose design advance is 1 em (14 px) is drawn at that size inside a 9 px
   cell — the powerline arrow above is 8 px wide but 18 px tall in a 17 px row,
   so a powerline prompt overlaps the row above it. Nothing fits a symbol to
   its cell, nothing gives a wide icon the second cell, and a pure-PUA
   codepoint falls back to whichever family fontconfig lists first.

**The fallback face is not a policy, it is whatever comes first.** Running the
same probe twice against the same config produced `0xProto Nerd Font Mono`
first and then `Courier New`; `face_for` walks `collection.family_ids()` and
takes the first family whose charmap maps the character. A proportional Latin
face with a stray PUA mapping wins over the installed Nerd Font whenever
fontconfig happens to list it first. Emoji come from `Unifont Upper` (or DejaVu
Sans) for the same reason — the first family with the codepoint, not the
terminal-appropriate one.

Also visible in the table, and cheaply fixable: a block element (`█`, from the
grid face) is 10×18 at (-1,-1) in a 9×17 cell, so full blocks overlap their
neighbours and every TUI box leaks by a pixel.

---

## 2. What dtr does today, in code

| what | where |
|---|---|
| cell metrics, per-face ascent | `render.rs` `Fonts::load` (the `Metrics` block), `Fonts::ascent` |
| which face has a character | `render.rs` `Fonts::face_for` — preferred list (`fonts::FALLBACK`), then every family |
| rasterise | `render.rs` `Fonts::rasterise` — swash, `Source::Outline` only, `.hint(true)` |
| place it | `render.rs` `Renderer::glyph` (`top = ascent(face) − placement.top`) |
| draw it | `render.rs` `prepare_pane` (the wide-scale branch) |
| cell text and width | `pane.rs` `fill_cells` (`unicode_width`, no spacer-cell knowledge) |
| cursor | `pane.rs` `Cursor` — one cell, no wide-tail |
| UI text fallback | `fonts.rs` `ui_fallback` (egui's own faces are Latin-only) |

The atlas key is `(face, glyph id, size × 4)`; the size is the same for every
face, so a fallback is rasterised at the grid's point size with no adjustment.

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

| # | gap | symptom | dtr's fix |
|---|---|---|---|
| 1 | wide text is scaled to fill the box | Han 21 % too big and low; **。 is a square** | draw at design size, on the grid baseline |
| 2 | baseline is per face | every fallback glyph floats or sinks by (face ascent − grid ascent) | one baseline from the grid metrics |
| 3 | symbols are unconstrained and 1 cell | powerline/block/Braille overlap their neighbours and the row above | `fit`/`stretch` + centring + the 2-cell rule |
| 4 | fallback face is "first in fontconfig order" | icons from Courier New, emoji from Unifont | preferred list, monospace preference, per-codepoint policy |
| 5 | no fallback size adjustment | CJK optically larger/smaller than Latin even when placed correctly | scale by x-height/ic-width at load |
| 6 | colour glyphs never requested | monochrome emoji, and boxes for bitmap-only faces | second atlas, swash colour sources |
| 7 | graphics elements come from a font | `█`/box drawing leak a pixel, seams between blocks | stretch, then optionally a sprite face |
| 8 | cell width guessed by `unicode-width` | wrong for VS16/ZWJ clusters; spacer cells unhandled; glyphs can bleed past the pane edge at a wrap | take `Cell::wide()` from the core |
| 9 | no per-cell shaper | combining marks stack on the base | shape a cell's cluster before placing |
| 10 | cursor is one cell | a block cursor covers half a Han character | `wide_tail` from the core, 2-cell mark |

---

## 5. Plan

Ordered so each step is independently visible and testable. M0 is the
instrument every later step needs; M1–M2 are the CJK fix proper; M3–M4 the
icons; M5–M6 the remaining families. Estimates are for a tested slice.

### M0 — make it measurable (`--font-report`)

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

### M1 — one cell canvas, one baseline

* `Metrics` gains `baseline` (from the cell top) computed the Ghostty way:
  `cell_height = ceil(ascent + descent + line_gap)` with half the gap on each
  side, `baseline = round(ascent + half_line_gap + (cell_height − face_height)/2)`.
  For DejaVu this is unchanged (9×17, 13); it matters for faces with a real
  line gap, and it fixes the asymmetric inset dtr has now.
* `Renderer::glyph` stores `top = metrics.baseline − placement.top` for **every**
  face (and its comment stops saying "ascent minus that"). `Fonts::ascent(face,
  size)` goes away; a fallback's own ascent is no longer allowed to move the
  baseline.
* Rasterise each face at its own size (see M4) and let the bearings place it.

Files: `render.rs`. Effort: half a day.
Acceptance (unit tests, skip-if-absent like the existing font tests):
`漢` is drawn at 14×14 with `top == 1`; `日` at 10×13 with `top == 2`;
a fallback glyph's baseline equals the grid's within 0.01 px.
Risk: glyphs with deep descenders now extend below the cell — Ghostty allows
that too (it is why `adjust-cell-height` exists); visual check with a font that
has a large descent.

### M2 — draw wide text, do not fit it; take the width from the core

* Delete the wide-scale branch. A 2-cell glyph is placed at
  `x + placement.left` inside the 2-cell box (18 px here), on the cell's
  baseline; the glyph's own bearings decide the rest.
* Take the width from the terminal: `pane.rs::fill_cells` reads
  `cell.raw_cell()?.wide()?` → `Narrow`/`Wide`/`SpacerTail`/`SpacerHead`.
  `Wide` draws across two cells; the spacers are skipped for text (the
  background stays per cell, which it already is). This replaces the
  `unicode-width` guess and makes VS16/ZWJ clusters agree with the cursor,
  selection and mouse.
* Clamp a `Wide` glyph at the last column to the pane's right edge: a wide
  character in the last fitting column can emit an instance past it today, and
  nothing scissors it.
* Cursor: carry `at_wide_tail` (`CursorViewport` already has it) and, when set,
  move the mark back one cell and double its width.

Files: `pane.rs`, `render.rs`. Effort: half a day.
Acceptance: `。` is drawn 5×5 at (0,10), not 17×17; `漢` at (0,1) not (0.5,0);
a cursor on `日` covers both cells; `the_width_comes_from_the_core` asserts a
known wide character is `Wide` and its second cell `SpacerTail`.
Risk: an 18 px pair for a 14 px glyph leaves a 4 px right gap between Han
characters. That is Ghostty's rendering too (its cell is narrower, so the gap
is smaller), and it is what keeps the advance honest. If it reads as loose,
the answer is the M4 size adjustment, not a per-glyph stretch.

### M3 — symbols are constrained

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

Keep it small and name it after Ghostty so the mental model transfers:

```toml
[font]
fallback = ["Noto Sans Mono CJK SC", "0xProto Nerd Font Mono"]  # M4
adjust_cell_height = 0      # M1, pixels; re-centres the baseline like Ghostty
adjust_baseline = 0         # M1
cjk_scale = 1.0             # M2, opt-in; the old stretch-the-box behaviour
icon_scale = 1.0            # M3
```

`cjk_scale > 1` is the escape hatch for people who prefer Han filling its two
cells; it must not be the default, because it is the bug this plan removes.

---

## 7. Tests and measurement

### 7.1 The probe that produced §1

A temporary `#[test]` in `render.rs`'s `font_tests` module: `Fonts::load(&Look::default())`,
then for each sample character `face_for` → `glyph_id` → `rasterise`, printing
the placement and recomputing the drawn rectangle with the same arithmetic as
`prepare_pane`. It needs no GPU (no atlas), so it runs anywhere. M0 promotes it
to `--font-report`; the tests below are its assertions.

### 7.2 Assertions, not pixels

Placement is deterministic arithmetic on a font, so assert the numbers with a
1 px tolerance rather than adding grid goldens (a golden would duplicate the
rasteriser-disagreement problem the UI snapshots already have):

* `a_wide_glyph_keeps_its_design_size` — `漢` height ≤ cell height, and its
  drawn top is within 1 px of `baseline − placement.top`.
* `a_fallback_shares_the_grid_baseline` — for a character from a fallback face,
  `top + placement.top == metrics.baseline` (0.01).
* `a_fullwidth_dot_is_not_a_square` — `。` drawn width ≤ cell width.
* `a_nerd_glyph_fits_its_cells` — PUA glyph inside `2 × cell_w` and
  `cell_height`.
* `a_block_fills_its_cell` — `█` size equals the cell (after M6).
* `the_width_comes_from_the_core` — `Wide` + `SpacerTail` for a known pair.
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
  installed here) so the PUA tests run in CI instead of skipping;
* every §1-style measurement must say whether it was taken inside or outside
  the devShell — the numbers differ;
* a bug report "CJK looks wrong" from someone running `nix develop` then `dtr`
  is reporting the *pinned* font set, not their system's.

### 7.4 What the screenshots can and cannot show

`--screenshot` renders offscreen at 1× and `--capture` copies the live window,
so a HiDPI check of CJK needs `--capture` (docs/windows.md §2). Neither is a
test gate; the placement assertions are.

---

## 8. Provenance and unknowns

Measured (§1): Linux, `Look::default()` (14), DejaVu Sans Mono → Noto Sans Mono
CJK SC → 0xProto Nerd Font Mono / Monaspace Radon / Unifont Upper, with the
probe on the real `Fonts` code. The two runs that disagreed about the PUA face
were the same config and package set, so the order fontconfig reports families
(and the state of its cache) is enough to change the answer — which is exactly
why a policy is needed.

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
