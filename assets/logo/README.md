# dtr logo

The logo is a shell prompt, because that is what dtr is for: `~/dtr` is the directory you are
standing in, the amber `$` is the prompt, and the block cursor is the line still open. The amber
is the phosphor of the serial consoles this tool serves, not the grey of a modern IDE; the cursor
is the one piece of chrome a terminal never removes. The mark on its own is the `$` — the atom of
the prompt, and the only part of the line that can still be read at 16 px.

The system is flat and built on a grid: one background and two inks. The mark sits on a 64-unit
square: the S is three horizontal bars (y = 12/32/52) joined by two 180° turns of r = 10, drawn
with an 8-unit stroke, with a lighter 6-unit stem running from y = 2 to y = 62. The lockup runs on
a 48-unit monospace cell with a 34-unit x-height and a 54-unit ascender; the letters carry a
10-unit stroke, while the `$` keeps the mark's 8-unit stroke (that is what keeps its counters open
at 16 px). Every letter is a path drawn for this project: there are no `<text>` elements, no
fonts, no filters, no embedded images and no external references.

## Files

| File | What it is | Use it on |
| --- | --- | --- |
| `dtr-mark.svg` | The mark: amber `$`, transparent background | Dark backgrounds, favicons, window icon |
| `dtr-mark-mono.svg` | The mark in `currentColor`, transparent | One-colour stamps, single-colour print, forced dark/light inversion |
| `dtr-mark-light.svg` | The mark in deep amber `#924F00`, transparent | Light backgrounds only |
| `dtr-logo.svg` | Primary lockup: `~/dtr $ █` in bone + amber, black background baked in | READMEs, slides, dark UI |
| `dtr-logo-light.svg` | The same lockup in ink + deep amber on paper | Light documents |
| `dtr-mark-512.png` | 512 px render of the mark on `#0B0F0E` | Preview, icon source |
| `dtr-mark-32.png` | 32 px render on `#0B0F0E` | Preview of the favicon size |
| `dtr-mark-16.png` | 16 px render on `#0B0F0E` | Proof of the minimum size |
| `dtr-logo-1024.png` | 1024 px render of the primary lockup | README header |
| `dtr-logo-light-1024.png` | 1024 px render of the light lockup | Light documents |

The PNGs are previews with the brand background baked in. The SVGs are the masters, and the three
mark files are transparent.

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

- **Mark** — 16 px on screen, 4 mm in print. On light surfaces use `dtr-mark-light.svg` (or the mono
  file) at any size; the primary amber mark is for dark surfaces.
- **Lockup** — 320 px wide on screen (the letters are then about 34 px tall), 30 mm in print.
  Below that, use the mark alone; the path and the cursor are the details that go first.
- **Cursor** — the cursor block is 5% of the lockup's width. At 320 px it is still 16 px wide.

## Clear space

- Around the **mark**: at least 16 units of the 64-unit box (25% of its height) on every side.
- Around the **lockup**: at least 22 units (the width of the block cursor) on every side. The file
  already carries at least a 26-unit margin, so the artwork can be placed as supplied.
- Don't set the mark on a busy photograph without the mono version or a solid tile; don't add
  glows, shadows or gradients; don't re-space the prompt line.

## Rendering

The PNGs were rendered with resvg:

```sh
nix run nixpkgs#resvg -- -w 512 -h 512 --background '#0B0F0E' assets/logo/dtr-mark.svg assets/logo/dtr-mark-512.png
nix run nixpkgs#resvg -- -w 32  -h 32  --background '#0B0F0E' assets/logo/dtr-mark.svg assets/logo/dtr-mark-32.png
nix run nixpkgs#resvg -- -w 1024 assets/logo/dtr-logo.svg assets/logo/dtr-logo-1024.png
```

## License

Original artwork for dtr, released under **MIT OR Unlicense**, the same as the rest of the
repository. No third-party font, icon or trademark is used.
