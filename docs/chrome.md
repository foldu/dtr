# Window chrome: the tabs are the title bar

Status: design memo, then implemented, 2026-11-07 · Answers note 4 of
[windows.md](windows.md) — "the tabs should be in the header". §3 records the mechanisms
considered, §4 the one chosen (client chrome on Windows and Linux; platform-looking
buttons, dtr-looking tabs), §7 the build order, and §11 what was built and what the
Windows guest verified. The code is `crates/dtr/src/chrome.rs`.

---

## 1. What the merge is worth, and what it costs

Two rows above the grid today: whatever the OS draws (default decorations,
`crates/dtr/src/app.rs:3255`) and a 30-point strip inside the client area
(`app.rs:4402`, `crates/dtr/src/ui.rs:208`). The strip is drawn even for a single tab
(`app.rs:3003`), because a shell has no status bar and a tab is where the session is
named. Merging removes the first row and changes nothing about the second.

| where | OS bar at 100% | after the merge |
|---|---|---|
| Windows 11 | 31 px (measured) | strip at y=0, buttons inside it |
| GNOME (X11 or Wayland) | 46 points (measured) | same |
| KDE Plasma | ~30 px (not measured here) | same |
| i3 / sway | 0 | nothing to gain — `chrome = native` |

(The two measured rows are the ones this tree now reproduces — §5 has the queries and the
CSS rule. KDE's number is still an estimate; under a tiling WM there is no bar to merge
with.)

Three costs, and they are layout facts rather than opinions:

- **138 points of strip** go to the Windows buttons (46 each at 100%) — roughly a chip and
  a half of width, on a row that already clips when the window is narrow.
- The strip becomes the **drag surface**, so "grab the window" and the tab chips become hit
  targets in one rectangle. That is the situation that already made a chip and its close
  cross fight for presses (AGENTS: overlapping click targets).
- The OS title still exists for the taskbar and Alt-Tab, and it is a constant `"dtr"`
  (`app.rs:3256`). Merged, the window says nothing about the session unless the title
  starts following the active tab.

---

## 2. What winit and egui already do

The first version of this note said "custom decorations", which sounds like writing a
window manager. Almost all of it exists — and on Windows `set_decorations(false)` is
deliberately *not* `WS_POPUP`:

- winit keeps `WS_CAPTION | WS_BORDER | WS_SYSMENU` (plus `WS_SIZEBOX` when resizable)
  because they are, in its own comment, "Required styles to properly support common window
  functionality like aero snap"
  (`winit-0.30.13/src/platform_impl/windows/window_state.rs:257`), and hides the frame by
  returning 0 from `WM_NCCALCSIZE` (`.../windows/event_loop.rs:1162`), clamping the client
  to the monitor's work area when maximized (`:1171`). So Aero Snap, the maximise
  animation, the system shadow (`with_undecorated_shadow`,
  `src/platform/windows.rs:509`) and the OS resize frame survive; only the caption goes
  away — which is the point.
- Drag: `Window::drag_window` (`src/window.rs:1522`) → Windows posts
  `WM_NCLBUTTONDOWN`/`HTCAPTION` into the system move loop
  (`.../windows/window.rs:492`); X11 sends `_NET_WM_MOVERESIZE`
  (`.../linux/x11/window.rs:1661`); Wayland calls `xdg_toplevel.move` with the last button
  serial (`.../linux/wayland/window/state.rs:446`). All three therefore get the
  compositor's own snap previews while dragging.
- Resize: `Window::drag_resize_window` (`src/window.rs:1537`), same three backends,
  direction-typed.
- System menu: `Window::show_window_menu` (`src/window.rs:1555`) — the real menu, Windows
  only.
- egui speaks all of it: `ViewportCommand::{StartDrag, BeginResize, Maximized, Minimized,
  Decorations, Title}` are handled in `egui-winit-0.37.0/src/lib.rs:1961-2080`, which also
  turns the Windows undecorated shadow on when decorations are switched off (`:2076`).
  No raw winit is needed in the drawing code.
- `ViewportInfo.maximized` is refreshed from `window.is_maximized()` every frame
  (`egui-winit/src/lib.rs:1506`), so a restore icon needs no bookkeeping of its own.

