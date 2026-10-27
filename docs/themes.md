# Terminal theme formats — decision memo

Status: research, 2026-10-27 · Scope: *which* theme formats dtr reads and *what shape* a theme
takes, not the renderer work. Written against `PLAN.md` M1/M2 and the code as it stands today
(`crates/dtr/src/ui.rs`, `pane.rs`, `crates/dtr-config`).

Question: dtr draws a `Frame` of cells whose colours the VT core has already resolved to RGB. A
theme therefore cannot be a renderer-only paint job — it has to reach the terminal as a palette,
and it has to survive the fact that a serial console has no colour agreement with its far end.

---

## 1. Recommendation

**Read Ghostty theme files as the on-disk dialect, plus `.itermcolors` as an import path. Keep
the theme in `config.toml` as a *reference plus a small override table*, not as a block of 22
hex values.**

Concretely: `[theme] name = "Dracula"` resolves against `~/.config/dtr/themes`, then
`~/.config/ghostty/themes`, then the Ghostty resources directory, then a single built-in
`builtin:default`; `[theme.overrides]` can pin any slot; `.itermcolors` is read only by
`dtr theme import <file>`, which writes a Ghostty-format file into `~/.config/dtr/themes`.

The trade-off, named: dtr's config no longer contains the whole truth about its colours — it can
point at a file that is missing, or that a package manager upgrades underneath it. The
alternative (inline only) trades that away for a config file that nobody will edit by hand and
that cannot share a theme with the user's Ghostty install. The override table is what makes the
first option affordable: changing one colour is still a one-line config edit.

**Runner-up: inline only** — a `[theme]` table holding all 16 ANSI slots plus six scalars. Zero
file lookup, zero "theme not found" error class, every colour editable through the existing
`dtr config set`. Switch to this if the Ghostty-directory search turns out to be fragile on
Windows, or if the corpus is judged not worth a licence review (§7.2).

**Reject: one parser per format.** Six of the eight formats below are *outputs* of the same
corpus generator that also emits Ghostty themes (measured in §3). Every extra parser is
permanent code and a permanent import-fidelity bug surface, and buys a corpus dtr can already
read.

---

## 2. What dtr actually needs from a theme

### 2.1 Where the colours come from today

`pane.rs::publish` builds a `Frame` from `snapshot.colors()` and per-cell accessors:

```rust
default_fg: rgb(colors.foreground),
default_bg: rgb(colors.background),
// per cell:
if let Some(fg) = cell.fg_color()? { out.fg = rgb(fg); }
if let Some(bg) = cell.bg_color()? { out.bg = rgb(bg); out.has_bg = true; }
```

With no defaults set, `colors.foreground`/`background` are the VT core's own defaults (white on
black per the pane's unit test) and every cell colour is resolved inside libghostty-vt. So:

- **The theme must be applied to the `Terminal`, not to `Frame`.** Setting it in the UI would
  leave the emulator answering OSC 10/11 and OSC 4 lookups with different colours than the
  screen shows — the exact class of bug that makes `vim` and `fzf` look broken.
- **`Frame` needs no new fields for this.** Once the terminal has defaults, the existing
  `default_fg`/`default_bg`/`cell.fg`/`cell.bg` carry the themed values through unchanged.

### 2.2 The API that makes it possible (verified from the pinned crate source)

`Cargo.toml` pins `libghostty-rs` at `9ede35b8ea94a55ed3608408d67e102883129f1e`; I read that
checkout directly. `libghostty_vt::Terminal` has (docs + `terminal.rs:1108–1196`):

```rust
set_default_fg_color(Option<RgbColor>)        -> Result<&mut Self>
set_default_bg_color(Option<RgbColor>)        -> Result<&mut Self>
set_default_cursor_color(Option<RgbColor>)    -> Result<&mut Self>
set_default_cursor_style(Option<CursorStyle>) -> Result<&mut Self>
set_default_color_palette(Option<Palette>)    -> Result<&mut Self>   // None = built-in default
```

and `style.rs` gives us, all `Copy`:

```rust
pub struct RgbColor { pub r: u8, pub g: u8, pub b: u8 }
pub struct Palette(pub [RgbColor; 256]);
pub struct PaletteIndex(pub u8);           // 16 named consts: BLACK … BRIGHT_WHITE
pub fn Palette::set(&mut self, PaletteIndex, RgbColor)
pub fn Palette::parse_palette_entry(&str) -> Result<(PaletteIndex, RgbColor)>  // "N=COLOR"
pub fn Palette::generate(base, skip, bg, fg, harmonious) -> Palette            // the cube
pub fn RgbColor::parse(&str) -> Result<RgbColor>                              // Ghostty colour syntax
pub fn RgbColor::perceived_luminance(self) -> f64
pub fn RgbColor::contrast(self, other) -> f64
```

Three consequences worth stating plainly, because they set the whole design:

1. **We do not have to write a colour parser.** `RgbColor::parse` accepts `#rrggbb`, bare
   `rrggbb`, 3/9/12-digit hex, X11 names, and `rgb:`/`rgbi:` — the same syntax Ghostty's own
   config uses. `Palette::parse_palette_entry` parses `0=#21222c` with index prefixes
   (decimal/`0x`/`0o`/`0b`). Both come free with the VT core we already link.
2. **We do not have to invent the 256-colour cube.** `Palette::generate` is Ghostty's own
   generator — CIELAB trilinear cube at 16–231, a background→foreground ramp at 232–255,
   indices 0–15 preserved. `Palette::default()` is Ghostty's built-in 256-colour default.
