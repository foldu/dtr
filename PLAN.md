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

**A frame per read chunk is a thousand frames a second nobody sees.** The pane
published from the read arm, so every chunk rebuilt the entire grid — cell by cell
through the FFI, with a `String` for each cell that has a grapheme in it — at
about 50 µs a frame. A flood arrives
in chunks small enough that a 32 MiB payload became **1.1 million publishes and 57 s
of CPU inside a 60 s run**, all of it thrown away because a screen shows sixty
frames a second. The tell was a profile where 92% of the samples sat in
`pane::fill_cells` and none of them in the parser.

The gate is now half a refresh interval — 8.3 ms on a 60 Hz panel, 2.1 ms on a
240 Hz one — with a timer arm that publishes the frame the pane owes, so a skipped
publish cannot leave the picture stale. Half an interval and not a fixed 8 ms,
because the gate is a latency floor as much as a work cap: 8 ms would serve a
240 Hz panel at 125 Hz and leave every other frame stale. It lives in an atomic
the UI rewrites rather than a constant, because the panel changes under the
window — on `Moved` and `ScaleFactorChanged`, and once a second from the loop
report, since a Wayland move tells the client nothing at all. The same flood is
484 publishes and 31 ms. Two smaller traps came with it: `bytes_rx.recv()`
returning `None` is not a no-op, because that arm is ready for ever and the pane
spun at 100% after its child exited; and vtebench cannot see either of these,
because what it times is the pty, not the frame.

**What the per-cell `String` actually costs.** The frame path allocates a `Cell` with a `String`
for every cell, which looks like the obvious thing to fix and is not the big half. Measured by
swapping the global allocator out: a flood of two-byte lines leaves only 34 cells a frame with
text in them, and mimalloc against the system allocator is **53 µs against 54 µs** a frame —
nothing. A payload that fills every cell, 3808 allocations a frame, is **41 µs against 50 µs**:
about 2.4 ns an allocation, and the other 8 ns a cell are the five FFI crossings it takes to read
one (graphemes, foreground, background, styling, and the iterator step). At sixty frames a second
the whole publish is 0.25% of a core and the strings are 0.05% of it, so this is a number to know
before refactoring, not a reason to.

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

Encoding keys turned out not to be a table, and the table was wrong in a way a table cannot
fix: what a key sends is the *terminal's* state, not a function of the key and its modifiers.
`DECCKM` (`ESC [ ? 1 h`) is what ncurses sends for every program that calls `keypad()`, and from
then on an unmodified arrow is `ESC O A`; htop, less, man and mc all ask for it, so all of them
had a dead keyboard. On top of that comes `DECKPAM` for the keypad, mode 1036 for the alt
prefix, `modifyOtherKeys`, and the kitty keyboard protocol (`CSI > flags u`), which turns
ctrl+a into `CSI 97;5u` and can report releases.

So the encoding runs in the pane, against `libghostty_vt::key::Encoder` configured from the
terminal before each key: `Command::Key` carries a stroke, not bytes, and `input::fill` only maps
winit's vocabulary onto the encoder's. The kitty protocol is now live rather than an M2 item —
fish asks for `kitty=5`, nvim for `kitty=3`, the core answers their `CSI ? u`, and ctrl+a reaches
them as `CSI 97;5u` with releases reported when asked. Three details that bit: the *physical*
key decides (a real event has one, `--keys` only implies it), a ctrl or super combination must
not hand its `text` to the encoder (it typed a literal `a` for ctrl+a), and the kitty flags are a
*stack* that `reset_modes` has to pop (`CSI < 999 u`, then `CSI = 0 u`) or a dead program's push
reaches the shell that replaces it. A paste follows the same rule — the program's `?2004h`
decides the wrapping — which is why both live in the pane.

**IME is a picture and a commit, not keys.** A composition arrives as `Ime::Preedit` (what is being
composed) and `Ime::Commit` (what was decided). The preedit is drawn in a box hung above the
terminal's caret (`crate::ime`), clamped into the pane, with the platform's candidate window told
the same rectangle through `set_ime_cursor_area`; nothing reaches the pty until the commit, which
is what makes Escape cancel a composition instead of typing half of it. The keystrokes *during* a
composition are not keys either: they carry `composing`, and the encoder suppresses their text,
because the commit will send it — without that, pinyin types the romaji and then the character.
`set_ime_allowed(true)` is the switch that makes any of it happen at all; before it dtr had no IME,
so a CJK keyboard typed latin letters at the shell.

The half that is not obvious is the font. egui's bundled families are latin-only and it **panics**
on a glyph none of them has (`No glyph for "に" in Monospace`), and a terminal is exactly where
CJK gets typed. `Look::install` therefore appends one CJK face — the first family from the grid's
own `FALLBACK` list that has kana *and* Han — to both egui families. That fixes the preedit box and
every other UI surface a person can type into, the palette query included.

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

