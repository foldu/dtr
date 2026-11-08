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
characters and caps the result at 512 bytes, because the string arrives from a program and
ends up in a window title.

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

**An ssh failure's cause is in its stderr.** With `Stdio::null()` a dead `ssh` reached the
UI as `sender dropped` — true, useless, and indistinguishable from a bug in the pane. So
stderr is piped, quoted, and shown with the exit status. `examples/probe.rs` runs the same
path from a shell without the window; `DTR_SSH_CONFIG=/dev/null` gets past a broken config
and `DTR_SSH_BATCH=0` lets a password prompt be seen rather than silently refused.

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
- **15 s deadline**, then the child is killed and the last line of stderr is logged;
- **one log line** either way (`terminfo entry copied` / `could not copy …`), because a
  failure here is not the session's failure;
- **once per host per run**, so a reconnect does not repeat it;
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

## 7. The modes outlive the program that set them

A drop can happen while vim holds the alternate screen with mouse modes on and the kitty
keyboard protocol stacked. Nothing at the far end will undo any of it, and a new shell does
not either — the modes live in the terminal, which is the local one.

So `reset_modes` clears them through the core before the reopen: mouse tracking, the
alternate screen, `DECSCUSR`'s shape and visibility, and the kitty stack (`CSI < 999 u`).
The *text* is left alone, because the last screen before the drop is the first thing a
person wants to read.

The notice — `-- reconnecting to jupyter in /srv/app --` — is written into the terminal
rather than the pty, because nothing at the other end would read it, and writing it to a pty
that does not exist yet is how the reconnect message used to disappear.

## 8. A session knows its directory only because it says so

`/proc` is Linux-only and an ssh session has no local process at all, so `CwdSniffer`
watches for OSC 7 (`ESC ] 7 ; file://host/path`) *before* the VT core sees the bytes, and
keeps the tail across chunk boundaries: the introducer is held back, prefix included,
until it either completes or is flushed past `CARRY_MAX`, so a bogus introducer cannot stall
a session forever.

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
one that only exists here. Files differ: `~/.ssh/config` is watched with `notify`, because
that mechanism *is* portable.

Two traps in the transport. **"No bytes" is not "the reader ended"**: the pane keeps the
byte sender alive for reconnects, so `bytes_rx.recv()` never returns `None` and a disconnect
arrives on `closed_rx` instead. And a serial port renders `reconnecting…` when it cannot
disconnect, which means `Stats::connected` was left at its `Default` — only the serial
reader ever turns it false.