**Both of these were built** — §11 is the verification. They are kept here because they are
why the design is what it is, not because they are outstanding.

1. **Windows 11's snap-layout flyout.** It appears when `WM_NCHITTEST` answers
   `HTMAXBUTTON` over the maximise button. winit never sends that, and
   `EventLoopBuilderExtWindows::with_msg_hook` cannot help: the hook sees *posted*
   messages, and hit tests are sent. So `crates/dtr/src/chrome/windows.rs` subclasses the
   window with `SetWindowSubclass` — a direct `windows-sys` dependency — and answers the
   hit test itself. What is still open is not the hit test but the flyout: the guest
   answers `HTMAXBUTTON` when asked directly and still shows no flyout, while Edge in the
   same guest does. §11 is what that rules out and what is left.
2. **Linux edge zones.** With no WM frame nothing resizes the window, so eight border
   zones are ours to hit-test and answer with `BeginResize`: `chrome::resize_edges`. On
   Windows this code is inert — the retained OS frame takes the press before egui sees it
   — which is worth confirming on the VM rather than assuming.

---

## 3. The mechanisms that were considered

| | what it does | cost | verdict |
|---|---|---|---|
| A. leave it | two rows, native semantics everywhere | 0 | not what was asked for |
| B. merge only while maximized | `set_decorations(!is_maximized)` on `Resized` | S | rejected |
| C. borderless fullscreen only | F11, strip at the top, no buttons | XS | keep as a feature |
| D. client chrome everywhere | strip *is* the title bar | S–M + M | **chosen** |

B was rejected for three reasons: a relayout on every maximise and restore (chips move
under the pointer), two layouts that have to stay consistent — the class of bug `top_inset`
exists to prevent — and the platform mechanics. On Wayland `xdg_decoration` is a request a
compositor may refuse; on X11 motif hints on a mapped window are WM-dependent.

C is worth having anyway: `set_fullscreen(Borderless)` gives the merged look on one display
with no buttons and no drag code. A cheap taste of D, and a feature in its own right.

---

## 4. The chosen design

`chrome = "client" | "native"` in `[window]`, default `client`:

- **Everywhere: `client`.** Wayland has no server-side decorations at all — GNOME Terminal
  and kgx draw their own header bars — so this is the platform's normal shape; Windows and
  X11 get it for the one look, with no platform detection and no special case for tiling WMs.
- **`native`** is the escape hatch: the WM's frame with our strip under it, exactly today's
  layout. Nothing reads it except the startup attributes.
- **macOS later**, and not with custom buttons: `with_titlebar_transparent`,
  `with_title_hidden` and the real traffic lights, which winit already exposes.

The strip is the title bar:

```
[icon] [ 1 shell ][ 2 nvim ] … [ + ]      … drag …      [–] [□] [×]
```

- **Tabs** keep dtr's chips, palette and hit targets unchanged (`ui::tab_bar`,
  `ui.rs:317`), so its two kittest baselines (`ui.rs:760`, `ui.rs:837`) stay valid. The
  lit chip is the whole name: there is no separate session title in the caption, and the
  OS title (taskbar, Alt-Tab) is the only place the name is also written as text.
- **Buttons** take the platform's metrics, glyphs and hover structure (§5), because that is
  what the eye reads as native. Their *colours* come from the strip's own palette, so a
  dark theme never gets light-mode greys glued to it — with the Windows close-hover red as
  the one fixed colour.
- **The mark** is the application's, in both frames. Windows puts it 8 points in, in a
  16-point square, where its caption icon goes and where the system menu is, and a click
  opens that menu. A GNOME header bar puts it beside the first tab at 20 points, and there
  it is not a button: no pointer, no focus, no accessible name. `chips_left()` reserves 8
  points either side of it, so a chip never starts on top of the mark.
- **Drag** lives only in the rectangle no chip and no button claimed. Double-click toggles
  maximise; Alt+F4 still closes.
- **`top_inset` and `fit_panes` do not change.** The reservation is still one row; that the
  row is now the title bar is the whole change.

---

## 5. Looking native without being native

Three independent levers.

