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
  four radii — `control`, `card`, `chip` and `check`, the last two separate
  because Adwaita rounds a 20-point box by 6 while a tab is a pill, and one
  radius for both draws either a circle that is not a checkbox or a chip that
  does not look like one — the hairline width, the fonts, and the few
  *behavioural* choices a dialog makes — `Title::{Eyebrow,Bar}`,
  `Section::{Rule,Bold}`, `Footer::{PrimaryLast,PrimaryFirst}`,
  `Focus::{None,Ring}`, plus an optional card shadow. Each of those is a token
  rather than a `cfg!` so that both looks render on one machine, which is the
  only way the Windows look stays honest without a VM.
- **`Skin` is the pair**, and `Skin::egui_style` derives an `egui::Style` from
  it, so the widgets that are still egui's own (`TextEdit`, `ScrollArea`, popup
  frames) share the surface with the painted ones. Without that derivation the
  stock widgets keep egui's grey next to a hand-painted card.

## Four presets, and what `native` means

`Preset::{Native,Adwaita,Windows,Egui}`; `Choice { preset, scheme, accent }` and
`Choice::skin(dtr_palette, native)` build one, where the second argument is the
platform's answer or `None`. `native` is the default — a terminal that has to look
like a system window should do so unless asked otherwise.

The four split one way: **`native` asks the machine**, and the other three are
looks the window carries, so they can be seen and chosen on any platform. That
split is why only three of them have a scheme and an accent to configure
(`Preset::owns_its_colours`): `native` takes both from the desktop, along with
the accent ladder Windows keeps for its own hover shades, and a settings row for
either would be a control nothing reads.

