# SFTP integration — decision memo

Status: research, 2026-10-27 · Scope: *how* SFTP gets into `dtr`, not the implementation.

**Implemented as recommended.** `crates/dtr-sftp` spawns `ssh -s <alias>` and drives
`russh-sftp` over the child's stdio — §1's choice, and neither runner-up. This memo stays
the place to read for *why*, because the measurements in §3 are what decided it.

Question: dtr needs file transfer over SSH, sharing authentication with the SSH terminal
session where possible, without breaking the Windows single-`.exe` rule (PLAN §7) or the
cross-build (§5.3). This memo picks an approach and records what was actually measured.

---

## 1. Recommendation

**Spawn the host's own `ssh -s <alias> sftp` and drive `russh-sftp`'s client over the child's
stdin/stdout.** dtr contains no SSH implementation at all: keys, agent, `known_hosts`,
`~/.ssh/config` (`Include`, `ProxyJump`, `Match`), and host-key prompts are whatever the user's
`ssh` already does, and the only new code is a ~60-line `AsyncRead + AsyncWrite` adapter plus
the transfer UI.

**Runner-up: in-process `russh` + `russh-sftp`** — one TCP connection, many channels, so a
shell and a file browser genuinely share a session on every platform including Windows, with
token-by-token control of auth and host-key UX.

**Switch to the runner-up when** any one of these becomes true: (a) Windows must share one
connection between shell and transfers — `ControlMaster` is unimplemented there (§3.4), so the
spawned-`ssh` design falls back to two connections; (b) dtr must work on hosts without an
`ssh` binary; (c) host-key and password prompts must be rendered inside the dtr window rather
than on an inherited console. The cost of switching is one crate behind the same trait, and the
`russh-sftp` protocol layer is reused either way — `SftpSession::new` takes any stream, so the
UI and transfer code do not change.

---

## 2. Options matrix

| # | Approach | Auth | Connection sharing with the terminal | Deps / license | Windows | Effort | Verdict |
|---|---|---|---|---|---|---|---|
| **2** | **Spawn `ssh -s host sftp`, speak SFTP over stdio** | Whatever `ssh` does: keys, agent, `ProxyJump`, password/askpass, host keys — for free | **Unix: yes, via `ControlMaster`** (measured, 20.6 ms vs 211.5 ms). **Windows: no** (mux unimplemented) → second connection, prompt-free with agent auth | `russh-sftp` 3.0.1, Apache-2.0, **42 crates, zero `-sys`/C**. Plus the host's `ssh` binary | Needs `ssh.exe` on the box (default since 10 1809). Single-`.exe` untouched: nothing new is linked in | ~3–5 d | **Chosen** |
| 1 | In-process `russh` + `russh-sftp` | Ours to build: key files, agent (Pageant/OpenSSH agent), encrypted-key passphrases, `known_hosts` verification UI, password prompts | **Yes everywhere**: one `russh` connection, `channel_open_session()` per channel | `russh` 0.65.0 Apache-2.0; **default features pull `aws-lc-rs`** (C + asm) and `ssh-key =0.7.0-rc.11`; large dependency tree | Works, but the crypto backend is the riskiest thing in the tree to cross-build | 2–4 wk | Runner-up |
| 3 | Spawn `sftp`/`scp`, scrape text | Reuses `ssh`, but a *second* auth per transfer unless muxed | No (separate process per command) | none | Needs `sftp.exe`; output parsing | ~3 d | **Reject.** `sftp -b` gives no machine-readable progress, no byte-accurate cancel, no per-file status, and stderr/stdout interleaving is the only error channel. It converts a solved protocol problem back into a parsing problem. |
| 4 | `ssh2` (libssh2) | libssh2's own: keys, agent, password; **no OpenSSH config, no ProxyJump, no `known_hosts` interoperability** | No | MIT OR Apache-2.0 wrapper, **BSD-3 libssh2**, vendored C via `libssh2-sys` + `libz-sys`, `pkg-config`, `cc` | Builds, but needs a cross C toolchain; OpenSSL or `openssl-on-win32` | 2–4 wk | **Reject.** All the work of (1), fewer features, plus a C dependency on the cross path. |
| 5a | `openssh-sftp-client` over the same spawned `ssh` | same as (2) | same as (2) | MIT, pure Rust; takes `(stdin, stdout)` as separate halves | same as (2) | ~3–5 d | Viable **swap** for the protocol layer inside (2); see §3.7 |
| 5b | `ControlMaster` multiplexing | — | — | — | — | — | **Not an approach — a complement to (2).** Adopt on Unix, unavailable on Windows |
| 5c | Separate helper binary | — | — | — | violates "no bundled external binaries" | — | **Reject** on the single-`.exe` rule |

