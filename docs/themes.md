# Terminal theme formats — decision memo

Status: research, 2026-10-27 · Scope: *which* format dtr reads, *what shape* a theme takes, and
*how the colours reach the VT core*. Not the renderer work.

Written against the code as it stands today (`crates/dtr/src/ui.rs`, `pane.rs`,
`crates/dtr-config`), and against the pinned VT crate (`libghostty-rs` rev `84373f8d`), whose
source I read directly rather than from docs.

Question: dtr draws a `Frame` of cells whose colours the VT core has already resolved to RGB. A
theme therefore cannot be a renderer-only paint job — it has to reach the terminal as a palette —
and it has to survive the fact that a serial console has no colour agreement with its far end.

**Owner's shape, taken as settled:** one theme per file, referenced by name from the config —
`theme = "kanagawa wave"`, where `kanagawa wave` is a file on disk, exactly as Ghostty does it.
Everything below is written to fit that; §7 and §8 are the parts that make it precise.

---

## 1. Recommendation

**dtr's theme format is the Ghostty theme file. One file per theme, in
`~/.config/dtr/themes/<name>`, selected by `theme = "<name>"` in `config.toml`. No second
dialect.**

**As shipped on 2026-11-08, three details of that sentence are out of date.** The search is
not three directories but six — `~/.config/dtr/themes`, `~/.config/ghostty/themes`,
`~/.local/share/dtr/themes`, `~/.local/share/ghostty/themes`,
`/usr/local/share/ghostty/themes` and `/usr/share/ghostty/themes`, in that order — because
reading the themes the user already has is the point. The lookup is exact **and then**
case-insensitive, because Ghostty capitalises its file names and people do not. And one
entry is compiled into the binary, `Kanagawa Wave`, used when nothing on disk matches,
because the places dtr runs have nothing to read: a Nix store path, or the one-file Windows
exe with no `share/`. A file of that name beats the compiled-in copy, which is how it is
intended to be changed.

```toml
# ~/.config/dtr/config.toml
theme = "kanagawa wave"
```

The trade-off, named: **there is no per-colour override in the config.** Customising a theme means
owning a copy of it — `dtr theme cp "kanagawa wave" mine`, then edit `~/.config/dtr/themes/mine`.
That is Ghostty's model and it is right here: an override table would be a second place colours
live, a second precedence rule to document, and a second thing to get wrong in `dtr config set`.
The escape hatch costs one command and produces a file that is still a valid Ghostty theme.

**Runner-up: inline colours in `config.toml`.** Switch only if the directory search proves fragile
on Windows (§7.5) — it trades a file lookup for a config file nobody edits by hand that cannot
share a theme with the user's Ghostty install.

**Rejected: one parser per format.** Six of the eight formats in §3 are *exports of the same
corpus* that also exports Ghostty themes. Every extra parser is permanent code and a permanent
import-fidelity bug surface — and one of those exports is provably wrong (§4.4).

---

## 2. What dtr needs, and where the colours actually go

### 2.1 Today

`pane.rs::publish` builds a `Frame` from `snapshot.colors()` and per-cell accessors:

```rust
default_fg: rgb(colors.foreground),
default_bg: rgb(colors.background),
// per cell:
if let Some(fg) = cell.fg_color()? { out.fg = rgb(fg); }
if let Some(bg) = cell.bg_color()? { out.bg = rgb(bg); out.has_bg = true; }
```

With no defaults set, those are the VT core's own defaults and every cell colour is resolved
inside libghostty-vt. A theme applied in the UI only would leave the emulator answering OSC 10/11
and OSC 4 with different colours than the screen shows — the class of bug that makes `vim` and
`fzf` look broken.

### 2.2 The answer to "can I set the palette, or only tint the paint?" — **you can set it**

Verified from the pinned crate's source (`crates/libghostty-vt/src/terminal.rs:1108–1196`,
`style.rs:224–330`). `libghostty_vt::Terminal` has:

```rust
set_default_fg_color(Option<RgbColor>)        -> Result<&mut Self>
set_default_bg_color(Option<RgbColor>)        -> Result<&mut Self>
set_default_cursor_color(Option<RgbColor>)    -> Result<&mut Self>
set_default_cursor_style(Option<CursorStyle>) -> Result<&mut Self>
set_default_cursor_blink(Option<bool>)        -> Result<&mut Self>
set_default_color_palette(Option<Palette>)    -> Result<&mut Self>   // None = built-in default
```

and in `style.rs`:

```rust
pub struct RgbColor { pub r: u8, pub g: u8, pub b: u8 }     // Clone, Copy
pub struct Palette(pub [RgbColor; 256]);                     // Clone, Copy — 768 bytes
pub struct PaletteIndex(pub u8);   // + BLACK … BRIGHT_WHITE consts
impl Palette {
    pub fn get(&self, PaletteIndex) -> RgbColor;
    pub fn set(&mut self, PaletteIndex, RgbColor);
    pub fn parse_palette_entry(&str) -> Result<(PaletteIndex, RgbColor)>;  // "N=COLOR"
    pub fn generate(base, skip, bg, fg, harmonious) -> Palette;            // cube + ramp
}
impl RgbColor {
    pub fn parse(&str) -> Result<RgbColor>;   // Ghostty colour syntax
    pub fn perceived_luminance(self) -> f64;
    pub fn contrast(self, other) -> f64;
}
```

So the whole 256-colour table follows: SGR `38;5;N` / `48;5;N` and anything that queries the
palette resolve **inside the VT core**, against what dtr sets. No renderer-side colour remapping
is needed or wanted.

Three consequences that shape the design:

1. **No colour parser to write.** `RgbColor::parse` accepts `#rrggbb`, bare `rrggbb`, 3/9/12-digit
   hex, X11 names, and `rgb:`/`rgbi:`. `Palette::parse_palette_entry` parses `0=#1f1f28` including
   `0x`/`0o`/`0b` index prefixes and surrounding whitespace. Both ship with the VT core we already
   link.
2. **No cube to invent.** `Palette::generate` is Ghostty's own generator (CIELAB trilinear cube at
   16–231, background→foreground ramp at 232–255, 0–15 preserved); `Palette::default()` is
   Ghostty's built-in 256-colour default.
