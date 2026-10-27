# dtr logo

Two directions live here: the **geometric** mark and lockup from round 1, and the **bitmap**
(character-cell) version from round 2. Both are original, vector-only, flat, two-colour, and
contain no `<text>`, no fonts, no filters, no embedded images and no external references.

![geometric, bitmap and hybrid compared](comparison.png)

## The idea

The logo is a shell prompt: `~/dtr` is the directory you are standing in, the amber `$` is the
prompt, and the block cursor is the line still open. Amber is the phosphor of the serial-era
consoles this tool is for; the block cursor is DTR asserted — the terminal saying "I am here,
you may talk to me". The mark on its own is the `$`: the atom of the prompt, and the only part
of the line that can still be read at 16 px.

## A. Geometric (round 1)

The mark sits on a 64-unit square: the S is three horizontal bars (y = 12/32/52) joined by two
180° turns of r = 10, drawn with an 8-unit stroke, with a lighter 6-unit stem running from y = 2
to y = 62. The lockup runs on a 48-unit monospace cell with a 34-unit x-height and a 54-unit
ascender; the letters carry a 10-unit stroke, while the `$` keeps the mark's 8-unit stroke. The
sides are curved, the terminals flat, the weight a medium.

## B. Bitmap (round 2)

Every glyph is drawn on a real **8×16 pixel grid** (the MDA/VGA character cell), with hard
stair-steps and `shape-rendering="crispEdges"` — no curves and no anti-aliasing. The lockup is
`~/dtr $ █` at one unit per pixel (80 × 24 units, nine cells); the cursor is one full 8×16 cell,
so it is exactly the size of the font, as asked. The mark is a hand-tuned **16×16 pixel `$`**
with 2-unit strokes: the 8×16 text glyph is too narrow and its counters too cramped to work as an
icon at 16 px, so the icon is drawn at icon resolution, on the same square-pixel grid and at the
same 2-unit stroke weight. This is the one place where two forms exist — a 16×16 icon glyph and
an 8×16 text glyph; the lockup's `$` is the text form, the mark file is the icon form.

## Three-way comparison and recommendation

**Bitmap at large sizes.** At 1024 px the bitmap lockup is not better than the geometric one. An
8×16 cell forces the compromises real terminal fonts make — a `$` that can read as a 5, a `t`
that is a cross with a foot, an `r` that is a gamma — and at roughly 13 px per bit those
compromises become the whole impression: it reads as a screenshot of a terminal, not a logo. The
1-pixel-stroke alternative is more honest to a ROM font but looks spindly at the same size. The
bitmap glyphs here are a first pass; a dedicated pixel typeface would improve them, but the 8×16
cell still caps how much detail the large lockup can hold. The bitmap genuinely wins at 320 px and below, where the geometric line thins out and the pixel line
gets sharper; the 16×16 mark is excellent at 16, 32 and 64.

**Hybrid.** Bitmap lockup with the round-1 geometric mark: in the comparison it reads as two
logos in one frame — a smooth `$` sitting beside pixel text is a visible dialect change. I would
not ship that combination.

**My pick.** Keep the geometric set as the primary brand, because the primary use is a README
banner, a window icon and a slide at 512 px and up, and that is where it is stronger. Use the
16×16 bitmap mark as the favicon / 16–32 px icon if you want the pixel treatment (that is the
inverse hybrid: pixel icon, geometric lockup — the two are rarely seen together at one size).
If the bitmap screen is the identity you want everywhere, ship the bitmap set and accept the
coarser display lockup; to make a bitmap face that holds up at 1024, the next round should double
the cell (16×32) instead of stretching the 8×16 one.

## Files

