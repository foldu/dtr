# What the TTL interpreter does not do yet

A snapshot of the gap between `crates/dtr-ttl` and Tera Term's macro language as
`crates/dtr-ttl/MANUAL.md` documents it. It exists so that "it runs the corpus"
is not mistaken for "it implements TTL", and so the next person does not have to
re-derive the list by diffing §7 against `src/commands.rs`.

The short version: the language is implemented and the corpus runs. What is
missing is the wiring into the terminal, regular expressions, file transfer, and
a long tail of commands that mean nothing outside Tera Term's own window.

## Reminder: ask for real macros

> **The corpus is upstream's bug reproductions, not a working set.** It proves
> the interpreter survives real TTL syntax; it does not prove it does what a
> person's script needs, because nobody's job depends on those 53 files. Every
> decision below would be better informed by ten macros that somebody actually
> runs — from a serial device, a firmware flash, a login sequence, a bench rig.
>
> Ask the human who owns this repository to drop real `.ttl` files, with a note
> on what each one is for, into `crates/dtr-ttl/tests/field/`. They are not
> vendored code: they are the specification of what "works" means here. A test
> beside `run_corpus.rs` can run them the same way (recording host, dialogs as
> assertions) as soon as there are any.

Until then, "works" is measured by §7 of the manual and by upstream's
reproductions, which is exactly the state this file describes.

## 1. Nothing in `dtr` runs a macro

The crate is complete enough to drive a terminal, and the terminal is not wired
to it.

- `Host` has one implementation, `RecordingHost`, in `src/host.rs`. There is no
  `Host` over a live pane: no `send`/`recv` into a session, no dialogs as
  windows, no `fileopen` over the VFS.
- `crates/dtr` does not depend on `dtr-ttl`.
- There is no CLI (a `--macro FILE` alongside `--shell`, `--send`, `--keys`), no
  place to set `param1`…`paramN`, and no way to say what `connect '/C=3
  /BAUD=115200'` means — that vocabulary has to be translated by the host.

This is the gap that makes the rest academic: everything else only matters
because a macro should run in dtr.

## 2. Regular expressions are absent

- `strmatch` and `regexoption` are **not implemented**: a macro that uses them
  stops with `Unknown command.`
- `strreplace` and `waitregex` match **literally** where Tera Term uses
  Oniguruma. So `strreplace` has no capture groups and a pattern like
  `temp=([0-9]+)` never matches.

`strmatch` is how a real macro parses device output, so this is the sharpest
functional edge. No corpus macro uses it, which is why nothing forced it.

## 3. Commands that do not exist (48)

| Family | Commands | Why |
| --- | --- | --- |
| transfer | `bplussend`, `bplusrecv`, `kmtsend`, `kmtrecv`, `kmtget`, `kmtfinish`, `scpsend`, `scprecv`, `recvfile` | needs a link layer; `xmodem`/`ymodem`/`zmodem` exist only as success stubs |
| checksums over files | `checksum8file`, `checksum16file`, `checksum32file`, `crc16`, `crc16file`, `crc32`, `crc32file` | the string forms and `checksum8/16/32` work |
| clipboard | `clipb2var`, `var2clipb` | the clipboard belongs to the window |
| Tera Term's own UI | `clearscreen`, `gettitle`, `settitle`, `hide`, `minimize`, `maximize`, `showtt`, `getttpos`, `setdebug`, `loadkeymap`, `restoresetup` | dtr has no Tera Term menu, setup file or keyboard file |
| serial lines | `setdtr`, `setrts`, `setspeed`, `getmodemstatus` | needs a real port behind the host |
| clock | `settime`, `setdate` | setting the system clock is not a terminal's job |
| files | `filelock`, `fileunlock`, `filestrseek`, `filestrseek2` | |
| network | `gethostname`, `getipv4addr`, `getipv6addr` | |
| logging | `logstart`, `logpause`, `logautoclosemode` | `logopen`/`logwrite`/`logrotate`/`logclose` are recorded stubs |
| waiting on the link | `wait4all`, `waitevent` | need a link lifecycle to wait on |

A macro reaching any of these gets `Unknown command.` and stops, which is §8's
answer and the right one — a macro told its menu command worked is worse.

## 4. Accepted and recorded, but with no effect (23)

`changedir`, `enablekeyb`, `setecho`, `setsync`, `setmulticastname`,
`setflowctrl`, `setserialdelaychar`, `setserialdelayline`, `sendkcode`,
`getfileattr`, `setfileattr`, `folderdelete`, `logopen`, `logwrite`, `logrotate`,
`logclose`, `loginfo`, and `xmodemsend`/`xmodemrecv`/`ymodemsend`/`ymodemrecv`/
`zmodemsend`/`zmodemrecv`.

They land in one `setting()` arm in `src/commands.rs`, which records the call on
the host and moves on. That is deliberate — a macro sets these and never reads
them back, and refusing them would stop scripts that only want their settings
set — but it is not implementation, and the transfer stubs report success
without transferring.

## 5. Implemented, but shorter than the manual

- `sendln` appends CR LF; §7.1 says the terminal's newline, which is a setting.
- `sendfile` ignores `<binary>` 0 (no CR → CR LF, no control-character removal).
- `filestat` fills mtime and drive with zero.
- `gettime`/`getdate` record `<timezone>` instead of honouring it.
- `sprintf` has no `%a`/`%A` and no `'` flag; `%g` is approximate.
- `wait`/`waitln` matching lives in the `Host`, so "a whole line" and pattern
  priority are the host's business, and `RecordingHost` answers from script.
- `listbox` ignores its 5.3+ keyword parameters (`dblclick=on`,
  `listboxsize=WxH`).
- A dialog's obsolete `<special>` parameter is accepted, not expanded.
- `execcmnd` parses a fresh program, so `goto`/`call` to the enclosing file's
  labels do not resolve.
- `include` path resolution is the host's; there is no relative-to-the-including-
  file rule.
- `connect` passes Tera Term's command line through opaquely.
- `getver` answers `5.8.0` so version gates behave.
- `random` is an LCG, not Tera Term's sequence (undocumented upstream anyway).
- The password store is plaintext in memory, not Tera Term's encrypted file.

## 6. Deliberate divergences

Both are written where they happen and are not planned for change:

- A macro that executes 2 000 000 ops stops with `Step limit reached.`
  (Tera Term would run until somebody closed the window).
- `return` with nothing to return from ends the file, rather than inventing a
  message §8 does not have.
- §3.1's ANSI fallback is byte-preserving and there is no code-page decoder: a
  macro for dtr is UTF-8, and a legacy CP932 file still reads and runs with its
  non-ASCII text arriving as the bytes it holds.

## Suggested order

1. **A `Host` over a live pane, and `dtr --macro FILE`.** Until this exists
   nothing above can be discovered in use, and the shape of what matters may
   change once real macros run.
2. **`strmatch`, then real `strreplace`/`waitregex`.** The one functional gap
   that stops ordinary parsing macros.
3. **`wait4all`/`waitevent`, `filestrseek`, `filelock`, the `…file` checksums.**
   Cheap, and they fill gaps a working script trips over.
4. **Transfer** (`scpsend`/`scprecv`, xmodem), once there is a link that can
   carry it.
5. **Tera Term's own UI commands.** Many will resolve to nothing in dtr; what
   dtr does with `loadkeymap` or `restoresetup` is a product question before it
   is a parsing one.
