# Sessions: a shell, an ssh host, a serial port

The account behind the "Sessions" bullets in [AGENTS.md](../AGENTS.md) — what a session
is, how it ends, what happens when the far end goes away, and what it takes to put one
back. The rules are in `AGENTS.md` so they are loaded before anyone edits `pane.rs`; the
reasoning, the measurements and the things that were tried and were wrong are here.

A session is a pty with a child on it (`crates/dtr/src/pty.rs`), or a serial port, and the
pane thread owns both. Everything below is about the gap between that and a person's
expectation of a terminal window.

## 1. A tab is named by the program, not by the transport

OSC 0 and OSC 2 set the title, `Frame::title` carries it to the strip and to the window
manager, and `1 shell` is only the fallback (`tab_name`). The title is not trustworthy
bytes and not a dirty cell: it is compared with the last published title inside the same
`Displayed` record that holds the cursor, so a program that renames itself and draws
nothing at all still publishes a frame. `clean_title` decodes lossily, drops control
characters and caps the result at 512 characters, because the string arrives from a program
and ends up in a window title.

**A shell that exits closes its tab.** The pane cannot do it — the UI owns the tab — so the
pty reader sets `Pane::ended` on EOF and `App::reap_ended` closes tabs, highest index
first; the last one takes the window. That is correct for one session per tab and wrong the
moment a tab can hold two, which is why `docs/panes.md` §1 is a bug report rather than a
wish: `reap_ended` still closes *tabs* that contain an ended pane, so `exit` in one half of
a split takes the other half with it.

## 2. A drop is not an `exit`, and the exit status is the only thing that tells them apart

`ssh` exits 255 when ssh itself failed and anything else is the remote command's status,
which for an interactive session means somebody typed `exit`. `pane::ending` is that rule:
a drop keeps the tab, says so and reconnects; an `exit` closes it. Getting this wrong in
either direction is bad in a way the user notices immediately — a reconnect loop over a
deliberate `exit`, or a tab that vanishes because a Wi-Fi network hiccuped.

## 3. A client that never speaks is worse than one that fails

A refused connect fails in milliseconds. A blackhole sits in `connect` for two minutes. And
a connection that goes away without a reset — a NAT entry expiring, a host suspended —
leaves an `ssh` that looks perfectly alive to the kernel, so reconnect never fires at all
and the window sits there looking like a terminal that has stopped printing.

So `pty::Config::ssh` passes three options, **all before the destination**:
`ConnectTimeout=10`, `ServerAliveInterval=20`, `ServerAliveCountMax=3`. ssh's own default
is no keepalive at all. These override `~/.ssh/config`, deliberately: the point is a bound
nobody has to have configured. `ssh_args` builds the list and a test asserts that every
flag precedes the alias, because an option written after the destination stops being an
option and becomes a command for the far end.

**An ssh failure's cause is its stderr, and the pane shows it the only way it can.** The
pane's `ssh` runs *on the pty*, so all three descriptors are the terminal: a refusal —
`Permission denied`, `Could not resolve hostname` — is drawn in the grid, above the
reconnect notice, where the person is already looking. What the pane acts on is the status
(`pane::ending`: 255 or a signal is a drop, anything else an `exit`), which is why a message
that has scrolled away still reconnects correctly. The connections *nobody* is watching are
the ones that pipe stderr and quote its last line: `dtr-sftp`'s child
(`crates/dtr-sftp/src/lib.rs`) and the terminfo copy in §4.

## 4. dtr carries `xterm-ghostty` to the far end

A vanilla Ubuntu has no terminfo entry for Ghostty's name — `ncurses-term` does not carry
one — so `tput`, `clear` and `reset` answer `unknown terminal type` in a session that
otherwise looks fine. That is the whole visible symptom of the second ssh defect: the
staircase was the raw pty (see the "tty is the kernel's" bullet in `AGENTS.md`), and this
was the one that survived it.