The Linux package is also *installable*, not just runnable: `assets/dtr.desktop` is installed to
`$out/share/applications`, and the logo set goes to `$out/share/icons/hicolor` under the name the
entry's `Icon=` looks up — the vector one plus the three sizes the set actually has. Both halves
have to agree on a name (`dtr`), and so does the window: `App::resumed` sets the app id and
`WM_CLASS` to `dtr`, because without a name X11 derives `WM_CLASS` from argv[0] — under the
wrapper that is `.dtr-unwrapped` — and Wayland gets no app id at all, which is a second, iconless
window in the dock. `checks.<system>.desktop-entry` validates the *installed* entry and asserts
each size exists, because a menu shows a blank square rather than an error.

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

**The palette is drawn and it starts sessions.** Ctrl+Shift+P (or `--palette`) opens an overlay in
the same `Ui`, not an `egui::Window`: a palette is a fixed thing at a fixed place, and a floating
window would invite dragging it around a terminal that has no room for it. It owns the keyboard while
open, so arrow keys move the selection instead of the caret, and it swaps the session underneath the
window when you accept a line — `connect /dev/ttyUSB0` replaces the pane, using the baud remembered
for that port.

Sources are read when the palette opens, not at startup: the device you want is usually the one you
just plugged in. Ports carry USB VID:PID and product strings, which is what tells two identical
adapters apart and what reconnect-by-device will match on. SSH hosts come from `dtr-sshconfig` as
aliases only — a wildcard pattern is not something you can connect to — and `ssh` currently reports
that it is not implemented rather than silently doing nothing.

Two things this cost: `egui::Window` produced nothing inside a `run_ui` pass (the palette drew no
shapes at all), and painting the selected row's highlight *after* its labels hid the selected row's
own text. Both are the kind of thing only a screenshot catches.

**Reconnect follows the device, not the path.** When the reader ends, the pane goes back to
watching: every 500 ms it tries to reopen, and if it knows the adapter's USB serial number it looks
the device up by that rather than by path — a replug usually renames `/dev/ttyUSB0` to
`ttyUSB1`, and a terminal that gives up on the first read error is not a console. The status bar
says `reconnecting…` while it waits. Verified by killing a socat pty mid-session and bringing it
back: `first`, then `second`, with `device gone; watching for it` and `device is back` 2.5 s apart.

The bug worth remembering: the pane kept the byte-channel sender alive for reconnects, so
`bytes_rx.recv()` never returned `None` and a disconnect went unnoticed — the reader needs its own
"ended" signal, not the channel closing.

**Modem lines, echo, scrollback and the timestamped log are in.** DTR, RTS and break are ioctls on a
*duplicated* descriptor — `tokio_serial` refuses `try_clone`, and reopening the device would assert
DTR and reset the board, which is the whole thing we were trying to avoid. `Ctrl+Shift+D/R/B` toggle
DTR, RTS and send a break; the status bar shows `DTR● RTS●`. `Ctrl+Shift+E` toggles local echo for
devices that do not echo. The wheel scrolls the viewport (`ScrollViewport::Delta`, up is negative).

`--log-format raw|timestamped`: raw is the wire's bytes and is replayable, timestamped is one escaped
line per read and is readable. Verified: `[+0.005s] sensor ready\r` / `[+1.082s] temp=21.4C\r`.

Modem control is **Unix only**. Windows needs `EscapeCommFunction` through a duplicated HANDLE, and
blind unsafe FFI on the platform we cannot run is worse than a clear refusal; it logs and does
nothing there.

**Clipboard in and out.** `Ctrl+Shift+C` copies the visible screen — rows trimmed of trailing blanks,
no empty tail — which is what copy means in a terminal; scrollback and mouse selection wait for the M2
renderer, where hit-testing lives. Paste comes in as an egui `Event::Paste`; on a **serial** transport the pane writes it 128 bytes at a
time, 20 ms apart, because a device with a small input buffer drops bytes when the whole clipboard
arrives at once. On a shell in a pty the paste is handed over in one write — that buffer is the
kernel's, pacing it only makes the terminal feel broken.

