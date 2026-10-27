# dtr — implementation plan

A serial-first terminal for Windows and Linux. Rust + `libghostty-vt` + egui, built with Nix flakes.

Status: draft for review · 2026-10-27

---

## 0. MVP definition

**The MVP is a serial console that displays something and accepts input.**

```
dtr --port /dev/ttyUSB0 --baud 115200
```

- Opens a serial port (Linux `/dev/tty*`, Windows `COM*`) with 8N1 and configurable baud.
- Reads bytes on a thread, feeds them to `libghostty-vt`, draws the resulting grid.
- Sends keystrokes back to the port, encoded by libghostty's key encoder.
- Shows a status line: port, baud, RX/TX byte counts, DTR/RTS.
- Can screenshot itself (`--screenshot`, `F12`) so the UI is inspectable without a human.
- Builds and runs natively on Linux; cross-compiles to `dtr.exe`.

**Non-goals for MVP:** tabs, splits, multiple windows, ConPTY/local shells, config files, themes,
font shaping, selection, scrollback UI, search, IME, any visual polish. A 80×24 grid in one window,
configured entirely by clap flags.

Rationale: serial is the differentiator and the reason this exists; everything else is chrome
that can be added around a working byte pipeline. "Pretty" is a later milestone, and the MVP
renderer is deliberately a throwaway layer behind a trait.

---

## 1. Decisions

| # | Decision | Why |
|---|---|---|
| D1 | **`libghostty-vt` only**, not full `libghostty` | Zero-dependency VT core; full lib brings font + GL renderer with non-Windows backends we don't want |
| D2 | **Serial first, ConPTY second** | User priority; serial is also the better first transport (no process/exit/ConPTY semantics) |
| D3 | **`Transport` trait from day one** | Serial, ConPTY, and later TCP/SSH differ in resize, exit, cwd — the pane must not assume PTY |
| D4 | **egui for chrome, egui painter for the grid in M0** | Zero font/atlas work to reach "displays something"; small serial grids (80×24) are well within egui's painter budget |
| D5 | **Grid renderer behind a `GridRenderer` trait; wgpu cell renderer in M2** | MVP renderer is disposable without being a dead end |
| D6 | **Own `winit` loop, not `eframe`** | egui's `Modifiers` has no left/right distinction; kitty keyboard protocol needs it. Also need redraw-on-bytes |
| D7 | **Pane owns `Terminal` on its own thread** | libghostty types are `!Send`/`!Sync`; upstream recommends a dedicated emulation thread |
| D8 | **Screenshot is an M0 feature, not a debug hack** | It is the feedback loop for agent-assisted UI work |
| D9 | **`x86_64-pc-windows-gnu`** | Self-contained static mingw link, no MSVC SDK; matches `libghostty-vt-sys` target map |
| D10 | **Nix provides `libghostty-vt` via pkg-config**, not `vendored` | One C library per target, built once, no zig on the Rust build path |
| D11 | **Single executable, Windows only** | See §7. Windows: literally one `.exe`. Linux ships one ELF with normal system library deps |
| D12 | **Static link only; dual-license MIT / Unlicense** | libghostty is MIT, deps are permissive; see §8 for the obligation list |
| D13 | **Clap for all configuration; no config UI in M0** | Fastest path to a usable thing; the CLI is also the agent's interface |
| D14 | **Remember serial parameters per port** | Re-opening a device shouldn't mean retyping `--baud 57600`; persisted in redb, CLI wins |
| D15 | **redb for persistence** | ACID, pure Rust, one file, stable format, multi-process; buys the M1 logbook without a rewrite |
| D16 | **tokio everywhere for I/O; emulation stays on a LocalSet** | Serial via `tokio-serial`; the `!Send` `Terminal` forces one current-thread runtime per pane (see §2.4) |
| D17 | **Pin the Rust wrapper to git `9ede35b`, not crates.io 0.2.2** | 0.2.2's pregenerated bindings call three C functions that do not exist at its own pinned ghostty commit (§12) |
| D18 | **Name: `dtr`** | Data **T**erminal **R**eady — the RS-232 line a terminal asserts to announce itself. Three characters, technically exact, and the joke only lands for the audience |

Pinned upstream: ghostty commit **`521cf2c5d30bd48028ca61546c64e7913764073d`** — the commit
`libghostty-vt-sys` 0.2.2's bindings were generated against. Do **not** float this: the generated
FFI is ABI-specific to that revision.

---

## 2. Architecture

```
main thread (winit + egui)
   │  commands  (tokio mpsc)
   ▼                                  ▲  FrameSnapshot  (crossbeam)
pane thread — one per session, current-thread tokio runtime + LocalSet
   ├─ libghostty_vt::Terminal + RenderState   (!Send, never on a work-stealing pool)
   ├─ SerialStream (tokio-serial): read/write, select!, timeouts, cancellation
   └─ reconnect backoff, break, modem-line polling
```

### 2.1 Traits

```rust
/// A byte pipe that a pane drives. Serial today; ConPTY, tcp, ssh later.
/// Async because the I/O is; the futures are `!Send`-friendly so the pane can
/// keep them on its LocalSet next to the `!Send` Terminal.
pub trait Transport {
    async fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    async fn write_all(&mut self, bytes: &[u8]) -> io::Result<()>;
    async fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()>;
    fn capabilities(&self) -> Caps;
}

/// What the UI is allowed to assume about a session.
pub struct Caps {
    pub can_resize: bool,      // serial: false, pty: true
    pub has_exit_code: bool,   // serial: false
    pub has_cwd: bool,         // serial: false
    pub has_modem_lines: bool, // serial: true
}
```

`async fn` in traits (Rust 1.75+, we are on 1.99) keeps this dyn-free; M0 has exactly one concrete
transport plus a replay transport, selected by enum. If dynamic dispatch is ever needed, box the
future (`Pin<Box<dyn Future<Output = …> + '_>>`) rather than reaching for `#[async_trait]`, which
would add `Send` bounds and break the LocalSet.

