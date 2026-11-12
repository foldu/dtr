# What Ghostty has that dtr does not

Status: 2026-11-12, against the Ghostty source the pinned `libghostty-vt` is built
from (1.3.2-dev, vendored under
`target/debug/build/libghostty-vt-sys-*/out/ghostty-src`, paths below relative to
it) and against dtr after the compio port. A work list, ordered at the end — not
a promise, and not a comparison of feature *counts*. Ghostty is a desktop
terminal for people who run shells; dtr is a serial console with a logbook, a
macro language and a file pane. Half of Ghostty's configuration is about a window
manager dtr does not have.

## 1. The two are the same VT and a different frontend

dtr links the VT library that was extracted from Ghostty, so parsing, modes,
scrollback, selection, search-in-core, and key/mouse/clipboard encoding are
literally the same code. Everything in this document is **frontend**: what the
window does with what the core parsed.

That makes the comparison unusually mechanical, because the core publishes the
list of what a frontend owes it — a struct of callbacks, `Effects`
(`src/terminal/stream_terminal.zig:154`), whose doc comments say what each one is
for. Ghostty's frontends fill nearly all of them. dtr registered **two** —
`on_pty_write` and `on_clipboard_write` — until 2026-11-12, when the
escape-sequence sweep took it to eight (`on_size`, `on_xtversion`,
`on_color_scheme`, `on_render_hold`, `on_bell`, `on_clipboard_read` are the new
ones); the table in §2 says what still answers nothing, and why.

The gaps sort into four kinds, and they are not equally excusable:

| kind | example | why it is a gap rather than a difference |
|---|---|---|
| a dropped duty | `OSC 52 ; c ; ?` (clipboard read) | a *program* asked and got silence. dtr cannot even say "no" |
| something the core parses and nothing draws | SGR 4 underline | the bytes arrive, the cell knows, the picture does not show it |
| a window feature | search, quick terminal, fullscreen | dtr's own UI is missing it; the core is ready |
| a configuration knob | `mouse-scroll-multiplier` | a behaviour exists and cannot be tuned |

Being serial-first changes the *value* of these, not their honesty: a progress
report (OSC 9;4) is worth more on a two-minute flash than on a laptop, and a
clipboard read is worth less on a machine with no clipboard.

## 2. The duties the core hands the embedder

Measured, not read: a probe that creates a terminal with only `on_pty_write`
installed — dtr's arrangement — and feeds it each query. The snippet is in §2.1.
"silence" means the program gets no bytes at all, which is indistinguishable from
a terminal that does not understand the question.