That closes M1 except two honest gaps: **mouse selection** (needs the M2 renderer's grid hit-testing)
and **reconnect by USB serial number** (only exercised against a pty here — no adapter to unplug).
M1's own line item "selection" is therefore half done: copy exists, choosing what to copy does not.

**Config and font are configurable.** `crates/dtr-config` reads
`<config dir>/dtr/config.toml` and edits it with `toml_edit` — the editing half is not a formality:
a config file is a document a human wrote, and a program that strips the comments and re-sorts the
keys on save is a program people stop editing. `dtr --config-init` writes a commented template,
`dtr --config-set font.size 20` changes one value and leaves every other byte alone, `--config-path`
prints where it lives. A broken config is reported and the defaults are used: a typo must not stop a
terminal from opening a port.

Precedence is command line, then config file, then the remembered port settings, then the built-in
115200 8N1. The config file outranks memory because it is a declaration rather than the side effect
of the last session.

`font.size` and `font.path` are honoured: a `.ttf`/`.otf` is loaded into egui's monospace family at
context creation (loading per frame would rebuild the glyph atlas on every repaint).

**Themes are one file per theme, referenced by name**: `theme = "kanagawa wave"` in the config, and
the file lives in `<config>/dtr/themes/`, falling back to `<config>/ghostty/themes/` so the themes a
user already collected work unchanged. The format is Ghostty's `key = value` — `background`,
`foreground`, `cursor-color`, and `palette = N=#rrggbb` for the sixteen ANSI colours — parsed with the
VT core's own colour parser, so `#rgb`, `#rrggbb` and X11 names all work. Unknown keys (fonts, window
chrome) are ignored rather than treated as errors, which is what makes reading someone else's theme
files possible. `dtr --themes` lists what it can see, and the palette offers them: `theme kanagawa wave` switches the
live terminal — cells are re-read from the terminal, so existing text takes the palette too — and the
chrome is rebuilt in the same action, because a window showing one theme's text on another theme's
surface is worse than no theme at all.

The argument after a palette command is taken as the rest of the line, not the next word: most of the
819 shipped theme names contain a space, and a path handed to `replay` can too.

The theme is applied to the *terminal*, not to the renderer: `set_default_color_palette` plus the
default fg/bg/cursor means the whole 256-colour cube follows, not just the sixteen named colours.
The chrome follows the theme too. No theme format carries colours for a status bar or a launcher, so
they are **derived**: a surface lifted 10% from the background toward the foreground, secondary text
55% of the way along the same line, and the theme's cursor colour (or palette 3) as the accent. That
keeps a theme a document about the *terminal* while the window still looks like one piece.

Colours in the config file itself are deliberately not supported: a theme is a document with a name,
and pointing at one beats duplicating sixteen hex triplets inline.

`docs/themes.md` is the research behind this and goes further than the implementation: it measured the
corpora (819 Ghostty themes shipped, 463 already installed on this machine, 860 in the upstream hub),
checked the licences (Ghostty is MIT, but the underlying iTerm2 corpus is only ~259/860
redistributable — which is why dtr reads theme files instead of shipping them), and specifies an
`.itermcolors` importer and a `[theme.overrides]` table. Two deliberate divergences: the config uses
the owner's shape `theme = "kanagawa wave"` rather than a `[theme] name = ...` table, and the
`[theme.overrides]` table is not implemented, so `dtr config set` never needs to walk a three-level
dotted path.

`dtr theme import <file> [--name NAME] [--force]` implements the importer that memo specifies. It is
the first real subcommand rather than another flat flag, because `theme` is a family (`list`, `cp`,
`edit` follow) and a flag per verb would sprawl; the existing flags are unchanged. The plist is read
with `plist` — the memo's one sanctioned dependency — and converted by §6.2's rules verbatim:
`round(c*255)`, the P3→sRGB matrix, and `Calibrated`/absent colour spaces left as sRGB. Alpha follows
§6.3: ignored with one warning per file, except a translucent selection background, which is
composited over the background; absent cursor and selection slots take §5.1's documented fallbacks.
The name is the input file's stem, the output is `Theme::render()` at `<config>/dtr/themes/<name>`,
and an existing file is refused unless `--force`. A real `.itermcolors` fixture in
`crates/dtr/tests/fixtures/` round-trips through `Theme::parse` to the exact RGB bytes; alpha, missing
keys, P3 conversion, a malformed colour key and the overwrite refusal have unit tests.

### M2 — in progress

**The grid is drawn by wgpu now, not by egui.** M0 painted one `text` call per non-empty cell; at 80×24
that is 1920 layout passes a frame and bold/italic were whatever egui's single face could fake. The
renderer is the thing a terminal actually is: a glyph atlas (R8, shelf-packed, 1024²) and one instanced
draw call for the whole screen. Backgrounds and the cursor share that pipeline by sampling a single
white texel at the atlas origin — one pipeline, one draw, no special cases.

Cells are whole-pixel and glyphs land on integer offsets, because a fractional advance puts every glyph
on a half-pixel and the entire screen looks out of focus; that was the difference between the first
attempt and the screenshot that made it into the status report.

Two bugs worth keeping: sampling the white texel at the *centre of the atlas* rather than the centre of
texel (0,0) made every solid quad invisible with no validation error, and `queue.write_texture` on a
1-pixel-wide atlas row is only valid because the row is a single texel.