3. **Not everything is in the API.** There is **no selection colour, no bold colour, and no dim
   colour** anywhere in it. Selection is a renderer concept in Ghostty and will be in M2; bold and
   dim are policies (§6.5, §6.6). The cell accessor says so explicitly: *"Bold color handling is
   not applied; the caller should handle bold styling separately."*

### 2.3 What this means for the existing code

- `Frame` needs **no new fields**: once the terminal has defaults, `default_fg`/`default_bg` and
  every cell's `fg`/`bg` are already themed.
- `ui.rs` needs no change for fg/bg. The only places it wants the new slots are the cursor
  (`default_fg.gamma_multiply(0.45)` today — a theme has a real `cursor` and `cursor_text`) and,
  in M2, selection.
- Apply the theme **on the pane thread**, after `Terminal::new(COLS, ROWS)` and before the first
  `vt_write`, because `Terminal` is `!Send`. `Theme` is plain `[u8; 3]`s and a `String`, so clone
  it in; reconnects rebuild the pane, so re-application is free.

> Naming: `crates/dtr/src/palette.rs` is the *command* palette. A colour type must be `Theme`, not
> `Palette` — `libghostty_vt::style::Palette` is already the 256-entry type.

---

## 3. Inventory: formats, corpora, licences

Counts and licences **fetched today** (GitHub API listings, raw files, crates.io). "Hub" =
[`mbadolato/iTerm2-Color-Schemes`](https://github.com/mbadolato/iTerm2-Color-Schemes), which
exports every other format from one source tree.

| # | Format | File shape | Canonical corpus | Count | Licence |
|---|---|---|---|---|---|
| 1 | **Ghostty** theme | `palette = N=#RRGGBB` ×16, then `background`, `foreground`, `cursor-color`, `cursor-text`, `selection-background`, `selection-foreground` | Ghostty ships a **build-time tarball** (not in the repo); hub also exports `ghostty/` | **819** shipped (`ghostty-themes-release-20261019`, dep name `iterm2_themes`); **860** hub `ghostty/`; **463** in installed `ghostty-1.3.1` here | Ghostty MIT (© Mitchell Hashimoto, Ghostty contributors); the *values* come from the iTerm2 corpus — §9 |
| 2 | **iTerm2** `.itermcolors` | XML plist; `Ansi 0 Color`…`Ansi 15 Color`, `Background Color`, `Foreground Color`, `Cursor Color`, `Bold Color`, `Cursor Guide Color`, `Cursor Text Color`, `Selection Color`, `Selected Text Color`; each a dict of `Red/Green/Blue/Alpha Component` `<real>` + `Color Space` `<string>` | hub `schemes/` | **860** | Repo MIT (© Mark Badolato) **+ per-scheme `licenses.yml`**: 205 MIT, 583 `unknown`, 18 `nonfree`, 16 GPL-2.0-or-later, 9 Apache-2.0, 5 CC0-1.0, 5 ISC, 4 WTFPL, 3 GPL-3.0-only, 2 LGPL-2.1-or-later, 2 GPL-2.0-only, 2 CC-BY-SA-4.0 ⇒ **~259 redistributable** |
| 3 | **Windows Terminal** `schemes` | JSON object, or an element of `"schemes": [...]`: `name`, `black`…`white`, `brightBlack`…`brightWhite`, `purple` (magenta), `background`, `foreground`, `cursorColor`, `selectionBackground` | WT `defaults.json`; hub `windowsterminal/` | **16** built-in; **860** hub | Terminal MIT; hub MIT-collection |
| 4 | **Alacritty** TOML (≥0.13) | `[colors.primary]`, `[colors.normal]`, `[colors.bright]`, `[colors.dim]`, `[colors.cursor]`, `[colors.selection]` | hub `alacritty/` | **860** | Alacritty Apache-2.0; hub MIT-collection |
| 5 | **Alacritty** YAML (≤0.12) | `colors:` → `primary`/`normal`/`bright`, values `'0x282a36'` | `eendroroy/alacritty-theme`; hub `yaml/` (400, different generic schema) | **65** | Apache-2.0; hub MIT-collection |
| 6 | **kitty** `.conf` | one option per line, space-separated: `color0`…`color255`, `background`, `foreground`, `cursor`, `cursor_text_color`, `selection_background`, `selection_foreground`, `url_color`, `underline_color` | hub `kitty/`; `dexpota/kitty-themes` | **860** hub; **169** dexpota | kitty GPL-3.0; hub MIT-collection; dexpota MIT |
| 7 | **base16** YAML | `system`, `name`, `author`, `description`, `variant`, `palette: base00`…`base0F` | `tinted-theming/schemes` `base16/` | **360** | MIT (© Tinted Theming) |
| 8 | **base24** YAML | base16 + `base10`…`base17` | same, `base24/` | **240** | MIT |
| 9 | **base16-shell** / tinted-shell | `color00="28/2a/36"` … `color21`, `color_foreground`, `color_background`; emits `OSC 4` itself | `tinted-theming/base16-shell` `scripts/` | **615** | MIT (© Chris Kempson, Tinted Theming) |
| 10 | **Gogh** | YAML: `name`, `author`, `variant`, `color_01`…`color_16` (16 = bright white), `background`, `foreground`, `cursor` | `Gogh-Co/Gogh` `themes/` | **1000** | MIT **or** Apache-2.0 |
| 11 | **terminal.sexy** | JSON: `{name, author, color: [16 hex], foreground, background}` | `stayradiated/terminal.sexy` `dist/schemes/` | **159** | MIT |

### 3.1 The fact that reorganises the problem

Ghostty does not maintain a theme corpus. Its `build.zig.zon`:

```zig
.iterm2_themes = .{
    .url = "https://deps.files.ghostty.org/ghostty-themes-release-20261019-151001-86a776d.tgz",
    .hash = "N-V-__8AAJ3vBQAq1fLMh5J8uWLkeDLS6WAdR2x5KcrmEyz6",
    .lazy = true,
},
```

— a tarball of **already-converted Ghostty-format files**, fetched at build time and installed to
`share/ghostty/themes` (`theme.zig::Location.resources`). The dependency is named `iterm2_themes`
because that is where the colours come from. Every one of the 819 names is a file in the hub's
`schemes/`, and the hub's own `tools/templates/ghostty` emits exactly the same shape.

So these are not eleven peer formats. They are **one corpus (860 iTerm2 schemes) with eleven
exporters**, and the Ghostty exporter keeps the most fields while being the easiest to parse.

Unverified: whether Ghostty's tarball is produced by the hub's `tools/gen.py`. The generator is not
in Ghostty's repo (`build.zig` never mentions it beyond the dependency).

---

## 4. The files, as they really are

Quoted from files fetched today — nothing here is from memory.

### 4.1 Ghostty — the shipped `Kanagawa Wave`

```
palette = 0=#090618
…
palette = 15=#dcd7ba
background = #1f1f28
foreground = #dcd7ba
cursor-color = #dcd7ba
cursor-text = #1f1f28
selection-background = #dcd7ba
selection-foreground = #1f1f28
```

I parsed all **819** shipped files: every one has exactly 16 `palette` lines (indices 0–15) and
exactly those seven scalar keys — no comments, no duplicates, no non-hex values, no extra options.
That uniformity is a property of the *generated* corpus, **not** of the format: a hand-written
Ghostty theme "is simply another Ghostty configuration file" and may contain `font-family`,
`background-opacity`, or `command = …` (Config.zig:575–588). A dtr parser must whitelist keys
(§8.1).

Filename hazards, measured: **560 of 819 names contain spaces** ("12-bit Rainbow", "Springfield
Resident Hight Contrast"), none are non-ASCII, longest is 35 characters. No file has an extension.

### 4.2 iTerm2 — `schemes/Kanagawa Wave.itermcolors`

```xml
<key>Ansi 0 Color</key>
<dict>
    <key>Alpha Component</key><real>1</real>
    <key>Blue Component</key><real>0.0941</real>
    <key>Color Space</key><string>sRGB</string>
    <key>Green Component</key><real>0.0235</real>
    <key>Red Component</key><real>0.0353</real>
</dict>
```

Top-level keys in this file: `Ansi 0`…`Ansi 15 Color`, `Background Color`, `Bold Color`,
`Cursor Color`, `Cursor Guide Color`, `Cursor Text Color`, `Foreground Color`, `Selected Text
Color`, `Selection Color`. Naming trap: **`Selection Color` is the selection *background*;
`Selected Text Color` is the selection foreground.**

Components are floats 0–1 truncated to four decimals (0.0353 is 9/255 = 0.035294…), so the 8-bit
value comes back only with **rounding** — measured in §6.2.

### 4.3 Alacritty TOML

```toml
[colors.bright]
black = '#6272a4'
…
[colors.cursor]
cursor = '#f8f8f2'
text = '#282a36'
[colors.normal]
black = '#21222c'
…
[colors.primary]
background = '#282a36'
foreground = '#f8f8f2'
[colors.selection]
background = '#44475a'
text = '#ffffff'
```

The ≤0.12 YAML dialect is the same tree with `0x`-prefixed values:

```yaml
colors:
  primary:
    background: '0x282a36'
    foreground: '0xf8f8f2'
```

Alacritty's man page adds fields no exported theme uses: `primary.dim_foreground`,
`primary.bright_foreground`, `[colors.dim]` (all eight names, auto-derived when absent),
`[colors.search]`, `[colors.hints]`, `[colors.line_indicator]`, `[colors.footer_bar]`. Cursor and
selection values may be the **sentinels** `CellForeground` / `CellBackground`.

### 4.4 kitty — and the bug that proves a point

```
color0 #21222c
…
color15 #ffffff
background #282a36
selection_foreground #282a36
cursor #f8f8f2
cursor_text_color #282a36
foreground #f8f8f2
selection_background #f8f8f2
```

**This file is wrong.** Compare §4.1/§4.3: selection background should be `#44475a`, selection
foreground `#ffffff`. The hub's `tools/templates/kitty.conf` hard-codes
`selection_foreground = Background_Color` and `selection_background = Foreground_Color`, so all 860
of its kitty exports have inverted selection colours. Derived exports are not interchangeable
sources of truth — the argument for reading the upstream format, or the one whose exporter is
obviously simple enough to be right.

kitty does support all 256 entries (`definition.py`: "You can set the remaining 240 colors as
color16 to color255"), and its syntax is space-separated, not `key = value`.

### 4.5 Windows Terminal

```json
{ "name": "Dracula", "black": "#21222c", "red": "#ff5555", …,
  "brightBlack": "#6272a4", …, "background": "#282a36", "foreground": "#f8f8f2",
  "cursorColor": "#f8f8f2", "selectionBackground": "#44475a" }
```

The settings schema (`doc/cascadia/profiles.schema.json`, `$defs.SchemeList`) confirms exactly 20
colour keys and no others. Colour pattern allows 3- or 6-digit hex. **No `selectionForeground`, no
cursor-text key** — a format limitation, not an exporter omission.

### 4.6 base16 / base24

```yaml
system: "base16"
name: "Dracula"
variant: "dark"
palette:
  base00: "#282a36"   # Default Background
  base02: "#44475A"   # Selection Background
  base05: "#f8f8f2"   # Default Foreground
  base08: "#ff5555"   # Red
  …
  base0F: "#993333"
```

base16's sixteen slots are **semantic, not ANSI**: `base00` bg, `base01–02` raised backgrounds,
`base03` comments, `base04–07` foregrounds, `base08–0F` red/orange/yellow/green/cyan/blue/magenta/
brown. base24 adds `base10`–`base17` as true brights. **No cursor and no selection field** —
`base02` is documented as the selection background.

The shell variant writes R/G/B as slash-separated hex pairs and aliases brights to base colours:

```sh
color00="28/2a/36" # Base 00 - Black
color01="ff/55/55" # Base 08 - Red
color09="$color01" # Base 08 - Bright Red
color_foreground="f8/f8/f2" # Base 05
```

### 4.7 Gogh

```yaml
name: 'Dracula'
variant: 'dark'
color_01: '#21222C'    # Black (Host)
…
color_16: '#FFFFFF'    # Bright White
background: '#282A36'
foreground: '#F8F8F2'
cursor: '#F8F8F2'
```

`color_01`…`color_16` is **1-based**, with 16 = bright white. No selection, no cursor text.

### 4.8 terminal.sexy

```json
{ "name": "3024 (dark)", "author": "Chris Kempson",
  "color": ["#090300", "#db2d20", … 16 entries …],
  "foreground": "#a5a2a2", "background": "#090300" }
```

Zero-based 16, no cursor, no selection. Its 159 schemes are base16-derived, so it adds no corpus
base16 does not already have.

---

## 5. Mapping: what each format can and cannot supply

`✓` present · `✗` absent (dtr must invent) · `~` present but needs conversion or a policy choice

| dtr slot | Ghostty | iTerm2 | WT | Alacritty TOML | Alacritty YAML | kitty | base16 | base24 | Gogh | terminal.sexy |
|---|---|---|---|---|---|---|---|---|---|---|
| 16 ANSI | ✓ `palette = N` | ✓ `Ansi N Color` | ✓ | ✓ | ✓ | ✓ `color0-15` | ~ semantic → ANSI | ~ + brights | ✓ `color_01-16` | ✓ `color[0..15]` |
| default fg / bg | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ~ `base05`/`base00` | ~ | ✓ | ✓ |
| cursor colour | ✓ | ✓ | ✓ `cursorColor` | ~ may be `CellForeground` | ~ | ✓ | ✗ | ✗ | ✓ | ✗ |
| cursor text | ✓ | ✓ | ✗ | ~ may be `CellBackground` | ~ | ✓ | ✗ | ✗ | ✗ | ✗ |
| selection bg | ✓ | ✓ | ✓ | ✓ | ~ | ✓ | ~ `base02` by spec | ~ | ✗ | ✗ |
| selection fg | ✓ | ✓ | ✗ | ✓ | ~ | ✓ | ✗ | ✗ | ✗ | ✗ |
| indices 16–255 | ~ allowed, never used by the corpus | ✗ | ✗ | ✗ | ✗ | ✓ `color16-255` | ✗ | ✗ | ✗ | ✗ |
| bold colour | ~ `bold-color` exists, corpus never sets it | ~ `Bold Color` (39/39 sampled) | ✗ | ~ `bright_foreground` | ~ | ✗ | ✗ | ✗ | ✗ | ✗ |
| dim colour | ✗ (`faint-opacity` is alpha) | ✗ | ✗ | ~ `[colors.dim]`, auto-derived | ~ | ✗ | ✗ | ✗ | ✗ | ✗ |

### 5.1 Where a conversion has to invent something

Only six slots are ever missing, each with a defensible default. These are the same fallbacks the
hub's generator applies (`tools/gen.py`, `fallback_color_map`) — as close to a consensus as this
corpus has:

| Missing slot | Invent as | Why |
|---|---|---|
| `cursor-text` | `background` | The cursor sits on a background-coloured cell; inverting is universal |
| `selection-background` | `foreground` | Inverse video: legible against the theme's own fg/bg pair, for light and dark alike, with no luminance test |
| `selection-foreground` | `background` | The other half of the same inversion |
| `cursor` (colour) | `foreground` | What Ghostty and Alacritty default to (`CellForeground`) |
| bold colour | *none* — weight only | §6.5 |
| dim colour | derived at paint time | §6.6 |

Bold **brights** never need inventing: every in-scope format that supplies ANSI supplies all 16.
(`Palette::generate` is *not* a bright-generator — it preserves 0–15 and only fills 16–255.)

---

## 6. Conversion rules, to implement verbatim

### 6.1 Ghostty theme parser

```
For each line, take it raw (do not pre-trim inside values):
  1. If the first non-space character is '#', skip.   ⚠ ONLY here — '#' is also the value prefix.
  2. If the line is blank, skip.
  3. Split at the FIRST '=' → key (trimmed), value (trimmed). No '=' → warn(file, line), skip.
  4. Match key exactly (case-sensitive):
       "palette"              → Palette::parse_palette_entry(value)
                                → (index, rgb): explicit[index] = rgb, mask.set(index)
       "background"           → theme.background
       "foreground"           → theme.foreground
       "cursor-color"         → theme.cursor
       "cursor-text"          → theme.cursor_text
       "selection-background" → theme.selection_background
       "selection-foreground" → theme.selection_foreground
       anything else          → ignore (debug log only)
  5. Malformed colour, or palette index outside 0..=255 → warning naming file+line, skip the
     line, keep going. One bad line does not poison the file.
Afterwards: if any of indices 0..15 is unset → reject the file: "theme X is missing ANSI colour N".
```

Non-obvious rules worth code comments:

- **Never strip trailing `#` comments.** `palette = 0=#1f1f28` is a value. A generic "strip from
  `#`" pass corrupts every file in the corpus. Ghostty only treats `#` at line start as a comment.
- **Last duplicate wins** (`background` twice, `palette = 4=` twice). None of the 819 shipped files
  does it; a hand-edited file will, and "last wins" is what a config reader means.
- **Ignore non-colour keys, never error.** Erroring on `background-opacity` rejects real themes;
  honouring it means a downloaded theme can set `command`. Whitelist.
- **Do not hand-roll index parsing.** `Palette::parse_palette_entry` already handles `0x`/`0o`/`0b`,
  decimal, and whitespace around `N` and `=`.

### 6.2 `.itermcolors` → `Rgb`

```
plist::Value::from_file(path) → Dictionary
for each (key, value):
    value must be a Dictionary; take Red/Green/Blue Component as f64
    if "Color Space" == "P3": (r,g,b) = p3_to_srgb(r,g,b)
    otherwise (sRGB, Calibrated, absent): no conversion
    out = Rgb { round(r*255), round(g*255), round(b*255) }   ← ROUND; clamp 0..=255
    if "Alpha Component" present and < 1.0 → warn once per file, ignore alpha (§6.3)
```

**Measured, not assumed.** 39 hub `.itermcolors` files compared against the same scheme's Ghostty
export, 132 mapped fields:

- `round(c*255)`: **116/116 exact matches on every sRGB file**; `trunc(c*255)`: 83/132.
- All 16 mismatches came from the single `P3` file in the sample, and all 16 are fixed by the hub's
  own matrix (`tools/gen.py::_p3_to_srgb`): 128/132 with it, and the remaining 4 are schemes whose
  colours changed between export and today (Alabaster, Spring, Symfonic whites).

```
M = [[ 1.22494018, -0.22469880, -0.00012973],
     [-0.04205631,  1.04203282, -0.00000601],
     [-0.01963755, -0.07862814,  1.09826365]]
srgb_to_linear(c) = c < 0.04045 ? c/12.92 : ((c+0.055)/1.055)^2.4
linear_to_srgb(c) = c < 0.0031308 ? 12.92*c : 1.055*c^(1/2.4) - 0.055
rgb_linear = M · linearise(rgb); clamp each to 0..1; re-encode
```

Colour-space and alpha census over those 39 files (1012 colour dicts): `sRGB` 795, `P3` 76,
`Calibrated` 26, **absent 115**; alpha `1.0` ×876, `0.5` ×9, `0.25` ×10, `0.491` ×1, `0.082` ×1,
absent ×115. Missing canonical keys: only `Cursor Guide Color` (6 files). One file had 81 top-level
keys — a "dynamic colours" export — so unknown keys must be ignored, not rejected.

Guesses flagged as such: treating `Calibrated` as sRGB is wrong in principle (iTerm2's "Calibrated"
is generic RGB) but it is 26 of 1012 entries in my sample and every converter in this ecosystem
does the same; and I measured hub-generated files only, not files exported by iTerm2 itself.

### 6.3 Alpha

Every slot in `Theme` is opaque. **Ignore alpha everywhere, warn once per file** — except
`selection-background`, where translucent values are common and a solid one looks wrong; composite
over `background`:

```
out = round(alpha * c + (1 - alpha) * background)
```

That is an approximation of what iTerm2 draws; label it as one. Do not add a general alpha channel
to the model for this.

### 6.4 The 256-colour cube and the ramp

Theme files carry 0–15. Indices 16–255 come from one of:

- **`Palette::default()`** — Ghostty's built-in 256: its base16 defaults, the xterm 6×6×6 cube, and
  the greyscale ramp. Legacy programs assume the xterm cube, and this is what Ghostty does when
  `palette-generate` is off (its default, Config.zig:799–820).
- **`Palette::generate(&p, &mask, bg, fg, false)`** — Ghostty's CIELAB cube plus a bg→fg ramp. Opt
  in per theme, and put any explicitly-set index ≥16 into `mask` so it survives.

```rust
use libghostty_vt::style::{Palette, PaletteIndex, PaletteMask, RgbColor};

fn palette_for(theme: &Theme) -> Palette {
    let mut p = Palette::default();                        // 16–255: Ghostty's defaults
    for (i, c) in theme.ansi.iter().enumerate() {
        p.set(PaletteIndex(i as u8), RgbColor { r: c[0], g: c[1], b: c[2] });
    }
    if theme.palette_generate {
        let mut mask = PaletteMask::new();
        for (i, c) in theme.explicit.iter().enumerate() {  // palette = 200=#… , if any
            if let Some(c) = c {
                p.set(PaletteIndex(i as u8), RgbColor { r: c[0], g: c[1], b: c[2] });
                mask.set(PaletteIndex(i as u8));
            }
        }
        p = Palette::generate(Some(&p), Some(&mask),
                              rgb(theme.background), rgb(theme.foreground), false);
    }
    p
}
```

`PaletteIndex(i as u8)` compiles because `PaletteIndex`'s field is `pub` and the underlying type is
`u8` (`bindings.rs`: `pub type ColorPaletteIndex = u8`). The named constants are for readability
only.

Recommendation: **`default()` by default, `generate` behind `palette_generate = true`.** Not because
`generate` is worse — it is what makes a 16-colour theme feel coherent — but because silently
breaking the xterm cube surfaces six weeks later as "htop looks wrong". Ghostty shipped the same
switch with the same default for the same reason.

The model must **not** store 256 colours: store 16 + the flag, derive at apply time, so changing
policy never means rewriting theme files.

### 6.5 Bold

The library does nothing here, and it does not give us the fg **palette index**: `Cell` exposes
`bg_color_palette()` and `bg_color_rgb()`, but for the foreground only `fg_color() -> RgbColor`.
Index-based brightening (`red` → `bright red`) needs the index, and it is not available.

So:

1. **Bold is weight only** (`bold = "none"`, the default). Matches Ghostty's default
   (`bold-color = null`), matches what M2's `swash` renderer does anyway.
2. **`bold = "bright"`**, done at frame-build time by exact-match lookup: resolved fg RGB compared
   against `theme.ansi[0..8]`; hit → `ansi[i+8]`, miss → leave alone. Correct for SGR 30–37, which
   is where nearly all bold-colour use is; wrong only if a cell's colour coincides with one of the
   eight base colours. Cheap and predictable. **The honest limitation to write down.**
3. **`bold = "#rrggbb"`** — one fixed colour (Ghostty's fixed `bold-color` form). Trivial novelty.

Do (1) and (2); mention the RGB-match caveat in the manual rather than pretending otherwise.

### 6.6 Dim

Nothing in dtr carries a dim flag today (`Cell` has `bold` and `italic`), and libghostty-vt has no
faint-colour API — only Ghostty's *app-level* `faint-opacity = 0.5`, which is renderer alpha.

- **Renderer alpha** (`faint_opacity = 0.5`): `fg.with_alpha(...)`. No colour to store, composites
  correctly on reverse-video cells, matches Ghostty.
- **Derived colour** (Alacritty's `[colors.dim]`): `dim(c) = mix(c, background, 0.5)`.

Recommend the first: one float, no 8-entry table, no re-derivation when the background changes. If
SGR 2 never reaches `Frame`, the slot stays unused — better than a field that lies.

### 6.7 Alacritty TOML (import only, if wanted)

```
[colors.primary]   foreground, background            → fg, bg
[colors.normal]    black red green yellow blue magenta cyan white → ansi 0..7 (by name)
[colors.bright]    same names                        → ansi 8..15
[colors.cursor]    cursor, text                      → cursor, cursor_text
                   "CellForeground" → fg, "CellBackground" → bg
[colors.selection] background, text                  → selection_bg, selection_fg (same sentinels)
[colors.dim], primary.dim_foreground                 → ignore (§6.6)
values: '#rrggbb', '0xrrggbb', 'rrggbb', named
```

`0xrrggbb` is not Ghostty syntax (`RgbColor::parse` takes `#`-prefixed or bare hex), so strip a
leading `0x` first.

### 6.8 Applying a theme

Once, on the pane thread, immediately after `Terminal::new(COLS, ROWS)` and before the first
`vt_write`:

```rust
term.set_default_fg_color(Some(rgb(theme.foreground)))?
    .set_default_bg_color(Some(rgb(theme.background)))?
    .set_default_cursor_color(Some(rgb(theme.cursor)))?
    .set_default_color_palette(Some(palette_for(theme)))?;
```

Two documented behaviours that matter (`terminal.rs:156–190`): `set_default_color_palette`
**preserves per-index OSC 4 overrides** an application has already applied (only untouched indices
change), and `None` resets to the built-in default. So re-applying on a config change is safe and
will not stomp an application's own recolouring — worth a test, because it is surprising in a good
way.

---

## 7. Naming, lookup and search paths

### 7.1 What Ghostty actually does (read from `src/config/theme.zig::open`)

The rule is short, and it is worth copying exactly:

1. **If the string is an absolute path**, open that path and nothing else. No `~` expansion —
   `openAbsolute` calls `openFileAbsolute` on the bytes as given.
2. **Otherwise it must be a bare basename.** If `basename(theme) != theme` — i.e. it contains a
   path separator — the diagnostic is *"theme \"…\" cannot include path separators unless it is an
   absolute path"*, and the theme fails.
3. **Then, for each location in order**, `join(location_dir, theme)` and try to open it. The first
   **regular file** wins; a directory, FIFO or device at that name is a diagnostic, not a fallback.

There is **no extension handling**: no `.conf` appended, no extension stripped. **No case folding**
— the docs say "This is case sensitive on systems with case-sensitive filesystems". **Spaces are
just bytes** in the filename; nothing quotes or splits them. `ghostty +list-themes` scans the same
directories, takes every regular file or symlink except `.DS_Store`, displays the filename verbatim
(extension included, if someone put one there), and sorts case-insensitively for display only.

### 7.2 What dtr should do — the same, plus a helpful error

- `theme = "kanagawa wave"` → **exact filename match**. No lowercase retry, no extension guessing.
- A name containing `/` or `\` is an error with Ghostty's wording. Absolute paths: **accept** them
  (it is the user's own config, and it is the one way to try a theme before copying it in), but say
  in the manual that a path is not a name.
- On failure, the error lists the directories searched **and** offers case-insensitive near
  matches: `theme "kanagawa Wave" not found; did you mean "Kanagawa Wave"?` — suggestion only,
  never resolution. Resolving case-insensitively would give one file two names and make "which
  theme am I on?" unanswerable.
- One name resolves to one file: **first directory wins**, and `dtr theme list` shows the winning
  path plus any shadowed duplicates. Silence about shadowing is how people end up editing the wrong
  file.
- `dtr theme list` prints `<name>\t<path>` from the same scan Ghostty does (all regular files and
  symlinks, no extension filter), so a theme dtr can see is a theme Ghostty can see.

### 7.3 Search order

| # | Directory | Why here |
|---|---|---|
| 1 | `dirs::config_dir()/dtr/themes` | dtr's own. Where `dtr theme cp/edit/import` write. First so a user's local copy always beats a package-managed one — the same principle as `PATH`, and the same reason Ghostty puts `user` before `resources` |
| 2 | `dirs::config_dir()/ghostty/themes` | The user's own Ghostty themes. Second because it is the migration path that justifies the format choice. On this machine it exists and is **empty** (`~/.config/ghostty/config` only) — the themes are in dir 3 |
| 3 | `$XDG_DATA_DIRS/ghostty/themes`, `/usr/local/share/ghostty/themes`, `/usr/share/ghostty/themes` | Ghostty's installed resource dir. **463 themes here** in `/nix/store/…-ghostty-1.3.1/share/ghostty/themes`; package-managed, so it lags (463 vs 819 upstream) and can vanish on upgrade |
| 4 | `/Applications/Ghostty.app/Contents/Resources/ghostty/themes` (macOS), `%LOCALAPPDATA%\ghostty\themes` (Windows, unverified — check `%APPDATA%` too) | Platform equivalents |
| 5 | compiled-in `builtin:default` | So dtr always has a theme, even with no Ghostty and no config |

First match wins, and the *name* is the filename — so `theme = "Kanagawa Wave"` keeps working when
a package upgrade adds a newer `Kanagawa Wave` in a later directory: the file the user pinned in
dir 1 or 2 shadows it.

### 7.4 Is reading the user's Ghostty themes a trap?

Mostly no, with three caveats worth knowing before writing the parser:

- **Includes: not a thing.** Ghostty's config-file mechanism is `config-file` (repeatable path), and
  a theme file *cannot* set `config-file` or `theme` — Ghostty silently ignores both (Config.zig:
  575–590). So a theme file cannot pull in another file, and there is no include graph to resolve.
- **Format drift: real but harmless.** Hand-written themes may use keys dtr has never heard of, and
  future Ghostty will add more. Because dtr's parser is a whitelist that ignores unknown keys, drift
  costs nothing — a new key simply does not affect dtr. Do not "validate strictly": a theme that
  Ghostty accepts must load in dtr.
- **Unsafe options: the actual trap.** A theme file is a config file and can set `command`,
  `font-family`, `keybind`, `background-opacity`, `window-*`. Ghostty's own docs warn "please do not
  use a theme file from an untrusted source. The built-in themes are audited to only set safe
  configuration options." dtr's whitelist is therefore a security boundary, not tidiness — say so
  when documenting import.

**Precedence**, which the owner asked about: in Ghostty, colour keys in the main config override the
theme ("Any additional colors specified via background, foreground, palette, etc. will override the
colors specified in the theme"), and light/dark can be selected with `light:X,dark:Y`. dtr has
neither: `theme` is the only place colours come from, so there is no override order to document and
no way for a stale loose `background =` line to half-apply a theme. That is a feature of the settled
shape, and worth stating in the manual so nobody hunts for the missing knob.

### 7.5 The Windows caveat

Ghostty's `theme.zig` documents only the XDG path (`$XDG_CONFIG_HOME/ghostty/themes` or
`~/.config/ghostty/themes`). `dirs::config_dir()` on Windows gives `%APPDATA%` (Roaming), while
Ghostty on Windows most likely uses `%LOCALAPPDATA%\ghostty\themes`. **Unverified.** dtr should try
both, treat dirs 2–4 as best-effort everywhere, and never fail to start because a search directory
is missing.

---

## 8. The file format dtr reads and writes

### 8.1 Keys

A dtr theme file is a **Ghostty theme file**, and dtr writes a complete one — all 22 values — so
that a file dtr writes is also a file Ghostty can use unchanged.

| Key | Required in a file dtr *reads*? | dtr writes | Honoured |
|---|---|---|---|
| `palette = 0=…` … `palette = 15=…` | yes, all 16 (missing → reject, naming the index) | ✓ | ✓ into `Palette[0..16]` |
| `palette = N=…` for N ≥ 16 | no | never | ✓ only if `palette_generate`; otherwise stored and applied as an explicit override |
| `background` | yes | ✓ | ✓ `set_default_bg_color` |
| `foreground` | yes | ✓ | ✓ `set_default_fg_color` |
| `cursor-color` | no → `foreground` | ✓ | ✓ `set_default_cursor_color` |
| `cursor-text` | no → `background` | ✓ | renderer (cursor glyph colour) |
| `selection-background` | no → `foreground` | ✓ | renderer (M2 selection) |
| `selection-foreground` | no → `background` | ✓ | renderer (M2 selection) |
| `bold-color` | no | never | **not honoured** — weight-only decision (§6.5); read, then warn once |
| `faint-opacity` | no | never | **not honoured** in the first cut; becomes renderer alpha in M2 (§6.6) |
| `background-opacity`, `background-blur`, `background-image` | no | never | **cannot honour**: no compositor alpha in scope |
| `font-family`, `font-size`, `font-*`, `adjust-*` | no | never | **must not honour**: fonts are M2's, and a theme must not repoint them |
| `cursor-style`, `cursor-blink` | no | never | honourable later (`set_default_cursor_style` exists), not in the first cut |
| `minimum-contrast` | no | never | **cannot honour**: no contrast enforcement |
| `command`, `initial-command`, `keybind`, `config-file`, `theme`, `window-*`, `macos-*`, `gtk-*` | no | never | **must not honour** (security, §7.4) |
| `# …` comment lines | — | ✓ provenance header | ignored; dtr may write them |

That is the whole contract: 7 colour keys plus `palette`, everything else ignored on read and never
written.

### 8.2 A worked example: `~/.config/dtr/themes/kanagawa wave`

Exact bytes as dtr would store it:

```
# kanagawa wave
# Imported from Kanagawa Wave.itermcolors (iTerm2-Color-Schemes) on 2026-10-27.
# Ghostty theme format; dtr honours only the colour keys below.
palette = 0=#090618
palette = 1=#c34043
palette = 2=#76946a
palette = 3=#c0a36e
palette = 4=#7e9cd8
palette = 5=#957fb8
palette = 6=#6a9589
palette = 7=#c8c093
palette = 8=#727169
palette = 9=#e82424
palette = 10=#98bb6c
palette = 11=#e6c384
palette = 12=#7fb4ca
palette = 13=#938aa9
palette = 14=#7aa89f
palette = 15=#dcd7ba
background = #1f1f28
foreground = #dcd7ba
cursor-color = #dcd7ba
cursor-text = #1f1f28
selection-background = #dcd7ba
selection-foreground = #1f1f28
```

and the config line that selects it:

```toml
theme = "kanagawa wave"
```

These are not invented values: they are the shipped corpus file verbatim, and I confirmed that the
`.itermcolors` for the same scheme converts to the same bytes under §6.2's rule — both paths agree
exactly, which is the round-trip test I would write first. The scheme is **MIT**
(`rebelot/kanagawa.nvim`, per the hub's `licenses.yml`), so this example is safe to ship if a
built-in ever happens.

It did happen: `crates/dtr/assets/themes/Kanagawa Wave` is this file, `include_str!`-ed into the
binary, under a three-line header naming the scheme, its SPDX id and the Ghostty release the bytes
came from. Strip the `#` lines and the rest is byte-identical to
`ghostty-1.3.1/share/ghostty/themes/Kanagawa Wave`. A file of the same name in `<config>/dtr/themes`
still wins — the override story §1 promises — which is what keeps this one file inside §9.1's rule
instead of becoming the vendored corpus that section forbids.

Note the selection pair is the *inverted* fg/bg. That is what this scheme actually specifies (its
`.itermcolors` has `Selection Color` = `#dcd7ba`), not a fallback artifact — worth knowing before
someone "fixes" it. Kanagawa's own terminal palette uses `#2d4f67` for selection; if the owner
prefers that, it is a one-line edit to the file, which is exactly the override story §1 promises.

---

## 9. Which sources to read, and the licence finding

| Option | Verdict |
|---|---|
| **Ghostty files, native** | **Yes.** One ~80-line parser, the richest field set, 819 upstream, 463 installed here, and it is the format the owner named. |
| **`.itermcolors`, import only** | **Yes.** It is the corpus's *upstream*, it carries `Bold Color`, and users have their own exports. `dtr theme import x.itermcolors` converts and writes a Ghostty file (§6.2); the app then only ever loads one dialect. One new dependency (`plist`). |
| **Alacritty TOML, import only** | **Optional, cheap.** Zero new dependencies (`toml_edit` is already in the workspace) and it is the migration path for someone with `~/.config/alacritty/alacritty.toml`. ~60 lines. Defer freely. |
| **Ship a curated corpus** | **No** — see §9.1. Ship exactly one hand-written `builtin:default`. |
| **Read `~/.config/ghostty/themes` (dir 2)** | **Yes**, as a search path, not as an import: no copying, no staleness, and no licence question about the user's own files. |
| **WT JSON / kitty conf / base16 / base24 / Gogh / terminal.sexy / Alacritty YAML** | **No.** Each has a Ghostty export beside its corpus, so a parser buys no themes; the kitty export is actively wrong (§4.4), which is the general hazard of derived exports. Two formats is a budget; eight is a hobby. |

### 9.1 The licence finding (the reason not to vendor)

- **Ghostty is MIT** (`LICENSE`: "Copyright (c) 2024 Mitchell Hashimoto, Ghostty contributors"), and
  its build fetches the 819-file tarball, so Ghostty ships them without a separate licence file.
- **But the colours are not Ghostty's to license.** The corpus is the iTerm2 collection, whose
  `LICENSE` says explicitly: *"This license covers the iTerm-Color-Schemes repository collection of
  themes. The copyright/license for each individual theme belongs to the author of that theme."*
- The hub now records that per scheme in `licenses.yml` (860 entries), and the numbers are not
  encouraging: **583 `unknown`, 18 `nonfree`**, and only ~**259** clearly redistributable (205 MIT,
  16 GPL-2.0-or-later, 9 Apache-2.0, 5 CC0-1.0, 5 ISC, 4 WTFPL, 3 GPL-3.0-only, 2 LGPL-2.1-or-later,
  2 GPL-2.0-only, 2 CC-BY-SA-4.0). The hub's `gen.py --redistributable` exists to filter to that
  subset.
- Copying Ghostty's MIT tarball into dtr would copy ~560 files of undocumented provenance into a
  repo licensed `MIT OR Unlicense`, and re-copy them whenever upstream's snapshot moves (463 → 819
  between `ghostty-1.3.1` and the 2026-10-19 release; 380 KB uncompressed).

**Do not vendor a corpus.** If a built-in set is ever wanted, vendor only from the documented-
permissive 259, record source URL and SPDX id per file, and keep the count in single digits so the
review stays possible. dtr ships exactly one — `Kanagawa Wave` (§8.2) — which is that rule applied,
not an exception to it: the count is a budget, and the next theme has to earn its own licence check
the same way. There is no `builtin:default`: with no theme configured, the VT core's own colours are
the default, and they are the one palette that cannot be wrong.

---

## 10. Parsing plan

| Format | Crate | Notes |
|---|---|---|
| Ghostty theme | **none** — hand parser + `RgbColor::parse` / `Palette::parse_palette_entry` | ~80 lines + tests. Colour syntax and index prefixes come free from libghostty-vt, so the hand-written part is line splitting. The `#`-is-not-a-comment rule is the whole risk (§6.1). |
| `.itermcolors` | **`plist` 1.10.3, MIT**, updated 2026-10-21 | XML and binary plists, `<real>`/`<integer>`. `round(c*255)`; P3 matrix (§6.2). The only genuinely new dependency in the plan. |
| Alacritty TOML | **`toml_edit`** (already a workspace dep) | `DocumentMut` gives `[colors.primary]` etc. directly; no `serde`. Strip `0x`; handle `CellForeground`/`CellBackground`. |
| kitty, base16/24, Gogh, terminal.sexy, WT | — | Not read. If one is ever added: WT and terminal.sexy need `serde_json`; base16/Gogh need YAML, and **there is no maintained YAML crate to reach for** — `serde_yaml`'s last release is `0.9.34+deprecated` (2024-03-25); the maintained successor is `saphyr`. A 20-line flat `baseXX: "#hex"` reader is the better answer for base16. |
| Alacritty YAML | — | Same YAML problem, plus the dialect is dead upstream (0.13 removed it). |

Nothing here is a `-sys` crate or a C build, so the single-`.exe` rule and the Windows cross-build
(PLAN §5.3, §7) are untouched.

### 10.1 Integration with `dtr-config`

`theme` is a **top-level scalar**, not a section, which collides with the current parser's shape in
two places worth knowing before writing code:

- `Config::parse` reads sections by name (`document.get("serial")`). A top-level string is
  `document.get("theme").and_then(Item::as_str)` — simpler, but it is a third access pattern, and
  `Config` needs a `theme: String` field with a default (empty = no theme, i.e. `builtin:default`).
- `set_in_text` splits on the **first** `.` and requires a section, so `dtr config set theme …`
  currently fails with *"… is not section.key"*. Setting a top-level scalar needs a small branch:
  if the key has no `.`, insert `document[key] = value`. Do not discover this after documenting the
  CLI.

`set_in_text` already replaces values in place while keeping comments, so `theme = "x"` is one
line and one code path.

---

## 11. What I could not verify

- **Whether Ghostty's theme tarball is produced by the hub's `tools/gen.py`.** The dependency is
  named `iterm2_themes`, the output shape matches the hub's `ghostty/` template byte for byte, and
  every name exists in the hub — but the generator is not in Ghostty's repo, so "same generator" is
  an inference.
- **Windows theme paths** (§7.5). Only XDG is documented; `%LOCALAPPDATA%` is a guess.
- **Real-world deviation from the generated corpus.** All 819 Ghostty files I measured are uniform
  because they are generated. Hand-written themes may use `palette-generate`, X11 names, or
  indices ≥16 — §6.1/§8.1 are written for that, but I have not measured such a file.
- **iTerm2's own exports.** I measured hub-generated `.itermcolors` only; iTerm2 may emit integer
  components, other colour spaces, or alpha more often.
- **`Palette::generate` stability across libghostty-vt versions.** It is the same C the Ghostty app
  runs, but the ABI manifest says the types are "not a cross-version stability promise" — another
  argument for keeping the flag off by default and the derived cube out of the file format.
- **Alacritty's exact dim formula.** Its docs say the eight `[colors.dim]` defaults are "calculated
  automatically" and the source ships precomputed constants; I did not derive it, hence §6.6
  prefers alpha.

---

## 12. Cost and risk

| Item | Estimate | Risk |
|---|---|---|
| Ghostty theme parser + tests | ~1 day | Low. Corpus is uniform; the only trap is `#` comments. |
| `Theme` model, `theme = "name"`, search path, `dtr theme list` | ~1 day | Low–medium: `set_in_text` needs a top-level branch (§10.1). |
| Apply to `Terminal` + a golden test that OSC 4/10/11 answer with themed colours | ~half a day | Low, and this test is what prevents the renderer/emulator divergence bug. |
| `.itermcolors` import (`plist`) | ~1 day | Medium: P3, alpha, missing keys — all specified in §6.2 and measured. |
| Alacritty TOML import | ~half a day | Low, optional. |
| Renderer changes (cursor + selection colours, bold/dim policy) | M2 work | Out of scope here; `Theme` carries the fields so M2 does not have to invent them. |

**What would change my mind:** if M2 turns out to need more than 16 ANSI slots plus selection — for
example per-cell underline colours, which none of these formats has — or if libghostty-vt ever
exposes a bold-colour setter, §6.5 collapses to "set it and be done" and the RGB-matching caveat
disappears. Neither is true at the pinned rev.

