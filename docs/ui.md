# The chrome: the skin, its widgets, and how they are tested

A decision memo for `dtr-widgets` and for every surface that draws with it: why the
chrome has a style at all, what a token is, what "native" means on each platform, and the
rules the moving parts in a tiled window obey — the tab strip, the settings menu, the
palette, the keyboard map. Read it before adding a colour to a screen; the short version is
in `AGENTS.md` §The UI, and that file carries the rules an edit must not break.

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

`-v` logs `chrome skin=… requested=… accent=…` at startup. On Linux that line
reads `requested="native" skin="dtr"`, which is the design, not a failure.

**Verified in the `winvm` guest, 2026-11-07** (a light-theme Windows 11 with
`AccentColor = 0xFFD47800`):

    chrome skin="windows-light" requested="native" accent="#0078D4"
    chrome skin="dtr" requested="dtr" accent="#FFB000"

The first is the registry read; the second is the same binary a flag away, still
on the theme's amber. The run is over ssh, so the process is in session 0 and
cannot create a window — `Surface::configure` fails there, which is why only the
line before window creation is usable. A visual check of the native chrome needs
the console session, as `docs/windows.md` §2 did for `--capture`.

That measurement found a bug the unit tests could not: `AccentColor` is under
`HKCU\Software\Microsoft\Windows\DWM`, not under `...\CurrentVersion\DWM`, and
a wrong key silently produced WinUI's default blue instead of an error.

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

## How the widgets are checked

`crates/dtr-widgets/tests/widgets.rs` renders the whole serial sheet — every row,
in the real order — in each preset, and snapshots it. Behaviour has its own
tests: a click by accessible name, and an assertion on what the widget reported.
`crates/dtr-widgets/tests/snapshots.rs` fails when a `snapshot(…)` has no
baseline or a baseline has no test, and the baselines are committed through LFS
and must be regenerated inside the devShell, through lavapipe.

## The tab strip

**A chip is as wide as its title, until the title is longer than the strip.** The label is
elided at `TAB_LABEL_MAX_WIDTH` (180 points) with `…`, on one row, broken anywhere; the
full name stays in the tooltip and in the accessibility node. Unbounded, one deep directory
pushes every tab and the `+` off the strip.

**A chip has two hit targets**: the label switches tabs, a cross on the right closes that
tab, whichever is active, and both declare accessibility names (`2 shell`, `close tab 2`).
The cross is `dtr_widgets::cross_at` — two strokes in the chip's own reserved rect, not
`×`, because egui's font has no multiplication sign. The active tab's title follows its
focused session, and every switch goes through `App::set_active`, which logs
`active tab from=… to=… why=…`. The chip's colours and radii are the skin's; its elision and
its two rects are the strip's, and `New tab`'s menu is `menu_row` rows.

**The strip's own menu is a burger at the right-hand end, next to the caption buttons**, on
both platforms: `crate::ui::app_menu`, a Lucide `menu.svg` icon, a `Popup::menu` of
`menu_row`s — settings, keyboard map, about. The right end because the left is where tabs
start and a button there would move every tab; the *same* place on Linux, where a client
frame draws the same caption buttons. Its rect is part of the caption's obstacle list, not
just painted over it: the drag region stops at the menu's left edge (`chrome::drag_area`),
or a press on the menu starts a window drag.

**About is a card, not a dialog** (`crates/dtr/src/about.rs`): every fact on it is a
compile-time constant (`env!("CARGO_PKG_VERSION")`), so it cannot disagree with the binary,
and it is opened by the menu and by `about` in the palette.

