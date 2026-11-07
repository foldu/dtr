# The look of the chrome

A decision memo for `dtr-widgets`: why the chrome has a style at all, what a
token is, and what "native" means on each platform. Read it before adding a
colour to a screen; the short version is in `AGENTS.md` §The UI.

## What was wrong

Every surface invented its own controls. The serial sheet hand-painted eleven of
them, the settings menu used egui's stock widgets, the tab strip painted chips,
and the palette, the file explorer and the link menu each had a fourth answer.

The visible symptom was that the same control existed four times and looked
different in each place — three spellings of "segmented switch", a bespoke baud
field beside a stock combo box, a name field in its own row because a combo box
cannot be typed into. The mechanical symptom was worse: the palette at the root
of the app is *one theme*, a 16-colour terminal palette, and the chrome wrote
`mix(surface, foreground, 0.16)` at thirty call sites with fifteen different
factors standing in for "raised", "border", "hover" and "scrim". A rule that is
a float literal inside a button cannot be changed by a setting.

So the crate holds one vocabulary, and a surface asks it for a *skin*.

## Three types, and which one owns what

- **`Palette` is colour by role**: `window`, `card`, `well`, `control`,
  `control_hover`, `control_active`, `border` (hairline), `border_strong`
  (hover), `control_strong` (the resting stroke of a field or a checkbox),
  `divider`, `text`, `dim`, `disabled`, `accent` and its hover/active, the
  two `on_*` foregrounds, `selection`, `scrim`, `danger`, and the two-tone
  focus ring. A widget names a role and never a grey.
- **`Style` is shape**: the row height, the gaps, the label column, padding,
  three radii, the hairline width, the fonts, and the few *behavioural* choices
  a dialog makes — `Title::{Eyebrow,Bar}`, `Section::{Rule,Bold}`,
  `Footer::{PrimaryLast,PrimaryFirst}`, `Focus::{None,Ring}`, plus an optional
  card shadow. Each of those is a token rather than a `cfg!` so that both looks
  render on one machine, which is the only way the Windows look stays honest
  without a VM.
- **`Skin` is the pair**, and `Skin::egui_style` derives an `egui::Style` from
  it, so the widgets that are still egui's own (`TextEdit`, `ScrollArea`, popup
  frames) share the surface with the painted ones. Without that derivation the
  stock widgets keep egui's grey next to a hand-painted card.

## Two presets, and what `native` means

`Preset::{Dtr,Native}`; `Skin::resolve(preset, dtr_palette)` builds one.
`native` is the default — a terminal that has to look like a system window
should do so unless asked otherwise.

- **`dtr`** is the terminal's own chrome: monospace, an 11.5-point label, a
  23-point row, a 5-point gap, an 82-point label column, radius 4 for a control
  and 10 for a card, no shadow, hover as the whole affordance. Its colours come
  from the terminal theme, so the chrome changes when the theme does.