The entry is `crates/dtr/assets/terminfo/xterm-ghostty`: `tic` source, dumped with
`infocmp -x` from ghostty's own and stamped with a provenance header.
`the_entry_compiles_back_into_ghostty` compiles it and reads it back with `infocmp -x`,
asserting the capabilities a modern program actually asks for (`Smulx`, `Se`, `E3`), so the
file cannot rot into something `tic` accepts and no program recognises.

When an ssh session opens, `crate::terminfo` starts a **second connection beside it**:
`ssh -T`, `BatchMode=yes`, the source on stdin, and a command of
`sh -c '… tic -x -o "${TERMINFO:-$HOME/.terminfo}" -'`.

**The `sh -c` is load bearing.** `sshd` hands a command to the *account's* login shell, and
that shell may be fish, elvish or ion, where `${TERMINFO:-…}` is a parse error and the entry
would have been installed by nobody. `pty::sh_command` is the one wrapper, shared with the
reconnect command below, so a remote script cannot be written for bash and then run by
something else.

It is best effort by design, and every one of those words is a decision:

- **its own thread** — the pane must not wait for it, and closing the tab must not wait
  either;
- **15 s deadline**, after which the child is killed and the log line says `no answer in
  15 s`; the last line of stderr is quoted only when ssh *exited* non-zero, because a
  hang has no message;
- **one log line** either way (`terminfo entry copied` / `could not copy …`), because a
  failure here is not the session's failure;
- **once per host per run on success**, so a reconnect does not repeat it — a failure is
  retried by the next tab, because a connection refused by a locked vault is worth
  another try;
- **`DTR_TERMINFO=0`** to switch it off.

Two honest gaps. A host that wants a password for the extra connection never gets the
entry: `BatchMode=yes` means ssh fails instead of prompting, which is the right trade for a
background copy and leaves `tput` failing. And a command typed in the ~200 ms while both
handshakes run can still see one `unknown terminal type` — a person cannot type that fast,
a `--send` script can. The copy starts *before* the session's connect for exactly that
reason: the two handshakes overlap.

Why not copy the compiled `xterm-ghostty` file instead of the source? Compiled entries are
byte-order dependent, and the far end may be a different architecture than this one. `tic`
source plus a remote `tic` is portable and `tic` ships everywhere; it is the reason the
asset is an `infocmp` dump rather than a binary.

## 5. Retries are bounded in rate, not in number

Waits go 1, 2, 4, 8, 16, 30, 30, … seconds, forever. Closing the tab is how a person gives
up; a cap would turn "laptop shut for a day" into "the tab closed while you were not
looking". The reset needs ten seconds of *uptime*, not merely a successful connect, so a
host that accepts a connection and immediately drops it keeps the long wait instead of
hammering itself.

The child belongs to the reader thread — `wait` blocks, and the exit status is only
available after EOF — and the first connection opens inside the event loop by the same code
as every reopen, because a host that is down at startup should be a pane that waits rather
than a tab that never appears. That is also why `--send` is held in `typed_early` until the
first pty exists.

## 6. A reconnected session is put back where it was

`ssh -t host` starts in `$HOME`, so the reopen adds one:

```
cd <dir> ; exec "${SHELL:-/bin/sh}" -l
```

wrapped in `pty::sh_command` for the reason in §4 — `sshd` gives a command to the account's
login shell and fish or elvish cannot parse that line.

- The directory is quoted by `shlex` for the shell at the *other* end. Any path may hold
  anything but `/` and NUL, and NUL is refused.
- `;` rather than `&&`, so a directory that has since been deleted still leaves a usable
  shell instead of a session that closes on connect.
- `exec`ed, so the login shell *is* the session and there is no wrapper process to explain
  in `ps`.

`a_hostile_directory_is_still_one_word` round-trips a dozen hostile paths through
`shlex::split`, and `a_quote_in_the_directory_survives_the_outer_shell` runs one through
`sh -c` for real — `/tmp/it's here` is the case that proves the nested quoting, because
`shlex` alone says nothing about the outer wrapper.

