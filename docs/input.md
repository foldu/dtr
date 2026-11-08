# Keys, mouse, IME, selection: what the program is told

The account behind the "Keys, mouse, IME, selection" bullets in
[AGENTS.md](../AGENTS.md). The rules are in `AGENTS.md` because they are what stops a
regression; this is why each one is true, and what it looked like when it was not.

The shape of the problem is the same in every section below: the *pane* is the only thing
that knows the terminal's state, and the *UI* is the only thing that sees the keyboard. So
the UI reports what a person did and the pane decides what it means.

## 1. Only the pane knows what a key means

`Command::Key` carries a *stroke* — a logical key, a physical position, modifiers and the
text winit produced — and the pane's `key::Encoder` turns it into bytes after calling
`set_options_from_terminal`. The sequence for one key is not a constant of the key; it is
terminal state:

- `DECCKM` (`?1h`) turns `\e[A` into `\eOA`;
- `DECKPAM` makes the keypad send `=` rather than `>`;
- mode 1036 puts the modifiers in the sequence;
- `modifyOtherKeys` (`CSI > 4 ; 2 m`) does the same by another spelling;
- the kitty keyboard protocol (`CSI > flags u`) is a *stack* of flags, and ctrl+a becomes
  `CSI 97 ; 5 u`, with releases included when the flags ask for them.

A table in the UI cannot be right, because a full-screen program changes these on the fly
and expects the very next keystroke to obey the new ones. That is why the encoder lives on
the pane thread with the terminal it is reading.

## 2. A release is not a second press

winit reports a key release with the same `logical_key` as its press. Encoding both sends
every keystroke twice — the symptom is "typing `ls` shows `lsls`" — so a release encodes to
nothing unless the kitty protocol asked for releases.

The tempting fix is a guard in the UI ("ignore releases"), and it is wrong twice: the
encoder is where the policy belongs, and the policy is not "drop releases" but "drop them
*unless the terminal has asked for them*", which only the pane knows.

## 3. A shifted character is that character, not the key plus a shift bit

`input::fill` passes `consumed_mods` down, so shift+`;` arrives at the program as `;` — not
as `;` with shift held. Without it the sequence is `CSI 59 ; 2 u` and the program applies
the shift itself, which Neovim does for letters only, so `:` and `#` never arrived at all.

winit has no `getConsumedModifiers`, so it is derived: shift counted as consumed when the
text winit produced is one character and differs from the key's unshifted character. A key
with no unshifted character keeps its shift, which is how `<S-CR>` stays expressible.

The *position* decides, not the character — `physical_key`, so a layout that puts `;`
somewhere else still works — and `--keys` assumes US, which is documented where the flag
is. And ctrl or super must not pass `text` through while a plain character must, or accents
and CJK vanish: the text *is* the character when nothing is consuming it.

## 4. …except where the position is a legend

`caps:escape` reports position `CapsLock` with logical key `Escape`. The encoder has no
`CapsLock` bytes and never will, so the keystroke vanished — and it was invisible for a
while because every menu still closed on it: menus read the *logical* key, which was
correct, while the terminal read the position, which had no answer.

`input::key_of` takes the layout's answer for `CapsLock`, `NumLock`, `ScrollLock`, `Fn` and
`FnLock` and keeps the position everywhere else — `DECKPAM`'s `Numpad8` really is a
position. The tests drive a pty in both encodings (`ESC` and `CSI 27 u`), because the bug
only appears in one of them.

## 5. A click is encoded where the modes are

`Command::Mouse` carries pane-relative position, button, modifiers and held state, and
`mouse::Encoder` reads the tracking mode per event: `?9h` sends presses, `?1000h` presses
and releases, `?1002h` and `?1003h` motion, `?1006h` SGR coordinates.

The mode lives in the `!Send` terminal, so `Stats::mouse()` is an atomic published by
`note_modes` — without it the UI does not know whether a click belongs to the program, and
a click in vim starts a *selection* instead of moving the cursor. Shift hands the mouse
back regardless, or `set mouse=a` makes copying impossible; a press outside the pane is
refused while a drag that leaves it is clamped (`pane_pixels`); a wheel notch is buttons 4
and 5, press *and* release (X10 has no other spelling), and shift sends it to scrollback
instead.

**A paste is the program's too**: `pane::pasted` turns breaks into CR and wraps the text in
`ESC[200~ … ESC[201~` when `?2004h` is on, because a bracketed paste is what stops a
multi-line paste from executing its middle lines.

## 6. IME text is not a keystroke, and egui has no CJK

The preedit is a *picture* (`crate::ime`, drawn above the caret) and nothing reaches the pty
until commit. That is what makes a composition cancelable: `KeyStroke::composing` keeps the
romaji out of the terminal, and `set_ime_allowed(true)` keeps the pinyin latin out of it
too — without the second, `ni` would be typed and then replaced by `你` inside the shell.
`set_ime_cursor_area`, called per caret cell and deduplicated, is what puts the candidate
window beside the text rather than in a corner.

egui's own faces are Latin-only and *panic* on a missing glyph (`No glyph for "に" in
Monospace`), so `Look::install` appends `fonts::ui_fallback()` to both families, and the
test checks a kana *and* a Han character. A UI font that cannot draw the text a person is
composing is a crash, not a cosmetic gap.

## 7. A selection is anchored to characters, not to the grid

The pane holds two `TrackedGridRef`s, which follow the page list through scrolling,
pruning and reflow and resolve on *every* publish, so the frame carries a range even after
the text under it has moved. That is the only way a selection survives output arriving
behind it.

`PointSpace::Viewport` resolves but does not *bound* — `y = 13` below a five-row viewport
resolves to a cell — so the bounds check is ours. And once either end leaves the frame the
selection is **dropped, not clamped**: a clamped highlight claims cells it does not cover,
and a copy of it hands back a fragment that is not what was selected. Dropping is the
honest answer, and one click re-anchors.

A press that has not moved is an anchor, so a single click deselects. A word, a line and a
drag are `deliberate`, and that distinction is what lets a click-through clear a selection
without also starting a new one.

## 8. Double- and triple-click are counted, not reported

winit reports clicks, not click *counts*, so `App::count_clicks` is 400 ms and four pixels:
two takes the word and three takes the line, through the core's `select_word` and
`select_line`. Four pixels rather than zero because a hand moves between the two clicks, and
a drag inside the press's own cell must change nothing — jitter must not turn a word into a
single character.

A double-click copies on release, because that is what a terminal's users expect and
because it makes the feature discoverable without a chord.

## 9. A copy cannot use the published frame, and a toast is not a clipboard

Drawing the frame is what *removes* it from the slot, so by the time a copy is handled the
frame is `None`. That shipped, once, as "copied 0 chars" with the selection visible on
screen. `Command::Copy` and `CopySelection` therefore rebuild the text from the terminal,
which is the only thing that holds the cells.

And `ui.ctx().copy_text` is not the system clipboard: it goes through `egui-winit`, whose
clipboard falls back to an in-app buffer when it cannot reach the platform one. So dtr also
writes with `arboard` and holds the handle on `App` for the life of the process — on X11 the
selection owner must stay alive or the text goes away with it.

## 10. OSC 52 writes arrive on the pane thread

A program asking to copy (OSC 52) is answered on the pane thread, which must not touch the
clipboard, so `on_clipboard_write` parks the text in a slot and the UI does the work on the
next frame. `Err` for a non-text MIME is how the program is told the write was refused,
which is better than a silent success it will never see.