### 2.2 Workspace layout

A virtual workspace at the root; every crate lives under `crates/`.

```
Cargo.toml            // [workspace] + shared dependencies and metadata
crates/
  dtr/                // the binary: CLI, wiring, window
    Cargo.toml
    src/…
  dtr-sshconfig/      // ~/.ssh/config parser, for the future host picker
```

`dtr-sshconfig` exists already (landed ahead of its feature): OpenSSH config grammar, `Include`
with globs and cycle/depth limits, `*`/`?`/negation host patterns, and first-value-wins
resolution. It is pure parsing with no network or async, so it can be tested and reviewed
independently of the terminal.

One crate for now is deliberate. The split below is where it should go as boundaries earn it, not
a target to hit up front:

| future crate | contents | why it separates |
|---|---|---|
| `dtr` | clap, winit/egui wiring, window | the only thing that needs to be a binary |
| `dtr-term` | libghostty-vt wrapper, pane actor, `FrameSnapshot` | owns the `unsafe` FFI boundary and the `!Send` rule |
| `dtr-transport` | `Transport` trait, serial, replay | the whole tokio surface; testable without a window |
| `dtr-store` | redb schema, migrations | persistence without pulling in egui |
| `dtr-ui` | chrome widgets, `GridRenderer` | the part that changes when only the looks change |

Good split criterion: extract a crate when it can be tested without a window, or when it owns a
rule (unsafe, `!Send`, single-writer) that is better enforced by a dependency edge than by
discipline.

### 2.3 Module tree (inside `crates/dtr/src`)

```
main.rs            // CLI parse, backend pick, run loop or headless render
  app.rs             // winit + egui plumbing, input routing, hotkeys
  ui/
    chrome.rs        // status bar, later tabs/palette
    grid.rs          // GridRenderer trait + EguiGridRenderer
  pane/
    mod.rs           // Pane thread: Terminal + RenderState + command channel
    snapshot.rs      // FrameSnapshot: rows of resolved cells (fg/bg/attrs/text)
    input.rs         // winit KeyEvent → libghostty key encoder
  transport/
    mod.rs           // Transport trait, Caps, open() factory from a session spec
    serial.rs        // tokio-serial SerialStream; byte mapper; modem lines
    replay.rs        // file-backed transport for tests/CI
  session.rs         // SessionSpec (Local | Serial | Replay) → Transport
  store/
    mod.rs           // redb handle, schema version, open/repair policy
    settings.rs      // per-port serial settings (read on open, write on success)
    logbook.rs       // M1: connection history, notes, tags
  screenshot.rs      // surface/offscreen capture → PNG
```

**Persistence is redb** (pure Rust, ACID, copy-on-write B+tree, stable file format, multi-process
access). One database at `$XDG_STATE_HOME/dtr/dtr.redb`
(`%LOCALAPPDATA%\dtr\dtr.redb` on Windows), `--db <path>` to override, `--no-remember`
to skip opening it entirely.

```
settings   <port-key>  -> { baud, data_bits, parity, stop_bits, flow, imap }
meta       "schema"    -> u32            // migration gate, refuse newer than we know
logbook    <ulid>      -> { port, settings, started, ended, note, tags }   // M1
```

Precedence for every serial parameter: **CLI flag > settings row for that port > built-in default
(115200 8N1, CR→CRLF on)**. The row is written only after a port opens successfully. A corrupt or
unreadable database is not fatal: log it, fall back to an in-memory backend, keep running. Tests
and CI use the in-memory backend (or a tempfile), so `--no-remember` runs stay deterministic.

Why a database rather than a TOML file: the logbook. "Which port was this, at what baud, when, and
what did I plug into it" is the thing serial work actually needs and the thing no TOML file does
well. M0 only uses `settings` + `meta`; that is deliberately overkill for one integer, but it means
M1's logbook is a schema addition rather than a rewrite.

### 2.4 Threading and snapshotting

The pane thread runs `Terminal::feed(bytes)` then `RenderState::update(&terminal)`, walks dirty
rows, and publishes a `FrameSnapshot` over a channel. `RenderState` gives global + per-row dirty
state; the pane **must** clear both after publishing (`dirty` is sticky by design).

For M0 the snapshot copies the whole grid — 80×24 cells is nothing. Per-row dirty copy is a
later optimization, not a design change.

### 2.5 Concurrency: tokio for I/O, LocalSet per pane

**tokio from day one; serial I/O goes through `tokio-serial`.** The constraint that shapes
everything is that `Terminal` is `!Send` — so the emulation loop cannot live on a work-stealing
runtime. The resolution:

- Each pane gets **one OS thread** running `Builder::new_current_thread().enable_all()` plus a
  `LocalSet`. Inside that `LocalSet` a single task owns `Terminal` + `RenderState` + the
  `SerialStream` and does `select! { read, command, tick }`.
- `!Send` is therefore *enforced by the compiler*, not by discipline: a future that touches
  `Terminal` simply cannot be `tokio::spawn`ed, and `spawn_local` on a LocalSet is where it must
  live. That fence is the main reason this shape is safe.
- Snapshots go to the UI over a `crossbeam` channel plus an `EventLoopProxy` wake, so the UI thread
  never blocks and never needs a runtime. Commands (keys, resize, break, close) come back over a
  `tokio::sync::mpsc::unbounded_channel`, which the UI thread can `send` into without one.

What async actually buys us here — the list is short but real:

- `select!` over read / commands / timers gives clean cancellation and shutdown instead of a thread
  stuck in a blocking `read` that we have to poke with a signal or a closed handle.
- Reconnect-on-replug becomes `tokio::time::sleep` backoff rather than a timer thread. This is M1's
  best serial feature and it is genuinely nicer async.
- Windows overlapped I/O: cancellation of a pending read is handled by the runtime instead of us
  hand-rolling `CancelIoEx` around a blocking `ReadFile` (the classic "CloseHandle hangs" bug).
- TCP and SSH transports later drop in as the same task shape. `russh` is tokio-native, so the
  "async island" question disappears before it is asked.