| | Windows 11 | GNOME / Adwaita |
|---|---|---|
| button | 46×32 pt, butted together, full strip height | 24×24 pt circles, ~6 pt gaps, centred |
| glyph | 10 pt | 16 pt |
| caption icon | 16 pt, 8 pt in, opens the system menu | 20 pt, 8 pt in, a mark |
| strip height | a tabbed caption: 40 pt, the queried band as a floor | 46 pt (libadwaita) |
| hover | flat fill per button | circle behind the glyph |
| close | `#C42B1C` fill, white glyph | the same grey as the others |

**The strip's height is a *tabbed* caption's, with the platform's own band as a floor.** With
client chrome the strip *is* the title bar, and 30 points was a tab row: beside a 46-point
GNOME header bar it read as a strip of afterthought. `chrome::title_height(scale)` answers,
and `-v` logs the answer as `strip geometry strip_points=… chip_points=…` so a picture is not
the only evidence:

- **Windows** asks, and then raises the answer to a caption with tabs in it.
  `SM_CAPTION + SM_CYSIZEFRAME + SM_CXPADDEDBORDER` via `GetSystemMetricsForDpi` at the
  window's DPI is where a standard window's client area starts — the band a client-side
  caption stands in — and `AdjustWindowRectExForDpi`, asked with the style winit really
  keeps, returns the same pixels at every DPI (`chrome/windows.rs`). Measured in the
  Windows 11 guest (build 26300):

  | DPI | caption | frame | padded | pixels | points |
  | --- | --- | --- | --- | --- | --- |
  | 96 (100 %) | 23 | 4 | 4 | 31 | 31 |
  | 120 (125 %) | 29 | 4 | 5 | 38 | 30.4 |
  | 144 (150 %) | 34 | 5 | 6 | 45 | 30 |
  | 192 (200 %) | 45 | 5 | 8 | 58 | 29 |

  A WinForms window in the same guest put its client area 31 pixels below the window top at
  96 DPI, so the sum is the band; the 32 some sources quote includes the 1-pixel top border,
  which dtr draws as the strip's own hairline. `GetSystemMetricsForDpi` and not
  `GetSystemMetrics`: winit's windows are per-monitor DPI aware, and a window dragged to a
  125 % monitor has 120-DPI metrics.

  **That band is a floor, not the answer, because this strip carries the tabs.** Measured in
  the same guest (`winverify`, clean guest, exclusive window fits; the 150 % screenshots are
  `/tmp/winverify/150-side.png`):

  | caption | 96 dpi (100 %) | 144 dpi (150 %), windowed | 144 dpi, maximised |
  | --- | --- | --- | --- |
  | a plain Win32 caption (`SM_CAPTION` + frame + padded) | 31 px | 45 px | — |
  | Explorer's title/tab band | 40 px | 58 px | 49 px |
  | Settings' application-frame bar | 41 px | ~61 px | 61 px |
  | Notepad's tabbed caption | 42 px | 62 px | 49 px |
  | Windows Terminal's row (`titlebarHeight = 40`) | 40 px | 60 px | 60 px |
  | libadwaita's GNOME header bar | 46 pt | — | — |
  | **dtr's own client caption** | **40 px** | **60 px** | **60 px** |

  The comparison behind "titlebar too small (4k hidpi 150%)" was one of the *tabbed* rows,
  and the band dtr was using shrinks in points as the display scales — 31, 30.4, 30, 29 — so
  at 150 % it drew a 45-pixel ribbon beside the 58–62-pixel tabbed captions around it.
  **40 is Windows Terminal's row**, and it is the middle of the native tabbed range: windowed
  it is within two pixels of Explorer, Settings and Notepad; maximised it is ~11 px taller
  than Explorer and Notepad, which is only because the Win10/11 hidden frame's top 11 px are
  off screen — do not lower the constant to their 49. Windows Terminal is 60 px in both
  states, which is what a terminal with tabs should be. The queried band stays as a floor, so
  the strip is never shorter than the platform's own caption at any scale, and a person who
  has made their system captions taller than 40 keeps theirs. At 150 % that is 60 px of strip
  over a 66-pixel reservation (with the 4-point gap between strip and grid); at 100 %, 40
  over 44. The maximised window is not a second geometry: winit clamps the client area to the
  monitor's work area, and the strip still starts at its top and reserves the same inset.