A session inside tmux is put back by the same `cd` and a different last word
(`exec tmux new-session -A -s <name> -c "$PWD"`), because the directory is not the thing
that was lost; §12 is that account.

## 7. The modes outlive the program that set them

A drop can happen while vim holds the alternate screen with mouse modes on and the kitty
keyboard protocol stacked. Nothing at the far end will undo any of it, and a new shell does
not either — the modes live in the terminal, which is the local one.

So `reset_modes` clears them through the core before the reopen: mouse tracking, the
alternate screen, `DECSCUSR`'s shape and visibility, and the kitty stack (`CSI < 999 u`).
The *text* is left alone, because the last screen before the drop is the first thing a
person wants to read.

### The wait is a state, not a line of output

The wait used to be announced the only way a terminal can announce anything: `-- reconnecting
to jupyter in /srv/app --` written into the grid, right after the text it was interrupting.
It was wrong in three ways, and each of them only shows up once somebody is using the
terminal for work.

**It is history the moment it is printed.** A wait of a second leaves a line in the
scrollback that is true for a second and a lie for the rest of the session; a host that is
down for a minute leaves thirty of them, and the output the person wanted to read is now
thirty lines up. **It reads as output.** Nothing distinguishes it from something the far end
said, and a terminal's job is to be a pipe for what the far end said. And **it cannot change
its mind**: the one thing a person watching a dropped connection wants to know is whether
anything is happening — *is it still trying, and how long until the next attempt?* — and a
printed line cannot answer that, because a line cannot count down.

So the drop is a **state**, not an event. The pane publishes `pane::Retry` — what it is
connecting to, the directory it will come back in, which attempt this is, and either the
moment of the next one, "dialling now", or, for a serial port, "watching for the device" —
and `ui::reconnect` draws it, in one of two shapes.

**Covered**, the notice *is* the pane: its whole rectangle is repainted in the terminal's own
background, and the state is written in the middle of it. That is loud on purpose, because a
drop should be noticed, and a card floating on a dimmed screen would claim something else —
that the terminal is still there with something on top of it, when the true thing is that
this pane is not connected.

**Folded**, it is one bar across the top of the pane: the state in a line, `retry now` and
`close tab` at the right, and everything below it is the terminal's again — scrollback,
selection, copy, hyperlinks. A press anywhere on the notice, or a wheel notch over it, folds
it; pressing the bar unfolds it. That second shape is the one that matters, because the last
screen before a drop is usually the thing a person wants: the stack trace, the command that
was running, the address of the box that just vanished. A notice that covered it until the
host came back would turn an outage into a data loss, and the terminal's own scrollback is
the only copy of it there is.

Both shapes carry the two things worth offering while they are up: **retry now**, which moves
the deadline without resetting the ladder — pressing it twice a minute must not turn the pane
into a one-second dialler — and **close tab**, which is how a person gives up. Both are
`App`'s: the pane can hurry its own wait, and only the app can close a tab. The bar has no
room for the directory or for what the last attempt said, so both live in its tooltip.

Three consequences worth writing down. The countdown is a state that changes with the clock,
so a waiting pane asks for a frame twice a second (`App::about_to_wait`) — a countdown that
only moved when the pointer did would be a still picture pretending to be a timer. A fold
belongs to *one* pane and *one* outage: `App::folded_notices` drops an id as soon as that pane
stops waiting, so the next drop is loud again. And covered, the notice owns the pointer:
`App::left_press` asks `over_egui` before it maps a point to a cell — the same question the
wheel and the link hover already ask — so a press on it, or on a card's button, no longer
starts a selection in the grid behind.

The notice is drawn in `Order::Middle`, so it is above the grid (and above a file pane in the
next leaf) and below every modal: a palette opened over it must not be covered by a notice
about a session. Its rectangle is the *pane's*, so a split says which half is waiting.