| File | What it is | Use it on |
| --- | --- | --- |
| `dtr-mark.svg` | Geometric mark: amber `$`, transparent | Dark backgrounds, favicons, window icon |
| `dtr-mark-mono.svg` | Geometric mark in `currentColor`, transparent | One-colour stamps, print, inversion |
| `dtr-mark-light.svg` | Geometric mark in deep amber `#924F00`, transparent | Light backgrounds |
| `dtr-logo.svg` | Geometric lockup `~/dtr $ █`, black background baked in | READMEs, slides, dark UI |
| `dtr-logo-light.svg` | Geometric lockup, ink + deep amber on paper | Light documents |
| `dtr-mark-bitmap.svg` | 16×16 pixel mark, amber, transparent | 16/32/64 px icon, favicon, dark UI |
| `dtr-mark-bitmap-mono.svg` | 16×16 pixel mark in `currentColor` | One-colour stamps, inversion |
| `dtr-mark-bitmap-light.svg` | 16×16 pixel mark in deep amber | Light backgrounds |
| `dtr-logo-bitmap.svg` | 8×16-cell lockup, black background baked in | Small/retro lockup |
| `dtr-logo-bitmap-light.svg` | 8×16-cell lockup, ink + deep amber on paper | Light documents |
| `dtr-mark-512.png`, `dtr-mark-32.png`, `dtr-mark-16.png` | Geometric mark previews on `#0B0F0E` | Preview |
| `dtr-logo-1024.png`, `dtr-logo-light-1024.png` | Geometric lockup previews | Preview |
| `dtr-mark-bitmap-512.png`, `-32.png`, `-16.png` | Bitmap mark previews on `#0B0F0E` | Preview |
| `dtr-logo-bitmap-1024.png`, `dtr-logo-bitmap-light-1024.png` | Bitmap lockup previews | Preview |
| `comparison.png` | The three-way comparison above | Review |

The PNGs are previews with the brand background baked in. The SVGs are the masters, and the mark
files are transparent.

## Palette

| Token | Hex | Where |
| --- | --- | --- |
| Amber | `#FFB000` | The `$` in the mark and lockup, the block cursor, accents on dark |
| Amber, deep | `#924F00` | The same accents on light surfaces (5.5:1 on paper) |
| CRT black | `#0B0F0E` | Primary background; ink for the one-colour mark |
| Bone | `#EDE6D6` | The `~/dtr` path on dark (15.5:1 on CRT black) |
| Paper | `#F4EFE4` | Background of the light lockup |

Amber on CRT black is 10.5:1. Amber on white is only 1.8:1, which is why light surfaces get the
deep amber and the mono file rather than the primary amber.

## Minimum sizes

- **Geometric mark** — 16 px on screen, 4 mm in print. On light surfaces use the light or mono file.
- **Geometric lockup** — 320 px wide on screen, 30 mm in print. Below that, use the mark.
- **Bitmap mark** — 16 px (native 1 px per bit), crisp at 16/32/64/512; do not go below 16.
- **Bitmap lockup** — 240 px wide at the smallest (3 px per bit); 320–640 px is its comfortable
  range, and it is legible but coarse at 1024.
- **Cursor** — in the bitmap lockup the cursor is one full cell (8 × 16 px at native), exactly the
  font size; in the geometric lockup it is 22 units wide at a 423-unit line width.

## Clear space

- Around the **geometric mark**: at least 16 units of the 64-unit box (25% of its height).
- Around the **geometric lockup**: at least 22 units (the width of the block cursor); the file
  already carries at least a 26-unit margin.
- Around the **bitmap mark**: at least one cell (1/16 of the icon); the SVG already has 1–2 cells
  of padding.
- Around the **bitmap lockup**: at least four cells (the built-in margin), so the artwork can be
  placed as supplied.
- Don't add glows, shadows, gradients, or re-space either prompt line; don't scale the bitmap
  files by a non-integer factor if you need the stair-steps to stay exact.

## Rendering

The PNGs were rendered with resvg:

```sh
nix run nixpkgs#resvg -- -w 512 -h 512 --background '#0B0F0E' assets/logo/dtr-mark.svg assets/logo/dtr-mark-512.png
nix run nixpkgs#resvg -- -w 1024 assets/logo/dtr-logo.svg assets/logo/dtr-logo-1024.png
nix run nixpkgs#resvg -- -w 16 -h 16 --background '#0B0F0E' assets/logo/dtr-mark-bitmap.svg assets/logo/dtr-mark-bitmap-16.png
nix run nixpkgs#resvg -- -w 1024 assets/logo/dtr-logo-bitmap.svg assets/logo/dtr-logo-bitmap-1024.png
```

## License

Original artwork for dtr, released under **MIT OR Unlicense**, the same as the rest of the
repository. No third-party font, icon or trademark is used.
