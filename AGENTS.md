# Working on dtr

Notes for anyone — human or agent — changing this repository. The design and
the milestone list live in [PLAN.md](PLAN.md); this file is the stuff that
bites you while you are in there.

## What it is

A serial-first terminal in Rust: `libghostty-vt` for the VT core, egui/wgpu for
the window, one OS thread per pane. `crates/` holds the workspace; `dtr` is the
binary, `dtr-config` the config file, `dtr-complete` the palette's completion
engine, `dtr-store` the redb logbook, `dtr-sshconfig` the ssh config parser.

## Building and testing

Everything goes through the flake; the toolchain is not on `PATH`.

```sh
nix develop .#default --command bash -c 'cargo build -p dtr'
nix develop .#default --command bash -c 'cargo test --workspace'
nix build .#dtr                          # Linux package
nix build .#windows                      # the one-file Windows exe (cross)
nix build .#checks.x86_64-linux.single-exe   # asserts the one-file property
```

If `cargo build` fails to find libghostty's pkg-config file, point it at the
derivation (this is in the devShell hook too, but not in every shell):

```sh
export PKG_CONFIG_PATH=$(nix build .#libghostty-vt.dev --no-link --print-out-paths)/share/pkgconfig
```

Serial work in a sandbox: `socat pty,raw,echo=0,link=/tmp/dtr-serial
EXEC:'some script',pty,raw,echo=0` gives a fake device that can be killed
mid-session to test reconnect. Kill it with `pkill -x socat` — `pkill -f
"socat pty"` matches the shell that is running it and kills your own test.

## The house rules

- **Edit files with the editor tools, not shell redirection.**
- **Never `git add -A`.** Subagents write files (logos, CI, docs) while you are
  working, and `-A` sweeps their work into your commit. Stage explicit paths:
  `git add crates Cargo.toml Cargo.lock flake.nix PLAN.md`. This mistake was
  made twice here.
- **`git add` before `nix build`.** A flake build sees only git-tracked files,
  so a new source file that is untracked produces a confusing "file not found"
  or a phantom compile error about a *different* crate.
- **Screenshots are in-app only** (`--screenshot`), never a compositor grab.
- Commits end with the co-author trailer.

## Things that look like bugs and are not

**`O` and `0` look identical.** egui's default monospace draws a capital O and
a zero with almost the same shape at terminal sizes, so a screen showing `OK`
looks like `0K` in a screenshot. The text is fine. Before touching the render
path, run:

```sh
nix develop .#default --command bash -c 'cargo test -p dtr --bin dtr the_vt_core'
```

It feeds `"OK"` through the VT core and asserts the cells come back as `"O"`
and `"K"`. If that passes, the glyphs are right and you are looking at a font
ambiguity — this cost an hour once. It is also a real reason the M2 font stack
matters: a coding font with a slashed or dotted zero removes the ambiguity.

**`reconnecting…` on a transport that cannot disconnect.** `Stats::connected`
starts true and only the serial reader turns it false. If you add a new
transport, do not leave the flag at its `Default`.

**"No bytes" is not "the reader ended".** The pane keeps the byte-channel
sender alive for reconnects, so `bytes_rx.recv()` never returns `None`. A
disconnect is signalled on its own channel (`closed_rx`). Assuming otherwise
made reconnect silently do nothing.

**A clean frame is not a changed frame.** `publish` skips `Dirty::Clean`; the
first publish after a session starts is forced, or a pane with no output would
show nothing at all.

**`egui::Window` renders nothing inside `Context::run_ui`.** The palette is an
overlay drawn into the same `Ui` for this reason, not by preference.

**A widget's background painted after its text hides the text.** The palette's
selected row was invisible until the highlight was drawn first.

**A solid quad that samples the atlas can vanish silently.** Backgrounds, the
cursor and the selection were solid rectangles sampling one white texel; the
`write_texture` for that byte did not take effect, so all three were invisible
with no validation error and no log line. Solids now carry a negative uv and
take their colour from the instance, and the shader branches. If a rectangle
disappears, suspect the texture path before the blend state.

**A key release carries the same `logical_key` as its press.** Only `text` differs,
and `encode` falls back to the character when `text` is empty — so encoding every
`KeyboardInput` sends each keystroke twice ("typing `ls` shows `lsls`"). `input::encode`
drops anything whose state is not `Pressed`; do not move that check out.

**Feeding `RedrawRequested` to egui creates an infinite redraw loop.** egui replies
"yes, repaint" to that event, we ask winit for another frame, and the app spins at
the refresh rate while looking idle — 133 frames in 1.2 s of a terminal doing
nothing. Every event *except* `RedrawRequested` goes to `egui_state.on_window_event`.