Two bugs are worth keeping in mind whenever a chip changes. **A widget painted by hand has
no accessibility node** unless `Response::widget_info` declares one, and the chips were
invisible to the harness *and* to a screen reader. And `Sense::click()` is **not
pointer-only**: egui turns Enter into a click for any focused widget that senses clicks, so
a clicked chip stayed focused and every Enter typed into the shell switched tabs. The fix is
to accept a hit only when `pointer.any_click()` says the pointer clicked, and to have the
chip surrender focus after being drawn. **Overlapping click targets read as "sometimes
unresponsive"**, too: egui gives the press to the topmost widget that senses clicks, so a
chip, its close cross and the icon on top made the winner depend on allocation order —
disjoint rects, and `Image::paint_at` for anything that must not sense anything.

## The settings menu

The settings menu is mouse-first: rows are click targets, font size is a drag value, family
and theme are combo boxes, toggles are checkboxes. `Settings::ui` takes `&self` — the
renderer holds a borrow — so a control *reports* its edit as a `Change` and `Settings::apply`
writes the draft.

Every control declares an accessible name of the form `<row> value`, not the row name,
because the row's label already carries the bare name and two nodes with one name cannot be
addressed from a test. Port rows exist only when a port is focused: baud and CR translation
belong to one device.

**A menu that opens slowly the first time is doing its work in the frame**:
`usable_families` builds a fontconfig collection and probes every family (the same
`FcFontSort` cost that showed up in a profile at 16%), and it is cached in a `OnceLock` so
the second open is instant. The app warms both lists on a `dtr-warm` thread at startup, and
`-v` logs `warmed the settings lists` and `settings populated`.

## The palette

**`egui::Window` renders nothing inside `Context::run_ui`**, which is why the palette is an
overlay drawn into the same `Ui`. Two drawing traps come with it: **a widget's background
painted after its text hides the text** — the palette's selected row was invisible until the
highlight was drawn first — and **a solid quad that samples the atlas can vanish silently**,
because solids carry a negative uv, take their colour from the instance, and the shader
branches.

**A trailing space is a word break, not whitespace to trim**: the palette used to trim it
before deciding which word was being completed, so `ssh ` looked like the command `ssh` and
offered commands where hosts belong. `Parsed::of` keeps it, and after a space there is
nothing to complete for a command with no arguments.

**A command's first letter is its accelerator, so it is unique**: `s` is `ssh` and nothing
else may start with `s`, so rename the new command rather than share the letter (`shell`
became `exec`). `crates/dtr-complete` fails the build when two commands collide and holds the
ranking to it — one letter must *complete* to its command, not just rank it first. A command
with no `palette::action_for` arm does not belong in the list: `log` and `logbook` were
offered and inert, and are gone.

**`C-n` and `C-p` are consumed like the arrows**, taken from `input_mut` before the text
field sees them so navigating cannot land an `n` in the query. **Reopening the palette starts
a fresh sentence**: `toggle` clears query, selection and completions. **A modal must ignore
key releases** — the release of the keystroke that opened it arrives while it is open, which
made the settings menu flash and vanish — so overlays check `ElementState::Pressed` first.
**`Ctrl+Shift+T` opens a tab**; `Ctrl+Tab` only cycles ones that exist.

## The keyboard map

**A binding nobody can find is a binding nobody has**: `ctrl-?` opens the keyboard map
(`crates/dtr/src/keymap.rs`), and `keys` in the palette opens it too, because the chord
itself is the thing a new person does not know.

The card is built from the live `keys::Keymap`, so it shows the rebindings `[keys]` in the
config has applied and says `unbound` where a binding was switched off — "does nothing" and
"is not bound" are the same silence at the keyboard. Each row also names the action as
`[keys]` spells it (`new-tab`, `split-horizontal`), because the card is where a person finds
out what to *write*, not only what to press.

**A row is a button**, because rebinding has to start somewhere a person can find: pressing
one opens a popup that waits for a key, with a `reset`. The key is captured by `App::on_key`
rather than by the card — the card draws and reports, and dispatch is the only thing that
knows what a keystroke means — and a replacement is `dtr_config::set_value("keys.<name>", …)`
followed by rebuilding the map from `config.keys`, so the card cannot show a chord a restart
would not have. Reset is `dtr_config::remove_value`, not "write the default back": deleting
the override is what makes a future default apply. `keys::refuse_reason` is the one policy
for a chord that cannot be taken — no modifier, already another action's, or the start of a
longer binding — and the popup says which, because a refused key and a dead card look alike.