- **GNOME** is a constant, because the number is in libadwaita's CSS and dtr does not parse
  CSS (`system/linux.rs` says why): libadwaita 1.9.3's `window:not(.ssd-frame) >
  headerbar.titlebar { min-height: 46px }` — `headerbar { min-height: 47px }` for one out of
  the titlebar position. `gresource extract $libadwaita/org/gnome/Adwaita/styles/gtk.css`
  and grep the rule is how it was checked here; Nautilus, GNOME Terminal and Settings are
  all 46.

**The chip follows the strip.** `ui::tab_chip_height` is the strip less six points of air
above and below, floored at the 20-point chip and capped at Adwaita's own tab
(`tabbar tab { min-height: 26px }`): the 40-point Windows caption would give 28 points, so
the cap keeps its chip at Adwaita's 26 — tall enough that the row reads as a place for tabs
rather than a pill in a caption — and GNOME's 46-point bar gets the same 26. With
`chrome = "native"` the strip is only a tab row under the window manager's own title bar,
and stays 30 points.

**Glyphs** — the lever that matters most, because shape is what the eye recognises:

- **Windows** ships the exact ones as a font: `Segoe Fluent Icons` on 11
  (`SegoeIcons.ttf`), `Segoe MDL2 Assets` on 10 (`segmdl2.ttf`). The codepoints are `E921`
  minimise, `E922` maximise, `E923` restore, `E8BB` close. Resolve the family through
  `fontique` — the path `fonts::ui_fallback` already uses to hand egui `(name, bytes)`
  (`crates/dtr/src/fonts.rs:150`) — and draw with a dedicated egui family. Nothing is
  redistributed: it is a font on the user's machine. Absent, fall back to the Lucide
  `minus`/`square`/`copy`/`x` already embedded.
- **GNOME** has no icon font, and Adwaita's symbolic icons are CC-BY-SA/LGPL while dtr is
  `MIT OR Unlicense`, so the four shapes are drawn on Adwaita's own 16-unit grid with its
  1.5 pt round-cap stroke. Lucide's can be tinted to the right colour and are the right
  *kind* of shape, but their 2-in-24 geometry and square caps are a different drawing, and
  at 16 pt that difference is the whole glyph.

**A GNOME header bar does have the application's mark**, which this memo said it did not,
so the caption drew nothing there and `chips_left()` gave the chips the strip's own 6
points. Adwaita's own header bars put the window's icon where the same bar puts its title;
a libadwaita window that sets no icon shows nothing, which is why the claim survived — dtr
*sets* one, so it shows it. The mark is drawn from `assets/logo/dtr-icon.svg` rather than
the 32-pixel PNG the Windows caption uses: 20 points is 40 pixels at 2x, and the raster is
soft at exactly the scale this bar is measured in. It is painted, not added as a widget —
a mark that takes a press and does nothing with it is worse than one that cannot be
pressed, and the tab chips behind it are the things a pointer should find.

**The window icon** is three separate things, and only one of them is about a window. The
running window's icon is set at creation — `with_window_icon(window_icon())` in
`crates/dtr/src/app.rs`, an `Icon::from_rgba` over the 32-pixel render of
`assets/logo/dtr-icon.svg` — so the taskbar shows the mark while the app runs. Explorer, the
taskbar's *pin* menu, the Start menu and Alt-Tab read a resource out of the **file** instead,
which is why `crates/dtr/build.rs` compiles `assets/logo/dtr.ico` with `zig rc`; `WM_SETICON`
cannot fix those. On Linux the desktop entry supplies `Icon=dtr`. §11 is how the three were
made to agree.

---

## 6. Shape in dtr

| piece | where |
|---|---|
| policy read from config | `dtr-config`, `[window] chrome = …` |
| decorations, shadow, icon at creation | `App::resumed` (`with_decorations`, `with_undecorated_shadow`, `with_window_icon`) |
| tokens, hit test, paint, glyph source | `crates/dtr/src/chrome.rs`, and `chrome/windows.rs` behind `#[cfg(target_os = "windows")]` |
| caption row: chips left, buttons right, drag between | the `Panel::top` in `App::render` |
| OS title follows the active tab | `App::set_active` |
| tests | kittest for the rects, `--capture` for the look, winvm for Windows |