- **`native` on Windows** is Windows 11's: Windows UI 3's palette (`#F3F3F3`
  window, `#FFFFFF` card, the user's accent), a 32-point control, an 8-point
  gap, a 110-point label column, radius 4 and 8, a shadow of `0 8 24`, and a
  two-tone focus ring. The values are taken from
  `controls/dev/CommonStyles/Common_themeresources_any.xaml` in
  microsoft/microsoft-ui-xaml and composited over the base colour, because XAML
  carries its alpha in `#AARRGGBB` and a naive read of `#0FFFFFFF` as "grey"
  gets a translucent control.
- **`native` on Linux** resolves to the `dtr` skin, by decision: there is no
  portable system palette, and "read the GTK theme and the portal" is a project
  of its own. `Skin::resolve` takes the platform's skin as an argument, so a
  caller with no answer passes `None` and gets dtr.
- **`native` on macOS** has no palette either, so it is the dtr skin for now.
  A `windows()`-shaped constructor exists for Windows only, which is the honest
  state rather than a guess at the macOS look.

The Windows palettes are rendered and snapshot-tested **on Linux** on purpose.
A look that can only be seen on Windows is a look that is only checked when
somebody remembers to boot the VM.

## Choosing one

`[ui] widgets` in the config file, or `--widgets dtr|native` on the command line,
which wins over the file. The default is `native`.

The Windows read is in `crates/dtr/src/system.rs`: `AppsUseLightTheme` under
`HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize` for light
or dark, and `AccentColor` under `...\DWM` for the user's accent — stored there
as `0xAABBGGRR`, which is the byte order of a `COLORREF`, so reading it as
`AARRGGBB` swaps red and blue and turns a blue accent amber. Absent means the
user never chose, and the answer is WinUI's own default. It is read once and
cached: `Look::skin()` is asked by every surface on every frame.

`-v` logs `chrome skin=… requested=…` at startup. On Linux that line reads
`requested="native" skin="dtr"`, which is the design, not a failure.

## The rule that keeps it from rotting

> A colour, a radius or a stroke width written inside a screen is a bug in
> `dtr-widgets`.

When a screen needs something the skin cannot express, the fix is a token or a
primitive in the crate — never a literal at the call site — and a new primitive
arrives with a picture. Otherwise the next surface adds a fourth answer to a
question that already has three, which is exactly how this started.

## The primitives

In `crates/dtr-widgets/src/widgets/`, each a free function or a small builder
that *reports* what the user did; none of them owns the value it edits, which is
what makes them testable and what lets a caller `--send` into them.

- `Editable` — a text field with a dropdown arrow. It reports `Edit::Typed` and
  `Edit::Picked` as different answers, because a name that happens to equal a
  saved profile's must not silently load it.
- `label_cell`, `row`, `section` — the layout of a sheet: a fixed label column,
  a row of one control, and the two ways a group announces itself.
- `check` — a 16- or 20-point box with the tick drawn from two strokes, because
  egui's faces have no check mark and an icon would make every harness install
  an image loader first.
- `Button` — primary (accent) or secondary (control surface), with the label's
  width as a floor so a clipped word is never a button.
- `segmented` — one chip per option; the chosen one is the accent with
  `on_accent` on it, which is what makes "selected" mean one thing everywhere.
- `menu_row` — a full-width list entry, with the tick marking the value already
  in the field.
- `cross` — a close button drawn from two strokes, sized by its target rather
  than its glyph.

The serial sheet is drawn entirely with these: `serial_dialog.rs` contains no
colour, radius or stroke width, and its row heights and gaps come from
`Look::skin()`. `menu_row` and `cross` exist because the sheet was the second
place to need them.

**Still to move**: the tab strip paints its own chip (the elided label and the
two hit targets are the strip's own rules, but its colours and radii are the
skin's now), and the settings menu, the palette overlay, the file explorer, the
link menu and the pager still mix their own colours from `Look`'s three RGB
triples. Those are the sites where a `native` skin would be ignored, so they are
the next thing to convert rather than a style done.

Every painted widget declares an accessibility node for its response, refuses
the Enter that egui turns into a click, and keeps its hit targets disjoint.
Those are not style decisions; they were each a bug once.

## How it is checked

`crates/dtr-widgets/tests/widgets.rs` renders the whole serial sheet — every row,
in the real order — in each preset, and snapshots it. Behaviour has its own
tests: a click by accessible name, and an assertion on what the widget reported.
`crates/dtr-widgets/tests/snapshots.rs` fails when a `snapshot(…)` has no
baseline or a baseline has no test, and the baselines are committed through LFS
and must be regenerated inside the devShell, through lavapipe.

## Not decided

- The macOS palette, and whether `native` should read the desktop's accent on
  Linux through the XDG portal.
- Whether the settings menu's ComboBoxes should become a `Combo` primitive: they
  are the last stock egui controls with a popup list, and their rows are painted
  by `menu_row` but the closed control is still egui's.
- Whether a chip and a checkbox should share `chip_radius` (they do today).
- Where a key hint is drawn: the serial sheet's footer writes `enter connect`
  as text, and nothing paints a keycap yet.