What it costs, honestly:

- `!Send` discipline: every future that touches `Terminal` must be `spawn_local`. Compile errors
  enforce it, but contributors will hit those errors; document the rule in `pane/mod.rs`.
- Parse work blocks the pane's thread — which is *desirable*: it backpressures the read loop, and
  the pane is the only thing on that thread. A pane cannot stall another pane or the UI.
- Binary size and compile time (tokio adds roughly half a megabyte before stripping).
- Two channel flavours in the codebase, and a rule about which direction uses which.

Guardrails: no `spawn_blocking` for emulation, no `Arc<Mutex<Terminal>>`, no multi-thread runtime
for panes, never `Handle::current().block_on()` on the UI thread. If a future API forces a
`Send + 'static` task that needs terminal state, the answer is a channel into the LocalSet, not a
lock.

### 2.6 libghostty notes that will bite

- `TerminalOptions { cols, rows, max_scrollback }`; `RenderState::new()` once, reused per frame.
- Set `GHOSTTY_TERMINAL_OPT_RESIZE_PULL_SCROLLBACK = false` for **every** non-local-shell
  transport: the far end keeps no scrollback, so libghostty must not try to pull rows back on
  resize. ConPTY is the documented motivating case; serial is the same situation.
- `Terminal` is `!Send`; keep it inside the pane thread only. No `Arc<Mutex<Terminal>>` anywhere.
- Kitty graphics are in the render state but textures/clipboard are ours — M3.

### 2.7 Dependencies (M0)

| Crate | Role | Notes |
|---|---|---|
| `libghostty-vt` | VT core | `default-features = false, features = ["pkg-config"]` |
| `clap` | CLI | derive; single source of configuration (D13) |
| `serialport` | builders + port enumeration | pulled in by `tokio-serial`; MPL-2.0 |
| `tokio` | async runtime | `rt`, `time`, `sync`, `io-util`, `macros`; one current-thread runtime per pane |
| `tokio-serial` | serial transport | `SerialStream`, `available_ports`; MIT |
| `redb` | persistence | settings now, logbook in M1; in-memory backend for tests |
| `winit` / `egui` / `egui-winit` / `egui-wgpu` / `wgpu` | window + chrome + grid | own event loop (D6) |
| `pollster` | `block_on` for wgpu init | startup only; everything else is tokio |
| `crossbeam-channel` | pane ↔ UI, transport ↔ pane | `select!` on multiple sources later |
| `anyhow` / `thiserror` | errors | |
| `tracing` + `tracing-subscriber` | logs | |
| `png` | screenshots | pure Rust |
| `dirs` | state dirs | `$XDG_STATE_HOME`, `%LOCALAPPDATA%` |
| `serde` + `bincode` | redb encoding | or hand-rolled fixed structs for `settings` if we want zero deps |

---

## 3. Milestones

### M0 — serial console that displays (the whole MVP)

| # | Task | Notes |
|---|---|---|
| 0.1 | flake skeleton: devShell + linux build + `libghostty-vt` derivation | Prove `zig build -Demit-lib-vt=true` and pkg-config discovery before writing app code |
| 0.2 | winit + egui + wgpu window with a status bar | `egui-winit`, `egui-wgpu`, no `eframe` |
| 0.3 | pane thread: `Terminal` + `RenderState` + `Feed`/`Frame` channel | Feed a hardcoded `"hello\e[31m red\e[0m\r\n"` first |
| 0.4 | `EguiGridRenderer`: cell bg quads + monospace text, cursor | Cell metrics from egui's built-in monospace; no font work |
| 0.5 | `tokio-serial` transport + pane LocalSet + remembered settings | clap flags `--port --baud --data-bits --parity --stop-bits --flow --imap --db --no-remember`; redb `settings` row written after a successful open (§2.2) |
| 0.6 | key input → libghostty key encoder → `write_all` | Port ghostling-rs's key mapping to winit events |
| 0.7 | serial byte mapper, **CR→CRLF on by default** (`--imap cr=crlf`, `lf=crlf`, `del=bs`, `--imap none`) | Needs a pending-`\r` state machine, not a blind replace: emit `\r\n` for a lone CR, never double an existing CRLF, flush the pending CR when the stream goes idle |
| 0.8 | screenshot: `--screenshot out.png`, `F12`, `--headless` | See §4 |
| 0.9 | Windows cross build in the flake; smoke-run under Wine or a VM | `nix build .#windows` |
| 0.10 | `scripts/dev-serial.sh` (socat pair + shell) | See §6 |
| 0.11 | Single-file discipline: embed icon/shaders/fonts, `--licenses`, `checks.single-exe` | See §7, §8 |
| 0.12 | Local shell over a PTY (`portable-pty`: `openpty` on Unix, ConPTY on Windows) | Default when no `--port`; `--shell` overrides, `--demo` keeps the canned screen |
| 0.13 | Terminal replies routed back to the session (`Terminal::on_pty_write`) | Without it, anything that probes DA1/XTGETTCAP — fish, tmux — waits on an answer that never comes |

**Status (2026-10-27):** 0.1–0.8, 0.11, 0.12, 0.13 done. Serial receive is verified end to end
over a socat pty with a byte sequence that exercises the mapper: `alpha\r` (lone CR → newline),
`beta\r\n` (real CRLF → not doubled), `gamma\r` (trailing CR → flushed by the idle timeout).
0.9 is half done: `scripts/wine-smoke.sh` runs the PE under Wine and asserts its frame is
**byte-identical** to the Linux build's — same fonts, layout and colours out of a binary built by
a different compiler for a different target. What that does *not* prove is the console path:
Wine spawns the ConPTY shell and then delivers no bytes (`cmd /c echo` included), so Windows
shells remain unexecuted. Only a real Windows box closes that.

**Allocator:** `mimalloc` is the global allocator on every platform. Windows' default heap contends
badly between the pane thread, the PTY reader and the UI, and the frame path allocates a `Cell`
(with a `String`) 1920 times per publish, which is exactly the shape mimalloc is good at. It is
enabled everywhere so a bug in it shows up on the platform that is easier to debug.