`chrome.rs` holds the geometry, the hit test, the painted glyphs and the strip's menu, and
knows nothing about windows; `chrome/windows.rs` is the Windows token table plus the `Font`
glyph source. `Glyphs::{Font, Painted}` is the one place the shared code asks which, so it
never branches on the platform anywhere else.

---

## 7. Build order

All four steps landed; §11 is the verification and the one thing that did not reproduce.

1. **Policy and a frame you can move** (S). `chrome = client`, decorations off, drag in the
   free rectangle, double-click maximise, title follows the tab — no buttons yet.
   Gate: a frameless window that moves, maximises on double-click and closes with Alt+F4,
   on X11, Wayland and Windows.
2. **Buttons** (S–M). Tokens per platform, Lucide glyphs first, hit targets disjoint from
   the chips, accessibility names, `--capture` at 100% and 150%.
3. **Platform glyphs and the icon** (S). Segoe Fluent Icons on Windows, painted Adwaita
   shapes on Linux, `with_window_icon`, icon → `show_window_menu`.
4. **Snap flyout** (M, Windows). The `WM_NCHITTEST` subclass over the maximise button —
   the `windows-sys` dependency is worth it, so this is in scope rather than polish; winvm
   to see the flyout.

Afterwards, when it bites: tab overflow (the 138 points make it arrive sooner), and macOS.

---

## 8. Traps

- **Windows moves the client down by a pixel.** The undecorated-shadow path does
  `params.rgrc[0].top += 1` (`event_loop.rs:1194`), so the strip starts one pixel below the
  window's top edge when not maximized. Not a bug; do not chase it.
- **Wayland decorations are chosen at creation.** `xdg_decoration` is a request; winit asks
  for client mode when decorations are off and hides its own frame
  (`wayland/window/mod.rs:101`, `wayland/window/state.rs:983`). Toggling later is what
  mechanism B needed, and is exactly why B was rejected: read the policy once, at startup.
- **`StartDrag` needs the button still down.** Wayland's `xdg_toplevel.move` takes the last
  button serial, so initiate on `drag_started()`, never on `clicked()`.
- **X11 needs focus before the drag.** egui-winit guards `StartDrag` with
  `window.has_focus()` (`egui-winit/src/lib.rs:1961`), because an unfocused drag on X11
  keeps the input grab until the app is killed. With focus-follows-mouse that is already
  true; under sloppy focus the first drag would be dropped, so focus first and skip the
  drag that frame.
- **One `Sense::drag` over the whole strip breaks the chips.** egui gives the press to the
  topmost click-sensing widget and the winner depends on allocation order — the
  "sometimes unresponsive" bug the tab chips already carry a comment about. The drag region
  is an explicit rect over what is left.
- **A `ViewportCommand` is not a winit call.** `StartDrag`, `BeginResize`, `Maximized`,
  `Minimized` and `SetTheme` leave the frame in `FullOutput::viewport_output`, and they only
  become window operations when `egui_winit::process_viewport_commands` is handed that
  output; `update_viewport_info` is the other half, because egui reads the maximised flag
  from `RawInput::viewports` and defaults it to `None`. dtr did neither until 2026-11-12,
  so on Windows the caption could not drag the window and the resize borders could not
  resize it — the maximise button still worked only because `WM_NCLBUTTONDOWN` is handled
  by hand — and the frame never knew it was maximised, so the restore glyph never appeared.
  `-v` logs `viewport commands commands=[…]` for every list egui produced; a missing
  `StartDrag` there is a press that never reached the drag region, and a present one is a
  command the backend was not delivering.
- **`HTMAXBUTTON` costs egui the hover.** Answering the hit test moves the pointer into the
  non-client area for that rectangle, so Windows sends `WM_NCMOUSEMOVE` and egui-winit never
  sees a pointer position there — the button stayed unlit on Windows while the other two,
  which are client widgets, lit up. The hit test is therefore also the hover recorder:
  `chrome/windows.rs` keeps `MAXIMIZE_HOVER`, invalidates the window when it changes (a hit
  test is not a paint, so the flag alone would arrive a frame late or never), and
  `chrome::buttons_for` ORs it into `response.hovered()` for the maximise button.
