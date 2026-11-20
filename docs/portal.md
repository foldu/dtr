# dtr-portal: the desktop's file chooser, answered by dtr's browser

`crates/dtr-portal` is the `org.freedesktop.impl.portal.FileChooser` backend — the
daemon an application's file dialog actually reaches through `xdg-desktop-portal`. The
browser, its picker mode and the footer are already `dtr-files` (`docs/manager.md` §4);
this crate is the protocol half that turns a portal call into a `Request`, a person's
answer into `uris`, and a frontend's `Close()` into a cancelled request.

**F3 is split.** This is the D-Bus half: the interface, the option mapping, the URI
encoding, the request lifecycle, the packaging and the NixOS module, all tested over a
private bus. The picker **window** is the follow-up, because it needs `crates/dtr-frame`
(the multi-window event loop, surface and DPI handling) which is being extracted in
parallel. The window is one `Host` implementation, not a rewrite: §9 is the checklist.

## 1. What is built

| file | what |
| --- | --- |
| `src/portal.rs` | the zbus interface (`OpenFile`, `SaveFile`, `SaveFiles`), the per-request `Request` object, the answer/close race, the results encoding |
| `src/options.rs` | the `a{sv}` decode onto `dtr_files::pick::{Request, Select, Filter}`, and the raw-filter bookkeeping `current_filter` needs |
| `src/uri.rs` | path **bytes** → percent-encoded `file://` URI |
| `src/paths.rs` | the `SaveFiles` uniquifier (`report(1).pdf`) |
| `src/host.rs` | `Host`, `Call`, `Reply`, `Answer` — the one seam the window plugs into |
| `src/main.rs` | the daemon `xdg-desktop-portal` activates |
| `assets/` | `dtr.portal`, the D-Bus `.service` file, and the `portals.conf` example |
| `tests/private_bus.rs` | a private `dbus-daemon`, this backend, and a client acting as the frontend |

The installed XML was read rather than remembered —
`$datadir/dbus-1/interfaces/org.freedesktop.impl.portal.FileChooser.xml` from
`xdg-desktop-portal` 1.22.1 — and the interface is:

```
org.freedesktop.impl.portal.FileChooser
  OpenFile  (o handle, s app_id, s parent_window, s title, a{sv} options)
            → (u response, a{sv} results)
  SaveFile  (…same…)
  SaveFiles (…same…)      — 1.22.1 puts the file list in options, not an argument
org.freedesktop.impl.portal.Request at `handle`: Close ()
```

There is no `OpenFiles` in this version's backend interface. `response` 0 is a choice,
1 a cancel; `results` is a vardict of `uris` (`as`), `current_filter` (`(sa(us))`) and
`writable` (`b`). The private-bus test asserts the generated introspection against that
XML's type sequences, so a change to the Rust surface fails with a diff instead of with
a frontend that cannot call the backend.

## 2. Options, and the decisions in them

The vardict maps onto the picker's own `Request`; what is a decision is where a portal
option has no field on the picker:

| option | type | becomes |
| --- | --- | --- |
| `multiple` | `b` | `Select::Files` instead of `Select::File` |
| `directory` | `b` | `Select::Directory`; wins over `multiple` (below) |
| `accept_label` | `s` | `Request.accept`; defaults are the desktop's (`Select` / `Open` / `Save`) |
| `filters` | `a(sa(us))` | `Request.filters`, keeping only glob patterns (below) |
| `current_filter` | `(sa(us))` | `Call.filter`, the index into `Request.filters` the window starts on; echoed back exactly as sent |
| `current_folder` | `ay` (NUL-terminated) | `Request.folder`, decoded from bytes |
| `current_name` | `s` | `Request.name` for `SaveFile` |
| `current_file` | `ay` (NUL-terminated) | `Request.name` from the basename, and its folder when `current_folder` is absent |
| `files` | `aay` | kept for the `SaveFiles` answer, appended to the chosen folder |
| `modal` | `b` | `WindowContext.modal`, for the window |
| `choices` | `a(ssa(ss)s)` | **ignored**: the picker has no combo-box/check-button widget, and the interface says a frontend must cope |

Two corners are decided rather than forwarded:

- **`directory` with `multiple`**: the picker answers with the folder on screen, and
  "several folders" is not a shape F2 has. GTK selects several here; dtr returns one,
  and this is written down rather than silently half-implemented.