| what the program sends | Ghostty | dtr today | where it would go | size |
|---|---|---|---|---|
| `CSI c`, `CSI > c`, `CSI = c` — device attributes | answers | **answers**, e.g. `?62;22c` and `>1;0;0c` | — (the C layer fills a default) | — |
| `CSI 5 n`, `CSI 6 n`, `CSI ? Ps $ p` — status, cursor, DECRQM | answers | **answers** | — | — |
| `OSC 4 ; Ps ; ?` — palette query | answers from the theme | **answers** | — | — |
| `OSC 10/11/12 ; ?` — dynamic colours | answers | **answers**, because the theme sets them (`theme.rs:96-101`) | — | — |
| `XTGETTCAP` capability keys (`colors`, `RGB`, …) | answers | **answers** from Ghostty's static map | — | — |
| `XTGETTCAP TN` — "which terminfo entry are you?" | `xterm-ghostty` | **answers** `xterm-ghostty`: the core refuses to claim an entry unless the embedder names it, and dtr ships that entry | — | done |
| `CSI > 0 q` — XTVERSION | `ghostty 1.3.2` | **`dtr <version>`**, because `on_xtversion` overrides the library's own answer | — | done |
| `CSI 14/16/18/19 t` — window and **cell** size in pixels | answers from the surface | **answers** from the cell the last `Command::Resize` measured; silent only before the first resize, when no cell has been measured | — | done |
| `CSI ? 2048 h` — in-band resize reports | reports on every resize | **answers**: the report carries the new size, flushed on the resize itself rather than waiting for the program to type | — | done |
| `CSI ? 996 n` — "is the terminal light or dark?" | answers from the theme | **answers** from the theme's background (Rec. 601 luma); no theme means dark | — | done |
| `OSC 52 ; c ; ?` — clipboard **read** | mediated: `clipboard-read = ask/allow/deny` | **answers**, from the clipboard the UI last read, or **refuses** with an empty selection — `terminal.clipboard_read`, off by default, because dtr has no prompt to ask with | — | done |
| `BEL` | `bell-features`: visual, window attention, audio | **flashes** the pane for 140 ms and never makes a sound; a bell in a tab nobody is looking at asks the window for attention instead | — | done (visual only) |
| `OSC 9 ; …`, `OSC 777` — desktop notification | system notification | **shown on the desktop's channel**: `org.freedesktop.Notifications` when the machine has a daemon (`crate::notify`, behind `[notifications] os`, on by default), and dtr's own toast when it does not. Windows gets only the toast for now — a toast there needs a Start Menu shortcut carrying an AppUserModelID, and writing one is a decision, not a fallback. The window asks for attention when the notice came from a background tab | — | done (Linux; Windows in-window) |
| `OSC 9 ; 4 ; …` — progress | tab/taskbar progress | **drawn** as a stripe on the tab chip: a fraction when the program gave one, a sliding band when it is working with no number | — | done |
| `OSC 7501` — program status (working, done, waiting) | title/status integration, and the protocol's support query is answered | **drawn** as the same stripe: per-record states are folded into one answer (error beats blocked beats working), and blocked or failed fills it in the palette's danger colour. The support query is answered because the callback is registered. No warning slot, so a blocked build and a failed one look alike | — | done |
| `CSI ? 2026 h … l` — synchronized output | holds the frame and captures it at the start of the hold | **holds**: the burst publish and its gate timer are suppressed until the release, which publishes in the batch that carries it; a scroll or a keystroke still paints | — | done |
| `OSC 11 ; …` — **set** the default background | repaints the window background | **repaints**: the pane paints its own default background first in the grid pass, the colours come from the terminal's *effective* `bg_color`/`fg_color`, and the frame gate compares them so a set with no cell change still publishes | — | done |
| `CSI 8 ; rows ; cols t` — resize the window | may resize the surface (`vt-window-resize-allowed`) | **honoured behind `terminal.window_resize`** (off by default): the UI resizes by the delta in cells, and keeps a request that arrives before the first frame has measured one | — | done |
| `CSI ? 1004 h` — report focus | sends `CSI I`/`CSI O` | **answers** on `WindowEvent::Focused`, for the panes of the tab on screen, and only to a program that enabled the mode | — | done |
| `OSC 22 ; …` — pointer shape | changes the mouse cursor (a text beam over text) | none, and **cannot** be wired yet: the bindings expose no pointer-shape callback | the binding, then the surface | S |
| `CSI ? 2031 h`, `? 2033 h` — report scheme and visibility changes | tells the program when the theme flips or the window hides | `?2031` needs nothing while a pane's theme cannot change (it is applied once at spawn); `?2033` needs a `visible` setter the C surface does not expose, though the core would send the report itself | the binding, then a `Command` | S |
| `OSC 133` — prompt marks | marks, jump-to-prompt, selection, click-to-move | **works**, but through dtr's own scanner in front of the core (`pane.rs:2522`), not the callback | — | — |

Two consequences worth naming, because they are not visible in the table:

- **The size reports are how a program learns the cell.** Anything that places
  pixels in the grid — image viewers, `chafa`, a TUI preview, neovim's image
  plugins — asks `CSI 16 t`; without an answer it has to guess from the font
  size, and guesses wrong on a HiDPI window or a resized pane. dtr answers now.
- **Synchronized output is not a nicety on a slow transport.** A 9600-baud
  screen is written over hundreds of milliseconds; framing that with
  `?2026h`/`?2026l` is the only way a program can ask dtr to show the previous
  picture until the new one is complete. The core offered the hook and dtr
  painted on a timer; the hold is honoured now, which matters most on exactly
  the transport this terminal exists for.

