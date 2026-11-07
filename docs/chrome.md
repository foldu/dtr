# Window chrome: the tabs are the title bar

Status: design memo, 2026-11-07 · Answers note 4 of [windows.md](windows.md) — "the tabs
should be in the header". §3 records the mechanisms considered, §4 the one chosen (client
chrome on Windows and Linux; platform-looking buttons, dtr-looking tabs), §7 the build
order. Nothing here is implemented yet.

---

## 1. What the merge is worth, and what it costs

Two rows above the grid today: whatever the OS draws (default decorations,
`crates/dtr/src/app.rs:3255`) and a 30-point strip inside the client area
(`app.rs:4402`, `crates/dtr/src/ui.rs:208`). The strip is drawn even for a single tab
(`app.rs:3003`), because a shell has no status bar and a tab is where the session is
named. Merging removes the first row and changes nothing about the second.

| where | OS bar at 100% | after the merge |
|---|---|---|
| Windows 11 | ~32 px | strip at y=0, buttons inside it |
| GNOME (X11 or Wayland) | ~37 px | same |
| KDE Plasma | ~30 px | same |
| i3 / sway | 0 | nothing to gain — `chrome = native` |

(The heights are approximate and were not measured in this tree; the point is that under a
tiling WM there is no bar to merge with.)

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

What is missing — the actual platform work:

1. **Windows 11's snap-layout flyout.** It appears when `WM_NCHITTEST` answers
   `HTMAXBUTTON` over the maximise button. winit never sends that, and
   `EventLoopBuilderExtWindows::with_msg_hook` cannot help: the hook sees *posted*
   messages, and hit tests are sent. This needs `SetWindowSubclass` and therefore a
   direct `windows-sys` dependency.
2. **Linux edge zones.** With no WM frame nothing resizes the window, so eight border
   zones are ours to hit-test and answer with `BeginResize`. On Windows this code is inert
   — the retained OS frame takes the press before egui sees it — which is worth confirming
   on the VM rather than assuming.

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

`chrome = "auto" | "client" | "native"` in `[window]`, default `auto`:

- **Windows and Wayland: `client`.** Wayland has no server-side decorations at all — GNOME
  Terminal and kgx draw their own header bars — so this is the platform's normal shape
  there.
- **X11: `client` too**, for one look across Linux, with `native` documented for tiling
  window managers (where there is no bar to merge with, and three buttons that mostly do
  nothing would be worse than none).
- **macOS later**, and not with custom buttons: `with_titlebar_transparent`,
  `with_title_hidden` and the real traffic lights, which winit already exposes.

The strip is the title bar:

```
[icon] [ 1 shell ][ 2 nvim ] … [ + ]      … drag …      [–] [□] [×]
```

- **Tabs** keep dtr's chips, palette and hit targets unchanged (`ui::tab_bar`,
  `ui.rs:317`), so its two kittest baselines (`ui.rs:760`, `ui.rs:837`) stay valid.
- **Buttons** take the platform's metrics, glyphs and hover structure (§5), because that is
  what the eye reads as native. Their *colours* come from the strip's own palette, so a
  dark theme never gets light-mode greys glued to it — with the Windows close-hover red as
  the one fixed colour.
- **The icon** is Windows-only in the caption (Windows has one; GNOME header bars do not),
  and clicking it shows the system menu.
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
| caption icon | 16 pt, ~10 pt in from the edge | none |
| strip height | 32 | 32 |
| hover | flat fill per button | circle behind the glyph |
| close | `#C42B1C` fill, white glyph | the same grey as the others |

**Metrics.** egui works in points, and Windows scales caption metrics linearly with DPI,
so 32 pt tall and 46 pt wide reproduce 32 px and 46 px at 100%, and 48/69 at 150%. No
`GetSystemMetricsForDpi` call is needed for the common sizes; the exact 125% metric may be
a pixel off, and that is the point at which to query instead of multiply.

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

**The window icon** is a separate thing and does not exist today: nothing calls
`with_window_icon` or `with_taskbar_icon`, so the Windows exe shows the generic blank in
the taskbar. The artwork is already in the tree (`assets/logo/dtr-mark-32.png`,
`-512.png`) and needs `Icon::from_rgba` at creation. On Linux the desktop entry supplies
`Icon=dtr`, so this is mostly a Windows fix.

---

## 6. Shape in dtr

| piece | where |
|---|---|
| policy read from config | `dtr-config`, `[window] chrome = …` |
| decorations, shadow, icon at creation | `app.rs:3255` (`with_decorations`, `with_undecorated_shadow`, `with_window_icon`) |
| tokens, hit test, paint, glyph source | `crates/dtr/src/chrome/{mod,windows,adwaita}.rs` |
| caption row: chips left, buttons right, drag between | the `Panel::top` in `app.rs:4396` |
| OS title follows the active tab | `App::set_active` |
| tests | kittest for the rects, `--capture` for the look, winvm for Windows |

`chrome/mod.rs` holds the geometry and the hit test and knows nothing about windows; the
two submodules are a token table plus a glyph source (`Font` on Windows, `Painted` on
Linux), so the shared code never branches on platform except to pick one.

---

## 7. Build order

1. **Policy and a frame you can move** (S). `chrome = client`, decorations off, drag in the
   free rectangle, double-click maximise, title follows the tab — no buttons yet.
   Gate: a frameless window that moves, maximises on double-click and closes with Alt+F4,
   on X11, Wayland and Windows.
2. **Buttons** (S–M). Tokens per platform, Lucide glyphs first, hit targets disjoint from
   the chips, accessibility names, `--capture` at 100% and 150%.
3. **Platform glyphs and the icon** (S). Segoe Fluent Icons on Windows, painted Adwaita
   shapes on Linux, `with_window_icon`, icon → `show_window_menu`.
4. **Snap flyout** (M, Windows). The `WM_NCHITTEST` subclass over the maximise button;
   winvm to see the flyout.

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
- **The offscreen path has no window.** `--screenshot` has no maximise state to read, so it
  keeps drawing tabs without buttons. The caption is drawn by `app.rs`, not by
  `ui::tab_bar`, which is why the kittest baselines do not move.
- **The status bar's `STATUS_HEIGHT * scale()` is still wrong** (`app.rs:4429`) and gets no
  better here; a merged caption changes nothing about it (`windows.md` §2.2).

---

## 9. What will still not match

High-contrast mode (native buttons re-theme; ours would need `SystemParametersInfo`), the
Win11 flyout until step 4, Windows' hover tooltip (there is none), GNOME's per-theme accent
and non-Adwaita themes (Breeze gets Adwaita geometry with our colours), and any WM feature
driven by the WM's own title bar (there no longer is one).

---

## 10. Open questions

1. X11 default: `client` everywhere, as this memo assumes, or `native` until the tiling-WM
   users complain?
2. Is the Win11 flyout worth a Win32 module and a `windows-sys` dependency now, or is it
   polish for later?
3. Does the caption want a session title beside the chips (Windows Terminal shows the
   profile name), or is the lit chip enough?
