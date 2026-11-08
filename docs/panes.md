# What the pane layer does not do yet

A snapshot of the gap between dtr's pane layer and a multiplexer's, so that "splits
work" is not mistaken for "the pane layer is finished". The session layer — detach and
attach, a server that outlives the window, a wire protocol — is a different problem and
is named at the end, so the boundary is explicit rather than implied.

What exists: a split tree of leaf ids (`crates/dtr/src/layout.rs`) with a pane per leaf
(`Pane::Terminal(Session) | Pane::Files(Explorer)`), per-tab focus, a draggable divider,
keyboard focus/resize/zoom, and a pty refit from the same geometry that draws the panes.
The list below is what a person using it as a mux would reach for next, roughly in the
order it is worth doing.

## 1. A pane that exits takes its tab with it (a bug, not a gap)

Measured, not inferred: split a tab, then `exit` in one of the two shells, and the
window goes away at 1.1 s instead of staying with the surviving pane.

```
$ dtr --shell "bash --norc --noprofile" --send "sleep 1; exit\n" --keys "ctrl-|"
INFO  dtr::pane: session ended code=0
DEBUG dtr::app: session ended; closing its tab tab=0
```

`App::reap_ended` collects *tabs* that contain an ended pane and calls `App::close_tab`,
which was right when a tab held exactly one session and is wrong now: it closes every
sibling. (The same pass logs twice because the tab is still there on the next frame —
`close_tab` sets `quit` for the last tab rather than removing it — which is harmless but
means the function is idempotent by accident.)

The fix is the pane-level version of the same rule: a tab removes the ended leaf through
`Layout::remove` (which already collapses a split into its sibling), focuses the nearest
remaining pane rather than the first, and only the last pane takes the tab — and only the
last tab takes the window. `Tab::remove(LeafId)` is where that belongs, so `reap_ended`
and `close_active` cannot disagree about it.

## 2. There is no way to move or swap a pane

No `swap-pane`, no `move-pane` between tabs, no `break-pane` into a new tab. The tree is
the only thing that would change: a swap exchanges two leaf *ids* and leaves the shape and
the ratios alone; a move is `Layout::detach(leaf)` (the same walk as `remove`, but
returning the leaf and letting the caller decide) plus an `attach` beside a target leaf in
another tab, with the `Pane` entry carried across in `Tab::leaves` — ids are monotonic and
never reused, so nothing else has to be renumbered.

The cheap version is a **swap with the next pane in tree order**, which needs no picker and
covers most of the value ("these two are the wrong way round"); moving between tabs needs
somewhere to say where it went, which is item 5. What proves either: the multiset of
rectangles is unchanged by a swap, the ids trade places in `leaves()`, and a detached leaf
leaves its sibling holding the parent's area.

## 3. A split only ever opens to the right or below

`Layout::split(target, axis, id, new_first)` already takes the side; every caller in
`App` passes `false`. `split-before-horizontal` and `split-before-vertical` are two
actions, two chords and two rows in the `panes` group of the keyboard map — the asymmetry
is visible the moment somebody wants a pane on the left.

## 4. Closing a pane picks the first pane, and there is no way back

`App::close_active` focuses `tab.layout.leaves().first()`. The neighbour is what a hand
expects (the one that just grew into the space), and `last-pane` — "go back to where I
was" — has no state at all: `Tab` keeps `focus` and nothing else. Both are small: a
`previous_focus` field updated wherever `focus` is written (eight places, all in `App`),
and the close path choosing by geometry, which `layout::neighbour` already answers once
the closed rect is known.

## 5. No pane numbers, so no "select pane 2"

Focus is moved by arrow or by pointer; there is no `select-pane -t :.2`. `alt+digit`
already selects a *tab* by number, so pane numbers need a different modifier, and a number
nobody can see is not a name: the numbers have to be drawn first (item 6). Until then a
picker for item 2 has nothing to point with either.

## 6. The divider has no picture, and a pane has no label

The gap between two panes is three points of background. The resize cursor over it is the
only affordance that a border can be grabbed, and the grab is a seven-point strip; a pane
has no title of its own (the tab is named by the *focused* pane, so the other one is
silent) and no focus marker beyond the block cursor.

The smallest version that helps: paint the divider on hover — a hairline in
`palette.focus_outer` across the gap, drawn in the frame that already knows the pointer
and the geometry. `dtr-widgets` first, per the house rule: a `border`/`divider` token and a
painted `divider` primitive, not a colour in a screen. The same primitive then carries a
focused-pane hairline and, later, a pane number and a title, which is why it is worth doing
before items 5 and 7 rather than after.

## 7. Nothing is broadcast

No `synchronize-panes`: keys go to the focused pane only. For a serial-first terminal this
is the feature with an obvious use — two ports, one command, one firmware — and it is
dangerous for exactly the same reason: typing into a pane you are not looking at must be
visible, or it is a footgun. So it depends on item 6 (or a word in the status strip).
Mechanically it is small: a flag on `Tab`, and `App::on_key` sending the same `Command::Key`
to every `Pane::Terminal` in the tab — the encoder is per pane, so each terminal still
decides its own bytes, which is the only correct way to do it.

## 8. A hidden tab keeps the grid it had

`App::fit_panes` walks the *active* tab. A background tab's pty keeps the size it had when
it was last shown, so a program there that prints a ruler (`stty size`, a full-screen app
that has not reflowed yet) sees the window as it was, and the first frame after switching
is the one that fixes it. Sizing every tab costs one `Resize` command per hidden pane per
window resize, which is the price a mux pays to be able to say a window has one size.

## 9. Panes are invisible to a screen reader

The grid is drawn on the GPU; only egui widgets declare accessibility nodes
(`Response::widget_info`). A split is not announced, focus movement is not announced, and
the terminal's own text is not a node at all. This is terminal-wide rather than pane-wide,
which is why it is last — but it is the reason "keyboard driven" is not the same as
"accessible".

## The order worth doing it in

1. **The exit bug** (1). It is a bug: a working pane disappears because a sibling exited.
2. **Split before** (3) and **close focuses the neighbour, plus `last-pane`** (4): a
   handful of lines each, and each removes an asymmetry that is felt immediately.
3. **Swap and move** (2). The first missing *verb*; `detach`/`attach` are the tree
   primitives the rest of the list would otherwise grow its own copy of.
4. **A drawn divider** (6), then **broadcast** (7) and **numbers** (5), which both need
   somewhere to be visible.
5. **Hidden tabs sized** (8).
6. **Accessibility** (9), when there is a story for the grid itself.

## What is not here on purpose

The session multiplexer: detach and attach, a process that outlives the window, a client
protocol, control mode, layout persistence across restarts, more than one window, more than
one client. Those are one design decision — where the terminal state lives — and it is not
a pane question. Naming it here is so the list above is not read as "the mux is nearly
done": it is the *pane* half that is nearly done.