3. **There is no bold colour and no dim colour in the API.** Only fg, bg, cursor, palette. And
   the cell accessor documents the gap: *"Resolves palette indices through the palette. Bold
   color handling is not applied; the caller should handle bold styling separately."* See §6.5.

There is **no selection colour in the VT core at all** — selection is a renderer concept in
Ghostty, as it will be in M2. So selection fg/bg live in `Theme` and are consumed by the
renderer, never by `Terminal`.

> Naming: `crates/dtr/src/palette.rs` is the *command* palette. Anything holding colours must be
> `Theme` / `Colours`, never `Palette`; `libghostty_vt::style::Palette` is already the 256-entry
> type and having two of them in one crate would be a bad afternoon.

---

## 3. Inventory: the formats, their corpora, their licences

Counts and licences below were **fetched today** (GitHub API listings, raw files, crates.io).
"Hub" = [`mbadolato/iTerm2-Color-Schemes`](https://github.com/mbadolato/iTerm2-Color-Schemes),
which exports every other format from one source tree.

| # | Format | File shape | Canonical corpus | Count | Licence |
|---|---|---|---|---|---|
| 1 | **Ghostty** theme | `palette = N=#RRGGBB` ×16, then `background`, `foreground`, `cursor-color`, `cursor-text`, `selection-background`, `selection-foreground` | Ghostty ships a **build-time tarball** (not in the repo). Hub also exports `ghostty/` | **819** shipped (`ghostty-themes-release-20261019`, dependency name `iterm2_themes`); **860** in hub `ghostty/`; **463** in the installed `ghostty-1.3.1` here | Ghostty MIT (© Mitchell Hashimoto, Ghostty contributors). Theme *values* come from the iTerm2 corpus — see §7.2 |
| 2 | **iTerm2** `.itermcolors` | XML plist; `Ansi 0 Color`…`Ansi 15 Color`, `Background Color`, `Foreground Color`, `Cursor Color`, `Bold Color`, `Cursor Guide Color`, `Cursor Text Color`, `Selection Color`, `Selected Text Color`; each a dict of `Red/Green/Blue/Alpha Component` `<real>` + `Color Space` `<string>` | hub `schemes/` | **860** | Repo MIT (© Mark Badolato) **+ per-scheme `licenses.yml`**: 205 MIT, 583 `unknown`, 18 `nonfree`, 16 GPL-2.0-or-later, 9 Apache-2.0, 5 CC0-1.0, 5 ISC, 4 WTFPL, 3 GPL-3.0-only, 2 LGPL-2.1-or-later, 2 GPL-2.0-only, 2 CC-BY-SA-4.0 ⇒ **~259 redistributable** |
| 3 | **Windows Terminal** `schemes` | JSON object, or an element of `"schemes": [...]` in `settings.json`: `name`, `black`…`white`, `brightBlack`…`brightWhite`, `purple` for magenta, `background`, `foreground`, `cursorColor`, `selectionBackground` | WT's own `defaults.json`; hub `windowsterminal/` | **16** built-in; **860** in hub | Terminal MIT; hub MIT-collection |
| 4 | **Alacritty** TOML (≥0.13) | `[colors.primary] foreground/background/dim_foreground/bright_foreground`, `[colors.normal]`, `[colors.bright]`, `[colors.dim]`, `[colors.cursor] {text,cursor}`, `[colors.selection] {text,background}` | hub `alacritty/` | **860** | Alacritty Apache-2.0; hub MIT-collection |
| 5 | **Alacritty** YAML (≤0.12) | `colors:` → `primary`/`normal`/`bright`, values `'0x282a36'` | `eendroroy/alacritty-theme`; hub `yaml/` (400, a *different* generic schema) | **65** (59 `.yaml` + 6 `.yml`) | Apache-2.0 (that repo); hub MIT-collection |
| 6 | **kitty** `.conf` | one option per line, space-separated: `color0`…`color255`, `background`, `foreground`, `cursor`, `cursor_text_color`, `selection_background`, `selection_foreground`, `url_color`, `underline_color` | hub `kitty/`; `dexpota/kitty-themes` | **860** hub; **169** dexpota | kitty GPL-3.0; hub MIT-collection; dexpota MIT |
| 7 | **base16** YAML | `system`, `name`, `author`, `description`, `variant`, `palette: base00`…`base0F` | `tinted-theming/schemes` `base16/` | **360** | MIT (© Tinted Theming) |
| 8 | **base24** YAML | as base16 plus `base10`…`base17` | same repo `base24/` | **240** | MIT |
| 9 | **base16-shell** / tinted-shell | `export BASE16_THEME=x`; `color00="28/2a/36"` … `color21`, `color_foreground`, `color_background`; emits `OSC 4` itself | `tinted-theming/base16-shell` `scripts/` | **615** | MIT (© Chris Kempson, Tinted Theming) |
| 10 | **Gogh** | YAML: `name`, `author`, `variant`, `color_01`…`color_16` (16 = bright white), `background`, `foreground`, `cursor` | `Gogh-Co/Gogh` `themes/` | **1000** | MIT **or** Apache-2.0 (dual) |
| 11 | **terminal.sexy** | JSON: `{name, author, color: [16 hex], foreground, background}` | `stayradiated/terminal.sexy` `dist/schemes/` | **159** | MIT |

### 3.1 The one fact that reorganises the whole problem

Ghostty does not maintain a theme corpus. Its `build.zig.zon` has:

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
`schemes/`, and the hub's own `tools/templates/ghostty` emits byte-for-byte the same shape.

So the formats are not eleven peers. They are **one corpus (860 iTerm2 schemes) with eleven
exporters**, and Ghostty-format is the exporter that keeps the most fields while staying trivial
to parse.

I could not find the script that produces Ghostty's tarball (it is not in the repo — `build.zig`
never mentions it beyond the dependency). The hub's `tools/gen.py` produces the same output
shape; whether it is literally the same program is unverified (§10).