- **`native` on Windows** is Windows 11's: Windows UI 3's palette (`#F3F3F3`
  window, `#FFFFFF` card, the user's accent), a 32-point control, an 8-point
  gap, a 110-point label column, radius 4 and 8, a shadow of `0 8 24`, and a
  two-tone focus ring. The values are taken from
  `controls/dev/CommonStyles/Common_themeresources_any.xaml` in
  microsoft/microsoft-ui-xaml and composited over the base colour, because XAML
  carries its alpha in `#AARRGGBB` and a naive read of `#0FFFFFFF` as "grey"
  gets a translucent control.
- **`native` on Linux is Adwaita**, read from the running desktop: the scheme
  and the accent come from the XDG settings portal
  (`org.freedesktop.appearance`, the same service libadwaita asks), and the gate
  is the theme *name* (`GTK_THEME`, then `settings.ini`) — a theme that is not
  Adwaita is refused, because painting Adwaita's `#36363a` surfaces on Breeze is
  a confident lie. The surfaces themselves are libadwaita 1.9.3's own constants,
  measured out of its gresource, so the only values a session can change are
  those two; nothing parses CSS. This is `crates/dtr/src/system/linux.rs`, and
  `docs/gtk-palette.md` is the whole story — §6 for the refusal, §8 for what a
  second version would have to read. The portal half sits behind the `portal`
  feature, so a build that will not take zbus's tree falls back to the theme's
  name and never watches.
- **`native` on macOS** has no reader, so it is the terminal's own chrome for now:
  `Choice::skin` takes the platform's skin as an argument and a caller with no
  answer passes `None`. The constructors are `dtr`, `adwaita`, `windows`,
  `egui`, and the `windows_light`/`windows_dark`/`egui_light`/`egui_dark`
  shortcuts the preview switch and the tests use; a guess at the macOS look would
  be worse than saying there is none.
- **`adwaita` and `windows`** are those same two looks *by name*, painted on any
  machine. Their palettes and metrics are the ones above — the same constants,
  the same measurement — and they take the scheme and the accent from the config
  instead of from a desktop.
- **`egui`** is egui's own defaults, read out of `egui::Visuals` and
  `egui::Spacing` at run time rather than copied, so the preset cannot drift from
  the egui the crate is built against: a 20-point control, a 3-point gap, radius
  6, the 13-point proportional face, egui's own window shadow. Its palette is
  `panel_fill`/`window_fill`/`extreme_bg_color` for the three surfaces, the weak
  fill of a button for a control, the separator grey for a hairline, and its
  `selection` for a selection. It is also what `widgets = "dtr"` in an old config
  means: the look the window had before there were four.

`Skin::dtr` still exists — it is the terminal's own chrome, monospace over the
theme's colours — but no preset selects it any more. It is what `native` falls
back to on a machine that cannot say what its own chrome looks like, and it is
the skin the widget gallery and the older snapshots are drawn in.

The Windows and Adwaita palettes are rendered and snapshot-tested **on Linux** on
purpose. A look that can only be seen on Windows is a look that is only checked
when somebody remembers to boot the VM; the same argument is why `--widgets` can
ask for GNOME's chrome in the Windows guest. Every preset is a picture over the
same sheet — `widgets_dtr`, `widgets_adwaita_dark`/`_light`,
`widgets_windows_dark`/`_light`, `widgets_egui_dark`/`_light` — so a palette or a
row height that drifts is a diff before it is a dialog.

## Choosing one

`[ui] widgets` in the config file, or `--widgets native|adwaita|windows|egui` on
the command line, which wins over the file (and leaves the scheme and the accent
alone — the flag chooses a look, it does not reset it). The default is `native`,
and `dtr` is read as an old spelling of `egui`.

Two more keys ride with the look, and both mean something only to the three looks
that own their colours:

- `scheme = "system" | "light" | "dark"`, default `system`. `system` is the
  desktop's own answer, which is the one that changes at sunset; the other two are
  an instruction, for someone who would rather the chrome stayed put. With no
  desktop answer — no portal, no registry — the terminal theme's own lightness
  stands in, since a light theme on such a machine is the only hint there is; no
  hint at all is dark, which is what the built-in theme is.
- `accent = "#RRGGBB"`, or empty for the look's own. Adwaita's is
  `--accent-blue` (#3584E4), Windows' is its default (#005FB8 light, #60CDFF
  dark), egui's is its hyperlink colour (#009BFF / #5AAAFF). The ink drawn on it
  is chosen by luminance for `egui` — white on anything darker than mid-grey —
  while Adwaita keeps libadwaita's own white on *every* accent, yellow included,
  because matching the desktop means matching it.

The Windows read is `crates/dtr/src/system/windows.rs`: `AppsUseLightTheme` under
`HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize` for light
or dark, and `AccentColor` under `...\DWM` for the user's accent — stored there
as `0xAABBGGRR`, which is the byte order of a `COLORREF`, so reading it as
`AARRGGBB` swaps red and blue and turns a blue accent amber — plus the accent
ladder, so a hover lands on the shade Windows' own controls use. Absent means the
user never chose, and the answer is WinUI's own default. That file is one of a
pair: `system.rs` is the `Platform` trait both readers are written to
(`source`, `read`, `skin`, over one `Reading { dark, accent, ladder }`), and the
cell that holds whichever one answered. It is read once and kept current;
`Look::skin()` builds the skin from the terminal theme's colours plus that
answer, on every frame, because a palette is a few dozen mixes.

`-v` logs a `chrome` event with `skin`, `requested`, `scheme`, `accent`, `hover` and
`source` — `windows-registry`, `portal`, or `none` — and `reason` when there is
no skin. That pair is the difference between a preference and an outcome: on a
GNOME desktop the line now reads `source="portal" skin="adwaita-light"` (or
`-dark`), while a machine with no portal, or a theme that is not Adwaita, gets
`skin="dtr"` *with* the reason it is.

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

**Verified on Xvfb, 2026-11-12**, with `[ui] widgets = "adwaita"`,
`scheme = "light"`, `accent = "#ED5B00"`:

    chrome skin="adwaita-light" requested="adwaita" scheme="light"
           accent="#ED5B00" hover="#DA5706" source="portal"

— the two keys reached the skin, and the hover is the accent blended toward the
text, not the desktop's accent ladder, because there is no system accent to have
a ladder for.

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
  saved profile's must not silently load it. **Its text is centred in the row, not in a
  control**: a single-line `TextEdit` allocates `interact_size.y` and draws its galley at
  the top of that rect, and `interact_size` is the *parent's* idea of a control rather than
  this row's — so the field is made row-height and the galley asks to centre itself
  (`align(Align2::LEFT_CENTER)`). Without that the accent's `#RRGGBB` sat against the top of
  a 34-point Adwaita field while the dropdowns beside it were centred, and the serial
  sheet's profile and baud fields had the same tilt.
- `label_cell`, `row`, `section` — the layout of a sheet: a fixed label column,
  a row of one control, and the two ways a group announces itself.
- `check` — a 16- or 20-point box with the tick drawn from two strokes, because
  egui's faces have no check mark and an icon would make every harness install
  an image loader first.
- `Button` — primary (accent) or secondary (control surface), with the label's
  width as a floor so a clipped word is never a button.
- `segmented` — one chip per option; the chosen one is the accent with
  `on_accent` on it, which is what makes "selected" mean one thing everywhere.
- `tabs` — a row of labels with an accent rule under the current one. Not a
  `segmented`, although the two look alike: a chip *sets a value*, and a tab
  *changes which panel is on screen*, so the label stays text, the rule is an
  underline, the row is exactly `Style::row` tall (the rule lives inside it) and
  the accessibility role is `Tab` with `Selected` set — which egui does not do
  for a `WidgetInfo::selected`, since it maps that to *toggled*. A segmented row
  in a row pinned to the right edge used to draw its options backwards, because
  `ui.horizontal` inherits the parent's direction; both widgets now ask for
  left-to-right themselves, so the order a caller passes is the order on screen.
- `menu_row` — a full-width list entry, with the tick marking the value already
  in the field.
- `cross`, `cross_at`, `plus` — the icon buttons, drawn from two strokes and
  sized by their target rather than their glyph; `cross_at` takes a rect the tab
  strip has already reserved, which is how a chip gets a close target that does
  not overlap its label.

The serial sheet is drawn entirely with these: `serial_dialog.rs` contains no
colour, radius or stroke width, and its row heights and gaps come from
`Look::skin()`. `menu_row` and `cross` exist because the sheet was the second
place to need them.

**That conversion is finished.** The tab strip, the settings menu, the palette
overlay, the file explorer, the link menu, the pager, the keymap card and the
about card each take `look.skin()` and paint from `skin.palette`, so a `native`
skin reaches all of them. (It was not, for a while: this paragraph used to list
five of them as still mixing their own colours from `Look`'s three RGB triples.)

What still reads `Look`'s raw triples is the **terminal itself** — `render.rs`'s
cells, the IME preedit, and the window's clear colour. That is correct rather
than a leftover: a grid cell's colour belongs to the terminal theme, not to the
chrome, and a chromed grid would be a terminal that ignored its own theme. The
seam is `Look`'s theme colours out, `Skin`'s roles in, and a surface that is a
terminal is on the theme side of it.

Every painted widget declares an accessibility node for its response, refuses
the Enter that egui turns into a click, and keeps its hit targets disjoint.
Those are not style decisions; they were each a bug once.

## How the widgets are checked

`crates/dtr-widgets/tests/widgets.rs` renders the whole serial sheet — every row,
in the real order — once per skin the crate can build (dtr, Windows dark and light,
Adwaita dark and light), and snapshots it: a widget painted from a token is exactly the
thing that cannot be reviewed by eye on the platform it is for.

Behaviour has its own tests: a click by accessible name, and an assertion on what the
widget reported. `crates/dtr-widgets/tests/snapshots.rs` fails when a `snapshot(…)` has no
baseline or a baseline has no test, and the baselines are committed through LFS and must be
regenerated inside the devShell, through lavapipe.

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

**The `+` menu scrolls when the destinations do not fit.** A config with dozens of `Host`
entries made a menu taller than the window, and the last hosts were below the taskbar with
no way to reach them. The body is now an `egui::ScrollArea` capped at the window less a
strip, with a **solid bar**: a floating bar appears only while the wheel turns — which is
no help to a person who has not found the wheel — and paints over the host names it is
there to reveal. `new_tab_menu_scrolled` is the snapshot of forty hosts at 640×360.

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

**A row's height is the row's, not the context style's.** `ui.horizontal` sizes its child
from `interact_size.y`, and that number is part of the *skin*: `Skin::apply` puts it on the
context (34 points for Adwaita, 23 for dtr's own) because the stock widgets egui draws
inside a dialog's popups read the context style rather than the dialog's. A dialog therefore
leaves its row height behind, and the strip inherited it: after one visit to the settings
menu the chips were centred in a 34-point row inside a 30-point panel, seven points lower
than the `TAB_CHIP_HEIGHT` box they had been given, and the strip's clip rect cut their
bottoms off. That is the whole of "the tab strip has things cut off", arithmetic included —
`(34 − 20) / 2` is the seven. Two changes, because either alone leaves a hole: `tab_bar` and
`status_bar` set `interact_size.y` themselves (the strip's row is a chip, the bar's row is
the bar), and `Gpu::render` installs the window's resolved skin on the context whenever it
changes, so the window is the authority and every dialog a guest.
`ui::kittest_tests::a_skins_row_does_not_move_the_chips` fails if either half is undone.

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

## The card

**One card, four surfaces.** `dtr_widgets::Card` owns the modal shell — the
screen-clamped and centered rectangle, the primed `Area` (so the first frame is
not in the corner), the scrim, the frame, the skin installed on the context, and
the header. Four surfaces had each spelled that shell out: about, the keyboard
map, the settings menu and the serial sheet. Their bodies differ and stay their
own — two-column scrolling map, a form of rows, a list of prose — but the shell
does not, and four copies of a scrim and a radius are four places for them to
drift.

The header convention is the card's too: an optional dim **kicker** above the
title (it carries the cross, because that is the way out of the sentence the
kicker starts), the **title**, an inline **note** (`version 0.1.0`) or a
right-aligned **trailing** detail (`115200 8N1`), and the named cross.

**The body scrolls, and the footer does not.** A card is clamped to the window and
a form is not: the serial sheet is nine rows tall, and the 800×600 the harness
draws it at is already too small — before this, the card simply grew and the
window cut it, footer and all. The body is now an `egui::ScrollArea` the card
owns, capped at the window's edge less the margin and the footer's band, with a
**solid** bar: a floating one paints over the right-hand edge of the body, which is
where a value, a key name or a `[keys]` spelling ends. A screen that wants an
answer at the bottom supplies a **footer** closure, drawn *outside* that viewport
in a band of one row plus a gap — save/cancel, connect/cancel, the keyboard map's
hint. A button that scrolls out of reach is a button that is not there, and a
settings menu whose Save you must hunt for is the bug this shape prevents.

The requested `size` stays what it was: where the sheet is *centered* and how big
it is meant to look, not a hard cap, because a card one line taller than its
formula should grow rather than produce a scroll bar for a line. Only a window
too small for the body scrolls.

`crates/dtr-widgets/tests/widgets.rs` keeps the standalone `card` baseline, so a
margin or a header row that moves is a picture before it is a dialog, and
`crates/dtr-widgets/src/widgets/card.rs` takes `card_scrolled` — forty lines in a
360×300 window — where the body is clipped, the bar is visible and the `done`
button is asserted to be inside the sheet and clickable.

**A card may also have tabs**, and they sit between the header and the body for
the reason the footer sits below it: the way from one panel to another must not
scroll out of reach either. The card reserves the row it is given, so the sheet's
height formula is the header plus a tab row plus the *longest* panel's rows plus
the footer — the longest, because a card that grew when a tab was clicked would
move its own buttons under the pointer. Settings is the only tabbed card; the
`tabs` primitive is in the crate so the next one does not invent a second.

## The settings menu

The settings menu is mouse-first: rows are click targets, font size is a drag value, and
every closed list — the look, the scheme, the theme, the family — is a combo box whose list
is `menu_row`s, the accent is a hex field, and toggles are checkboxes. `Settings::ui` takes
`&self` — the renderer holds a borrow — so a control *reports* its edit as a `Change` and
`Settings::apply` writes the draft.

A closed list is a dropdown rather than a row of chips because the chips spent the width the
values need: on Windows' 32-point metrics the look row was the one that made the sheet
scroll. The look was chips first, `native adwaita windows egui` across a 300-point body,
and the last of them sat under the card's edge.

**Every row is on the window before Save is pressed.** The card is drawn in the skin, and the
skin is derived per frame from `Look`, so the sheet can only be honest about what a value
means if the same `Look` the card is drawn in is the draft's: `App::preview_settings` applies
the report to the draft and calls `show_look`, which re-derives the window from
`Look::from_config` — the same function startup and the save use, so a previewed window and a
saved one cannot disagree. A font change rides the same path and sets `fonts_dirty`, which
rebuilds the grid on the next frame and lets `fit_panes` resize the pty; a theme change also
sends `SetTheme`, because a window whose chrome moved and whose text did not is half a
preview. Cancel restores the draft (`Settings::cancel`) and re-derives from it, which is why
"closed without saving" and "never opened" look the same.

The one thing that cannot preview is a serial row: a baud rate is not applied to an open port
in place — the pane reconnects, keeps its scrollback and comes back talking at the new rate —
so those rows land on Save, and the sheet says nothing about them until then. Everything else
is immediate, which is why the accent field, the scheme dropdown and the theme row are worth
dragging: the window behind the card is the answer.

Every control declares an accessible name of the form `<row> value`, not the row name,
because the row's label already carries the bare name and two nodes with one name cannot be
addressed from a test. Which rows are shown is a filter rather than a layout: port rows exist
only when a port is focused — baud and CR translation belong to one device — and the look's
scheme and accent only when the look owns its colours.

**Two tabs, and general comes first.** The sheet outgrew one column: it holds the window's
look (four presets, a scheme, an accent), the grid's colours and font, and a session's
serial policy. General is the first tab and the sheet opens on it, so where the rows are
never moves between openings; `Row::panel` is the one place the split is written, and a row
that belonged to neither tab is a setting nobody can reach (there is a test for that).

The look's list is four: `native`, `adwaita`, `windows`, `egui`. `dtr` was a fifth, and it
was the same thing as `egui` twice — the chrome this program draws with egui's own widgets —
so the config reads `dtr` as an old spelling of `egui` and the menu offers one entry.

**The tabs are the mouse's, and ctrl+tab's.** A tab row is a pointer target like
every other control here, and a keyboard with no way to reach the second panel is
a keyboard that cannot change the window's look at all — so `ctrl+tab` moves
between them and `ctrl+shift+tab` walks back, with plain tab left to the rows. The
sheet opens on the first tab, always, so where the rows are never moves between
openings.

The accent is a **hex number, not a colour picker**: egui has no picker, and a terminal
config file already speaks `#RRGGBB`. The field shows the colour a save would use — the
draft's accent, or the look's own for the scheme it would resolve to — so an unset accent
is Windows' `#005FB8` in the light rather than an empty box that reads as a broken control.
Behind its arrow are the four looks' own accents, with `default` first, so "the look's
own" stays reachable without deleting text. A half-typed hex is not a colour: the field
keeps what was typed, marks itself, and the draft keeps the last thing that parsed.

**The card is the keyboard map's kind, with both ways out.** It has the same header (a
title and a `cross`), the same centered dimmed card, and a footer with `save` (primary) and
`cancel`; the X cancels, because an X that saved would be the one control on the card that
does not say what it does. A mouse edit is reported as `Reply::Change`, and the buttons
report the same `Effect::Apply`/`Effect::Cancel` the keyboard's Enter and Esc do, so the
four ways out cannot disagree. It had neither a cross nor a button, which made the keyboard
the only way to finish a menu a mouse can otherwise drive completely.

**A menu that opens slowly the first time is doing its work in the frame**:
`usable_families` builds a fontconfig collection and probes every family by loading a face
and asking for `M`, `i`, `W` and a width ratio (the same `FcFontSort` cost that showed up in
a profile at 16%). Measured on this desktop, where 47 families pass: 290 ms in a release
build — inside the click that opens the menu, which is a third of a second of frozen window,
and the whole of "settings take a while to open". The list is a `OnceLock` filled by the
`dtr-warm` thread `main` starts before the window exists, read by the menu through
`ready_families`, which never blocks: a menu opened before the probe lands shows `default`
alone and `Settings::refresh_families` fills it in on a later frame. The theme list is
scanned once in `show` and kept — the controls used to call `theme::names()` (six
`read_dir`s) and `usable_families` again on *every frame the menu was open*. `-v` logs
`probed system fonts elapsed_ms=…` and `settings populated elapsed_us=…`; the second is the
click, and it reads single-digit microseconds once the warm has finished.

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
bar's width is the card's to reserve: a body is handed the width it has *inside* a solid bar,
which is why the columns no longer subtract one, and the `[keys]` names in the right-hand
column — the point of that column — are never painted over. `Group::COLUMNS` is the index the
two columns divide at, with a test that keeps the halves about as tall as each other. The
**rebind popup is an `Area`**, a layer of its own, because the body it is drawn from scrolls
and a child `Ui` would inherit the viewport's clip rect and be cut off at the sheet's edge.

## Icons

Icons are Lucide SVGs, fetched by a skill in this repository:
`.dsh/skills/lucide-icons/scripts/fetch.sh` pulls `https://lucide.dev/api/icons/<name>` into
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
pointer is elsewhere. The settings sheet has three, because it is the card with the most
states: `settings_menu` (the first tab as it opens), `settings_appearance` (a look that owns
its colours, so the scheme and accent rows are there) and `settings_general` (a port's rows).
`tabs` is the primitive's own gallery picture.

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

- The macOS palette. Linux's is no longer a question — `native` there is
  Adwaita read through the portal — so what is left is the platform nobody has
  measured, and a constructor that guesses would be the one thing worse than
  having none. (`adwaita`, `windows` and `egui` are *not* that guess: they are
  asked for by name, on any platform.)
- Whether `native` should accept a scheme override. It takes its scheme from the
  desktop today, which is what "native" means, but a person whose desktop turns
  over at sunset may want the chrome to stay put — the setting would be a fourth
  `scheme` value (`pin`) rather than a hidden override, so the row can say so.
- Whether the settings menu's five `egui::ComboBox`es (look, scheme, family, theme, baud)
  should become a painted
  `Combo`: their open rows are `menu_row` already, but the closed control is
  still egui's, so it is the last place a `native` skin stops. `Editable` is
  *not* the answer — it is a field that can be typed into, and it reports
  `Edit::Typed` as well as `Edit::Picked`.
- Where a key hint is drawn: the serial sheet's footer writes `enter connect`
  as text, and nothing paints a keycap yet.
- What a "general" row could be beyond serial: `terminal.scrollback_limit` is in
  the config file and the menu does not show it, because it takes effect when a
  terminal is created and the menu's promise is that everything applies live.

*(A chip and a checkbox sharing one radius was on this list; the answer was no.
`Style::check_radius` is separate from `chip_radius` because Adwaita rounds a
20-point box by 6 while a tab is a pill, and they differ in the native preset —
9 against 6 — for exactly that reason.)*
