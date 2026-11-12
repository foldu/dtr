# compio instead of tokio — decision memo

Status: measured, 2026-11-12 · Scope: whether the pane's runtime should be a completion-based
one, what that would do to the serial transport, and — because that is what the question turns
into — the process's thread count and the one dependency that looks replaceable.

**Not implemented, and not because it is impossible.** compio builds for both of dtr's targets
and a COM port is expressible in about thirty lines of its public API (§3.1, §3.2). It was
rejected as a *fix*: the read granularity that made a u-boot `#####` flood crawl is
`serial.sys`'s, and compio delivers it unchanged (§3.3). The bug was fixed by the read timeout
and a thread ([`docs/sessions.md` §11](sessions.md)), one commit before this memo.

Question: dtr drives one OS thread per pane with a current-thread tokio runtime, a `LocalSet`
and a single `select!`. A completion-based runtime matches "one thread per pane" more closely
than a poll-based one, and its Windows story is IOCP rather than tokio-serial's named-pipe
shim. Is it worth replacing tokio, and would it have fixed the serial flood? Two follow-ups are
answered here as well: what the thread count actually is (§4), and whether a compio watcher
could replace `notify` (§5).

---

## 1. Recommendation

**Keep tokio.** The two reasons the question was asked both dissolved under measurement:

- **The flood was not the runtime.** On Windows a `ReadFile` completes when the requested
  number of bytes has arrived or a timeout elapses; the same compio op, on the same port,
  returned exactly 8192 bytes every ~700 ms under `mio-serial`'s timeouts, and returned
  immediately with what was queued under a total timeout (§3.3). Completion-based I/O is
  faithful about *who decides* a read is finished: the driver does.
- **The pane loop's async surface is small.** `pane.rs` is 4768 lines with 19 `.await`s, four
  `spawn_local`s and one `select!`; the UI thread is synchronous winit; the pty transport is
  already an OS thread. The runtime is not where the complexity lives.

**If it is ever revisited, take the serial transport first.** `serial::Port` is already the
seam: one type, one `Read`/`Write` impl, one thread behind it. A compio backend for that seam
is §3.2's listing, and it would prove the runtime on the one transport where completion-based
I/O is a natural fit — without touching the pane loop, the VFS, or the UI.

**Measured against the thread count, the case is narrower than it feels** (§4): dtr's own is
eight threads for one shell pane, and two per pane after that, of which a completion runtime
removes exactly one — the parked read. It shortens no read (§3.3), and the pty's thread only
goes if the pty becomes ours (§3.5). What remains is cancellation and a smaller per-pane count
on Windows: real, and both smaller than "no more threads".