---

## 4. The files, as they really are

Quoted from files fetched today. Nothing here is from memory.

### 4.1 Ghostty — `ghostty/Dracula` (hub `ghostty/`, identical to the installed 1.3.1 copy)

```
palette = 0=#21222c
…
palette = 15=#ffffff
background = #282a36
foreground = #f8f8f2
cursor-color = #f8f8f2
cursor-text = #282a36
selection-background = #44475a
selection-foreground = #ffffff
```

I parsed all **819** shipped files: every one has exactly 16 `palette` lines, indices 0–15, and
exactly those seven scalar keys — no comments, no duplicates, no non-hex values, no extra
options. That uniformity is a property of the *generated* corpus, **not** of the format: a
hand-written Ghostty theme is "simply another Ghostty configuration file" and may contain
`font-family`, `background-opacity`, `command = …`, `theme = …` (Config.zig:575–588). A dtr
parser must whitelist keys, not trust the file (§6.1).

Names are hostile to naive lookup: **560 of 819 contain spaces** ("12-bit Rainbow", "Springfield
Resident Hight Contrast"), none are non-ASCII, longest is 35 chars. A name is a *file name*,
matched exactly (Ghostty is case-sensitive on case-sensitive filesystems).

### 4.2 iTerm2 — `schemes/Dracula.itermcolors`

```xml
<key>Ansi 0 Color</key>
<dict>
    <key>Alpha Component</key><real>1</real>
    <key>Blue Component</key><real>0.1725</real>
    <key>Color Space</key><string>sRGB</string>
    <key>Green Component</key><real>0.1333</real>
    <key>Red Component</key><real>0.1294</real>
</dict>
```

Top-level keys present in this file: `Ansi 0 Color` … `Ansi 15 Color`, `Background Color`,
`Bold Color`, `Cursor Color`, `Cursor Guide Color`, `Cursor Text Color`, `Foreground Color`,
`Selected Text Color`, `Selection Color`. Note the naming trap: **`Selection Color` is the
selection *background*; `Selected Text Color` is the selection foreground.**

Components are floats 0–1 truncated to four decimals (0.1294 is 33/255 = 0.129412…), so the
8-bit value only comes back with **rounding**, not truncation — measured in §6.2.

### 4.3 Alacritty TOML — `alacritty/Dracula.toml`

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

The 0.12 YAML dialect is the same tree with `colors:` → `primary`/`normal`/`bright` and values
quoted as `'0x282a36'`:

```yaml
colors:
  primary:
    background: '0x282a36'
    foreground: '0xf8f8f2'
```

Alacritty's man page adds fields no exported theme uses: `[colors.primary] dim_foreground`,
`bright_foreground`; `[colors.dim]` with all eight names (auto-derived when absent);
`[colors.search]`, `[colors.hints]`, `[colors.line_indicator]`, `[colors.footer_bar]`. Cursor and
selection values may also be the **sentinel strings** `CellForeground` / `CellBackground`.

### 4.4 kitty — `kitty/Dracula.conf`

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

**This file is wrong**, and it is the best argument for importing from the upstream format
rather than from a derived export: compare with §4.1/§4.3 — selection background should be
`#44475a` and selection foreground `#ffffff`. The hub's `tools/templates/kitty.conf` hard-codes
`selection_foreground = Background_Color` and `selection_background = Foreground_Color`, so every
one of its 860 kitty exports has inverted selection colours. Derived exports are not
interchangeable sources of truth.

kitty itself does support all 256 entries (`definition.py`: "You can set the remaining 240 colors
as color16 to color255"), and its syntax is space-separated, not `key = value`.

### 4.5 Windows Terminal — `windowsterminal/Dracula.json`

```json
{ "name": "Dracula", "black": "#21222c", "red": "#ff5555", …,
  "brightBlack": "#6272a4", …, "background": "#282a36", "foreground": "#f8f8f2",
  "cursorColor": "#f8f8f2", "selectionBackground": "#44475a" }
```

The settings schema (`doc/cascadia/profiles.schema.json`, `$defs.SchemeList`) confirms exactly 20
colour keys and no others: `background`, `foreground`, `cursorColor`, `selectionBackground`, and
`black`/`red`/`green`/`yellow`/`blue`/`purple`/`cyan`/`white` plus their `bright*` forms. Colour
pattern allows 3- or 6-digit hex. **There is no `selectionForeground` and no cursor-text key** —
that is a format limitation, not an exporter omission.

### 4.6 base16 / base24 — `tinted-theming/schemes/base16/dracula.yaml`

```yaml
system: "base16"
name: "Dracula"
author: "clach04 …"
variant: "dark"
palette:
  base00: "#282a36"   # Default Background
  base01: "#21222c"
  base02: "#44475A"   # Selection Background
  base05: "#f8f8f2"   # Default Foreground
  base08: "#ff5555"   # Red
  …
  base0F: "#993333"
```

base16's sixteen slots are **semantic**, not ANSI: `base00` bg, `base01–02` raised backgrounds,
`base03` comments, `base04–07` foregrounds, `base08–0F` red/orange/yellow/green/cyan/blue/magenta/
brown. base24 adds `base10`–`base17` as true brights. There is **no cursor and no selection
field** in the scheme file; `base02` is documented as the selection background.

The shell variant writes R/G/B as slash-separated hex pairs and aliases brights to the base
colours:

```sh
color00="28/2a/36" # Base 00 - Black
color01="ff/55/55" # Base 08 - Red
…
color09="$color01" # Base 08 - Bright Red
color_foreground="f8/f8/f2" # Base 05
color_background="28/2a/36" # Base 00
```

### 4.7 Gogh — `themes/Dracula.yml`

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

`color_01`…`color_16` is a **1-based** numbering where 16 is bright white. No selection, no
cursor text.

### 4.8 terminal.sexy — `dist/schemes/base16/3024.dark.json`

```json
{ "name": "3024 (dark)", "author": "Chris Kempson",
  "color": ["#090300", "#db2d20", … 16 entries …],
  "foreground": "#a5a2a2", "background": "#090300" }
```

Zero-based array of 16, no cursor, no selection. Its 159 shipped schemes are base16-derived, so it
adds no corpus that base16 does not already have.

---

## 5. Mapping: what each format can and cannot supply

`✓` present · `✗` absent (dtr must invent) · `~` present but needs conversion or a policy choice.

| dtr slot | Ghostty | iTerm2 | WT | Alacritty TOML | Alacritty YAML | kitty | base16 | base24 | Gogh | terminal.sexy |
|---|---|---|---|---|---|---|---|---|---|---|
| 16 ANSI | ✓ `palette = N` | ✓ `Ansi N Color` | ✓ | ✓ `normal`+`bright` | ✓ | ✓ `color0-15` | ~ semantic → ANSI | ~ + `base10-17` as brights | ✓ `color_01-16` | ✓ `color[0..15]` |
| default fg / bg | ✓ | ✓ | ✓ | ✓ `primary` | ✓ | ✓ | ~ `base05`/`base00` | ~ | ✓ | ✓ |
| cursor colour | ✓ `cursor-color` | ✓ `Cursor Color` | ✓ `cursorColor` | ~ `[colors.cursor].cursor` (may be `CellForeground`) | ~ | ✓ `cursor` | ✗ | ✗ | ✓ `cursor` | ✗ |
| cursor text | ✓ `cursor-text` | ✓ `Cursor Text Color` | ✗ | ~ `.text` (may be `CellBackground`) | ~ | ✓ `cursor_text_color` | ✗ | ✗ | ✗ | ✗ |
| selection bg | ✓ `selection-background` | ✓ `Selection Color` | ✓ `selectionBackground` | ✓ `[colors.selection].background` | ~ | ✓ `selection_background` | ~ `base02` (by spec) | ~ | ✗ | ✗ |
| selection fg | ✓ `selection-foreground` | ✓ `Selected Text Color` | ✗ | ✓ `.text` | ~ | ✓ `selection_foreground` | ✗ | ✗ | ✗ | ✗ |
| indices 16–255 | ~ `palette` allows 0–255; corpus never sets ≥16 | ✗ | ✗ | ✗ | ✗ | ✓ `color16-255` | ✗ | ✗ | ✗ | ✗ |
| bold colour | ~ `bold-color` key exists; corpus never sets it | ~ `Bold Color` (present in 39/39 sampled) | ✗ | ~ `primary.bright_foreground` + profile flag | ~ | ✗ | ✗ | ✗ | ✗ | ✗ |
| dim colour | ✗ (`faint-opacity` is alpha, not colour) | ✗ | ✗ | ~ `[colors.dim]`, auto-derived when absent | ~ | ✗ | ✗ | ✗ | ✗ | ✗ |

### 5.1 Where a conversion has to invent something

Only six slots are ever missing, and each has a defensible default. These are the same fallbacks
the hub's generator applies (`tools/gen.py`, `fallback_color_map`), which is as close to a
consensus as this corpus has:

| Missing slot | Invent as | Why |
|---|---|---|
| `cursor-text` | `background` | The cursor sits on a background-coloured cell; inverting is the universal convention |
| `selection-background` | `foreground` | Inverse video: always legible against the theme's own fg/bg pair, and works for light and dark themes without a luminance test |
| `selection-foreground` | `background` | The other half of the same inversion |
| `cursor` (colour) | `foreground` | What Ghostty and Alacritty default to (`CellForeground`) |
| bold colour | *none* — bold is weight only | See §6.5 |
| dim colour | derived by mixing with `background` | See §6.6 |

Bold **brights** never need inventing from these formats: all eight in-scope formats that supply
ANSI at all supply all 16. (`Palette::generate` is not a bright-generator — it preserves 0–15 and
only fills 16–255.)

---

## 6. Conversion rules, to implement verbatim

### 6.1 Ghostty theme parser

```
For each line, take it raw (do not pre-trim inside values):
  1. If the first non-space character is '#', skip.   ⚠ ONLY here — '#' is also the value prefix.
  2. If the line is blank, skip.
  3. Split at the FIRST '=' → key (trimmed), value (trimmed). No '=' → warn(file, line), skip.
  4. Match key (exact, case-sensitive):
       "palette"             → Palette::parse_palette_entry(value)
                               → (index, rgb); explicit[index] = rgb; explicit_mask.set(index)
       "background"          → theme.background
       "foreground"          → theme.foreground
       "cursor-color"        → theme.cursor
       "cursor-text"         → theme.cursor_text
       "selection-background"→ theme.selection_background
       "selection-foreground"→ theme.selection_foreground
       anything else         → ignore silently (font-family, command, background-opacity, …)
  5. A malformed colour, or a palette index outside 0..=255, is a warning naming file+line and
     the line is skipped; it is not a fatal error for the rest of the file.
Afterwards: if any of indices 0..15 is unset → reject the file with "theme X is missing ANSI
colour N" (a theme without all 16 is broken, and the error should say which one).
```

Rules that are not obvious and that I would put in the code as comments:

- **Never strip trailing `#` comments.** `palette = 0=#21222c` is a value, not a comment. A
  generic "strip from `#`" pass corrupts every single file in the corpus. Ghostty's own config
  syntax only treats `#` at the start of a line as a comment.
- **Last duplicate wins** (`background` twice; `palette = 4=` twice). None of the 819 shipped
  files does it, but a hand-edited file will, and "last wins" is what a config reader means.
- **Ignore non-colour keys, do not error.** Theme files are config files; erroring on
  `background-opacity` would reject real themes, and *honouring* them would mean a downloaded
  theme could set `command`. Whitelist.
- **Index prefixes**: accept what `Palette::parse_palette_entry` accepts (`0x`, `0o`, `0b`,
  decimal, spaces around `N` and around `=`). Do not hand-roll this.

### 6.2 `.itermcolors` → `Rgb`

```
plist::Value::from_file(path)
  → Dictionary; for each (key, value):
      value must be Dictionary; take the three components as f64
      if "Color Space" string == "P3": (r,g,b) = p3_to_srgb(r,g,b)
      (absent or any other value → treat as sRGB, no conversion)
      out = Rgb { r: round(r*255) as u8, … }      ← ROUND, not truncate; clamp 0..=255
      if "Alpha Component" present and < 1.0 → warn once per file, ignore alpha (§6.3)
```

**Measured, not assumed.** I downloaded 39 hub `.itermcolors` files and compared the result to the
same scheme's Ghostty export, 132 mapped fields:

- `round(c*255)`: **116/116 exact matches on every sRGB file**; `trunc(c*255)`: 83/132. Rounding
  is the rule.
- The only 16 mismatches came from the one file in the sample that uses `Color Space = P3`, and
  all 16 are fixed by the matrix the hub itself uses (`tools/gen.py::_p3_to_srgb`) — with it,
  128/132 match, and the remaining 4 are schemes whose colours changed between the export and
  today (Alabaster, Spring, Symfonic whites). The matrix, for `Rgb = M · linearise(rgb)` then
  re-encode:

```
M = [[ 1.22494018, -0.22469880, -0.00012973],
     [-0.04205631,  1.04203282, -0.00000601],
     [-0.01963755, -0.07862814,  1.09826365]]
srgb_to_linear(c) = c < 0.04045 ? c/12.92 : ((c+0.055)/1.055)^2.4
linear_to_srgb(c) = c < 0.0031308 ? 12.92*c : 1.055*c^(1/2.4) - 0.055
clamp each linear channel to 0..1 before re-encoding
```

Colour-space and alpha census over those 39 files (1012 colour dicts): `sRGB` 795, `P3` 76,
`Calibrated` 26, **absent 115**; alpha `1.0` ×876, `0.5` ×9, `0.25` ×10, `0.491` ×1, `0.082` ×1,
absent ×115. Missing canonical keys: only `Cursor Guide Color` (6 files). One file had 81
top-level keys — a "dynamic colours" export — so unknown keys must be ignored, not rejected.

Guesses I would flag in code comments: treating `Calibrated` as sRGB is wrong in principle
(iTerm2's "Calibrated" is generic RGB) but it affects 26 of 1012 entries in my sample and every
converter in this ecosystem does the same; and I have only measured hub-generated files, not
files exported by iTerm2 itself.

### 6.3 Alpha

Every slot in `Theme` is opaque. **Ignore alpha everywhere, warn once per file** — except
`selection-background`, where a translucent colour is common in real use and a solid one is
visually wrong; there, composite over `background`:

```
out = round(alpha * c + (1 - alpha) * background)
```

That composite is an approximation of what iTerm2 draws; label it as such. Do not add a general
alpha channel to the theme model for this.

### 6.4 The 256-colour cube and the ramp

Theme files (all formats in scope) carry 0–15. Indices 16–255 must come from somewhere, and the
honest options are:

- **`Palette::default()`** — Ghostty's built-in 256-colour palette: its base16 defaults, the
  xterm 6×6×6 cube, and the grayscale ramp. Legacy programs assume the xterm cube, so this is the
  compatibility choice, and it is exactly what Ghostty does when `palette-generate` is off
  (default `false`, Config.zig:799–820).
- **`Palette::generate(&p, &mask, bg, fg, false)`** — Ghostty's CIELAB cube + bg→fg ramp. Opt in,
  per theme or per config, and put any explicitly-set indices ≥16 into `mask` so they survive:

```rust
let mut p = libghostty_vt::style::Palette::default();   // 16–255 = Ghostty's defaults
for (i, c) in theme.ansi.iter().enumerate() {
    p.set(PaletteIndex(i as u8), RgbColor { r: c[0], g: c[1], b: c[2] });
}
let mut mask = PaletteMask::new();
for (i, c) in theme.ansi.iter().enumerate().skip(16) {  // explicit ≥16, if any
    p.set(PaletteIndex(i as u8), rgb(c));
    mask.set(PaletteIndex(i as u8));
}
if theme.palette_generate {
    p = Palette::generate(Some(&p), Some(&mask), rgb(theme.background), rgb(theme.foreground), false);
}
term.set_default_color_palette(Some(p))?;
```

Recommendation: **`default()` by default, `generate` behind `palette_generate = true`.** Not
because `generate` is worse — it is what makes a 16-colour theme feel coherent — but because
breaking the xterm cube silently is the kind of change that shows up as "htop looks wrong" in a
bug report six weeks later. Ghostty shipped the same switch with the same default and for the
same reason.

Note the direction of travel: the model must *not* store 256 colours. Store 16 + a flag; derive
256 at apply time, so a change of policy does not mean rewriting every stored theme file.

### 6.5 Bold

What the library does: nothing. `cell.fg_color()` resolves the palette and documents that bold
handling is *not* applied. What the library does not give us: the fg **palette index**. `Cell` has
`bg_color_palette()` and `bg_color_rgb()`, but for the foreground only `fg_color()` → `RgbColor`.
That is the crux: index-based brightening (`red` → `bright red`) needs the index, and we do not
have it.

So the options, honestly:

1. **Bold is weight only** (`bold = "none"`). Matches Ghostty's default (`bold-color = null`),
   matches what the M2 renderer will do with `swash` anyway, and is one less thing to get wrong.
2. **`bold = "bright"`**, implemented at frame-build time by exact-match lookup: the resolved fg
   RGB is compared against `theme.ansi[0..8]`; a hit maps to `ansi[i+8]`, a miss leaves it alone
   (truecolor cells keep their colour). This is correct for SGR 30–37 — which is where nearly all
   bold-colour use is — and it is wrong only if a cell's colour happens to equal one of the eight
   base colours by coincidence. Cheap and predictable.
3. **`bold = "#rrggbb"`** — one fixed colour for all bold text (Ghostty's `bold-color` fixed
   form). Trivial, and mostly a novelty.

Recommend (1) as the default and (2) as the only other value dtr documents. Saying "we do not
have the index, so this is a best-effort RGB match" in the manual is worth more than pretending
otherwise; option 3 costs three lines once `Theme` carries it, so carry the enum but do not
advertise it.

### 6.6 Dim

There is no dim colour and no dim flag anywhere in dtr today: `Cell` has `bold` and `italic`, and
libghostty-vt exposes no faint-colour API (only Ghostty's *app-level* `faint-opacity = 0.5`,
which is a renderer alpha). Two coherent choices:

- **Renderer alpha** (`faint_opacity = 0.5`): `fg.with_alpha(faint_opacity)` — no colour to
  store, matches Ghostty, and it composites against whatever background the cell has, so it works
  on reverse-video cells too.
- **Derived colour** (Alacritty's approach, `[colors.dim]`): `dim(c) = mix(c, background, 0.5)`.

Recommend the first: it needs one float in `Theme`, no 8-entry table, and no re-derivation when
the background changes. If SGR 2 (`faint`) is never plumbed through `Frame`, this whole slot stays
unused — which is fine, and better than a field that lies.

### 6.7 Alacritty TOML

```
[colors.primary] foreground, background            → fg, bg
[colors.normal]  black red green yellow blue magenta cyan white → ansi 0..7 (by name)
[colors.bright]  same names                        → ansi 8..15
[colors.cursor]  cursor, text                      → cursor, cursor_text
                 the strings "CellForeground"/"CellBackground" mean
                 cursor := fg, text := bg respectively
[colors.selection] background, text                → selection_bg, selection_fg (same sentinels)
[colors.dim] / [colors.primary] dim_foreground     → ignore (see §6.6)
values: '#rrggbb', '0xrrggbb', 'rrggbb', named
```

`0xrrggbb` is *not* Ghostty syntax (`RgbColor::parse` takes `#`-prefixed or bare hex, not `0x`), so
strip a leading `0x` before handing values to `RgbColor::parse`. The YAML dialect has the same
`0x` form.

### 6.8 Applying a theme to a terminal

Once, immediately after `Terminal::new(COLS, ROWS)` and before the first `vt_write`, because
`Terminal` is `!Send` and lives on the pane thread:

```rust
term.set_default_fg_color(Some(rgb(theme.foreground)))?
    .set_default_bg_color(Some(rgb(theme.background)))?
    .set_default_cursor_color(Some(rgb(theme.cursor)))?
    .set_default_color_palette(Some(palette))?;
```

Two behaviours of the C API that matter and are documented in `terminal.rs:156–190`:
`set_default_color_palette` **preserves per-index OSC 4 overrides** a program has already applied
(only untouched indices change), and passing `None` resets to the built-in default. So re-applying
a theme on a config change is safe but will not stomp an application's OSC 4 recolouring — which
is the behaviour you want, and worth a test.

The theme must be `Clone` (it is: `[u8;3]`s and a `String`) and moved into the pane thread at
construction; reconnects rebuild the pane, so re-application happens for free.

---

## 7. The canonical model and the config file

### 7.1 `Theme`

```rust
/// Exactly the slots dtr can honour. Nothing derived is stored here.
pub struct Theme {
    pub name: String,
    pub ansi: [[u8; 3]; 16],          // indices 0–15, sRGB
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    pub cursor: [u8; 3],
    pub cursor_text: [u8; 3],
    pub selection_background: [u8; 3],
    pub selection_foreground: [u8; 3],
    pub bold: BoldStyle,              // Weight (default) | Bright | Fixed([u8;3])
    pub faint_opacity: f32,           // 0.0..=1.0, renderer-side only
    pub palette_generate: bool,       // whether 16–255 are derived instead of Ghostty's default
    pub source: Source,               // Builtin | File(PathBuf)
}
```

Deliberately **not** in the model: the 256-colour palette (derived at apply time, §6.4); a
`variant: dark|light` (nice for picking a default; add it only when something reads it);
`dim: [Rgb; 8]` (§6.6); any alpha. Keeping `[u8; 3]` rather than `RgbColor` keeps `Theme` free of
a VT-core type, so it can live in `dtr-config` and be unit-tested without linking libghostty.

### 7.2 Config representation

```toml
[theme]
# A theme name is a file name in one of the theme directories, matched exactly.
# Names commonly contain spaces — keep the quotes.
name = "Dracula"
# path = "/home/x/.config/ghostty/themes/Dracula"   # exact file; wins over `name`
# dir  = "/home/x/my-themes"                        # prepended to the search path
# palette_generate = false
# bold = "none"                                     # "none" | "bright" | "#rrggbb"
# faint_opacity = 0.5

# Anything here overrides the file. Keys are the model's slot names; ANSI indices are
# quoted strings so the table stays valid TOML without bare-integer-key surprises.
[theme.overrides]
# foreground = "#f8f8f2"
# background = "#282a36"
# cursor = "#f8f8f2"
# cursor_text = "#282a36"
# selection_background = "#44475a"
# selection_foreground = "#ffffff"
# "8" = "#6272a4"
```

Why this shape rather than either extreme:

- **A reference, not a copy.** 22 hex values in `config.toml` is a file people look at once and
  then never edit; a name is a file they can swap. It also makes the user's existing
  `~/.config/ghostty/themes` usable with no import step, which is *the* reason to prefer Ghostty's
  dialect: the 819-theme corpus, 463 of them already on this machine.
- **An override table, not a second format.** One colour off is a real need (a theme with an
  unreadable bright-black), and without overrides it means copying a file into
  `~/.config/dtr/themes` — acceptable, but it turns a one-line change into a file-management
  problem. Overrides make the inline option *unnecessary* rather than merely inferior, which is
  what keeps the parser count at two.
- **Precedence, stated once**: `path` > `name` > `builtin:default`; then `overrides` > file > the
  fallbacks in §5.1. Unknown keys inside `[theme]` are an error naming the key (the config parser
  already does this for `[serial]`/`[font]`); unknown keys inside a *theme file* are ignored,
  because theme files are config files from another program.

Integration note for whoever writes it: `dtr_config::parse` is section-based and hand-rolled, so
`[theme]` is a third `document.get("theme").and_then(Item::as_table)` block, and the nested
`overrides` table is one more `as_table()`. But `dtr_config::set_in_text` splits on the **first**
`.` only (`split_once('.')`), so `dtr config set theme.name Dracula` works as-is while
`theme.overrides.foreground` does not — either extend `set_in_text` to walk a dotted path, or
document overrides as hand-edited. Do not discover this after writing the docs for the CLI.

### 7.3 Search order

```
1. [theme] path, if set                       (exact file; error if missing — the user asked for it)
2. [theme] dir, if set, then:
3. $XDG_CONFIG_HOME/dtr/themes  (dirs::config_dir()/dtr/themes)
4. $XDG_CONFIG_HOME/ghostty/themes            ← the user's own Ghostty themes
5. <ghostty resources>/themes                 ← /usr/share/ghostty/themes,
                                                /usr/local/share/ghostty/themes,
                                                /Applications/Ghostty.app/Contents/Resources/ghostty/themes
6. builtin:default
```

First match wins; a name found in two directories resolves to the earlier one. Error text should
list the directories searched — "theme `Dracula` not found in ~/.config/dtr/themes,
~/.config/ghostty/themes, /usr/share/ghostty/themes" is the difference between a two-minute fix
and a bug report.

Windows caveat, **unverified**: Ghostty's `theme.zig` documents only the XDG path. On Windows the
user's Ghostty themes are probably under `%LOCALAPPDATA%\ghostty\themes`, while `dirs::config_dir()`
gives `%APPDATA%` (Roaming). dtr should check both and not claim to know; step 3/4 should be
best-effort on every platform.

---

## 8. Which sources dtr should read

| Option | Verdict |
|---|---|
| **Ghostty files (native)** | **Yes.** One 80-line parser, the richest field set, 819 themes upstream, 463 already installed here, and it is the format the owner's instinct named. |
| **`.itermcolors` (import only)** | **Yes.** It is the *upstream* of the Ghostty corpus, it carries `Bold Color` (which the Ghostty exporter uses for its fallbacks), and users have their own exports. One extra dependency (`plist`), no extra concepts: import writes a Ghostty file and the app only ever loads one dialect. |
| **Alacritty TOML (import only)** | **Optional, cheap.** Zero new dependencies (`toml_edit` is already in the workspace) and it is the migration path for someone who already has `~/.config/alacritty/alacritty.toml`. Worth ~60 lines. Skip if the import command is deferred — nothing else depends on it. |
| **Ship a curated set** | **No.** See §8.1. Ship exactly one hand-written default. |
| **Read `~/.config/ghostty/themes`** | **Yes**, as a search path (§7.3), not as an import. No copying, no staleness, no licence question about the *user's* files. |
| **WT JSON / kitty conf / base16 / base24 / Gogh / terminal.sexy / Alacritty YAML** | **No.** Each has a Ghostty export in the same repo as its corpus (860 files each for WT/kitty/Alacritty), so a parser buys no themes; and the kitty export is actively wrong (§4.4), which is the general hazard of importing from derived exports. Two formats is a maintenance budget; eight is a hobby. |

### 8.1 The licence finding (the reason not to vendor)

- **Ghostty itself is MIT** (`LICENSE`: "Copyright (c) 2024 Mitchell Hashimoto, Ghostty
  contributors"), and its `themes/` tarball is fetched by its own build, so *Ghostty* ships those
  819 files without a separate licence file.
- **But the colours are not Ghostty's to license.** The corpus is the iTerm2 collection, whose
  `LICENSE` says, explicitly: *"This license covers the iTerm-Color-Schemes repository collection
  of themes. The copyright/license for each individual theme belongs to the author of that
  theme."*
- The hub now accumulates that per-scheme data in `licenses.yml` (860 entries), and the numbers
  are not encouraging: **583 `unknown`, 18 `nonfree`**, and only ~**259** with a licence that
  clearly permits redistribution (205 MIT, 16 GPL-2.0-or-later, 9 Apache-2.0, 5 CC0-1.0, 5 ISC,
  4 WTFPL, 3 GPL-3.0-only, 2 LGPL-2.1-or-later, 2 GPL-2.0-only, 2 CC-BY-SA-4.0). The repo's own
  `gen.py --redistributable` exists precisely to filter to that subset.
- Copying Ghostty's MIT tarball into dtr would therefore copy ~560 files whose provenance is
  undocumented, into a repo licensed `MIT OR Unlicense`, and would re-copy them every time
  upstream's snapshot moves (463 → 819 between the installed 1.3.1 and the 2026-10-19 release;
  380 KB uncompressed).

Conclusion: **do not vendor a corpus.** If a built-in set is ever wanted, vendor only from the
documented-permissive 259, record the source URL and SPDX id per file, and keep the count in the
single digits so the review stays possible. The default shipped theme should be dtr's own,
authored in-repo.

---

## 9. Parsing plan

| Format | Crate | Notes |
|---|---|---|
| Ghostty theme | **none** — hand parser + `RgbColor::parse` / `Palette::parse_palette_entry` | ~80 lines + tests. Colour syntax and index prefixes come free from libghostty-vt, so the hand-written part is line splitting only. The `#`-is-not-a-comment rule is the whole risk (§6.1). |
| `.itermcolors` | **`plist` 1.10.3, MIT**, updated 2026-10-21 | Handles XML and binary plists, `<real>`/`<integer>`. Convert with `round(c*255)`; P3 matrix (§6.2). Only genuinely new dependency in the whole plan. |
| Alacritty TOML | **`toml_edit`** (already a workspace dep) | `DocumentMut` gives `[colors.primary]` etc. directly; no `serde` needed. Strip `0x`; handle `CellForeground`/`CellBackground`. |
| kitty, base16/24, Gogh, terminal.sexy, WT | — | Not read. If one is ever added: WT and terminal.sexy need `serde_json`; base16/Gogh need YAML, and **there is no maintained YAML crate to reach for** — `serde_yaml`'s last release is `0.9.34+deprecated` (2024-03-25); the maintained successor is `saphyr`. A 20-line flat `baseXX: "#hex"` reader is honestly the better answer for base16. |
| Alacritty YAML | — | Same YAML problem, plus the dialect is dead upstream (0.13 removed it). Do not support. |

Nothing in this plan is a `-sys` crate or a C build, so the single-`.exe` rule and the Windows
cross-build (PLAN §5.3, §7) are untouched.

---

## 10. What I could not verify

- **Whether Ghostty's theme tarball is produced by the hub's `tools/gen.py`.** The dependency is
  named `iterm2_themes`, the output is byte-identical in shape to the hub's `ghostty/` template,
  and every name exists in the hub. But the generation script is not in the Ghostty repo, so
  "same generator" is an inference, not a fact.
- **Windows theme paths.** Only XDG paths are documented in `theme.zig`. The `%LOCALAPPDATA%`
  guess in §7.3 is exactly that.
- **Real-world deviation from the generated corpus.** All 819 Ghostty files I measured are
  uniform because they are generated. Hand-written themes in the wild may use `palette-generate`,
  named X11 colours, or keys ≥16 — the parser rules in §6.1 are written for that, but I have not
  measured such a file.
- **iTerm2's own exports.** I measured hub-generated `.itermcolors` only. iTerm2's exporter may
  emit integer components, more `Calibrated`/`P3` variants, or alpha more often.
- **`Palette::generate` output stability across libghostty-vt versions.** It is the same C the
  Ghostty app runs, but the ABI manifest says the types are "not a cross-version stability
  promise", so a generated base palette could shift under a VT-core bump. Another argument for
  keeping the flag off by default and the derived cube out of the on-disk model.
- **Alacritty's exact dim formula.** Its docs say the eight `[colors.dim]` defaults are
  "calculated automatically" and the source ships precomputed constants; I did not derive the
  formula, which is why §6.6 recommending an alpha instead.

---

## 11. Cost and risk

| Item | Estimate | Risk |
|---|---|---|
| Ghostty theme parser + tests | ~1 day | Low. The corpus is uniform; the only trap is `#` comments. |
| `Theme` model, config `[theme]` + overrides, search path | ~1 day | Low–medium: `set_in_text` only handles two levels (§7.2). |
| Apply to `Terminal` + a golden test that OSC 4/10/11 answer with themed colours | half a day | Low, and this test is what stops the renderer/emulator divergence bug. |
| `.itermcolors` import (`plist`) | ~1 day | Medium: P3, alpha, missing keys — all specified in §6.2, all measured. |
| Alacritty TOML import | half a day | Low, optional. |
| Renderer changes (cursor + selection colours, bold/dim policy) | M2 work | Out of scope here; `Theme` carries the fields so M2 does not have to invent them. |

**What would change my mind:** if the M2 renderer ends up needing more than 16 ANSI slots (for
example per-cell underline colours from a theme — none of these formats has one), or if
`libghostty-vt` ever exposes a bold-colour setter, §6.5 collapses to "set it and be done" and the
RGB-matching hack disappears. Neither is true at the pinned rev.