- **A filter is a promise about the answer**, and the picker can only promise by
  *name*. The portal's `a(us)` pairs may be globs (`0`) or MIME types (`1`); a name
  cannot be tested against `image/png`, so MIME rows are kept for the echo but are not
  handed to the picker. A filter left with **no glob patterns** (MIME-only) is dropped
  altogether: forwarding it would make the picker refuse every file, which is the one
  outcome a picker must never produce (`docs/manager.md` §4, "cannot tell"). A
  `current_filter` with no `filters` list is the portal's "apply unconditionally", and
  becomes the picker's only filter.
- **`current_filter` selects by name**, GTK's heuristic (the application's filter need
  not be pointer-identical to one in the list). If it matches nothing, the picker starts
  on index 0 and the log says why.
- **Typed strictly, extended leniently**: a known key with the wrong type is
  `org.freedesktop.DBus.Error.InvalidArgs` (a frontend that sends `multiple` as a
  string has a bug worth surfacing); an unknown key is ignored, because the interface
  may grow.

## 3. The answer

`uris` is built from paths, and a path is **bytes**, so `file_uri` encodes every byte
outside `A-Za-z0-9-._~` and `/` as `%XX`. Space is `%20` (not form encoding's `+`), `+`
is `%2B`, `#` is `%23`, and a non-UTF-8 byte encodes as itself — `to_string_lossy` is
not an encoder, it silently points the caller at a different file. A relative path is
discarded rather than given a base; if nothing usable remains, the request answers
cancel instead of a URI that cannot open.

`writable` is derived from the question, not chosen by the host: `false` for `OpenFile`
(this picker has no read-only toggle, so it grants read access only), `true` for
`SaveFile`/`SaveFiles` (the application has to write). GTK sends the key for all three
methods, so this does too.

For `SaveFiles`, the chosen folder is answered with each `options.files` name appended,
and a name that already exists gains a number — `report.pdf` → `report(1).pdf` — because
handing back a taken name would have the application overwrite a file on the way to
saving one. The split is at the first dot, as GTK does (`archive.tar.gz` →
`archive(1).tar.gz`). This is the one filesystem question on the answer path: one
`exists` per name, bounded at a thousand collisions, no directory read.

## 4. The request lifecycle

1. Options are decoded **before** anything is exported: a malformed request gets
   `InvalidArgs` and never reaches a host, so there is no window and no `Close` target
   to leak.
2. The `Request` object is exported at the frontend's `handle` **before** the host is
   told, because the frontend may call `Close()` the instant it sent the method.
3. The host gets a `Call` and the method future waits — zbus spawns a task per method
   call, so `Close()` is dispatched while the picker is still pending — on the answer
   and the close signal, whichever comes first.
4. **Exactly once.** The reply channel has one slot; `Reply::send` returns whether it
   won, and a second send is `false`, not an error.
5. The request object is unexported **before** the reply is sent: a `Close()` after the
   answer is `UnknownObject`, which is the truth — the interaction is over.
6. A host that drops its `Call` without answering wakes the method too, as a cancel.
   "Never hang" is worth more than blaming the window.

`Close()` answers response **1** (cancel), including while the picker is up. GTK's
backend answers **2** ("other error") for a portal-initiated close; the interface
documents 0 and 1, the task that specified this milestone says a close is a cancel, and
1 cannot be mistaken for a crash. The host is told via `Host::close` so the window comes
down, but it does not have to answer — the request is already answered by then.

`SaveFiles` with an empty `files` list has nothing to append to a folder; it answers
cancel rather than an `uris` array no caller can use. An unknown handle path already in
use is `InvalidArgs` rather than a second picker behind one `Close`.

## 5. Threads

The connection is zbus with the `async-io` backend, the one the settings portal already
uses: zbus drives its own executor and reactor on their own threads, so this crate pulls
in no tokio and needs no runtime of its own (`docs/compio.md` §2). `Host::open` and
`Host::close` are called on the D-Bus task under a mutex and must **post** to whatever
owns the window; the answer travels back through `Reply` from wherever the person is,
which may be another thread and minutes later. Nothing on the bus path reads a directory.

The binary's `main` is therefore one `pollster::block_on` for the handshake and a
`pending()` future after it: the executor has the sockets, `main` only has to not exit.

## 6. Packaging

Three text files, all searched for in `$XDG_DATA_DIRS`, so a trial under
`~/.local/share` needs no system change:

```
share/xdg-desktop-portal/portals/dtr.portal
    [portal]
    DBusName=org.freedesktop.impl.portal.desktop.dtr
    Interfaces=org.freedesktop.impl.portal.FileChooser
    UseIn=gnome

share/dbus-1/services/org.freedesktop.impl.portal.desktop.dtr.service
    [D-BUS Service]
    Name=org.freedesktop.impl.portal.desktop.dtr
    Exec=@DTR_PORTAL@              ← substituted at build time

~/.config/xdg-desktop-portal/portals.conf     (the example, not installed)
    [preferred]
    default=gnome
    org.freedesktop.impl.portal.FileChooser=dtr
```

The flake builds `packages.${system}.dtr-portal` with `craneLib.buildPackage` and
`cargoExtraArgs = "--locked --package dtr-portal"` (only the daemon; a bare `cargo
build` would build the terminal too), then installs both files in `postInstall` and runs

```
substituteInPlace $out/share/dbus-1/services/….service \
  --replace-fail '@DTR_PORTAL@' "$out/bin/dtr-portal"
```

so `Exec=` names the installed binary. A build directory there is a store path that
stops existing, and a file dialog that silently never opens is the worst failure mode
this packaging has — hence `checks.${system}.dtr-portal-packaging`, which checks the
*installed* files: the portal file's `DBusName` equals the service's `Name`, the
interface is the one this crate serves, and `Exec=` is an executable that lives in the
package as `bin/dtr-portal`.

`portals.conf(5)` precedence, highest first: `$XDG_CONFIG_HOME`, `$XDG_CONFIG_DIRS`
(`/etc/xdg`), the build-time sysconfdir, then the data dirs; inside each location a
`$desktop-portals.conf` (for each name in `XDG_CURRENT_DESKTOP`, case-folded) is read
before `portals.conf`, and the first file found wins. A key's value is a
semicolon-separated fallback list; `none` and `*` are special, `*` meaning "first
implementation found, lexicographical".

`nixosModules.dtr-portal` is the opt-in:

- `services.dtr-portal.enable` (default false) gates everything;
- `xdg.portal.enable = true` and `xdg.portal.extraPortals = [ dtr-portal ]`;
- `xdg.portal.config.common."org.freedesktop.impl.portal.FileChooser" = "dtr"` — NixOS
  writes `common` to `/etc/xdg/xdg-desktop-portal/portals.conf`, which is read before
  the data-dir `$desktop-portals.conf` files;
- `xdg.portal.config.common.default = lib.mkDefault "*"` — the `*` rule keeps every
  *other* interface on the machine's own backend, and being a `mkDefault` means a
  desktop's own NixOS module that names a default wins.

`checks.${system}.dtr-portal-module` evaluates a bare NixOS configuration with the
module imported and asserts the generated `portals.conf` names dtr for `FileChooser`,
keeps `default=*`, and that the package is on `xdg.portal.extraPortals` with
`xdg.portal.enable` true.

The `UseIn=gnome` line is for automatic selection (no `portals.conf` at all); an
explicit key wins over it, so the NixOS module does not depend on it.

## 7. Tests

The gate is the devShell one:

```sh
nix develop .#default --command bash -c \
  'cargo fmt --all && cargo test -p dtr-portal && cargo build -p dtr-portal'
```

and the packaging gates are in the flake (`nix build .#dtr-portal
.#checks.x86_64-linux.dtr-portal-packaging .#checks.x86_64-linux.dtr-portal-module`).

**Unit tests** (`options`, `uri`, `paths`, the results builder) need no bus: the decode
of a realistic `OpenFile` options dictionary, defaults, `directory` beating `multiple`,
the MIME-only filter drop, `current_filter` by name and unconditional, save name/folder
from `current_name`/`current_file`, `SaveFiles` names, NUL-terminated non-UTF-8
`current_folder` bytes, an unknown option ignored, a wrong type refused; percent
encoding of space/`+`/`#`/`?`/`%`/`:`, a non-UTF-8 path, a relative path discarded; the
uniquifier; and response 0/1, `writable`, the echoed filter and a missing/unknown filter
index.

**The private bus** (`tests/private_bus.rs`) copies `notify.rs`'s pattern: its own
`dbus-daemon --session --nofork --print-address=1` (so the test neither depends on nor
disturbs the desktop's bus), this backend served on its address with a headless `Host`,
and a client connection that acts as the frontend. `dbus-daemon` is not guaranteed on a
Nix check's `PATH`; when it is missing the tests print `no dbus-daemon on PATH; skipping
the file chooser portal test` and return, because a red suite for a missing tool is not
honest. On this machine (and in the devShell, whose `PATH` includes the system profile)
it runs:

- `the_interface_on_the_bus_is_the_one_the_frontend_expects` — introspection of
  `OpenFile`/`SaveFile`/`SaveFiles` equals the XML's `o s s s a{sv} → u a{sv}`;
- `a_real_open_file_is_answered_over_the_bus` — `multiple`, two filters with a MIME row,
  `current_filter`, a `current_folder` containing spaces/`+`/`#`, `accept_label`,
  `modal`; asserts the request the host got and response 0 with
  `file:///tmp/photo%20photo%2B%231.png`, `writable: false`, and the filter row echoed
  exactly;
- `a_directory_request_answers_the_folder_being_shown` — `directory` beats `multiple`;
- `a_multiple_open_answers_every_chosen_file` — order preserved;
- `a_save_file_answers_the_typed_name` — `Select::Save`, the name, one URI,
  `writable: true`;
- `a_save_files_answer_appends_the_names_to_the_folder` — a taken name becomes
  `taken(1).txt` (encoded as `taken%281%29.txt`), a free one is kept;
- `a_close_while_pending_answers_cancel_and_closes_the_window` — the picker is left
  pending, the request object introspects `Close` with no arguments, `Close()` answers
  response 1 with no results, the host is told to close, and a second `Close()` is an
  error because the object is gone;
- `a_malformed_option_is_refused_instead_of_hanging` — `InvalidArgs`;
- `a_malformed_request_never_reaches_the_host` — no call, no exported object;
- `the_daemon_binary_serves_the_interface` — the shipped `dtr-portal` binary (not the
  library) is started with `DBUS_SESSION_BUS_ADDRESS` pointing at the private bus,
  polled until it owns the name, sent a request, and answers response 1: the binary's
  placeholder host is exercised, and a `main` that failed its handshake would show up
  here as a timeout or a service error.

## 8. What is deliberately not here

- **The window.** See §9; until it lands the binary's host answers cancel and logs why.
  That is a milestone split, not a design.
- **`choices`.** No widget in the picker, and the interface lets a portal ignore them.
- **Several folders for `directory` + `multiple`** — one folder, as above.
- **A read-only toggle for `OpenFile`** — `writable` is always false for opens.
- **The Documents portal and write access.** The backend returns paths; granting an
  application access outside a sandbox is `xdg-desktop-portal`'s job with the Documents
  portal, and `writable: false` is the honest answer for a picker that reads.
- **Idle exit and a resident hidden window.** The process serves and stays; the window
  task decides when it may exit (`docs/manager.md` §5, §11: a resident daemon's memory).
- **`org.dtr.Files.OpenWindow`** (the manager window's private interface) is F4, and it
  shares this daemon — one more interface, not a second process.
- **Wayland transient windows.** `parent_window` is an opaque handle there and winit
  exposes no xdg-foreign, so the picker is not transient on Wayland. Written down, not
  lied about with a flag.
- **Windows.** The crate is Linux's; it compiles elsewhere but there is no portal there.

## 9. The window follow-up

The shape is already fixed; the work is a `Host` implementation in the binary (and the
`dtr-frame` dependency):

1. **A window thread.** `dtr-frame` owns the event loop and a `Vec` of windows over one
   `egui::Context`; the daemon's `main` hands it a channel and `Host::open` posts a
   `Call` to it (never blocks).
2. **Build the picker from the call.** `Mode::Pick(Pick::new(call.request))`,
   `pick.set_filter(call.filter.unwrap_or(0))`, the title from `call.window.title`, the
   confirm word from `call.request.accept`. The `Request` already carries
   `Select`, `folder` (`current_folder`), `name` (`current_name`/`current_file`) and the
   glob filters; nothing needs translating again.
3. **Transiency and modality.** Parse `call.window.parent_window`: `x11:0x…` becomes
   `Window::set_transient_for`, with `modal` from `call.window.modal`; document the
   Wayland limitation.
4. **The answer.** On accept, `Pick::accept(cwd, cursor, selection)` gives an `Outcome`:
   `Chosen(paths)` → `Answer::Chosen(Chosen { paths, filter: request.filters
   .is_empty().then_some(pick.filter()) })` (the `writable` result is derived by the
   backend, not the host); `Refused`/`Replace`/`Navigate` stay in the picker — the
   footer already says and does the right thing — so only `Chosen` and
   `Cancelled` (Escape, the close button, or the window being dismissed) reach `Reply`.
   `Reply::send` returning false means the request was already answered (the frontend
   closed it), so the window comes down without a second answer.
5. **`Host::close`.** Find the window by `RequestId` and close it without answering: the
   backend has already answered cancel.
6. **One window per request, hidden between them, idle-exit after a timeout** — the
   memo's reason for a daemon rather than a fresh process per dialog (`docs/manager.md`
   §5, §11). A request that arrives while the window thread is busy posts and waits
   rather than being dropped.
7. **The listing runs on its own thread/runtime**, as `Browser::reload` already expects;
   the D-Bus task keeps its promise of never doing I/O.

`#[cfg(test)]` has a headless `Host` in `tests/private_bus.rs` to copy from; the real
one differs only in where it posts and that it paints.