The columns, rows and fonts come from the skin, so the native style gets a taller card whose
body scrolls inside the same 588 points. **The chord column is measured, not a constant**:
each column asks the widest chord it holds, because `ctrl-alt-shift-right` and `shift-pageup`
are in different columns and one width that fits the first wastes a third of the second. The
scroll bar's width is reserved for the same reason — a floating bar paints over the last
characters of the `[keys]` names, which are the point of that column — and `Group::COLUMNS` is
the index the two columns divide at, with a test that keeps the halves about as tall as each
other.

## Icons

Icons are Lucide SVGs, fetched by a skill in this repository: `.dsh/skills/lucide-icons` and
`scripts/fetch.sh` pull `https://lucide.dev/api/icons/<name>` into
`crates/dtr/assets/icons/<name>.svg`, refusing to overwrite one already committed. They are
embedded with `egui::include_image!` and rasterised by `egui_extras`, so the one-file build
carries them.

Lucide's `currentColor` rasterises to *black* and `Image::tint` only multiplies, so `fetch.sh`
rewrites it to `#ffffff` and each file becomes a white mask the tint colours.
`install_image_loaders` is idempotent and `tab_bar` calls it rather than trusting startup,
because a kittest harness makes its own `Context` and would otherwise draw egui's
missing-image glyph, a `⚠` egui's own fonts cannot render.

## Driving the UI from a script

**Keys can be typed at the window from a script**: `--keys "ctrl-shift-p;down;down"` applies
one stroke per frame through `App::on_key`, `;` separates tokens because a chord may be a
comma, `+` joins the chords of one binding, and `text:` types characters.

`--click x,y` moves, waits a frame, then presses — what a hand does, which is what makes it
exercise the real hover-then-press path — and `--drag x1,y1,x2,y2` calls the same
`pointer_down/move/up`.

## How the chrome is tested

**The egui layer is tested with `egui_kittest`, not a window**: `cargo test` drives a
`Harness` with real layout and AccessKit, so clicks, keys and rendering are assertable —
query a widget by accessible label, `.click()`, `harness.run()`, assert what it reported.
`UPDATE_SNAPSHOTS=1 cargo test` writes a baseline PNG under `crates/dtr/tests/snapshots/` and
the plain run compares, with one snapshot per state worth seeing — `tab_bar` and
`tab_bar_hover` — because a hand-painted hover fill is invisible to every test where the
pointer is elsewhere.

These baselines follow the same LFS rule as the widget gallery — `.gitignore` ignores `*.png`
with an exception for this directory, and `.gitattributes` sends it through LFS — so a
checkout whose `git-lfs` did not smudge them holds 130-byte pointers, which `egui_kittest`
reports as a decode error that reads like a rendering bug, so `tests/snapshots.rs` checks
signature and size first and fails when a `snapshot("…")` has no baseline or a baseline has
no test. **The rasteriser that writes a baseline must be the one that reads it**: the
devShell selects lavapipe — Vulkan in software — when the machine has no `/dev/dri`, and CI
has no DRM node, so regenerate with `UPDATE_SNAPSHOTS=1 cargo test` *inside the devShell*,
never with the variable unset on a GPU machine, or the next pipeline fails on two rasterisers
disagreeing.

## Not decided

- The macOS palette, and whether `native` should read the desktop's accent on
  Linux through the XDG portal.
- Whether the settings menu's ComboBoxes should become a `Combo` primitive: they
  are the last stock egui controls with a popup list, and their rows are painted
  by `menu_row` but the closed control is still egui's.
- Whether a chip and a checkbox should share `chip_radius` (they do today).
- Where a key hint is drawn: the serial sheet's footer writes `enter connect`
  as text, and nothing paints a keycap yet.