**Switch when** any of these becomes true: (a) the pane loop needs cheap cancellation of many
concurrent ops (compio's `Submit` cancels on drop; tokio needs the op re-armed); (b) dtr grows
I/O that a poll reactor handles badly — thousands of files, or a transport per pane beyond the
two it has; (c) tokio-serial's Windows shim breaks in a way we cannot reach around, which is
what nearly happened here.

---

## 2. Options matrix

| # | Approach | Model | Windows | Serial port | Cost to adopt | Verdict |
|---|---|---|---|---|---|---|
| 1 | **keep tokio**, serial on a thread | poll-based, readiness | named-pipe shim for COM (avoided: we bypass it) | our own `serialport` + thread | none (done) | **chosen** |
| 2 | compio for the serial transport only | completion (io_uring/IOCP) | **compio_driver::op::Read → overlapped ReadFile** | ours: `CreateFileW` + `attach` + `op::Read` | ~30 lines + one dependency graph | runner-up: do this to evaluate |
| 3 | compio for the whole pane and app | completion | IOCP, first class | as above | every I/O site reshaped to owned buffers; zbus and `tokio::process` move; vfs/sftp rewritten | not now |
| 4 | mio + hand-rolled reactor | readiness | `mio::windows::NamedPipe` is what tokio-serial already does | same shim, same trap | reimplements tokio for less | no |

---

## 3. Verified facts

Measured on jupiter, 2026-11-12: compio 0.19.2 (`runtime`, `macros`), `windows-sys` 0.61.2,
rustup stable 1.99.1 with `x86_64-pc-windows-gnu`, `cargo zigbuild` 0.23.4 + zig 0.17.0; the
Windows guest is the `winvm` VM, and COM1 was fed at 11.4 KB/s through libvirt's console pty.

### 3.1 It builds for our release target

```
$ cargo zigbuild --release --target x86_64-pc-windows-gnu
    Finished `release` profile [optimized] target(s) in 0.21s
$ ls -l target/x86_64-pc-windows-gnu/release/compio_probe.exe
-rwxr-xr-x 1,223,680 compio_probe.exe
```

The probe then **ran** in the guest: the IOCP runtime started, `CreateFileW` on `\\.\COM1`
succeeded, `Runtime::attach` accepted the handle, and reads completed. compio's docs.rs builds
only advertise `x86_64-pc-windows-msvc`; our release exe is **gnu** (zig cc, mingw import
libraries), so this was the gating unknown. It is answered for a probe — not yet for the whole
of dtr, which also links libghostty and mimalloc on that target.

### 3.2 A COM port is expressible, without a fork

`compio_driver::op::Read<T, S: AsFd>` implements `OpCode` for Windows as an overlapped
`ReadFile` on the raw handle (`src/sys/op/general/iocp.rs`), and `compio::runtime::submit` is a
public escape hatch for any `OpCode`. So the transport is our own `CreateFileW` (with
`FILE_FLAG_OVERLAPPED`) plus the port's `COMMTIMEOUTS`, wrapped in
`SharedFd<OwnedFd>`, attached to the runtime and read through `op::Read`. The listing is in
§9.1; it is the ~30 lines that ran in the guest.

Two API details worth writing down, both found the hard way:

- **the current-runtime shortcut does not work outside `block_on`**: `compio_runtime::submit`
  panics with "not in a compio runtime" if the call is an argument to
  `runtime.block_on(...)` (it is evaluated first). Use `Runtime::submit` as a method, or
  `block_on(async { submit(op).await })`;
- **`Submit<T>`'s output is `BufResult<usize, T>`** — the buffer is still inside the op, so the
  bytes come from `result.1.into_inner()` (`compio::buf::IntoInner`), not from the tuple.

### 3.3 The runtime does not decide when a serial read is finished — the driver does

Same probe, same port, two `COMMTIMEOUTS` settings. Six rounds of one, then four of the other,
with `#` arriving continuously:

| `COMMTIMEOUTS` | what the compio read returned |
|---|---|
| `interval=MAXDWORD, mult=MAXDWORD, const=25` | `0 ms -> 64 bytes`, then `0 ms -> 576 bytes` ×5 (whatever was queued) |
| `interval=1, mult=0, const=0` (what `mio-serial` installs) | `667 ms -> 8192`, `697 ms -> 8192`, `697 ms`, `698 ms` — exactly the count, every time |

That second row *is* the original bug, reproduced by a completion-based runtime: a `#` flood at
115200 arrives as 8 KB lumps every ~0.7 s, which is what a pane that asked for 8 KB saw
(`docs/sessions.md` §11: 4096-byte lumps through tokio's named-pipe wrapper, 355 ms each).

The first row is the fix, and it is a `COMMTIMEOUTS` value, not a runtime. dtr now waits with
`serialport`'s `set_timeout(IDLE_FLUSH)` on a thread; the same numbers come out.

So: **compio would not have fixed it, and tokio did not cause it.** The wrapper tokio-serial put
between dtr and the handle (a COM handle dressed as a named pipe, with mio's 1 ms inter-byte
timeout) made a bad choice on our behalf, which is an argument against that *crate*, not
against poll-based I/O.

### 3.4 What the rest of the tree would cost

| seam | today | under compio |
|---|---|---|
| pane loop | 1 `select!`, 4 `spawn_local`, 19 awaits | `compio::runtime` + its own `select`; timers move to `compio::time` |
| app workers | 3 tokio `Runtime`s, `spawn_blocking` | `Runtime`/`AsyncifyPool`; 18 awaits |
| `dtr-vfs`, `dtr-sftp` | async `Backend`, `tokio::fs`/`process` | same shape, compio types; `io_uring`/IOCP fs |
| zbus (`system.rs`) | deliberately on the app's tokio runtime (`Cargo.toml`) | zbus is runtime-agnostic: `zruntime`, or a blocking connection on a thread |
| every I/O call site | `read(&mut buf).await` | `read(buf).await -> BufResult(n, op)`, buffer owned by the op |
| tests | 6 `#[tokio::test]` | `#[compio::test]` |
| dependency graph | tokio (already in the tree) | +87 packages for `compio` with `runtime` |

`Cargo.lock` gained 87 packages for `compio = { features = ["runtime", "macros"] }`, and that
is the *small* configuration: `fs`, `process`, `net` and the tls/ws/quic add-ons are separate
crates with their own versions (`compio-fs 0.12`, `compio-driver 0.12`, …). compio is
pre-1.0 and its modules version independently.

### 3.5 What the Linux side would look like

Untested here, and it is the reason §1 says "take the serial transport first" rather than "port
the panes": on Linux a serial port or a pty is a tty, and an io_uring read on a tty commonly
retires into an io-wq worker — a blocking thread — so the completion-based path buys less than
it does on Windows, where the handle is a real completion object. Keeping `portable-pty` would
keep the pty reader thread whatever the runtime does; dropping the *thread* means owning the
pty — `openpty`/`fork`/`TIOCSCTTY` on Unix, `CreatePseudoConsole` and its pipes on Windows —
which is the cost of the one thread §4 counts.

---

## 4. The thread census, and what a completion runtime would remove

Measured 2026-11-12, one shell pane with the window idle: thread names and kernel wait sites
from `/proc/<pid>/task/*/{comm,wchan}`.

| thread | waiting in | what it is |
|---|---|---|
| `dtr` | `do_epoll_wait` | winit's X11 loop |
| `dtr-pane` | `do_epoll_wait` | the pane's tokio `LocalSet`, idle |
| `dtr-pty` | `wait_woken` | a blocking `read` on the pty master |
| `dtr-scan` ×2 | `epoll` + `futex` | 1 worker, 1 blocking thread (`available_ports`) |
| `dtr-files` ×2 | `epoll` + `futex` | 2 workers, created at boot, unused by a terminal |
| `notify-rs inoti` | `do_epoll_wait` | notify's own `mio::Poll` (§5) |

77 threads exist in that process and **8 are dtr's**; the other 69 belong to the software
rasteriser — 32 `llvmpipe-N`, a larger anonymous pool named after the process, and its WSI
threads — which a machine with a GPU does not create. So the number is a census, not a scandal:
8 with one shell pane, 9 once a config directory exists (a second notify watcher, which this
machine lacks), and one more pair per pane — 14 with four serial panes. The pair is the design
(`Terminal` is `!Send`, one OS thread, no locks), and one of the two is the parked read.

**`spawn` accepts `!Send` futures**, which is what makes the pane expressible at all:

```rust
// compio-runtime-0.12.6/src/lib.rs:221, compio-executor-0.1.4/src/lib.rs:179
pub fn spawn<F: Future + 'static>(&self, future: F) -> JoinHandle<F::Output>
```

No `Send` bound, and the executor's own comment says why ("Executor cannot be sent to ther
thread"). The `LocalSet` is therefore not a thing to port: the pane becomes a `Runtime` and its
tasks, and a `Submit<Read>` on the pty or serial handle replaces the reader thread. §3.2 proved
the read half of that in the guest; the `!Send` half is this signature.

So **a completion runtime removes one thread per pane** — the parked read — and nothing else
that is dtr's: 8 → 7 with one shell pane, 14 → 10 with four serial panes. It does not remove
the main thread, one thread per pane, one to drive the completion port, notify's loop (§5), the
blocking `available_ports` call, or anything the driver started.

Two consequences worth writing down before anyone counts threads again.

- **`IDLE_FLUSH` survives the port.** §3.3 is that measurement: a completion port shortens no
  read, it only stops a thread from waiting on one. What the thread buys in exchange is
  *cancellation* — dropping the `Submit` cancels the overlapped `ReadFile`, so a closing tab
  stops paying the 25 ms — microseconds of a tab close, not a fix for a bug.
- **The cheapest three threads are not compio's.** `dtr-scan` and `dtr-files` are two tokio
  runtimes built at boot (3–4 threads) to hold one blocking enumeration and a file pane most
  terminals never open. One lazy, shared background runtime is a smaller change than a runtime
  swap and takes 4 → 1; with it the numbers above become **8 → 4** and **14 → 7**.

---

## 5. A watcher is not something compio can hand you — and dtr does not need one

The question that follows ("then what replaces `notify`?") has a factual answer, and then a
design one.

**There is no compio file watcher.** `cargo search compio` lists 219 crates and none of them
watch files: the compio org ships fs, net, process, signal, timer, dispatch, quic, tls, log,
buf, macros, and `compio-term` — which sounds like it and is a crossterm-shaped *event reader
for the terminal you are running in* (stdin events, raw mode, a command queue), not a directory
watcher, and not a pty spawner either. The async watchers that do exist are `tokio-inotify` and
`async-inotify`, both Linux-only, and everything portable — `wtr-watcher`, the
`notify-debouncer-*` family, `cargo-watch` — is `notify` underneath.

**Writing one is supported, and short on Windows.** `compio_driver`'s `OpCode` is a public
`unsafe trait` whose Windows shape is an `OVERLAPPED` and a completion routine:

```rust
// compio-driver-0.12.5/src/sys/driver/iocp/op.rs
unsafe trait OpCode {
    type Control: Default;
    fn op_type(&self, control: &Self::Control) -> OpType { OpType::Overlapped }
    unsafe fn operate(&mut self, control: &mut Self::Control,
                      optr: *mut OVERLAPPED) -> Poll<io::Result<usize>>;
    fn cancel(&mut self, control: &mut Self::Control, optr: *mut OVERLAPPED) -> io::Result<()> { … }
}
// compio-runtime-0.12.6/src/lib.rs:291 — "You only need this when authoring your own OpCode."
pub fn submit<T: OpCode + 'static>(&self, op: T) -> Submit<T>
```

notify's Windows backend is those calls plus two things compio deletes: it stashes its own
`Box<OVERLAPPED>` in `hEvent`, passes `Some(handle_event)` as the completion routine, then parks
a thread on `WaitForSingleObjectEx` over a `CreateSemaphoreW`. As an `OpCode` the first two stay
and the last two go — about 80 lines. Linux is the fiddly half: inotify is a readable fd, and
under the polling backend a custom op's `pre_submit`/`operate` is a readiness-then-syscall
`Decision`, which fits; under io_uring you would want a `POLL_ADD`-shaped op rather than
guessing what `op::Read` does with a pollable, non-regular fd (§6).

**But dtr should stop watching instead.** Two `NonRecursive` watches, one access-event filter
and a 150 ms settler is the whole of notify's work here:

- **`/dev` is redundant on every platform.** `available_ports` is already polled every two
  seconds (`App::request_scan`), and that poll is the only thing that finds a device on
  Windows. The watch only makes *Linux* discovery up to two seconds faster.
- **`~/.ssh/config` is one `stat`.** The same shape as `scan_due`: a deadline in
  `about_to_wait`, ~250 ms, on mtime and size, feeding the existing `Settle`. The
  directory-mtime trick already in `watch::paths` covers a file that does not exist yet and an
  editor's rename-over.

That deletes `notify`, and with it `inotify`, `inotify-sys`, `notify-types` and `kqueue` — all
four have notify as their only dependent in `Cargo.lock`; `walkdir` stays, `jni` wants it — and
two threads, for no new crate. The price is bounded lag: ≤250 ms before a saved config is
re-read, ≤2 s before a hotplugged port appears, and the second of those is already the Windows
behaviour.

---

## 6. What this memo does not cover

- **A compio build of dtr itself.** The probe links no libghostty, no wgpu, no mimalloc. Those
  are the ones with the awkward mingw link steps (`docs` and `flake.nix`: the
  `libsynchronization.a` stub, `CFLAGS_x86_64_pc_windows_gnu`, `PKG_CONFIG_PATH=""`), and they
  are the next thing that would break.
- **`compio::process` for sftp, and `compio::fs` for the VFS.** Both exist; neither was tried.
- **A pty under compio.** `compio-term` is not one (§5): it reads the terminal dtr is *in*.
  The pane's own pty would still be `portable-pty` and its thread (§3.5), or hand-rolled
  `openpty`/`ConPTY`.
- **An inotify fd under io_uring.** §5 needs to know whether a `POLL_ADD`-shaped op is
  required; that is a probe, not a paragraph.
- **The pty on Linux through io_uring.** §3.5 is a prediction from how ttys behave, not a
  measurement.
- **Latency, not just granularity.** The probe prints "blocked N ms" per read; it does not
  measure key-to-pixel latency, and the guest has no GPU, so every draw there is ~330 ms of
  WARP — a number that says nothing about a real machine.

---

## 7. Design sketch, if it were taken further

1. **A compio backend for `serial::Port`.** Same type, same `Read`/`Write` impls, a
   `#[cfg]`-selected body: the thread for `tokio` builds, a `SharedFd` + `op::Read` for a
   compio one. `serial.rs`'s tests (a real descriptor, asserting "what arrived, not what was
   asked for") carry over unchanged, which is the point of the seam.
2. **A benchmark gate** before anything else moves: `scripts/bench.sh` numbers on Linux, and
   the guest's publish cadence for a serial flood. dtr's own `-v` line
   (`burst parsed chunks= bytes=`) is the instrument that found this bug; it is the one that
   would judge a runtime swap.
3. **Only then** the pane loop. The `select!` becomes compio's, the timers move, and
   `tokio::sync::mpsc` becomes whatever the runtime offers — the one place where the change is
   structural rather than mechanical. The reader threads go with it, which needs two decisions
   taken first, while the runtime is still tokio because they are cheaper to make there: the
   pty becomes ours (§3.5), and the watcher is already gone (§5).

---

## 8. Risks that outlive the measurement

- **Pre-1.0, single maintainer, independently versioned modules.** dtr's pane loop would
  depend on `compio-runtime 0.12`'s behaviour for its event loop; a semver-minor change there
  is a rewrite of `select!`-shaped code.
- **Our Windows target is outside its CI matrix.** §3.1 built, but nothing pins it: a
  `windows-sys` bump could compile on msvc and not on gnu, and we would find out at release
  time.
- **Two runtimes during any transition.** zbus borrows a tokio runtime today (a measured
  choice: 77 lock entries against 86, `docs/gtk-palette.md` §6). The census (§4) weakens the
  "second reactor" objection: the process already runs a tokio runtime per pane, two app
  workers and notify's `mio` loop, so the cost of a transition is the migration surface, not
  plurality. The line that must hold is one *transport* per seam — a `#[cfg]`-selected serial
  backend (§7), not a bridge between both runtimes.

---

## 9. Appendix — what produced §3

### 9.1 The probe

`Cargo.toml`:

```toml
[dependencies]
compio = { version = "0.19.2", features = ["runtime", "macros"] }

[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.61", features = [
    "Win32_Foundation",
    "Win32_Storage_FileSystem",
    "Win32_Devices_Communication",
] }
```

`src/main.rs` (windows-only body; the rest is nothing):

```rust
use compio::buf::IntoInner;
use compio::driver::{op, OwnedFd, SharedFd};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::time::Instant;
use windows_sys::Win32::Devices::Communication::{SetCommTimeouts, COMMTIMEOUTS};
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
};

fn open(path: &str) -> OwnedHandle {
    let name: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            0x8000_0000 | 0x4000_0000, // GENERIC_READ | GENERIC_WRITE
            0,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
            std::ptr::null_mut(),
        )
    };
    assert!(handle != INVALID_HANDLE_VALUE, "{}", std::io::Error::last_os_error());
    unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) }
}

fn set_timeouts(handle: RawHandle, interval: u32, multiplier: u32, constant: u32) {
    let timeouts = COMMTIMEOUTS {
        ReadIntervalTimeout: interval,
        ReadTotalTimeoutMultiplier: multiplier,
        ReadTotalTimeoutConstant: constant,
        WriteTotalTimeoutMultiplier: 0,
        WriteTotalTimeoutConstant: 0,
    };
    assert!(unsafe { SetCommTimeouts(handle as _, &timeouts) } != 0);
}

fn main() {
    let runtime = compio::runtime::Runtime::new().expect("compio runtime");
    let port = open("\\\\.\\COM1");
    let handle = port.as_raw_handle();
    // serialport's 25 ms read timeout: "count satisfied or the constant elapses"
    set_timeouts(handle, u32::MAX, u32::MAX, 25);
    runtime.attach(handle as _).ok(); // the handle must join the IOCP
    let fd = SharedFd::new(OwnedFd::from(port));

    let mut rounds = vec![(u32::MAX, u32::MAX, 25); 6];
    rounds.extend([(1, 0, 0); 4]); // what mio-serial installs
    for (interval, multiplier, constant) in rounds {
        set_timeouts(handle, interval, multiplier, constant);
        let started = Instant::now();
        let op = op::Read::new(fd.clone(), vec![0u8; 8192]);
        let result = runtime.block_on(async { compio::runtime::submit(op).await });
        let n = result.0.unwrap_or(0);
        let buffer = result.1.into_inner();
        println!(
            "interval={interval} mult={multiplier} const={constant}: blocked {} ms -> {n} bytes ({:?})",
            started.elapsed().as_millis(),
            &buffer[..n.min(8)],
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
```

### 9.2 Build, feed, run

```sh
# toolchain: rustup stable with the GNU Windows target, inside the devShell for zig
rustup toolchain install stable --profile minimal --target x86_64-pc-windows-gnu
cargo zigbuild --release --target x86_64-pc-windows-gnu

# the guest's COM1 is a libvirt pty; feed it from the host while the probe runs
python3 /tmp/feed.py 11520 40 64 | python3 /tmp/ptyrun.py virsh --connect qemu:///system console win &
winvm ssh -- cmd /c 'C:\dtr\compio_probe.exe'
```

The feed is 64-byte writes at 11 520 B/s (115200 8N1) and the pty runner gives `virsh console`
the tty it insists on; without it, `virsh console` refuses to run with piped stdin.