**The font comes from fontique** (system discovery, DirectWrite on Windows, fontconfig on Linux) with
**swash** rasterising. That means a real system face — DejaVu Sans Mono here — instead of egui's bundled
one, and `font.path` still wins when set. Bold and italic are the next slice: the atlas is keyed by
glyph id and size, not by face, so the face selection has to come first.

**Scrollback you can see.** The wheel scrolls, `Shift+PageUp/PageDown` scrolls a page and `Shift+Home/End`
jump to the ends, and the status bar says `↑ 20 lines` while you are away from the live screen — because
a scrolled terminal that looks exactly like an idle one is a trap. Typing returns to the bottom, since
keys typed while scrolled back are almost always meant for the shell that is waiting there. Scrolling
forces a repaint: a viewport move does not necessarily dirty the render state, and a scroll that does
not repaint looks like a broken wheel.

**Selection is a drag and a wash, not a block.** A press anchors a cell, motion extends the range, and
`Ctrl+Shift+C` copies *the selection* (falling back to the whole screen when there is none). The range
is copied inclusively and cut at the selection's own edges, so a drag that starts mid-line does not
drag in the padding. Hit-testing needs cell metrics, which is why this waited for the renderer.

Drawing it exposed a bug that had been invisible for two commits: the cursor was **never drawn**. Both
the cursor and the selection were solid quads sampling a single white texel written into the atlas at
startup — and that one-byte `write_texture` did not do what it looked like, so every solid element
silently vanished with no validation error. Solids now carry a negative uv and take their colour from
the instance; the shader branches instead of sampling. The lesson is in the code as a comment, because
"invisible rectangle, no error" is a failure mode with no trail to follow.

**The Windows binary runs, and renders identically to the Linux build.** `scripts/wine-smoke.sh` runs the
cross-built PE under Wine, renders a frame offscreen and compares it byte-for-byte with the Linux build's
frame: same fonts, same layout, same colours out of a binary from a different compiler and target. That
is the closest this machine gets to a Windows test — the console path (ConPTY) is still untested, and
Wine cannot compile egui's shader through FXC, so the run forces the Vulkan backend.

**Captures now use the window's colour format.** The offscreen path renders to `Bgra8Unorm` — what a
swapchain actually prefers — and swaps to RGBA on readback, so the format the *window* uses is
exercised by every screenshot instead of being the one path nobody can look at. That is as close as
this machine gets: there is no display to open a window on.

**Importing themes from iTerm2** (`dtr theme import <FILE> [--name] [--force]`) is done: `plist` parses
the XML, the P3 matrix and the alpha policy come from `docs/themes.md` §6.2, and the output is a
Ghostty-format theme in `dtr`'s own themes directory, so an imported theme and a hand-written one are
indistinguishable afterwards. Names cannot escape the themes directory (`..` is rejected), and an
existing file is only overwritten with `--force`.

### M2 — the rest

- wgpu cell renderer behind `GridRenderer`: instanced bg + glyph quads, glyph atlas,
  `egui_wgpu::CallbackTrait`, per-row dirty scissor.
- Font stack: `fontique` (candidate selection + fallback), `rustybuzz` (shaping per style run),
  `swash` (raster, incl. COLR/CBDT emoji); cell metrics from the font, integer-snapped.
- Subpixel/grayscale AA decision; cursor shapes; underline styles.

### M3 — in progress

**SSH is a pty running the system's `ssh -t <host>`.** Not a transport of our own, on purpose: the client
already knows about the config file, the agent, jump hosts, `known_hosts`, host-key prompts and
ControlMaster, and re-implementing that is a project rather than a feature. The palette's `ssh <host>`
and `--ssh <host>` both do it, and the session is a normal shell pane from then on — which means
scrolling, selection, copy and the logbook all work over ssh with no further code. The cost is a
dependency on the platform's client (fine on Linux, present on Windows 10+, and something to check on
a machine that has none).