Getting it to cross-compile found two things worth keeping: `libmimalloc-sys` suppresses
`-Werror=date-time` only for non-Windows targets (its build script assumes MSVC, where the flag
does not exist), and **zig cc enables `-Werror=date-time` itself**, so the suppression has to be
appended after it — `CFLAGS_x86_64_pc_windows_gnu=-Wno-error=date-time`. The claim that mimalloc is
faster on Windows is not measured here; it needs a Windows machine.

A finding from getting there: on Windows wgpu selects **dx12** first, and Wine's FXC cannot
compile egui's shader (`Shader model 5.1+ resource array`). `WGPU_BACKEND` now overrides the
backend set, which is both the Wine workaround and a useful escape hatch for broken drivers.

**Remembered settings and the status bar landed with `crates/dtr-store`** (M0 0.5, and the bar
M0 0.2 asked for). The database is a redb file at `$XDG_STATE_HOME/dtr/dtr.redb`
(`%LOCALAPPDATA%` on Windows, `--db` to override), holding one key/value table namespaced by port,
so a new field never needs a migration. Precedence is CLI flag > remembered > 115200 8N1 with
CR→CRLF on, verified: `--baud 9600` once and the next run without it opens at 9600; `--no-remember`
ignores the store entirely; `--imap none` overrides a remembered mapping without forgetting the
baud. A failed open of the database is never fatal.

The status bar shows what the window is attached to, plus RX/TX totals and rates. Rates are
measured by the UI, not the pane: the pane counts bytes, the UI's clock decides what a second is.

0.6 is deliberately legacy VT/xterm encoding. The kitty keyboard protocol that libghostty
implements needs mode negotiation with the application, so it belongs with the renderer work in
M2 rather than in a mapping table.

Two things worth recording from getting a shell to run:

- **Because no test proved otherwise.** fish rendered nothing until terminal *replies* were wired:
  libghostty produces its DA1/XTGETTCAP answers through `on_pty_write`, and dropping them leaves
  the shell waiting on an alt-screen that never fills. bash never asked, so the gap stayed hidden.
- **`TERM` should be `xterm-ghostty`**, not `xterm-256color`. We are a libghostty terminal and the
  terminfo entry describes what we actually implement; it ships with ghostty and in nixpkgs. A
  system without the entry needs a fallback (open question).

**Acceptance:** `scripts/dev-serial.sh` starts a socat pair with `bash` on the far end;
`nix run .#default -- --port /dev/pts/N` shows a working shell prompt, colours, and line
editing; `nix build .#windows` produces `dtr.exe`; `--headless --screenshot` writes a PNG
in CI with no display server.

### M1 — usable serial workbench

- Logbook in redb: every session recorded (port, full settings, start/end, exit reason, note,
  tags), `--note` from the CLI, `dtr --logbook` to list/filter. This is what the database is
  actually for.
- Port picker with USB VID/PID names, reconnect-on-replug (match by USB serial number, not path —
  the `settings` key becomes `usb:<vid>:<pid>:<serial>` when available), DTR/RTS/break buttons,
  modem-line indicators, RX/TX rate.
- Raw log to file (timestamped + raw modes), linkable to the logbook row.
- Local echo toggle, char/line pacing on paste, "paste as file" pacing.
- Scrollback + scroll wheel + selection/copy (`libghostty_vt::selection`, `fmt` for HTML copy).
- `--replay <file>` transport and a golden-file test for byte→grid.

**Status (2026-10-27):** the logbook and `--note` are in. A row is written when a session
*starts*, so a crash leaves it without an end and `--logbook` shows it as `unfinished` rather than
pretending it closed. Times are stored as instants and printed in UTC: formatting is a display
concern, and the timezone is not the database's business. Building it exposed a bug worth more than
the feature — a port that failed to open only logged on the pane thread, so the app kept running and
recorded a clean exit for a session that never started; the pane now carries a fatal-error slot and
the failure surfaces in 0.2 s, non-zero, with `error: …` in the row.

Also in: `--replay <file>` (feeds a captured stream through the same path, paced, so the terminal
can be exercised with no hardware) and `--log <file>` (raw bytes as they arrived, *before* the
CR→CRLF mapping — verified byte-identical against a socat far end that sent `alpha\rbeta\r\ngamma\r`,
CRs intact, while the screen still showed three translated lines).

**DTR/RTS/break are blocked on a real design problem, not on effort.** `tokio_serial::SerialStream`
refuses `try_clone` ("Cannot clone Tokio handles"), and `tokio::io::split` — which the pane needs so
`select!` can read and write at once — yields halves that do not implement `SerialPort`. So there is
no handle left to call `write_data_terminal_ready` on, and opening the device a second time would
assert DTR again, resetting the board you were trying to talk to. The fix is to give the port to a
single owner task that receives `Write`/`Dtr`/`Rts`/`Break` commands, which means replacing the
split-read design; that is its own piece of work.

**The picker is a palette, and its logic is `crates/dtr-complete`.** One input line that means
different things by its first word — `connect /dev/ttyUSB0`, `ssh jupiter.home`, `shell fish` — so the
engine answers two questions: `complete` (ranked list) and `inline` (the grey suffix Tab accepts).
Inline is deliberately narrower: it only fires when the top candidate *starts with* what was typed.
`ssh jptr` ranks jupiter first but shares no prefix, and ghost text that has nothing to do with the
typed characters reads as a glitch, not a suggestion.

