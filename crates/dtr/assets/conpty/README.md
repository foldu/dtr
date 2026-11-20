# The bundled ConPTY

`conpty.dll` and `OpenConsole.exe` are Microsoft Windows Terminal's ConPTY host,
taken unmodified from the official `Microsoft.Windows.Console.ConPTY` NuGet
package and carried inside `dtr.exe` (see `crates/dtr/src/conpty.rs`).

| | |
| --- | --- |
| package | `Microsoft.Windows.Console.ConPTY` 1.25.261109001 |
| source | <https://github.com/microsoft/terminal>, tag `v1.25.3131.0` (MIT) |
| release asset | <https://github.com/microsoft/terminal/releases/download/v1.25.3131.0/Microsoft.Windows.Console.ConPTY.1.25.261109001.nupkg> |
| files | `runtimes/win-x64/native/conpty.dll`, `build/native/runtimes/x64/OpenConsole.exe` |
| nupkg sha256 | `2d4f846e9b59ab85977d4d15022c696b36e5400f6320608be1c75d3d0195fee7` |
| `conpty.dll` sha256 | `e971856f89748dc3b458125e9afdd79b5e9574a7c937676116d3cbbd3b71f492` |
| `OpenConsole.exe` sha256 | `b17e48b07ef153b420f0a6e178bacd31fb4c3390e21ae746586213fc0719a775` |
| license | MIT; `LICENSE` beside this file is the repository's, © Microsoft |

**Why.** Inbox ConPTY is a terminal emulator in the middle of a shell pane: it
renders the child's output and re-emits VT, consuming the unknown APC sequences
the kitty graphics protocol rides in. The bundled host forwards them — 29 of 29
`ESC _ G` chunks arrived verbatim in the A/B measurement — and `portable-pty`
already prefers a `conpty.dll` on the DLL search path, so carrying these two
files is the whole fix. `docs/windows.md` §12 has the measurement and the
screenshots.

**Why both files.** `conpty.dll` starts `OpenConsole.exe` from its own directory,
so they are unpacked together and a missing half is a broken ConPTY.

**Updating.** Both files must come from the same package. From the repository
root, with the release tag and package version of the new host:

```sh
tag=v1.25.3131.0
pkg=Microsoft.Windows.Console.ConPTY.1.25.261109001.nupkg
curl -fL -o "/tmp/$pkg" \
  "https://github.com/microsoft/terminal/releases/download/$tag/$pkg"
unzip -p "/tmp/$pkg" runtimes/win-x64/native/conpty.dll \
  > crates/dtr/assets/conpty/conpty.dll
unzip -p "/tmp/$pkg" build/native/runtimes/x64/OpenConsole.exe \
  > crates/dtr/assets/conpty/OpenConsole.exe
curl -fsS -o crates/dtr/assets/conpty/LICENSE \
  "https://raw.githubusercontent.com/microsoft/terminal/$tag/LICENSE"
sha256sum "/tmp/$pkg" crates/dtr/assets/conpty/conpty.dll \
  crates/dtr/assets/conpty/OpenConsole.exe
```

Then set `VERSION` in `crates/dtr/src/conpty.rs` to the package version (the
directory the files are unpacked into is named after it, so an upgrade cannot
reuse an old host), and replace the versions and hashes in the table above.
`cargo test -p dtr conpty` checks that the committed files are still PE images.
There is deliberately no build step: Microsoft's package is the build artifact,
and rebuilding Windows Terminal ourselves would mean an MSVC toolchain for two
files that ship from the same CI that produces Windows Terminal itself.