## 8. A session knows its directory only because it says so

`/proc` is Linux-only and an ssh session has no local process at all, so `CwdSniffer`
watches for OSC 7 (`ESC ] 7 ; file://host/path`) *before* the VT core sees the bytes, and
keeps the tail across chunk boundaries: an unterminated sequence is held from its
introducer until it completes, with everything else dropped, so a bogus introducer cannot
stall a session (the buffer is capped at 8 KiB; `CARRY_MAX` is `PromptMarks`' limit, a
different scanner with a different job).

The **host** decides which machine the path is on. Empty, `localhost` or this machine's name
is local; anything else is another machine, whose path must never be used as a local path,
or "new tab here" tries to `cd` into a directory on the wrong computer. `Session::remote`
records the ssh case too, and `+` → local shell opens in the focused tab's directory only
when that tab is local.

## 9. A prompt mark is a reference, not a row

OSC 133 is parsed by the core, so `previous-prompt` (`ctrl-shift-up`) walks
`ScrollViewport::DeltaPrompt` and needs nothing from us. What dtr keeps is for *capture*:
`PromptMarks` records each `A` as a `TrackedGridRef`, because a row number goes stale the
moment the block scrolls.

`select_output` is the tempting API and is not what a pager wants: it returns `None` unless
the reference is *inside* the output. `capture-block` (`ctrl-shift-o`) parks the text for
`crate::pager`; without shell integration there are no marks, and the toast says so rather
than copying an empty block.

**fish already speaks OSC 133** — nothing to install. It sends `ESC ] 133 ; A ;
click_events=1 ESC \`, with options after a `;` and ST rather than BEL, which is worth
knowing before writing a sniffer that only looks for BEL. bash and zsh need the snippet
from the shell-integration proposal sourced from their prompt.

The pager itself is a `Label` in a `ScrollArea` and that is the whole design: the text is in
memory, so wrapping, scrolling and mouse selection are egui's. Two costs, both accepted:
the page height a key scrolls by is only known from the *last* frame (remembered, not
measured), and "go to the end" is `f32::MAX` with the scroll area clamping. It is **modal**
because the keyboard is — `j`, `k`, `b` and space are a shell's keys — so `App::on_key`
hands every key to the pager first and returns, including keys it does not understand,
while chords are not pager keys and `ctrl+…` falls through to nothing. The pointer is modal
too, it draws in the foreground `overlay` layer because a file pane is an `Area`, and
closing drops the text.

## 10. Devices are polled, not watched

`serialport::available_ports` every couple of seconds. udev is Linux-only and Windows
enumerates COM ports by a different route, so one mechanism that works on both beats a fast
one that only exists here. The config file used to be watched with `notify`; it is now one
`stat` on a 250 ms deadline, which is cheaper than a watch and portable by construction
(`docs/compio.md` §5).

**`available_ports` lists ports that are not ports.** An x86 kernel creates `/dev/ttyS0..3` —
COM1–COM4's fixed I/O ports — whether or not the board has a serial header, and
`/sys/class/tty/ttySN/type` reads `0` (`PORT_UNKNOWN`, what `setserial` prints as `UART:
unknown`) when the driver found nothing behind one. They arrived in the new-tab menu as four
detail-less rows, and picking one opened a port that fails or waits forever.
`sources::is_a_phantom_com_port` drops a `ttyS` whose `type` is `0` and keeps everything else:
a USB or ACM adapter has no such file, a real 16550A reports `4`, and a host that is not Linux
has no file at all — so the filter can only remove what the kernel itself has said is empty.

Two traps in the transport. **"No bytes" is not "the reader ended"**: the pane keeps the
byte sender alive for reconnects, so `bytes_rx.recv()` never returns `None` and a disconnect
arrives on `closed_rx` instead. And a notice over a transport that cannot disconnect means
`Stats::retry` was set and never cleared — only a drop sets it, and only a connection that
opens clears it.

## 11. A serial read ends when the driver says so

A u-boot `#####` flood crawled on Windows: the screen jumped by fifty lines about three
times a second while the line was delivering four thousand characters a second. The pane's
log said why, and it said it in one line per jump:

```
burst parsed chunks=1 bytes=4096
frame published cells=3200 elapsed_us=77
```

Every read came back with **exactly 4096 bytes**, and the publish that followed cost 77 µs —
so the terminal was not busy, it was *waiting*. At 115200 those 4096 bytes are 355 ms.

The size was not dtr's: it asked for 8192, and asked through `tokio-serial`. Three
measurements on the guest (COM1 fed through libvirt's console pty at 11.4 KB/s) found the
cause, and none of them is in dtr:

| what was measured | result |
| --- | --- |
| `COMMTIMEOUTS` on the handle `mio-serial` opened | `interval=1 ms, multiplier=0, constant=0` |
| `ReadFile` of 256 bytes, those timeouts | 256 bytes, every 21 ms |
| `ReadFile` of 8192 bytes, those timeouts | 8192 bytes, every 710 ms |
| `ReadFile`, `interval=MAXDWORD, totals=0` | whatever is buffered, immediately — 512, 64, 64, **0**, 64 … |

On Windows a read completes when **the requested number of bytes has arrived** or a timeout
elapses, and the 1 ms `ReadIntervalTimeout` only ends a read when the *stream gaps* — which
a flood never does. So the request size is a latency floor of `size / baud`, and the request
size was a whole 8 KB. (The 4096 rather than 8192 is the overlapped path's staging: a
*synchronous* `ReadFile` of 8192 returns all 8192; the same request through tokio's
named-pipe wrapper, which is how tokio-serial reaches a COM handle, comes back at 4 KB.)

On Unix none of this exists: `read()` returns whatever is there, so the same code read
megabytes per chunk and looked fine. That is why this survived until someone ran a
bootloader's progress dots on Windows.

**The fix is a blocking port and a thread.** `serial::Port` is `serialport`'s own handle with
`IDLE_FLUSH` as its read timeout; `pane::spawn_serial_reader` gives one duplicate to a
`dtr-serial` thread and the writer keeps the other. A read now blocks for at most 25 ms and
returns what arrived, so the timeout is the *only* latency knob and it is the same number
that paces the CR flusher. Measured after, on the same two machines:

| | before | after |
| --- | --- | --- |
| Windows, 115200, `#` flood | 3 publishes/s, 4096 B each | ~91 publishes/s, 64 B each (the sender's chunk) |
| Linux, drain rate | 297 MB/s | 568 MB/s |
| Linux, publishes | ~110/s | ~126/s |

The drain rate doubling is the second surprise: a blocking read has no future to build and
no readiness registration per chunk, so the thread that used to be the slow path is the fast
one. `tokio-serial` left the tree with it — 41 lines of `Cargo.lock`, and one fewer place
where "the driver's completion rule" is decided by a wrapper.

Two traps, both now encoded in `serial.rs`'s tests: **`Ok(0)` is not a timeout** — on Unix
it is the line closing, and on Windows `serialport` never reports one, mapping a 0-byte
read to `TimedOut` — so the CR flusher and the disconnect teller read different returns;
and **a settings change still reconnects by stopping the reader**, which is now a flag the
thread polls between reads rather than a cancelled task.


## 12. A drop keeps the tab; tmux keeps the work

Everything above puts the *pane* back: the tab stays, the text stays, the modes are reset,
the directory is restored. None of it puts the *work* back, and none of it can. The remote
shell was a child of the ssh channel, so when the channel died the far end sent `SIGHUP` to
the process group, and a build that was running is gone. Where the terminal state lives is a
property of the machine at the other end, not of anything dtr does here: a mux server on
*this* side keeps the `ssh` client alive for a window restart, and does nothing at all for a
network drop.

So the durability is delegated, to the thing that is already installed. `[tmux] enabled` —
the `tmux.enabled` row in the preferences menu, off by default — puts every ssh tab inside a
tmux session on the far end. Off because it changes what the *other* machine does with a
connection and it needs tmux there, which is not a thing to assume on someone's behalf; the
row is a toggle like every other setting, and a save applies it to the next tab.

```
command -v tmux >/dev/null 2>&1 || exec "${SHELL:-/bin/sh}" -l
infocmp xterm-ghostty >/dev/null 2>&1 || { TERM=xterm-256color; export TERM; }
exec tmux new-session -A -s <name> -c "$PWD"
```

- **One command for both jobs.** `-A` is attach-or-create, so a new tab and a reconnection
  run the same thing; the *name* is the whole difference, and it is the only state the
  reconnection needs. `pty::Config::tmux` carries it beside `remote`, for the same reason:
  both are facts a reopen must not lose.
- **The name is `dtr-N`, the first free one.** Not the tab's title, which belongs to the
  program inside it and changes; not a random token, because the list is something a person
  reads and picks from. `free_name` scans *every* session, not just ours, so a hand-made
  `dtr-2` is not a gap to step into.
- **`-c "$PWD"` and not `-c <dir>`.** The `cd` runs first and is allowed to fail, so the
  directory passed to tmux is always one that exists — `cd` on a missing path leaves `$PWD`
  where the connection started. It matters because a *new* session in a *running* server
  starts at the server's directory, not the client's, which is how "new tab here" would
  otherwise land in the last tab's directory.
- **The `infocmp` line is not belt and braces.** tmux has to *draw* on dtr's terminal, and
  `TERM=xterm-ghostty` is a name the far end may not know yet: the terminfo copy (§4) runs
  beside the session, not before it, so the first tmux tab on a host can arrive before the
  entry does. A name tmux cannot find is `open terminal failed: terminal does not support
  clear` and a dead tab; one check, and the client falls back to a term tmux can draw.
  Inside, panes get tmux's own `default-terminal`, so this only changes what tmux itself
  looks like.

### Choosing the session is a question, asked before the tab exists

A drop that reattaches to a *new* session would be worse than useless, so the tab has to
know which one it is on before it opens:

- **`ssh -T -o BatchMode=yes`, one connection, 8 s.** The same shape as the terminfo copy
  (`pty::batch_args`), and the same trade: a host that wants a password for the extra
  connection, or whose host key is not known yet, does not answer.
- **Markers, not "output is empty".** `tmux list-sessions` prints nothing and exits non-zero
  when there is no server, which is indistinguishable from tmux not being installed, and a
  banner on a command channel is indistinguishable from a listing. So the script says
  `dtr-no-tmux`, or `dtr-listing` … `dtr-end`, and every name carries a `session ` prefix
  so a session literally called `dtr-end` cannot end the listing early.
- **A truncated listing is a failure, not a short list.** A connection that died mid-output
  could hide the very name a new session would take; `-A` turns that into an attach to
  someone else's session, which is the one outcome worth refusing. Silence and half an
  answer both fall back to a plain shell.
- **Three answers, three behaviours**: sessions → a card with `New session` first and the
  names under it (`dtr::tmux::Chooser`); none → `dtr-1`, no card, because the session
  someone would have picked is the one that did not exist; no tmux, or no answer → a plain
  login shell and a toast that says the tab will not survive a drop. The toast is not
  decoration: a silent fallback reads as the feature having worked.

The first connection to a fresh host is the gap: `BatchMode` cannot accept a host key, so
that tab is a plain shell — the tab it opens then *does* accept the key, and the next tab to
that host gets its tmux. `--ssh` at startup is plain too: it has no window yet to hang a
chooser on, and a startup that waits on a network round trip is a startup that hangs.

### What it costs, and what it does not do

Inside tmux, programs see `screen`/`tmux-256color`: the kitty keyboard protocol,
synchronized output, undercurl and the image protocols are filtered by tmux's feature table
before dtr's core ever sees them. That is the price of the durability, and it is the reason
a native mux would exist — not because tmux cannot keep a session, but because it cannot
keep *dtr's* VT.

Two traps, both found the hard way:

- **`-A` attaches, and `-d` does not veto it.** A second `new-session -A -s name -d` on an
  existing session goes down the attach path and fails with `open terminal failed: not a
  terminal` — so a test that wants "did it find it" cannot run the command twice headless
  and check the exit status. It checks that there is still one session, under the same id,
  and that the complaint is not `duplicate session` (which is what a missing `-A` looks
  like, and leaves the count at one, so the count alone cannot tell).
- **A person's `tmux.conf` runs when the server starts.** This machine's has
  `new-session -A -s 0`, so *every* listing has at least one session in it, the chooser
  always appears, and a test that assumes it is alone in the tmux server fails on the
  machine it was written on.

Not done, in the order they would be felt: clicking a session name in the strip is still a
tab; `dtr --server` (a native mux, so the far end needs dtr installed and version-matched)
is a different project with a different security story; and nothing here makes a *local*
shell survive the window closing.

## 13. The shells a machine has

The menu used to offer one row, `local shell`, which opened `$SHELL` — or, on Windows, the
first of `pwsh`, `powershell`, `$COMSPEC`, `cmd` — with a fixed list behind it: `/bin/bash`,
`/bin/sh` and `/usr/bin/fish` there, `pwsh.exe`, `powershell.exe` and `cmd.exe` here, each kept
only if it existed. That is the author's list, not the machine's: a person whose shells are
`/bin/zsh` and `/usr/bin/fish` saw neither, and a container with its shells under
`/usr/local/bin` saw whatever happened to be in `/bin`.

So the list is discovered. Unix has a file for exactly this question — `/etc/shells`, whose
purpose is to say which programs may be a login shell — and it is read comments and all, then
checked line by line: a path nothing answers to is a typo or a garbage-collected store path, not
a shell (this machine's file names `baash`, which turns out to be a real package). Windows has
no such file, so the places a shell is installed are searched — `%COMSPEC%`, the two PowerShells
in `System32` and in `%ProgramFiles%\PowerShell\7`, Git for Windows' `bash.exe`, MSYS2, Cygwin —
and `PATH` is walked for the names a shell has.

Both platforms then fall back to the usual paths (`/bin/sh`, `/usr/bin/zsh`,
`/usr/local/bin/fish`, …) because a container, a chroot or a fresh install may have none of
these lists, and the menu still has to offer something to type in. If nothing at all is found the
old `local shell` row comes back, and it asks the spawner to walk its own candidate list.

Two rules keep the list a menu rather than an inventory:

- **One row per name**, and the first candidate of that name wins: `$SHELL`, then `/etc/shells`,
  then the well-known paths, then `PATH`. So a NixOS profile symlinked into the store and named
  twice in `/etc/shells` is one row, the one the machine means, and a `nix develop` that puts a
  second `bash` first on `PATH` does not get a second row. The name is the executable's without
  the `.exe` Windows builds carry, and it is the whole row: the program is not printed beside it,
  because a store path is fifty characters of menu for a word that already says which shell it is,
  and one row per name means a second row could not be told apart by it anyway. `-v` logs
  `shell started program=…` when a tab opens, which is where "which bash was that" belongs.
- **The first row is the shell a bare `+` starts**, because it is the first candidate
  `pty::candidates()` would spawn. A menu whose first row is not what `Ctrl+Shift+T` does would
  be a lie about the default.

`dtr --shells` prints the same list, one name per line, which is how the Windows guest was
checked and how a person sees what was found without opening the menu.

Not in the list: `wsl.exe`, whose shells belong to another virtual machine and are found by
running `wsl -l` — a process launch inside a scan that runs every two seconds — and anything that
is not a file, since a name is only offered when something answers to it.