Ranking uses `nucleo-matcher` (helix's matcher): 5.2M downloads, two small pure-Rust dependencies
(`memchr`, `unicode-segmentation`), and much better scoring than a naive subsequence — word
boundaries and consecutive runs. It is **MPL-2.0**, file-level copyleft, unmodified; the same stance
we already take for `serialport`. The alternatives were `fuzzy-matcher` (MIT, SkimMatcherV2, last
released 2020, fine but quieter) and `frizbee` (MIT, active, carries `serde`). Nushell's completion
engine lives in `reedline`, which is a whole crossterm line editor — the right idea, the wrong shape
for an egui widget, so the engine is ours and only the scoring is borrowed.

Still open in M1: the palette UI itself, serial-port enumeration with USB VID/PID, reconnect-on-replug,
timestamped log mode.

### M2 — real renderer

- wgpu cell renderer behind `GridRenderer`: instanced bg + glyph quads, glyph atlas,
  `egui_wgpu::CallbackTrait`, per-row dirty scissor.
- Font stack: `fontique` (candidate selection + fallback), `rustybuzz` (shaping per style run),
  `swash` (raster, incl. COLR/CBDT emoji); cell metrics from the font, integer-snapped.
- Subpixel/grayscale AA decision; cursor shapes; underline styles.

### M3 — the rest of a terminal

- ConPTY transport (`portable-pty` or hand-rolled `CreatePseudoConsole`), local shells, WSL.
- Tabs/splits, torn-off windows (`egui::ViewportBuilder`), session restore.
- IME/TSF preedit, kitty graphics, search (needs hand-bound C API — not in the 0.2.2 Rust surface).
- Quake mode, tray, taskbar progress, single-instance CLI.

---

## 4. Screenshot / observability (M0, cross-cutting)

This is a **hard requirement of the dev loop, not a nice-to-have**: headless capture is the only
way the agent sees the UI. Which GPU backend CI ends up with (lavapipe, llvmpipe, or a real
device) is an implementation detail not worth optimising.

Three paths, all thin wrappers over one function:

1. **In-app frame capture.** After `egui_wgpu` render, `copy_texture_to_buffer` → map → encode PNG.
   Works on Windows and Linux, exact pixels, no compositor involvement.
   - `F12` → `$XDG_PICTURES_DIR/dtr-<ts>.png` (Windows: `%USERPROFILE%\Pictures`).
   - `--screenshot <path>` → capture the frame after `--screenshot-delay-ms` (default 500), then exit 0.
2. **Headless render.** `--headless --screenshot out.png` constructs wgpu with no surface,
   renders egui to an offscreen texture, writes PNG. No display server, no window manager —
   this is the CI path (`VK_ICD_FILENAMES` → lavapipe or `LIBGL_ALWAYS_SOFTWARE=1`).
   Fed with `--replay` bytes so the image has predictable content.
3. **Deterministic fixtures.** `tests/golden/*.vt` fed through `--replay`, with the resulting PNG
   compared by a tolerance check (or just committed for human/agent review).

Everything here is `main.rs` plumbing: the pane and renderer don't know they're being captured.
This is what makes UI work reviewable without a Windows machine.

**Implemented (M0):** `dtr --screenshot out.png --size 900x620`, wrapped by `scripts/shot.sh`. It
renders the real egui frame into an offscreen `Rgba8UnormSrgb` texture, reads it back with a
256-byte-padded copy, and writes PNG.

Lesson worth keeping: a compositor screenshot is not a substitute. The first attempt used an
external capture tool and photographed the **lock screen** — the window was up, the desktop was
locked, and the image was worthless. In-app capture has no such dependency.

---

## 5. Nix flake

### 5.1 Inputs and outputs

```nix
inputs.nixpkgs.url = "https://channels.nixos.org/nixos-unstable/nixexprs.tar.zst";
inputs.rust-overlay = { url = "github:oxalica/rust-overlay"; inputs.nixpkgs.follows = "nixpkgs"; };

outputs = { self, nixpkgs, rust-overlay }: {
  packages.x86_64-linux = {
    default  = dtr-linux;    # native ELF
    windows  = dtr-windows;  # PE32+ dtr.exe, built on linux
    libghostty-vt       = libghostty-vt-host;
    libghostty-vt-win   = libghostty-vt-windows;
  };
  devShells.x86_64-linux.default = ...;
  checks.x86_64-linux = { screenshot = ...; clippy = ...; };
};
```

Both `packages` are `x86_64-linux` because Nix does not run on Windows: the Windows artifact is
a cross build produced by a Linux derivation. Name it `.#windows` so nobody mistakes it for a
native package.

### 5.2 `libghostty-vt` derivation

Adapt upstream's [`nix/libghostty-vt.nix`](https://github.com/ghostty-org/ghostty/blob/main/nix/libghostty-vt.nix),
parameterized by zig target:

```nix
libghostty-vt = { zigTarget ? null }: pkgs.stdenv.mkDerivation {
  pname = "libghostty-vt"; version = "0.2.2+521cf2c";
  src = ghostty-src;                       # fetchFromGitHub @ 521cf2c
  nativeBuildInputs = [ pkgs.zig_0_17 pkgs.git pkgs.pkg-config ];
  dontSetZigDefaultFlags = true;           # upstream derivation sets this
  zigBuildFlags = [
    "--system" "${zig-deps}"               # offline package cache (FOD)
    "-Doptimize=ReleaseFast"
    "-Dcpu=baseline"
    "-Dapp-runtime=none"
    "-Demit-lib-vt=true"
  ] ++ lib.optional (zigTarget != null) "-Dtarget=${zigTarget}";
  outputs = [ "out" "dev" ];               # dev holds libghostty-vt.a + .pc
};
```

`zig-deps` is a fixed-output derivation: run `zig fetch` over `build.zig.zon.txt` (ghostty ships
that file and `build-support/fetch-zig-cache.sh`), export `ZIG_GLOBAL_CACHE_DIR`, install
`$cache/p` as `$out/p` so `--system $out` resolves. One hash to seed on first build.

Why not nixpkgs' own `libghostty-vt`: it is 2026-08-06, older than our pinned commit, and the
Rust bindings are ABI-tied to `521cf2c`. Consistency across Linux/Windows matters more than
reusing the packaged one.

Keep `simd = true` (upstream's default) and install both `.pc` modules; the crate probes
`libghostty-vt-static` in static mode, and that is the one that must point at `libghostty-vt.a`
rather than the DLL import library.

### 5.3 App derivations

**Linux** — ordinary `rustPlatform.buildRustPackage`:

```nix
rustPlatform.buildRustPackage {
  pname = "dtr"; version = "0.1.0"; src = self;
  cargoLock.lockFile = ./Cargo.lock;
  nativeBuildInputs = [ pkgs.pkg-config ];
  buildInputs = [ libghostty-vt-host.dev pkgs.vulkan-loader pkgs.libxkbcommon
                  pkgs.wayland pkgs.libX11 pkgs.libxcb pkgs.libXcursor pkgs.libXi pkgs.libXrandr ];
  preBuild = ''
    export PKG_CONFIG_PATH=${libghostty-vt-host.dev}/share/pkgconfig
  '';
}
```

**Windows** — `cargo zigbuild`, since `buildRustPackage` won't drive it:

```nix
stdenv.mkDerivation {
  pname = "dtr"; version = "0.1.0"; src = self;
  cargoDeps = rustPlatform.importCargoLock { lockFile = ./Cargo.lock; };
  nativeBuildInputs = [ rust-windows cargo-zigbuild pkgs.zig_0_17 pkgs.pkg-config
                        rustPlatform.cargoSetupHook ];
  buildPhase = ''
    export PKG_CONFIG_ALLOW_CROSS=1
    export PKG_CONFIG_LIBDIR=${libghostty-vt-windows.dev}/share/pkgconfig
    cargo zigbuild --offline --locked --release --target x86_64-pc-windows-gnu
  '';
  installPhase = ''
    install -Dm755 target/x86_64-pc-windows-gnu/release/dtr.exe $out/bin/dtr.exe
  '';
}
```

Details that must be right:

- `PKG_CONFIG_ALLOW_CROSS=1` — the `pkg-config` crate refuses cross probes without it.
- `PKG_CONFIG_LIBDIR` (not `PATH`) so the host's `.pc` files cannot leak into the target link.
- `libghostty-vt-sys` must be built with `default-features = false, features = ["pkg-config"]`;
  it probes module `libghostty-vt-static` (static link mode) — the `.pc` we install provides it.
- `rust-windows` is a rust-overlay toolchain with `targets = [ "x86_64-pc-windows-gnu" ];` — see §5.4.
- If `cargoSetupHook`'s read-only `CARGO_HOME` trips cargo-zigbuild, copy it to `$TMPDIR` and
  re-export; cargo-zigbuild writes linker config.
- `embed-resource` needs `llvm-rc` (nixpkgs `llvmPackages.llvm`) or `windres` in
  `nativeBuildInputs` for the icon/manifest step (§7). If it fights the cross build, ship M0
  without resources and add a runtime DPI-awareness call instead.

### 5.4 Rust std for the Windows target (resolved)

`cargo zigbuild` needs `rust-std` for `x86_64-pc-windows-gnu`; nixpkgs' rustc ships host std only.
Resolved: a rust-overlay toolchain with `targets = ["x86_64-pc-windows-gnu"]`, which installs both
stds. Verified in the spike (§12).

Two traps found:

- **Only one rust toolchain may be in the devShell.** Two `rust-bin` derivations in `packages` put
  two `rustc`s in `PATH`; the host-only one wins and the cross build fails with "can't find crate
  for `core`". The toolchain with extra targets includes the host std, so it is the only one needed.
- **`cargo-zigbuild` 0.23.4 (nixpkgs) works with zig 0.17.0.** No overlay needed.

---

## 6. Dev loop and testing

**Serial without hardware (Linux):**

```sh
scripts/dev-serial.sh          # socat pty pair + bash on the far end
# dtr --port /dev/pts/N
```

The far end runs `bash` attached to its pty, so readline, echo, colours, and cursor movement all
exercise the real path. This is a better test than a fake stream because it is bidirectional.

**No serial at all:** `--replay tests/fixtures/*.vt` drives the pane from recorded bytes.

**Windows:** com0com virtual pair, or a real USB-UART. CI gets a smoke run under Wine only for
"does the exe start and render a frame" — Wine's ConPTY is irrelevant here since M0 has no PTY,
which is a small argument for serial-first.

**CI (`nix flake check`):**
- `clippy` + `cargo test` (pane/transport logic, byte mapper, key mapping).
- `screenshot`: `--headless --replay fixture --screenshot $out.png` under lavapipe, asserting a
  non-blank image; the PNG is the artifact.
- `packages.default`, `packages.windows` build.
- `single-exe`: DLL allowlist assertion on the Windows binary (§7).

---

## 7. Single executable (Windows only)

**Windows: one `dtr.exe`.** No DLLs, no data directory, no runtime assets. This falls out of
the plan almost for free — static mingw link, static `libghostty-vt.a`, and everything else
embedded — but it needs to be defended by a check, not by hope.

**Linux: an ordinary binary.** One ELF with normal dynamic dependencies on X11/Wayland and the
graphics stack, distributed however is convenient (nix package, tarball). No single-file goal
there; that requirement exists because Windows users expect to drop an `.exe` somewhere.

Rules:

1. Link the **static** module: `libghostty-vt-static.pc`, `link-dynamic` feature **off**, so the
   `.a` is baked in. Never ship `ghostty-vt.dll`.
2. Nothing read from disk at startup except the optional redb database and config; absence of
   either means defaults compiled in, and the `.exe` runs with zero files present. The database is
   a runtime artifact the app creates on demand, not a shipped sidecar.
3. Embed: window icon (`include_bytes!` + `set_window_icon`), shaders (`include_str!`), any fonts
   we add later, and — on Windows — the icon + manifest via `embed-resource` (needs `llvm-rc` or
   `windres` in `nativeBuildInputs`; verify under cross).
4. `panic = "abort"`, `lto = "fat"`, `strip = "symbols"` in the release profile — after confirming
   wgpu/egui are happy with aborting panics.
5. Screenshots and logs are written only when explicitly requested.

**Enforced check** (`checks.single-exe`):

```sh
objdump -p result/bin/dtr.exe | awk '/DLL Name/ {print $3}'   # must ⊆ system allowlist
```

Allowlist, seeded from what the spike binaries actually import:
`KERNEL32 USER32 GDI32 ADVAPI32 ole32 SHELL32 DXGI d3d12 d3dcompiler_47 SETUPAPI WS2_32 bcrypt
bcryptprimitives ntdll msvcrt userenv uxtheme dwmapi imm32 comctl32 propsys shcore`, plus the
`api-ms-win-*` UCRT shims. Anything outside that fails the derivation. The check applies to the
Windows package only; the Linux package is not constrained this way.

## 8. Licensing

`libghostty` is **MIT** (`Copyright (c) 2024 Mitchell Hashimoto, Ghostty contributors`), and the
Rust wrapper crates are `MIT OR Apache-2.0`. Static linking into a single binary is fine and
closed-source distribution is permitted. There is no GPL/AGPL anywhere in the chain. The
obligations are attribution, not disclosure:

| Component | License | What we must do |
|---|---|---|
| `libghostty-vt` (C) | MIT | reproduce copyright + permission notice |
| `libghostty-vt`, `-sys` (Rust) | MIT OR Apache-2.0 | pick MIT, same notice |
| `uucode`, `zf`, `libxev`, `vaxis` (inside the `.a`) | MIT | same notice |
| `zlib` (inside the `.a`) | zlib | retain notice, no endorsement claim |
| `simdutf` | MIT OR Apache-2.0 | same notice |
| `highway` (`-Dsimd=true`) | **Apache-2.0** | ship license text + preserve its `NOTICE` |
| `serialport` | **MPL-2.0** | file-level copyleft: static link fine, but published modifications to *its* files stay MPL |
| egui / egui-wgpu / wgpu / clap / redb / tokio / tokio-serial | MIT OR Apache-2.0 (tokio crates: MIT) | notice |
| winit | **Apache-2.0** | license text + `NOTICE` |
| dtr itself | **MIT OR Unlicense** | our own choice; no obligation either way |

Practical implementation:

- Dual license as `MIT OR Unlicense`: `Cargo.toml` gets `license = "MIT OR Unlicense"`, repo root
  gets `LICENSE-MIT` and `UNLICENSE`, flake `meta.license = [ lib.licenses.mit
  lib.licenses.unlicense ]`. The user picks either; Unlicense is a public-domain dedication, MIT
  is the fallback for jurisdictions that don't accept dedications.
- Our own license choice does **not** reduce the dependency obligations below — the notices in the
  table are required regardless of how dtr itself is licensed.
- `THIRD_PARTY_LICENSES.txt` committed in-repo and embedded with `include_str!`; `--licenses`
  prints it; the Windows release ships it beside the `.exe` (the file is *beside*, not required by,
  the binary — the single-executable rule is about function, not paperwork).
- `-Dsimd=false` would drop `simdutf` + `highway` and remove both Apache-2.0 obligations. Not
  worth it: SIMD parsing is a real libghostty feature, and the obligation is just a NOTICE file.
- Name/logo: the MIT grant covers code, not the "Ghostty" trademark. Describe as "built on
  libghostty", don't imply endorsement, don't use their icon.

## 9. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| ~~Rust std for windows-gnu unavailable in Nix~~ | *resolved in spike* | rust-overlay toolchain with `targets`; one toolchain per shell (§12) |
| `libghostty-vt` 0.2.2 API churn (pre-1.0, "use with caution") | Rework | Pin crate + ghostty commit; keep all lib calls inside `pane/`; hand-bind if the wrapper lags |
| ~~zig 0.17 vs cargo-zigbuild 0.23.4~~ | *resolved in spike* | 0.23.4 + zig 0.17.0 link fine (§12) |
| No Windows dev machine | Blind UI/COM bugs | Headless screenshots, `--replay`, VM smoke tests, user-provided COM hardware |
| egui painter grid too slow later | Renderer rewrite | Already behind `GridRenderer`; serial grids are small anyway |
| ABI mismatch between nix-built lib and 0.2.2 bindings | Crashes | Build exactly commit `521cf2c`; assert `ghostty_*` symbols link in a `checks` derivation |
| `serialport` + `setupapi` under cross link | Windows build break | Defer `available_ports()` to M1; M0 takes an explicit path |
| A non-system DLL creeps into the Windows build (mesa, mingw runtime, `dxcompiler`) | Breaks "one exe" silently | `checks.single-exe` allowlist fails the derivation |
| We patch `serialport` (MPL-2.0) | Must publish those patched files | Prefer upstreaming; keep patches as separate `.patch` files in-repo so the MPL scope is obvious |
| redb open fails (locked, corrupt, newer schema) | Would block startup | Never fatal: warn, fall back to in-memory, keep running; `meta.schema` gates migrations; `--db` for isolation |
| `tokio-serial` 5.5.0 is a smaller project (mio-serial underneath) | Transport bug we can't fix quickly | The `Transport` trait is one impl wide: fall back to `spawn_blocking` around sync `serialport` without touching the pane |
| `!Send` futures leak onto a work-stealing runtime | Compile error at best, confusing structure at worst | Compiler is the fence; `spawn_local` only; rule documented in `pane/mod.rs` |

---

## 10. Open questions

1. **Windows-native look**: stay with winit decorations for M0, or custom titlebar immediately?
   (Mica/acrylic is M3 territory either way.)

---

## 11. First week, in order

1. **Spike:** flake + `libghostty-vt` derivation for host and `x86_64-windows-gnu`; `nm` the
   archives to confirm `ghostty_terminal_new` in both. *(de-risks D9/D10)*
2. **Spike:** Windows rust toolchain + `cargo zigbuild` on a hello-world winit window. *(de-risks §5.4)*
3. Commit the flake with `packages.default`, `packages.windows`, `checks.screenshot` stubs; add
   `LICENSE-MIT`, `UNLICENSE`, `THIRD_PARTY_LICENSES.txt` and the `--licenses` command (an hour of
   work, and it unblocks any release).
4. Cargo workspace + winit/egui/wgpu window with a status bar; `nix run .#default` opens it.
5. Pane thread with hardcoded feed; egui grid renderer draws it; `F12` screenshot; read the PNG.
6. `tokio-serial` transport + `store` (redb) + key encoder; `scripts/dev-serial.sh`; first real
   serial session, then relaunch and confirm the baud was remembered.
7. Wire `--imap`, RX/TX counters, DTR/RTS into the status bar. **M0 done.**

---

## 12. Spike results (2026-10-27)

Both spikes pass. The build story below is measured, not predicted.

### 12.1 `libghostty-vt` builds for both targets

Upstream's own `nix/libghostty-vt.nix` works as-is for Linux via
`pkgs.callPackage (ghostty + "/nix/libghostty-vt.nix")`, with ghostty pinned to
`521cf2c` and `zig_0_17` from nixpkgs. The zon2nix-generated `build.zig.zon.nix` in the ghostty
tree supplies every package, so the sandbox needs no network.

| Target | Artifacts | Notes |
|---|---|---|
| `x86_64-linux` | `dev/lib/libghostty-vt.a` (70 MB, debug), `include/ghostty/vt/…`, `share/pkgconfig/libghostty-vt{-static,}.pc` | `.pc` `Libs` is an absolute path to the `.a` |
| `x86_64-windows-gnu` | `lib/ghostty-vt-static.lib` (27 MB, 1460 symbols), `lib/ghostty-vt.lib` (import lib), `bin/ghostty-vt.dll` | needs a derivation override, see below |

`nm` confirms `ghostty_terminal_new`, `ghostty_render_state_begin_update`, etc. in both archives.

**Windows needs an override.** Zig names the archive `ghostty-vt-static.lib` there, so upstream's
`postInstall` (`mv $out/lib/libghostty-vt.a $dev/lib/`) fails. Keep everything in one output:

```nix
libghostty-vt-win = libghostty-vt.overrideAttrs (old: {
  pname = "libghostty-vt-win";
  outputs = ["out"];
  zigBuildFlags = old.zigBuildFlags ++ ["-Dtarget=x86_64-windows-gnu"];
  postInstall = "";
  postFixup = "";
});
```

### 12.2 Rust cross-compiles and links

`cargo zigbuild --target x86_64-pc-windows-gnu` with `cargo-zigbuild` 0.23.4 + `zig` 0.17.0
produced:

- a winit hello-world: 1.38 MB PE32+, system DLLs only;
- a winit + libghostty-vt program: 10.1 MB PE32+, `.text` 8.2 MB, 273 `ghostty` strings —
  the terminal really is statically inside the binary.

Both link fine with `-Dsimd=true`, so `simdutf`/`highway` cross-compile without incident.

### 12.3 The trap that cost an hour: `PKG_CONFIG_PATH`

For a cross build the pkg-config crate needs `PKG_CONFIG_ALLOW_CROSS=1` **and**
`PKG_CONFIG_LIBDIR=<windows .pc dir>`. But the devShell's `shellHook` sets `PKG_CONFIG_PATH` to the
Linux `.pc`, and `PKG_CONFIG_PATH` **takes precedence** over `PKG_CONFIG_LIBDIR`. The result is a
silent Linux search path and a confusing `library 'ghostty-vt-static.lib' not found`.

Every cross build must `export PKG_CONFIG_PATH=""` before setting `PKG_CONFIG_LIBDIR`. Encode this
in the Windows derivation, not in the docs.

### 12.4 Upstream crate bug: 0.2.2 is not self-consistent

`libghostty-vt` 0.2.2 (released 2026-09-28) declares three C functions that **do not exist** at the
ghostty commit its own `-sys` crate pins (`521cf2c`):

```
ghostty_terminal_mode_get
ghostty_terminal_mode_set
ghostty_render_state_colors_get
```

They are not in the header, not in `nm` of either archive, and not anywhere in the ghostty repo.
Linux links succeed only because the linker garbage-collects the unused methods; COFF pulls the
object member and the link fails. The safe crate's `Terminal::mode`, `Terminal::set_mode` and
`RenderState::colors()` are therefore unlinkable as published.

Git master `9ede35b` (which still declares itself 0.2.1) has bindings that match the pinned
library exactly — 205 externs declared, 0 missing. So D17: pin the git rev, not the release.

Verified by diffing declared externs in `bindings.rs` against `nm -g --defined-only` of the archive.
That diff is cheap and worth keeping as a `checks` derivation when we bump either side.

### 12.5 What the flake has now

| Output | What |
|---|---|
| `packages.default` / `.#dtr` | Linux ELF, linked against the nix-built static lib |
| `packages.windows` | `dtr.exe`, cross-built with `cargo zigbuild` |
| `packages.libghostty-vt{,-win}` | the C library per target, for direct consumption |
| `devShells.default` | rust (host + windows target), `cargo-zigbuild`, `zig_0_17`, `pkg-config`, `socat`, `just` |

The Rust source is `lib.fileset.toSource` over `Cargo.toml`, `Cargo.lock` and `src/`, so `target/`
and result symlinks stay out of the store.

**Git dependency hashes.** `importCargoLock` cannot vendor a git dependency without an
`outputHashes` entry, and both wrapper crates come from the same checkout so they share one hash:

```nix
outputHashes = {
  "libghostty-vt-0.2.1" = "sha256-Zojd1ziCw9+tbhmGYaFHFPlaxgbNbtmVBR99Otmg+o0=";
  "libghostty-vt-sys-0.2.1" = "sha256-Zojd1ziCw9+tbhmGYaFHFPlaxgbNbtmVBR99Otmg+o0=";
};
```

Bumping the wrapper rev means recomputing that hash
(`nix hash path $(nix eval --raw --impure --expr 'builtins.fetchGit { url = …; rev = …; }')`) and
updating three places: `Cargo.toml`, and both derivations. Worth a `just bump-wrapper` when it
starts to annoy.