**A dropped ssh connection reopens itself, in the directory the session was in.** The client exiting is
not the end of the session it was hosting, and the exit status is what says which is which: `ssh` exits
255 when ssh itself failed, so that is a connection that must be remade; any other status is the remote
command's own, which means somebody typed `exit`, and the tab closes exactly as before. A drop keeps
the tab and its scrollback, prints one yellow line of dtr's own (`-- reconnecting to jupyter in
/srv/app --`, because the shell that died cannot say anything and a pane that stops looks like one
waiting for a key) and starts the client again after a backoff — 1 s doubling to 30 s, reset once a
connection has stayed up for 10 s, so a host that is down is not asked once a second and a session that
comes back is not slow for the rest of the day. There is no give-up count: the wait is capped, the
number of attempts is not, because a laptop can be shut for a day and the session it was holding is
still worth reopening when the network returns — a cap would turn that into "closed while you were not
looking". A host that is down at *startup* goes through the same path, which is why the first
connection is opened inside the event loop and a failed `--ssh` is a pane that waits rather than an
error that takes the window with it.

Three things have to happen for any of that to fire, and two of them are not what an unconfigured
`ssh` does. A connect that is refused or unresolvable fails on its own in milliseconds; one into a
blackhole sits in the kernel's `connect` for about two minutes; and a connection that goes away
*without a reset* leaves an `ssh` that is still running and still looking alive, so the pane shows a
session that is not there and the reconnect never fires, because the child never exits.
`pty::Config::ssh` therefore passes `ConnectTimeout=10`, `ServerAliveInterval=20` and
`ServerAliveCountMax=3`, all before the destination: ten seconds to reach the host, a minute of
unanswered keepalives before ssh calls the connection dead and exits 255. OpenSSH's own default is no
keepalive at all, which is why this is not left to the config file; the values override
`~/.ssh/config` and live in one place in `pty.rs`.

Getting back to the directory needs a command, because a fresh `ssh -t host` starts in `$HOME`:
the reopen is `ssh -t host 'cd <quoted> ; exec "${SHELL:-/bin/sh}" -l'`. Quoted for the shell at the
*other* end, because a path may contain the quote the quoting is made of; `;` rather than `&&` so a
directory that has been deleted still leaves a usable shell; `exec`ed so the login shell *is* the
session rather than a child of a wrapper. The path is the session's own OSC 7 report, which is why the
sniffer now returns a report instead of a path: `file://build-host/srv/app` is not a local directory
(nothing here may `cd` into it) but it is exactly where a session on `build-host` belongs.

The terminal modes are cleared on the way, for the case the whole feature exists for: a connection that
drops while vim holds the alternate screen. The text is left alone — the last screen before the drop is
the first thing one wants to read — but the shell that replaces it must not start inside vim's modes.

**Tabs.** Every palette action now opens a *new* session instead of replacing the one you were looking at:
`connect`, `ssh`, `shell`, `replay`, `demo` all push a tab. `Ctrl+Tab` / `Ctrl+Shift+Tab` cycle,
`Ctrl+Shift+W` closes (the last tab closes the window), and the status bar shows `2/3` only once there
is more than one. Only the active tab is drawn, but the others keep running — they keep their own
scrollback, selection and byte counters, which is the whole point of a tab.

The index rules are pure functions with tests, because that is where the off-by-ones live: cycling
wraps both ways, a single tab goes nowhere, closing the last of several moves left, and closing the
only tab ends the window.

**Splits, and a layout that does not care what a pane is.** The tree lives in `layout.rs`: `Leaf(id)` or
`Split { axis, ratio, first, second }`, with `rects(area, gap)` giving every leaf its rectangle. It knows
nothing about terminals. A tab owns the layout plus a map from leaf id to *whatever* — today that map
holds terminal sessions, and a file-explorer pane is a second kind of entry in it rather than surgery on
the geometry. That is why splits went in generic: the widget pane is now a data change.

The renderer draws every pane in one instanced pass. Panes share an instance buffer because a per-pane
draw would need the buffer rewritten between them, and queue writes all land before the command buffer
is submitted — the last pane would win. Overflow is handled by not emitting cells outside a pane's
rectangle, which is cheaper than a scissor and cannot smear into a neighbour.