- **The offscreen path has no window.** `--screenshot` has no maximise state to read, so it
  keeps drawing tabs without buttons. The caption is drawn by `app.rs`, not by
  `ui::tab_bar`, which is why the kittest baselines do not move.
- **The status bar was this mistake in one place, and it is fixed.** The panel is
  `.exact_size(STATUS_HEIGHT)` in *points* while the reservation `fit_panes` takes is in
  *pixels*, which is `windows.md` §2.2's bug; the caption row inherits the same lesson,
  because the strip is reserved in pixels and laid out in points.

---

## 9. What will still not match

High-contrast mode (native buttons re-theme; ours would need `SystemParametersInfo`), the
Win11 flyout — the subclass answers the hit test and Windows still does not show it (§11) —
Windows' hover tooltip (there is none), a GTK theme that is not Adwaita (which is *refused*:
that machine gets the `dtr` chrome rather than Adwaita's geometry in our colours,
`gtk-palette.md` §6), and any WM feature driven by the WM's own title bar (there no longer
is one).

---

## 10. What was open, and the answers

All three were answered on 2026-11-07, before any code:

1. **X11** — `client`, no special case: one look, `native` is the escape hatch, and nothing
   detects a tiling WM.
2. **Windows 11 flyout** — in scope, worth the `windows-sys` dependency (§7 step 4).
3. **Caption title** — none. The lit chip is enough; §4 says so.

---

## 11. What was built, and what the guest verified

Everything in §7 landed: `[window] chrome` with its config test, decorations off at
creation (plus the Windows undecorated shadow), the drag region and double-click, the OS
title following the active tab, both button styles with their hover and accessibility
names, Segoe Fluent Icons through fontique with Lucide as the fallback, the painted
Adwaita shapes, the compiled-in window icon, the caption icon opening the system menu, the
eight resize borders, and the `WM_NCHITTEST`/`HTMAXBUTTON` subclass.

**The icon turned out to be three things, and only one of them is a window.** `WM_SETICON`
covers a running window; Explorer, the taskbar's pin menu, the Start menu and Alt-Tab read
a resource out of the PE, and that resource did not exist — so `dtr.exe` showed the
generic blank wherever the *file* was named, which is most of the places a person meets an
icon. `crates/dtr/build.rs` now compiles `assets/logo/dtr.ico` with `zig rc` — the
Windows build's own toolchain, so no new dependency and no wine — and links the resulting
`.rsrc` object. The artwork is new (`assets/logo/dtr-icon.svg`, rendered by
`scripts/icon.sh`): the pixel mark on a rounded CRT-black tile, because a transparent
amber glyph is 10.5:1 on the dark taskbar and 1.8:1 on the light one, and a tile is the
only version that holds either. The same 32-pixel render is what the window and the
caption icon load, so the three can no longer disagree.

Checked on the Windows 11 guest (build 26300), driving the exe as an interactive scheduled
task and injecting input over QMP:

| Claim | How | Result |
|---|---|---|
| the window has no visible frame | window styles dumped from the guest | `0x16CF0000`: `WS_CAPTION`/`WS_THICKFRAME` kept, frame hidden by `WM_NCCALCSIZE` — exactly the retained-styles design |
| the hit test answers `HTMAXBUTTON` | `SendMessage(WM_NCHITTEST)` at the button's centre | `9` |
| the button maximizes and restores | QMP click, then the window rect | `-8,-8,1288,760` and back |
| the close button exits | QMP click, then the process | gone, and the window with it |
| the caption icon opens the real system menu | QMP click | Restore/Move/Size/Minimize/Maximize/Close (Alt+F4) |
| the buttons look like Windows' | screenshot, enlarged | 46-point buttons, Fluent glyphs, the close hover in `#C42B1C` |
| the executable carries an icon | `[System.Drawing.Icon]::ExtractAssociatedIcon("C:\dtr\dtr.exe")` | the app icon, at 32×32 |
| the taskbar draws it | screenshot with dtr running | the tile, under the running bar, on a light taskbar |