### 2.1 The probe

```rust
// crates/dtr/tests/probe.rs — `cargo test -p dtr --test probe -- --nocapture`
use std::{cell::RefCell, rc::Rc};
use libghostty_vt::Terminal;

fn probe(input: &[u8]) -> String {
    let mut term = Terminal::new(10, 3).unwrap();
    let out: Rc<RefCell<Vec<u8>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&out);
    term.on_pty_write(move |_, data| sink.borrow_mut().extend_from_slice(data)).unwrap();
    term.vt_write(input);
    String::from_utf8_lossy(&out.borrow()).into_owned()
}
```

Answers change when the core is bumped, which is the point of recording them:
`XTGETTCAP TN` and the cell-size reports are silent in *this* pin, and the
frontend is what has to fix that.

## 3. What the renderer does not draw

The grid painter emits two kinds of instance, solids and glyphs, and reads five
flags off a cell: bold, italic, faint, concealed, and its colours
(`crates/dtr/src/pane.rs:92`, `crates/dtr/src/render.rs:969-1002`).

| gap | Ghostty | dtr today | size |
|---|---|---|---|
| **underline** | five styles (single, double, curly, dotted, dashed), a colour (SGR 58) and adjustable position and thickness | **not drawn at all**: the core parses SGR 4, 21 and 58 into the cell's style, `fill_cells` copies none of it, and `Cell` has no field to carry it | S–M |
| **strikethrough** | SGR 9, with position and thickness knobs | not drawn | S |
| per-cell shaping | shapes a cell's grapheme cluster, so `❤️` takes its colour form, a flag is one glyph and a combining mark is shaped with its base | every codepoint of the cluster is placed at the cell's origin, one glyph each: `e` + U+0301 lands right by luck, a flag or a ZWJ family draws blank (`docs/cjk.md` §4, gap 9) | L |
| ligatures | `font-feature`, `font-variation`, shaping across a run | one glyph per character, no features | L |
| synthetic bold and italic | `font-synthetic-style`: embolden, oblique, or neither | asks the family for a bold face; a family that has none answers with its regular face, and bold is invisible | S |
| font thickening, variable axes | `font-thicken`, `font-variation*` | none | M |
| codepoint remapping | `font-codepoint-map` — "draw this range from that family" | `[font] fallback` is a list of families, not a mapping | S |
| minimum contrast | `minimum-contrast` lifts unreadable fg/bg pairs | none; a theme with poor contrast stays poor | S |
| cell shape adjustment | `adjust-cell-width/height`, baseline, box thickness | none; the cell is the font's | S |
| inline images | kitty graphics protocol, fully: transmit/display/delete, PNG and raw, zlib, chunking, animations (Ghostty advertises no sixel, and `OSC 1337` `File=` is parsed and rejected) | none drawn: the core parses `_G` and answers its queries — measured, `supports_kitty_graphics()` is true in dtr's build — and the grid painter has no image layer, so the images are stored and dropped | L |
| background | opacity, blur, image, image fit/position/repeat | a flat colour from the theme | M |
| unfocused splits | `unfocused-split-opacity` / `-fill` | every pane is equally bright | S |
| box drawing, blocks, braille | a sprite face (the one dtr ported) | **the same**, `crates/dtr/src/sprite.rs` | — |

The first two rows are the ones to fix before anything else in this document:
underlines are how `man`, `diff`, `git`, `fish` and half of the TUI world signal
emphasis, and dtr currently throws the signal away — a styled link and a plain
one look identical. Strikethrough is the same bug with a smaller blast radius.

## 4. What the window cannot do