Effort figures are for a working, tested M3 slice with progress and cancel — not for polish.

---

## 3. Verified facts

Everything in §3.1–§3.6 was **run on this machine** (Linux, OpenSSH 10.6p1, rustc 1.98.1).
§3.7–§3.9 is **read** from vendored crate sources, the crates.io API, or upstream docs, and is
labelled as such. Scratch projects live in `/tmp/sftpprobe/`; nothing was added to the repo.

Host facts: `ssh -V` → `OpenSSH_10.6p1, OpenSSL 3.5.9 29 Sep 2026`; `sshd` active on `:22`;
`/etc/ssh/sshd_config` contains `Subsystem sftp …/libexec/sftp-server`, so a real SFTP server
was available for every test below.

### 3.1 `ssh -s host sftp` is a clean byte pipe — **ran it**

`ssh -s localhost sftp`, stdin/stdout piped, stderr inherited: the SFTP handshake completes and
every operation works. The entire adapter is one struct:

```rust
struct ChildStream { stdin: ChildStdin, stdout: ChildStdout, _child: Child }
// AsyncRead  -> polls stdout
// AsyncWrite -> polls stdin
// Unpin + Send + 'static follow from the tokio types
```

Full probe output against `localhost` (`/tmp/sftpprobe/sfptest/src/main.rs`):

```
[handshake] SftpSession::new ok in 205.862685ms
[canonicalize] . => "/home/barnabas"
[progress] 524288/2000000 bytes
[progress] 1048576/2000000 bytes
[progress] 1572864/2000000 bytes
[write] 2000000 bytes in 7.901015ms (241.40 MiB/s)
[stat] len=2000000 is_regular=true perms=Some(33188) mtime=Some(1793116178)
[read] 2000000 bytes in 6.904551ms, identical=true
[chunked-read] 2000000 bytes
[rename] exists(old)=false exists(new)=true
[list] 468 entries in /home/barnabas
[remove] exists after remove = false
[fs_info] statvfs supported = Ok(true)
```

So: list, stat, read, write, rename, remove all work over a spawned `ssh`, and a 64 KiB chunk
loop gives byte-accurate progress (the `[progress]` lines are emitted from that loop, which is
exactly what a UI progress bar consumes). 241 MiB/s is loopback with no encryption bottleneck
worth measuring further.

### 3.2 `russh-sftp` takes any stream — **ran it, and read the signature**

This is the load-bearing API fact. `russh-sftp` 3.0.1, `src/client/session.rs:38`:

```rust
pub async fn new<S>(stream: S) -> SftpResult<Self>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
```

and the lower-level entry point, `src/client/mod.rs`:

```rust
pub fn run<S, H>(stream: S, handler: H) -> mpsc::UnboundedSender<Bytes>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
          H: Handler + Send + 'static,
```

No `russh` types in either signature — the crate-level docs say it outright: *"Crate can
provide compatibility with anything that can provide the raw data stream in and out of the
subsystem channel."* The `russh` dev-dependency is used only by the examples.

### 3.3 The operations dtr needs — **read from the vendored 3.0.1 source**

```rust
pub async fn open<T: Into<String>>(&self, filename: T) -> SftpResult<File>          // :98
pub async fn create<T: Into<String>>(&self, filename: T) -> SftpResult<File>        // :105
pub async fn read<P: Into<String>>(&self, path: P) -> SftpResult<Vec<u8>>           // :152
pub async fn write<P: Into<String>>(&self, path: P, data: &[u8]) -> SftpResult<()>  // :163
pub async fn try_exists<P: Into<String>>(&self, path: P) -> SftpResult<bool>        // :171
pub async fn read_dir<P: Into<String>>(&self, path: P) -> SftpResult<ReadDir>       // :180
pub async fn remove_file<T: Into<String>>(&self, filename: T) -> SftpResult<()>     // :225
pub async fn rename<O, N>(&self, oldpath: O, newpath: N) -> SftpResult<()>          // :230
pub async fn metadata<P: Into<String>>(&self, path: P) -> SftpResult<Metadata>      // :248
pub async fn canonicalize<T: Into<String>>(&self, path: T) -> SftpResult<String>    // :135
pub async fn create_dir<T: Into<String>>(&self, path: T) -> SftpResult<()>          // :144
pub async fn fs_info<P: Into<String>>(&self, path: P) -> SftpResult<Option<Statvfs>>// :279
```