Click focuses the pane under the pointer, `Ctrl+Shift+\` splits side by side and `Ctrl+Shift+-` splits
stacked, `Ctrl+Shift+W` closes the focused pane and collapses its parent. Closing the last pane closes
the tab. A split opens a *shell*, never a copy of the pane: a serial port cannot be opened twice.

**The file pane is the first non-terminal leaf.** A leaf now maps to
`Pane::Terminal(Session) | Pane::Files(Explorer)`; a file pane owns its rectangle but no session, so
keys, selection, copy/paste, the status strip, splits and closing all have to say what they do when
there is no terminal. The palette's `files` command (and `--files [PATH]` for captures) opens one by
splitting the focused pane side by side. Only the focused leaf gets keys: in a file pane the arrows
move and clamp, `Enter` descends or writes the file's path to the tab's first terminal, `←`/`h`/
Backspace go to the parent, `r` reloads, and `g`/`G` jump to the ends; `Ctrl+Shift+W`, tab switching
and the split chords still work. The listing puts directories first, then case-insensitive names, hides
dotfiles, and inserts `..` at every non-root. A filesystem error is a message in the pane, not a panic
and not a blank rectangle. The egui pane is clipped to its leaf, and the palette is a foreground
overlay so it cannot be covered by a listing. Ordering, selection clamping, parent resolution and
`~` display are pure functions with tests that never touch a disk.

**SFTP is deliberately not here.** The owner's plan is a small file-explorer pane later on, not a
transfer command; `docs/sftp.md` has the measurements for when that happens.

**The mouse is the program's when the program said so, and the terminal's otherwise.** Three gestures
collide in a terminal: this one selects text, a program that asked for the mouse expects clicks, and the
wheel either scrolls the scrollback or is buttons 4 and 5 for the far end. Which one a press means is a
mode (`?9h`, `?1000h`, `?1002h`, `?1003h`, plus `?1005h`/`?1006h`/`?1015h` for the byte form), and a
mode is state of the terminal — so the *encoding* belongs in the pane
(`libghostty_vt::mouse::Encoder`, configured from the terminal per event exactly as the key encoder is)
and the *decision* belongs in the UI. That is the split `Command::Mouse` and `Stats::mouse()` make.

The decision needs one bit the UI cannot read for itself: the terminal is `!Send` and lives on the pane's
thread. `Stats::mouse()` is that bit, published by the same pass that parses a burst of output, so a
click one frame after `set mouse=a` is already the program's. The reverse direction is the same story as
keys: a press the UI handled as a selection can never be handed back, so the flag has to be right
*before* the press, not after it.

Shift is the escape hatch every terminal has, and it is the only reason `set mouse=a` is not a trap:
`mouse_goes_to_program` refuses a captured mouse while shift is held, so a selection inside vim still
works — and a *gesture already started* stays with the program even if shift goes down mid-drag, because
a release the program never hears leaves it dragging for ever.

Three details that are easy to get wrong, all found by test rather than by reading:

- **`Terminal::resize`'s pixel arguments are the *cell's*, not the pane's.** Handing it the pane's size
  made `width_px()` eighty times too large, so the encoder divided every pointer position by 800 and put
  every click in the top-left cell. The command still carries screen pixels (that is what the UI knows
  and what the pty is told), and the pane divides before calling the core.
- **The cell, not the pixel, is what a program can see**, so `set_track_last_cell(true)` makes a drag
  across a row one report per cell instead of one per pixel of motion.
- **X10's release is byte 3, and only normal tracking has one.** `?9h` is presses only, which is why a
  click in `htop` does something and a drag does not; the test pins both, so "the wheel works but the
  click does not" is a five-second diagnosis instead of an afternoon.

The wheel is a press *and* a release of button 4 or 5, because X10 has no wheel event; with shift, or
with nobody tracking, it scrolls the scrollback as it always did. A press outside the pane is not the
program's (a clamped click at the edge would land on the last cell), while a drag that leaves the pane
is — the difference between a wrong click and a selection in vim that freezes at the border.

Verified end to end with nvim rather than only with a byte dump: `a_click_moves_a_real_editor_cursor`
starts `nvim --clean -n -i NONE -c 'set mouse=a'` on a pty, waits for the program to *ask* for the mouse,
clicks the second line five cells in, and asserts the cursor ended up there. That is the test that would
catch a mode the encoder honours but the editor spells differently — and it caught two mistakes of my own
in the test: a click past the end of a short line is clamped by the editor, and nvim's state directory has
to point at the tempdir or it sits at a hit-enter prompt where a click answers the prompt.

**A cursor is a mode, and a mode is not a dirty row.** The core reports both halves: `cursor_viewport`
is *where* the cursor would be, `cursor().visible` is whether it is showing. Drawing the first without
the second put a block in the middle of btop's own screen, because every full-screen program hides the
cursor (`?25l`) and then paints the whole grid itself. That was the visible bug; the one under it was
worse. The render state counts *cells* as dirty and nothing else, so a program that hides the cursor
after drawing — or a search box that shows it again with `?25h` — produced no frame at all, and the old
picture, cursor included, stayed up. The first fix made `note_modes` force a frame; the one that
replaced it is simpler and covers more: `publish` remembers the `Cursor` the last frame carried
(`shown`, threaded through the same argument list as the selection) and publishes when the new one
differs, which is exactly the rule for anything else that is not a cell.

The *shape* falls out of the same comparison. DECSCUSR (`CSI Ps SP q`) is how an editor says "insert"
rather than "replace": nvim sends `6 q` when it enters insert mode and `2 q` when it leaves, and 3/4
ask for an underline. A terminal that draws every one of them as a block is telling the user they are
about to overwrite a character. `CursorShape::mark` turns that into a rectangle — a quarter of the
cell's width against the left edge for a bar, a fifth of its height along the bottom for an underline,
with a one-pixel floor, because a sub-pixel bar draws nothing and the user cannot then tell insert mode
from no cursor at all. The floor was the one thing worth pinning in a unit test; `nvim_insert_mode`
gets a bar through a real editor and back to a block on Escape.

`reset_modes` turns visibility back *on* and the shape back to a block: every other mode in that list is
switched off, but the cursor is the one thing a reconnect has to restore, and a bar belongs to the
insert mode of the editor that dropped rather than to the shell that replaces it.

**"Save as" used to be a text field, and the reason it was one was wrong.** The argument was that `rfd`
would drag GTK into the Linux build and a portal dependency into the one-file Windows exe, so a terminal
whose users know how to type a path should type one. Neither half survived contact with the crates: rfd's
defaults are `xdg-portal` (a D-Bus call to whatever file chooser the desktop already runs, with a `zenity`
fallback when no portal answers) and, on Windows, the Common Item Dialog through `windows-sys` — no GTK, no
extra DLL, and the one-file check still passes. `gtk3` is an opt-in feature nobody here turns on.

The typed field was also the wrong shape of answer. It could not browse, could not show what was already in
a folder, and asked the user to spell a path that the person who *drew* the folder already knows. A picker
hands back a `PathBuf` and with it the platform's own behaviour for overwriting, creating folders and
remembering where you were last time.

Two things the platform dialog forces, and both are in the code rather than in a comment. It **blocks**
until it is answered, so it runs on the files runtime and comes back as `UserEvent::SavePath` — called on
the drawing thread it would freeze the window, and a frozen window with a dialog behind it is
indistinguishable from a crash. And it is **parented** (`set_parent`): an unparented dialog opens behind
the window on Windows and wherever the compositor fancies on Wayland. `rfd` reports a portal that never
answered the same way it reports a cancel — `None` — so `LogTracer` is installed to put rfd's own `log`
complaint in `-v`; a silent button is the one outcome a dialog must not have.

The explorer's per-file download still lands in `~/Downloads` without asking, because that gesture means
"take it" rather than "put it somewhere", and both paths still share the directory so the two ways of
taking a remote file agree.

**Two verbs in one menu, and the type that was missing.** The new-tab menu had grown a run of
`/dev/ttyUSB0` rows and a run of `ssh` rows with nothing between them but a word prefix, so on a machine
with two adapters and three hosts it was a list of five things where two of them opened a serial console
and three opened a shell. They are now grouped behind separators — local shell, ports, hosts — and a group
that is empty takes its separator with it, so nothing changes for a machine with no serial hardware.

Grouping them meant naming them, and that found a bug worth the refactor on its own: the destinations were
`(String, String)` pairs and both the menu *and* the app decided what an entry was by looking at whether
the first field began with `/`. The menu called that "serial"; the app did not look at all — it opened
every entry as `ssh`. So picking a port ran `ssh /dev/ttyUSB0`, and a `Host prod/db` in `~/.ssh/config`
would have been run as a device path. `Destination` is an enum now (`Serial { path, .. }`,
`Ssh { alias, .. }`), which the menu groups by and the app matches on, and a port opens through the
palette's `Action::Connect` so the baud rate and CR translation someone set for that device are the ones
it is opened with.

### M3 — the rest — the rest of a terminal

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

## 13. Tera Term Language (TTL)

dtr is serial-first, and the terminal people actually use around serial hardware is Tera Term —
not for its VT emulation, but for TTL: a small line-oriented language that dials a port, waits for
a prompt, sends a command, parses the answer, logs it and sets an exit status. A terminal that
replaces Tera Term without running TTL inherits none of the scripts. So TTL is a requirement, and
the first deliverable is not an interpreter but the **specification** one can be written against:
`crates/dtr-ttl/MANUAL.md`.

The crate `dtr-ttl` exists with that manual and its design intent in `src/lib.rs`. No interpreter
yet. The manual is distilled from the Tera Term 5 macro reference (196 commands, eight syntax
chapters, five appendices), reorganised into a single narrative: the execution model first, then
lexical structure, types, expressions, statements, and the command reference grouped the way the
upstream index groups it. Every version gate is kept, and §5.5 lists what upstream leaves
*undefined* — division rounding on negatives, 32-bit overflow, `&&` short-circuiting, string
overflow — because those are exactly the places an independent implementation drifts silently.

The families in the manual are the intended module boundaries: a lexer and parser producing a
statement tree, an evaluator owning the four 256-entry variable budgets and the 512 labels, and a
host interface the terminal implements. Five facts shape that interface and are easy to get wrong:

- **`result` is the error channel, and it is not checked for you.** A timed-out wait or a failed
  transfer is a number the next line is expected to read; only interpreter errors stop the run.
  The two mechanisms are separate and the manual keeps them separate (§8).
- **Strings are 511 bytes of UTF-8 with no NUL; a line is 1023 characters and the rest is
  silently dropped.** A `send` of a long line is the usual way to meet the limit.
- **There is no floating point anywhere.** `sprintf` will format floats, but the number arrives
  as a string.
- **`groupmatchstr1`…`9` is the language's only array of fields.** `strsplit` writes it,
  `strjoin` reads it, `strmatch` fills it and `strreplace` clears it.
- **`execcmnd` re-enters the parser with a statement built at run time**, so the parser has to be
  callable from the evaluator, and a macro that pastes device output into it has given the device
  control of the host.

What does not map onto dtr is the interesting part, and it is a list of decisions rather than
work: `connect` and `cygconnect` take a *Tera Term command line* (`connect '/C=1 /BAUD=115200'`),
so dtr needs either that vocabulary or a translation; `callmenu`, `sendkcode`, `loadkeymap` and
`restoresetup` address Tera Term's menus and keyboard setup file, which dtr does not have; and
`getttpos`, `showtt` and `getspecialfolder` are window and shell concepts with no equivalent yet.
They are separable, though: nothing else in the language depends on them, so the first
implementation can refuse them explicitly — a macro that reaches such a command should be told
so, not left to conclude that its menu command worked.

### Where it stands

The whole reading half and most of the running half are implemented. `source` decodes a file
(§3.1) and cuts it into lines (§3.2); `lex` turns a line into tokens (§3.3–§3.7); `line`
classifies each line as one of the four kinds; `program` turns the lines into a flat list of ops
with every jump resolved, which is what §6 needs because `goto` can land in the middle of
anything and a tree cannot express that; `interp` runs the ops with the four 256-entry variable
budgets, the 512 labels, `call`/`return`, `include` as a stack of files, and the system variables
of §4.7; `host` is everything the interpreter does not own — the dialogs of §7.6, `pause`, the
file system, the receive queue §7.1's waits read from — so the same interpreter drives a window
and a recording host.

That boundary was chosen because it is the largest part of the task that can be *checked*
against real macros, and because it is where a TTL implementation usually gets things subtly
wrong: `'abc'#13#10'def'` is one string (§3.7), a sign is an operator and therefore `for i 5 -1`
has two parameters rather than three (§5.4), a `;` inside a quoted string is not a comment, a
`/* */` comment can end mid-line with code after it (§3.3), and `until` repeats while the
condition is *zero* (§6.3).