| gap | Ghostty | dtr today | size |
|---|---|---|---|
| **search** | `ctrl-shift-f`, four colour options, `navigate_search`, matches in the scrollback | none. The core has the engine (`libghostty_vt::search::Search`, with `set_needle`, `select_next`, `matches`); nothing in `app.rs` calls it | L |
| **quick terminal** | a dropdown window on a global hotkey, seven options | none | M–L |
| fullscreen | `toggle_fullscreen` | none; `set_fullscreen` is never called | S |
| more than one window | `new_window`, `window-save-state`, per-window font size | one window; the `+` is a tab (`app.rs:4101` is the only `create_window`) | M |
| session restore | `window-save-state`, working directories and tabs | none | M |
| confirm close | `confirm-close-surface`: a running command asks first | none — closing a tab kills the build silently | S–M |
| undo | `undo` / `redo` for a closed tab, `undo-timeout` | none | M |
| command finished | `notify-on-command-finish`, `bell-features`, OSC 133 exit status | none | M |
| tab verbs | move, `goto_tab N`, tab bar hidden or at the bottom, inherit cwd | `alt+digit` picks a tab by its number and `ctrl-tab` cycles, but chips cannot be reordered or moved between windows, and the strip is always on top (`docs/panes.md`) | S–M |
| split verbs | equalize, swap, move between tabs/windows, divider colour, preserve zoom | split, focus, resize, zoom (`docs/panes.md`) | S–M |
| padding | `window-padding-x/y`, balanced, coloured | a fixed four-point gap (`ui.rs` `GRID_GAP`), not configurable | S |
| cursor | a block **inverts its cell** and draws the glyph in `cursor-text`; plus opacity, blink and click-to-move | **fixed**: a block is painted with the backgrounds and its glyph in the theme's `cursor-text` (its background when the theme has none) — measured, `ABC` with the cursor moved left shows the `C` in the background colour on the cursor block, and `啊` inverts across both cells. What is left is the options: no blink, no opacity, no click-to-move | S |
| font size at runtime | four actions, plus a per-window size | a row in the settings menu | S |
| selection policy | clear-on-typing, clear-on-copy, word characters, `select_all` | copy-on-select **is** done (`app.rs:2487`), unconditionally; the selection survives typing, the word set is the core's default, and there is no select-all (`Terminal::select_all` is in the binding) | S each |
| clipboard policy | paste protection (a 100-line paste asks), trim trailing spaces, codepoint map, read/write policy | bracketed paste only; no protection, no trim knobs | S–M |
| mouse | hide-while-typing, scroll multiplier, right/middle-click actions | shift hands the mouse back and the wheel is one notch per event; no multiplier, no right/middle-click policy outside links | S |
| focus reporting (`?1004`) | sends `CSI I` / `CSI O` as the window gains and loses focus | nothing: the binding has `focus::Event`, but no `WindowEvent::Focused` handler encodes it, so a program that asked for focus events never gets one | S |
| shell integration | injected automatically for bash, zsh, fish, elvish, nushell; `ssh-env`, `ssh-terminfo` for remote hosts | relies on the shell already emitting OSC 133 (fish does; bash and zsh need a snippet) and copies only the terminfo entry (`docs/sessions.md`) | M |
| wait after command | `wait-after-command` keeps a dead pane on screen | the tab closes (`app.rs:1285`) | S |

Two of these rows are macOS-only in Ghostty — `undo` and `window-save-state`
(session restore) — so a Linux Ghostty does not have them either. They are listed
because dtr is one codebase on two platforms and could have them on both.

## 5. What the configuration cannot say

Ghostty 1.3.2 exposes 213 options (`src/config/Config.zig`). dtr's whole config
is `keys`, `serial.auto_reconnect`, `font.{size,family,path,fallback}`,
`window.chrome`, `ui.{widgets,scheme,accent}`, `theme`, `terminal.scrollback_limit`
(`crates/dtr-config/src/lib.rs:26-40`).

That difference is mostly **a decision, not a gap** (see §6). The knobs in it
that are a gap are the ones §3 and §4 name — a behaviour that exists with no
control, or a control for a behaviour that does not exist. The ones worth copying
first, because they are one line of plumbing into something that already works:
`cursor-style-blink`, `mouse-scroll-multiplier`, `mouse-hide-while-typing`,
`selection-clear-on-typing`, `confirm-close-surface`,
`notify-on-command-finish`, `window-padding-*`, `theme = light:…,dark:…` (dtr
already reads Ghostty theme files, so the pair is the natural next step —
`docs/themes.md`).