Notes that matter for the UI:

- **`ReadDir` is an eager `Iterator`, not a `Stream`** (`src/client/fs/dir.rs`): `read_dir`
  loops `readdir` until `EOF` and returns a `VecDeque`. A directory listing is therefore one
  round trip and one blocking await — fine for a picker, but do not call it on a huge directory
  from the frame loop.
- **Order is not sorted and not stable**: each batch is prepended
  (`files = new.chain(files).collect()`), so batches come out newest-first. Sort client-side.
- **`Metadata` is `FileAttributes`** (`src/client/fs/mod.rs`): `size: Option<u64>`,
  `permissions: Option<u32>`, `mtime: Option<u32>` (seconds — SFTP v3), with `len()`,
  `file_type()`, `is_dir()`, `is_regular()`. `DirEntry` gives `file_name()`, `path()`,
  `metadata()`, `file_type()`.
- **`File` is the streaming handle**: `impl AsyncRead` (:282), `impl AsyncWrite` (:358),
  `impl AsyncSeek` (:299), plus `async fn close(self)` (:222). `read`/`write` on the session
  buffer the whole file in memory — use `open`/`create` + chunked copy for anything large.
- **Cancel is per-transfer and safe**: `Drop for File` fires `close_nowait`, and dropping one
  `File` leaves the session usable. Verified (`/tmp/sftpprobe/sfptest/src/bin/cancel.rs`):

  ```
  [cancel] target /home/barnabas/Firmware 20.0.1.zip
  [cancel] dropped File after 3.165099ms
  [cancel] session still works: len=355997604
  [cancel] re-listed 467 entries
  ```

  Because the close is fire-and-forget, a cancelled **upload** can leave a partial remote file.
  Upload via `.part` + `rename`, never in place.

### 3.4 Connection sharing: `ControlMaster` works, `-s` rides it — **ran it**

`ssh -M -S <sock> -N -f` master, then an interactive command and the SFTP probe over the same
socket:

```
master: srw------- /tmp/sftpprobe/mux.sock
Master running (pid=6)
SHELL_ON_SAME_MASTER
[handshake] SftpSession::new ok in 20.643848ms     <- over the existing master
[handshake] SftpSession::new ok in 211.524466ms    <- fresh connection, same host
Master running (pid=6)
```

One TCP connection and one authentication served a shell and an SFTP subsystem channel; the
master survived both clients. The 10× handshake difference is the authentication round trip
that a second connection would have to repeat.