Two decisions are ours rather than Tera Term's and are written down where they happen: a macro
that executes two million ops stops with `Step limit reached.` instead of running until somebody
closes the window, and `return` with nothing to return from ends the file rather than inventing a
message §8 does not have.

What is left is honest rather than structural: the settings a macro sets and never reads back
(`setbaud`, `logrotate`, `sendkcode`) are recorded and ignored, file transfer is a stub that
reports success, `gettime`'s `<timezone>` parameter is recorded rather than honoured, and the
password store keeps its passwords in memory instead of in Tera Term's encrypted file. The
regular-expression commands are the sharp edge of what is missing: `strmatch` and `regexoption`
are not implemented at all, and `strreplace` and `waitregex` match literally where Tera Term uses
Oniguruma. Nothing in the corpus needs them, which is why they are named here rather than assumed.

**Code pages are a decision, not a gap.** A macro for dtr is a UTF-8 file. A legacy CP932 one
still reads, lexes and runs — its non-ASCII *text* arrives as the bytes it holds (§3.1's ANSI
fallback is byte-preserving), so the two corpus files that print Japanese print mojibake. Shipping
a table for every Windows code page to fix the text of other people's legacy files is not worth
it; the terminal's own text is UTF-8 everywhere.

Nothing in `dtr` calls this crate yet: `Host` has one implementation, the recording one, so the
terminal cannot run a macro until a `Host` over a live pane exists.