The ones that are not worth copying: `macos-*` (17), `gtk-*` (13), `x11-*`,
`linux-cgroup*`, `auto-update*`, `custom-shader`, `window-vsync` (egui already
presents on vsync), `async-backend`.

## 6. Decisions, not gaps

Named so that this list is not read as "we are 40 features behind":

- **A small config file is the point.** dtr's config is a handful of things a
  person changes; Ghostty's is a compatibility surface with every other
  terminal's spelling. Growing it past its use is how a settings menu becomes a
  form (`docs/ui.md`).
- **One window, many tabs.** dtr's `+` and its tab strip are the window model;
  a second window is a feature of a desktop terminal, not of a serial console.
  §4 lists it as a gap because it *is* one for someone who wants two monitors.
- **No GTK, no macOS apprt, no custom shaders, no inspector, no crash reporter,
  no auto-updater.** The window is egui/wgpu, the package is nix, and the
  platform chrome is `[window] chrome` (`docs/chrome.md`).
- **Scrollback is capped at 100 MB** (`AGENTS.md`); Ghostty defaults to
  unlimited. That is a decision about what a terminal on a 4 GB box does.
- **Kitty graphics are not refused, they are unscheduled.** A serial console
  showing a board's boot logo is a real use; it is also a renderer of its own,
  which is why it is last.

## 7. What dtr has that Ghostty does not

So the comparison is not one-directional: serial transports with modem lines
(DTR/RTS/BREAK), per-port baud and CR translation memory, auto-reconnect; the
redb logbook and the Tera Term macro interpreter (`dtr-ttl`); a file pane over a
VFS with local, in-memory and ssh backends and its own SFTP client; ssh hosts
from `~/.ssh/config` in the `+` menu; the palette, the keyboard-map card, the
capture pager over OSC 133 blocks; four skin presets with a live settings
preview.

## 8. The order worth doing them in

1. **Underline and strikethrough** (S). Two booleans on `Cell`, two solids per
   cell in the same instance buffer, positions from `Metrics`. The largest
   visible gap per line of code in this document. **Not done**: still the
   biggest hole, and still the first thing to do.
2. **The reply sweep** — *done 2026-11-12*: `set_terminfo_name("xterm-ghostty")`,
   `on_size` (14/16/18/19 t and mode 2048), `on_color_scheme` (`?996 n`),
   `on_xtversion` (`dtr <version>`), `on_clipboard_read`, focus reporting
   (`CSI I`/`CSI O` on `WindowEvent::Focused`) and `on_resize_window` behind
   `terminal.window_resize`.
3. **The window background ignores the terminal's default** — *done*: the pane
   paints its own background and the colours are read from the terminal. The
   cursor half went with it (a block inverts its cell).
4. **Synchronized output** — *done*: the burst publish and its gate timer are
   held until the release.
5. **Bell, notifications, command finished** — *done*: the bell flashes the pane
   (and asks the window for attention, never a sound), `OSC 9`/`777` are a toast,
   and `OSC 9;4`/`OSC 7501` are a stripe on the tab chip.
6. **Search** (L). The core has the search (`libghostty_vt::search::Search`); the
   work is the overlay, the six bindings and the colours.
7. **Confirm close, undo, select-all, font size keys, fullscreen** (S–M).
8. **Shaping** (L, `docs/cjk.md` §4, gap 9) — the emoji and combining-mark hole,
   shared with the ligature question.
9. **Quick terminal** (M–L) and **shell integration injection** (M), in either
   order.
10. **Kitty graphics** (L) when there is a use case, not before.

The undone items in `docs/panes.md` (the exit bug, split-before, swap) and
`docs/ttl-gaps.md` are *not* repeated here: they are gaps against a multiplexer
and against Tera Term, not against Ghostty.