**One thing did not reproduce: the snap-layout flyout.** The hit test answers
`HTMAXBUTTON` when asked directly, which is the documented requirement
([learn.microsoft.com](https://learn.microsoft.com/en-us/windows/apps/desktop/modernize/ui/apply-snap-layout-menu)),
but hovering the button in the guest shows no flyout — while Edge, in the same guest,
shows one. The remaining difference between Edge and dtr is plausibly
`DwmExtendFrameIntoClientArea`, which Chromium calls and dtr does not. Treat the flyout as
unverified on real hardware; the hit test and the button itself are not affected, and the
`WM_NCLBUTTONDOWN` handler exists because winit does not forward a non-client click to
`DefWindowProc` (that bug made the button dead until it was added).

**The 150 % caption** (2026-11-20). "Titlebar too small (4k hidpi 150%)" was measured
against title bars that have tabs in them, not against a plain `SM_CYCAPTION` band, and the
gap is real. Measured by `winverify` in the clean guest (windowed and maximised, 144 dpi;
screenshots `/tmp/winverify/150-side.png`):

| caption | 100 % | 150 % windowed | 150 % maximised |
| --- | --- | --- | --- |
| dtr, before | 31 px | 45 px | 45 px |
| Explorer | 40 px | 58 px | 49 px |
| Settings | 41 px | ~61 px | 61 px |
| Notepad | 42 px | 62 px | 49 px |
| Windows Terminal's row | 40 px | 60 px | 60 px |

The band dtr was using *shrinks* in points as DPI rises (31 / 30.4 / 30 / 29), so at 150 % it
drew a 45-pixel band where the tabbed captions around it are 58–62; maximised, Explorer and
Notepad only drop to 49 because the Win10/11 hidden frame's top 11 px are then off screen —
a terminal with tabs should follow Windows Terminal's 60 px in both states, not their 49.
`GetSystemMetricsForDpi` and `AdjustWindowRectExForDpi` agree at 96/120/144/192 DPI
(31/38/45/58), so no pixels were lost to rounding; the strip was sized as the wrong *kind*
of caption. It is now a tabbed caption's 40 points (§5) — 60 px at 150 %, 40 at 100 %, i.e.
within two pixels of Explorer/Settings/Notepad windowed and exactly Windows Terminal — with
the queried band as a floor. The drawing side was never in doubt: on Linux at
`WINIT_X11_SCALE_FACTOR=1.5` the Adwaita path was 46 pt = 69 physical px, logged as
`strip geometry strip_points=46 chip_points=26 reserved_px=75` and measured in the capture;
after the change the Windows path is 40 pt, so `-v` should print
`strip_points=40.0 chip_points=26.0 reserved_px=66.0 strip_px=60.0` at 150 %. The 40-point
Windows strip was cross-built (`.#windows`) but **not re-run in the guest**;
`the_windows_caption_is_a_tabbed_one` and `the_caption_is_scaled_once_on_each_side_of_the_boundary`
cover the arithmetic until it is.

Two smaller findings from the same session, one of them since fixed:

- **A stray console window used to sit behind dtr on Windows; it does not now.** The exe
  was console-subsystem, and a process started without a console gets one allocated.
  `main.rs` carries `#![cfg_attr(windows, windows_subsystem = "windows")]`, and the guest
  measured the difference: the same scheduled-task launch in `winvm`'s `base` snapshot gave
  `ConsoleWindowClass | C:\build\old\dtr.exe` — `console windows: 1` — for the console
  build, and `console windows: 0` for the GUI build, whose only windows were its `Window
  Class` (900 × 620) and its winit event target. A GUI process still inherits a parent
  console, and redirected handles still work — `dtr.exe --config-path` wrote its path to
  the redirected file from the guest — so `-v` and the non-interactive commands keep
  working from a shell. The one difference is `cmd`'s: it does not wait for a GUI-subsystem
  process, so an interactive `dtr.exe theme list` returns the prompt at once (a pipe or a
  redirect still gets every byte).
- **The caption icon was drawing egui's ⚠.** `include_image!` on a PNG needs egui_extras'
  `image` feature and only `svg` was enabled, so the corner of the strip showed egui's
  failed-image glyph — a small red outline — instead of the logo. (The "dark artwork on a
  dark strip" note in the first version of this section was that glyph, read as artwork.)
  `ui::every_embedded_image_decodes` asks every embedded source to load, so a dropped
  decoder is a failed test rather than a picture that quietly becomes a warning sign.