### The corpus

Tera Term's own macro tests are vendored at `crates/dtr-ttl/tests/corpus/`: 53 macros, mostly
the reproduction attached to a bug report and named after it, copied verbatim under the
BSD-3-Clause notice in the `NOTICE` beside them. Upstream has no runner and no CI job for them,
and two thirds end in `messagebox`, so they are a checklist rather than a suite — but they are
real TTL, written for Tera Term, and they contain what a synthetic test would not think of: 15
CP932 files, one UTF-16BE, one UTF-16LE, Windows drive letters, `ls` against a live host.

`tests/corpus.rs` asserts that all 53 decode, lex and nest, and that 52 compile to a program with
every jump resolved, pinning the totals — 53 files, 2 485 lines, 1 621 commands — so a scanner
change that silently stops seeing something fails. `tests/run_corpus.rs` goes further: it *runs*
them against a recording host. 47 run to completion, four stop where their own comments say they
should (they are reproductions of Tera Term errors), and one is skipped as too heavy — 110,000
samples for each of 100 buckets. `tests/corpus-dialogs.txt` is the result: every `messagebox` in
the corpus as `title :: message`, generated and then reviewed as data, so a macro that prints a
different number or stops talking is a diff.

That is the argument for making the dialog commands host calls from the start rather than drawing
them from the evaluator: the expected answers were already written in the files.