**Windows has no equivalent.** PowerShell/Win32-OpenSSH issue *Support for Control Master* is
**still open** — created 2019-01-23, last updated 2026-07-14, labelled `0 - Backlog` — so
`ssh -M`/`ControlPath` multiplexing is not available there
([#1328](https://github.com/PowerShell/Win32-OpenSSH/issues/1328)). This is the single biggest
difference between the two platforms for this feature, and it is why §5 falls back to two
connections on Windows.

### 3.5 It composes with the pane's `!Send` LocalSet topology — **ran it**

PLAN §2.5 puts each pane on a current-thread runtime + `LocalSet`. The SFTP client is `Send`
and needs no `LocalSet`, but it *does* work inside one. Probe
(`/tmp/sftpprobe/sfptest/src/bin/local.rs`) keeps an `Rc` alive across session creation and
moves the session into a `spawn_local` task:

```
[local] session ok; !Send marker alive: pane-terminal-stand-in
[local] listed 467 entries under /home/barnabas
[local] home is_dir=true
[cancel] leftover ssh sftp children: 0
```

The last line matters: dropping the session tears down the child `ssh` (it sees EOF on stdin
and exits). No orphan processes after a cancelled or failed transfer.

### 3.6 Failure modes — **ran them**

| Situation | What actually happens |
|---|---|
| Remote has no `sftp` subsystem | `ssh` prints `subsystem request failed on channel 0`, exits 255; `SftpSession::new` returns `UnexpectedBehavior("sender dropped")` after ~190 ms. **The protocol error carries no useful cause** — capture `ssh`'s stderr into a ring buffer and show it. |
| `ssh` not on `PATH` | `Command::spawn` → `ErrorKind::NotFound`, `raw_os_error = Some(2)` — detected before any protocol work, so the file panel can be disabled with an explanation. |
| Bad `ssh` config | Real example from this host: `Bad owner or permissions on …/20-systemd-ssh-proxy.conf` makes **every** `ssh` invocation fail. Invisible unless stderr is captured. |

### 3.7 Crate health — **crates.io API + GitHub API, fetched 2026-10-27**

| Crate | Latest | Published | License | Downloads (total / recent) | Repo |
|---|---|---|---|---|---|
| `russh-sftp` | **3.0.1** | 2026-09-28 | Apache-2.0 | 4.14 M / 1.91 M | `AspectUnk/russh-sftp`, 118★, last commit 2026-10-01, 11 open issues |
| `russh` | **0.65.0** | 2026-10-25 | Apache-2.0 | 9.14 M / 3.92 M | `Eugeny/russh` (crates.io still points at `warp-tech/russh`, which redirects), 1 913★, active 2026-10-25 |
| `ssh2` | **0.9.6** | 2026-06-30 | MIT OR Apache-2.0 | 8.44 M / 1.28 M | `rust-lang/ssh2-rs` (moved from `alexcrichton/ssh2-rs`), 570★, active 2026-10-26 |
| `openssh-sftp-client` | **0.15.10** | 2026-10-01 | MIT | 2.63 M / 0.27 M | `openssh-rust/openssh-sftp-client`, 63★, last commit 2026-10-15 |

All four are alive and permissively licensed. `russh-sftp` is a small project (118 stars) but
well-used and releasing steadily (2.3.0 → 3.0.1 in five months). If it stalls, the escape hatch
is `openssh-sftp-client` over the same spawned `ssh` — the transport decision survives.

Dependency weight, measured with `cargo tree` on the probe:

- `russh-sftp` → **42 dependency crates**, **zero `-sys` crates, zero C/`cc`/`cmake` build steps**.
  Licenses are all permissive (`Apache-2.0`, `MIT`, `MIT OR Apache-2.0`, `Unicode-3.0` for
  `unicode-ident`); no GPL/AGPL and no `NOTICE` obligation — the crate ships only `LICENSE`.
- `russh` → its default feature set is `["flate2", "aws-lc-rs", "rsa"]`, i.e. **`aws-lc-rs`
  (C + assembly crypto) is on by default**, and its dependency list pins pre-releases
  (`ssh-key =0.7.0-rc.11`, `rsa =0.10.0-rc.18`, `pkcs1 =0.8.0-rc.4`). A pure-Rust `ring`
  backend exists but is opt-in, and dropping defaults also drops RSA support.
- `ssh2` → `libssh2-sys` 0.3.3, which **vendors the libssh2 C sources and builds them with
  `cc::Build`**, probing `pkg-config` for a system libssh2 first, and pulls `libz-sys`.

### 3.8 `ssh -s` semantics — **read** (`ssh(1)`, OpenBSD-current, 2026-10-13)

> `-s` — May be used to request invocation of a subsystem on the remote system. Subsystems
> facilitate the use of SSH as a secure transport for other applications (e.g. `sftp(1)`). The
> subsystem is specified as the remote command.

The same `-s` flag is documented in `ssh_config(5)`'s `SessionType`, and the subsystem request
is a protocol-level `SSH_MSG_CHANNEL_REQUEST` — nothing about it is platform-specific. What
*is* platform-specific on Windows is multiplexing (§3.4) and askpass (§4).

### 3.9 Credential prompting — **partially verified; see §4.1**

`ssh(1)` documents two non-tty paths: `SSH_ASKPASS` together with `DISPLAY`, and
`SSH_ASKPASS_REQUIRE=force`, which uses the askpass program "regardless of whether `DISPLAY`
is set". On this host, **neither fired**: with stdin a pipe and no tty, a forced
keyboard-interactive attempt and an encrypted-key passphrase both failed with
`Permission denied (publickey,keyboard-interactive)` and **no askpass invocation**
(`/tmp/sftpprobe/askpass.log` was never created). The local `ssh` is a genuine ELF binary from
nixpkgs, not a wrapper, but this host also has a broken system `ssh_config` include, and the
operator reports askpass is unreliable here. I am therefore **not** claiming askpass works —
see §4.1.

---

## 4. What I could not verify

| # | Unverified | Why not | Cheapest experiment that settles it |
|---|---|---|---|
| 4.1 | That `ssh` reaches `SSH_ASKPASS` reliably (the "one password prompt" story for password auth) | This host has no account that accepts password/keyboard-interactive auth (`sshd` advertises the method but rejects it), and the local OpenSSH never invoked askpass even for a passphrase-protected key. Operator reports askpass is broken on this system. | A container/VM with `PasswordAuthentication yes` and one throwaway user; run the child with stdin a pipe, `SSH_ASKPASS_REQUIRE=force`, and log invocations. Repeat on Windows 10 and 11 — [#2115](https://github.com/PowerShell/Win32-OpenSSH/issues/2115) (closed 2023) was exactly this bug. |
| 4.2 | That the SFTP path cross-compiles and links for `x86_64-pc-windows-gnu` | No Windows `rust-std` in this sandbox: `rustup target add x86_64-pc-windows-gnu` failed with `Read-only file system` on the rustup home, and `cargo-zigbuild` is not installed here. | In the flake devShell (which already has rust-windows + zig 0.17): `cargo zigbuild --target x86_64-pc-windows-gnu --release` on the 20-line probe, then `objdump -p` the `.exe` and diff the DLL list against the §7 allowlist. |
| 4.3 | That `ssh -s host sftp` behaves identically on Win32-OpenSSH | No Windows machine. `-s` is a documented client flag and a protocol-level request (§3.8), so a behavioural difference would be a bug, not a design property. | Same Windows VM: run the probe and confirm the handshake completes — the first bytes on the child's stdout are the SFTP VERSION packet (`00 00 00 05 01 …`), not a shell prompt. |
| 4.4 | Behaviour against servers that speak only SFTP v0/v1, or that lack the `limits@openssh.com`/`statvfs@openssh.com` extensions | Only OpenSSH's own server (v3 + extensions) was reachable | Point the probe at an old embedded device or a non-OpenSSH server; `SftpSession::new` negotiates the version and `fs_info`/`expand_path` already return `None` when an extension is absent |
| 4.5 | Real-world throughput/latency over a WAN, and behaviour with `ProxyJump` | Localhost only | Point the probe at a real jump-hosted target; `time` the handshake and a 100 MB transfer |
| 4.6 | Whether `aws-lc-rs` can be cross-built with zig for the single-`.exe` | Not installed, and it needs cmake/perl-ish tooling plus target asm; out of scope for a memo that rejects approach 1 | `cargo zigbuild` on a `russh` hello-world with defaults vs `default-features = false, features = ["ring", "rsa"]` |

Note on the environment, not the design: this host's system `ssh_config` include chain is
broken (`Bad owner or permissions … 20-systemd-ssh-proxy.conf`), so every `ssh` here must be
given `-F <file>` to bypass it. All successful runs above used an explicit `-F`.

---

## 5. Design sketch

### 5.1 New crate: `crates/dtr-sftp`

```
crates/dtr-sftp/
  src/lib.rs        Session, SftpCommand, SftpEvent, HostSpec
  src/child.rs      ChildStream adapter + spawn_ssh() + stderr ring buffer
  src/ops.rs        list/stat/download/upload/rename/delete, all chunked
  src/master.rs     ControlMaster discovery/creation (unix; no-op on windows)
```

Dependencies: `russh-sftp`, `tokio` (`process`, `io-util`, `rt`, `sync`, `time`), `dtr-sshconfig`,
`thiserror`, `tracing`. No `egui`, no `winit`, no `libghostty-vt` — so it is testable with
`cargo test` and no window, which is PLAN §2.2's criterion for extracting a crate. Integration
tests can run against a real `sshd` when one is reachable (as here) and against an in-memory
`tokio::io::duplex` driving a mock SFTP server otherwise (the crate ships a server
implementation, so a mock is cheap).

### 5.2 Where the connection lives

Not on the pane thread. The pane's runtime exists to protect a `!Send` `Terminal`; SFTP has no
such constraint (`SftpSession` is `Send + Sync`, verified by static assertion in
`/tmp/sftpprobe/sfptest/src/bin/sendcheck.rs`) and a 2 GB transfer must never be able to stall
the emulation loop. Shape, mirroring `pane.rs`:

```
UI thread ──mpsc──▶ dtr-sftp worker thread (current_thread runtime)
          ◀──crossbeam── SftpEvent::{Listed, Progress, Done, Failed}
```

One worker per window, not per pane: the file browser is a window-level tool, and on Unix all
its transfers share one `ControlMaster` per host anyway.

### 5.3 Auth sharing with the SSH terminal

- **Host selection** comes from `dtr-sshconfig` for *display* (picker list, `User`, `Port`,
  `IdentityFile`) but the **alias is passed to `ssh` unresolved**. `dtr-sshconfig` deliberately
  does not implement token expansion, `Match` evaluation or `CanonicalizeHostname`; letting
  `ssh` re-read `~/.ssh/config` means `ProxyJump`, `Include`, `Match` and every keyword we do
  not model still work. Do not pre-resolve into flags — that silently drops jump hosts.
- **Unix**: dtr owns a master per alias, spawned as a child (not `-f`) so it can be killed on
  exit and its stderr captured:

  ```
  ssh -M -N -o ControlPath=<state>/mux/%C -o ControlPersist=300 <alias>
  ```

  The terminal pane and the SFTP worker both connect with `-o ControlPath=<state>/mux/%C`.
  Every channel after the first is silent. `%C` keeps the socket path short (the `sun_path`
  limit is ~104 bytes) and unique per host/port/user.
- **The master needs a tty, or askpass.** `ssh` reads passwords and key passphrases from
  `/dev/tty`; a child spawned with `Stdio::piped()` has none, and this host demonstrated the
  consequence (§3.9: it fails without ever invoking askpass). So spawn the master on a **pty**
  — the same `portable-pty` crate the ConPTY milestone needs, not a new dependency class — and
  either surface its prompt text as an in-app dialog or attach the pty to a hidden terminal.
  This is also the shape the terminal session itself uses, which is why the master and the
  shell can be the same process when a shell is already open: **the ideal path is that the
  terminal pane's `ssh -M` creates the master and SFTP merely joins it**, with `ControlPersist`
  keeping it alive across shell exit. The pty master is the fallback for "user opened Files
  first".
- **Windows**: no master. Open a plain second connection with `-o BatchMode=yes` and
  `-o NumberOfPasswordPrompts=0`. With key/agent auth this is invisible — no prompt to share —
  at the cost of one extra handshake (measured 211 ms locally). With password auth it means a
  second prompt, and since askpass is unverified (§4.1) **M3 should declare password auth
  unsupported for SFTP on Windows** and say so in the UI rather than fail mysteriously.

### 5.4 UI

First slice (M3), one pane, remote only:

- **Path bar + list**: name, kind, size, mtime, mode; sorted client-side; hidden-file toggle;
  refresh; breadcrumb. `read_dir` is one round trip, so refresh-on-demand, not on a timer.
- **Transfer queue**: one row per file with `bytes/total`, rate, elapsed; states
  queued/running/done/failed/cancelled. Progress comes from the chunk loop (§3.1); cancel is
  "drop the `File`", which we verified leaves the session healthy (§3.3).
- **Safety**: upload to `<name>.part` then `rename`; download to a temp file then rename; a
  cancelled upload therefore never destroys the previous version.
- **Errors**: show the last few lines of the child's stderr alongside the protocol error — the
  protocol error alone is `sender dropped` (§3.6).
- **Entry point**: a "Files" button on an SSH session, opening beside the terminal, not a
  separate window.

Later (M4+): dual pane with a local side (plain `std::fs`, no SSH involved), drag-and-drop
between panes, recursive transfer with a directory tree, resume/retry, permissions and
ownership editing, symlink handling, per-host bookmarks. Dual-pane is a UI decision, not a
protocol one — it needs nothing new from `dtr-sftp`.

### 5.5 Milestones

PLAN §3's M3 currently lists ConPTY, tabs/splits and IME; SFTP is not in it. Suggested split if
SFTP joins that milestone:

- **M3**: `dtr-sftp` + browse/download/upload + progress/cancel + ControlMaster on Unix;
  Windows: independent connection, agent auth only, `ssh`-absent and no-subsystem states
  detected and explained.
- **M4**: dual pane, drag-and-drop, recursive transfers, resume.
- **Trigger for revisit**: if Windows users need one shared connection, or host-key prompts
  must be in-window → swap the transport for `russh` behind the same `dtr-sftp` API (§1).

---

## 6. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| **`ssh` absent** (minimal Windows images, locked-down PATH) | SFTP silently unavailable | Probe `PATH` **and** `%SystemRoot%\System32\OpenSSH\ssh.exe`; if neither, disable the panel with an explanation. `NotFound` is cheap to detect (§3.6) |
| **No `ControlMaster` on Windows** (§3.4) | Two connections, two auths for password users | Agent auth makes it invisible; declare password auth unsupported on Windows for M3; document the one-connection advantage as the reason to revisit `russh` |
| **A piped child `ssh` cannot prompt** (no tty) | Password/passphrase auth fails with a bare `Permission denied` | Spawn the master on a pty (§5.3); treat its prompt as UI. Never rely on askpass as the only path |
| **askpass unreliable** (§4.1) | The "one prompt" promise breaks for password auth | Do not depend on askpass for the common path; prefer agent/keys; if we do add it, pass `SSH_ASKPASS_REQUIRE=force` and the path to **our own exe** (`current_exe()` + `--ssh-askpass`) — that keeps the single-`.exe` rule intact, since no sidecar binary is added |
| **`ssh` child writes to stderr and nobody reads it** | Pipe fills, child blocks, transfer hangs | Drain stderr into a bounded ring buffer on a dedicated task, always — for progress and for diagnosis |
| **Windows console window flash** when spawning `ssh.exe` from a GUI process | Ugly, looks like a crash | `std::os::windows::process::CommandExt::creation_flags(CREATE_NO_WINDOW)` on the child |
| **Broken user `ssh_config`** (hit on this host) | Every connection fails with an unrelated-looking protocol error | Surface stderr; offer `-F none` as a diagnostic toggle |
| **`russh-sftp` is a small project** (118★, 11 open issues) | A protocol bug we cannot fix quickly | Transport is one trait wide; `openssh-sftp-client` (MIT) is a drop-in swap over the same spawned `ssh`; worst case, shell out to `sftp` (§2 row 3) for one release |
| **Eager `read_dir`** on a directory with 100 k entries | UI stall on one await | Cap entries or move to `RawSftpSession::readdir` paging if it ever bites; not an M3 problem |
| **Single executable** (PLAN §7) | — | **Unaffected**: `russh-sftp` is pure Rust with no `-sys`/C, nothing new is linked, and `ssh.exe` is a *host* dependency (like the user's shell), not a bundled one. Keep the `checks.single-exe` DLL allowlist as-is — it should not change |
| **Cross-build** (PLAN §5.3) | — | **Low**: no new C toolchain requirement, no new pkg-config module, no build scripts. Must still be *proved* by building the Windows artefact in CI (§4.2), because "pure Rust" is not the same as "links for windows-gnu" |
| **Licensing** | — | `russh-sftp` is Apache-2.0 (no `NOTICE`, `LICENSE` only) → add one row to `THIRD_PARTY_LICENSES.txt` (PLAN §8) and keep the Apache text. `dtr` stays MIT OR Unlicense |
| **Cancelled upload leaves a partial file** (fire-and-forget close, §3.3) | Data loss if written in place | Always `.part` + `rename` |

---

## Appendix — commands that produced §3

Scratch workspace: `/tmp/sftpprobe/` (throwaway; nothing in the repo changed).

```sh
# crates.io metadata
curl -s https://crates.io/api/v1/crates/russh-sftp -H 'User-Agent: dtr-research'

# full probe: spawn ssh -s localhost sftp, drive russh-sftp over stdio
cd /tmp/sftpprobe/sfptest && cargo run --bin sfptest

# same, under a current-thread runtime + LocalSet (dtr's pane topology)
cargo run --bin local

# cancel one transfer, keep the session
cargo run --bin cancel

# connection sharing
ssh -F cfg3 -M -S /tmp/sftpprobe/mux.sock -N -f localhost
DTR_SSH_EXTRA="-o ControlPath=/tmp/sftpprobe/mux.sock" cargo run --bin sfptest

# failure paths
ssh -F cfg2 -s localhost nosuchsubsystem      # -> "subsystem request failed on channel 0", rc 255
cargo run --bin missing                       # -> kind=NotFound raw_os_error=Some(2)
```