**`Gpu`'s fields drop in declaration order, and it matters.** Anything holding a
texture, buffer or pipeline must drop before the device; the surface must drop
before the window it views; the window must drop last. Wrong order segfaults on
close instead of failing loudly.

**Panes share one instance buffer, and queue writes land before submission.** A
per-pane draw would rewrite the buffer between panes and the last pane would win;
that is why `render::prepare` takes every visible pane at once. Cells outside a
pane's rectangle are simply not emitted rather than scissored.

**A copy cannot use the published frame, because drawing it is what removes it.**
The UI takes the frame out of the pane's slot to render it, so a copy that reads
that slot gets `None` and copies the empty string — which shipped as "copied 0
chars" with a selection on screen. `Command::Copy`/`CopyRange` rebuild the text
from the terminal at copy time instead; the pane is the only place that still
holds the cells.

**OSC 52 writes arrive on the pane thread, not the UI.** The VT core calls
`Terminal::on_clipboard_write` synchronously while parsing bytes, so the handler
only parks the text in the pane's `copied` slot; the UI does the clipboard work
and raises the toast. Replying `Err` for a non-text MIME is what tells the program
the write was refused — returning without replying denies it silently.

## Profiling

`./scripts/flamegraph.sh --demo` builds the `profiling` profile (release + debug
info) and samples a real window. It uses `perf` through `cargo flamegraph` when
the kernel allows recording, and otherwise `dtr --cpu-profile <path>`, which
samples itself and needs no privileges. `--cpu-profile /tmp/x.svg` always prints
a text self-time report; the `.svg` suffix adds the graph. `--exit-after MS`
closes the window by itself, which is how shutdown gets exercised without a
window manager.

**The low-power adapter is the integrated GPU.** `PowerPreference::LowPower` hands
you the iGPU on a machine with a discrete card, which is the wrong answer for a
terminal someone stares at. `--gpu <substring>` (or `DTR_GPU`) picks by adapter
name; `-v` logs the adapter and every adapter seen.

## Gotchas that are real

| Symptom | Cause |
| --- | --- |
| `could not find native static library 'ghostty-vt-static.lib'` | `PKG_CONFIG_PATH` from the devShell wins over `PKG_CONFIG_LIBDIR`; set `PKG_CONFIG_PATH=""` in the Windows build |
| `unable to find dynamic system library "synchronization"` | Windows needs an empty `libsynchronization.a` stub on the link path |
| `libudev-sys` build failure | `serialport` enumerates through libudev on Linux; udev's `.pc` must be on `PKG_CONFIG_PATH` in both the package and the devShell |
| `The wayland library could not be loaded` | winit/wgpu `dlopen` their backends, so the wrapper needs `LD_LIBRARY_PATH`, not rpath |
| `-Werror=date-time` from mimalloc | zig's cc enables it; `CFLAGS_x86_64_pc_windows_gnu="-Wno-error=date-time"` |
| Wine: FXC shader error building egui | Wine finds dx12 first and cannot compile it; `WGPU_BACKEND=vulkan` |
| `Dropped TexturesDelta with 1 unapplied deltas` | egui panics unless `textures_delta.clear()` is called |
| Undefined `ghostty_terminal_mode_get` | crates.io `libghostty-vt 0.2.2` is broken; the git rev in `Cargo.toml` is the pinned one |
| `Terminal` cannot cross threads | it is `!Send`; each pane owns its own thread and `LocalSet` |
| fish renders nothing, or writes nothing | it needs writable `XDG_*` dirs in the sandbox, and the terminal must reply on `on_pty_write` (a shell waits for its answer) |
| `cargo test` does not refresh the binary | build first; `target/debug/dtr` is what you run, tests do not rebuild it |
| A removed pane takes two panes with it | the recursive remove returned "delete me" and the grandparent deleted the collapsed subtree; only the direct parent may collapse |
| Line height is far too small | swash reports `descent` as a *positive* distance below the baseline; negating it halves the cell |

## Where to look

- `PLAN.md` — design, milestones, spike findings, and the reasoning behind
  decisions that were reversed.
- `docs/sftp.md` — measured decision memo for the SFTP idea.
- `docs/themes.md` — which terminal theme formats dtr could read.
- `crates/dtr/src/pane.rs` — transports, publish, reconnect, modem, paste.
- `crates/dtr/src/app.rs` — the event loop, keys, and the palette glue.
- `crates/dtr/src/layout.rs` — the split tree. It knows nothing about terminals;
  a leaf is an id, and the tab's map decides what that id is.
- `crates/dtr/src/render.rs` — glyph atlas and the instanced grid pass.
