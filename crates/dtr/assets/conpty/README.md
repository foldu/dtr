# The bundled ConPTY

`conpty.dll` and `OpenConsole.exe` are Microsoft Windows Terminal's ConPTY host,
taken unmodified from the official `Microsoft.Windows.Console.ConPTY` NuGet
package and carried inside `dtr.exe` (see `crates/dtr/src/conpty.rs`).

[`UPSTREAM`](UPSTREAM) is the machine-readable provenance — the release tag, the
package name and URL, and the SHA-256 of the package and of both files — and the
suite checks it still describes the bytes beside it. `LICENSE` beside this file
is the package's: MIT, © Microsoft.

**Why.** Inbox ConPTY is a terminal emulator in the middle of a shell pane: it
renders the child's output and re-emits VT to dtr, consuming the unknown APC
sequences the kitty graphics protocol rides in. The bundled host forwards them —
29 of 29 `ESC _ G` chunks arrived verbatim in the A/B measurement — and
`portable-pty` already prefers a `conpty.dll` on the DLL search path, so carrying
these two files is the whole fix. `docs/windows.md` §12 has the measurement and
the captures.

**Why both files.** `conpty.dll` starts `OpenConsole.exe` from its own directory,
so they are unpacked together and a missing half is a broken ConPTY.

**Updating.** From the repository root:

```sh
just conpty latest        # or: just conpty v1.25.3131.0
```

The recipe asks that release for its `Microsoft.Windows.Console.ConPTY` asset,
unpacks the two x64 members, refuses anything that is not a PE image, replaces
the files, rewrites `UPSTREAM`, and bumps `VERSION` in `crates/dtr/src/conpty.rs`
(the directory the files are unpacked into is named after it, so an upgrade
cannot reuse an old host). Running it for the version already vendored changes
nothing, so it doubles as the check. Then `just fmt && just test conpty` and
commit the assets with the code.

There is deliberately no submodule and no build: Microsoft's package is the
build artifact, and rebuilding Windows Terminal ourselves would mean an MSVC
toolchain for two files that ship from the same CI that produces Windows
Terminal itself.
