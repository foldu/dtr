# dtr-portal: the desktop's file chooser, answered by dtr's browser

`crates/dtr-portal` is the `org.freedesktop.impl.portal.FileChooser` backend — the
daemon an application's file dialog actually reaches through `xdg-desktop-portal`. The
browser, its picker mode and the footer are already `dtr-files` (`docs/manager.md` §4);
this crate is the protocol half that turns a portal call into a `Request`, a person's
answer into `uris`, and a frontend's `Close()` into a cancelled request.

**F3 is built.** The interface, the option mapping, the URI encoding, the request
lifecycle, the packaging and the NixOS module are one half, tested over a private bus;
the picker **window** is the other: one `dtr-frame` window, kept hidden and warm between
requests, drawing the F2 browser and answering through the same `Host` seam the private-bus
tests use. §9 is the window, as built, and how it was verified.

## 1. What is built

| file | what |
| --- | --- |
| `src/portal.rs` | the zbus interface (`OpenFile`, `SaveFile`, `SaveFiles`), the per-request `Request` object, the answer/close race, the results encoding |
| `src/options.rs` | the `a{sv}` decode onto `dtr_files::pick::{Request, Select, Filter}`, and the raw-filter bookkeeping `current_filter` needs |
| `src/uri.rs` | path **bytes** → percent-encoded `file://` URI |
| `src/paths.rs` | the `SaveFiles` uniquifier (`report(1).pdf`) |
| `src/host.rs` | `Host`, `Call`, `Reply`, `Answer` — the one seam the window plugs into |
| `src/window.rs` | the picker **window**: the `dtr-frame` loop, the `Desk` state machine, the drawing, input and the answer |
| `src/x11.rs` | `parent_window` parsing, and the `WM_TRANSIENT_FOR`/`_NET_WM_STATE_MODAL` writes winit has no API for |
| `src/main.rs` | the daemon `xdg-desktop-portal` activates: the bus on a thread, the window loop on `main` |
| `assets/` | `dtr.portal`, the D-Bus `.service` file, and the `portals.conf` example |
| `examples/picker_call.rs` | a stand-in frontend: calls `OpenFile`, waits for the window capture, checks the X11 hints, closes |
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
`Host::close` are called on the D-Bus task under a mutex and **post** to the window loop
(`EventLoopProxy::send_event`); the answer travels back through `Reply` from wherever the
person is, which may be another thread and minutes later. Nothing on the bus path reads a
directory.

**The window loop is the process's main thread, not a thread beside it.** winit 0.30
refuses to create its `EventLoop` anywhere else — it panics with a message that names
`with_any_thread` as the escape hatch it is there to talk you out of — so the split is the
reverse of the obvious one:

```text
main             bring up the picker loop, hand the bus a host, run the loop
dtr-portal-bus   serve the interface, hold the connection, never return
dtr-list / …     dtr-vfs's own walk threads, streaming batches to a Browser
```

`main` waits for the D-Bus handshake before entering the loop, so a name it cannot own is
an exit status rather than a window nobody can reach; and it returns when the loop exits,
which is the idle timeout. The process lives exactly as long as the picker loop, and
"idle-exit so activation can start it again" is one `event_loop.exit()` rather than a
second way to shut a daemon down.

The listing is a third thread and already existed: `dtr-vfs`'s local backend starts a
`dtr-list` thread per walk (`local.rs` names the picker as one of the callers that has no
runtime to borrow) and the window only drains its batches per frame. Neither the bus path
nor the frame path does a `read_dir`.

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
- `the_daemon_binary_serves_the_interface_without_a_display` — the shipped `dtr-portal`
  binary (not the library) is started with `DBUS_SESSION_BUS_ADDRESS` pointing at the
  private bus and with `DISPLAY`/`WAYLAND_DISPLAY` removed, polled until it owns the name,
  sent a request, and answers response 1: the binary's documented no-window path (serve,
  answer cancel, say why in the log) is exercised, and a `main` that failed its handshake
  would show up here as a timeout or a service error. The display variables are removed
  on purpose: the real binary runs the picker window on its main thread, and a test on a
  developer's desktop must not leave a dialog waiting for a person who is not there.

**The window's own unit tests** need no display either, because `src/window.rs` keeps the
decisions apart from the window:

- `one_picker_at_a_time_and_the_rest_wait` — a second request queues and is shown when the
  first is answered;
- `a_close_before_the_window_showed_it_drops_it_without_an_answer` — a `Close` for a
  queued request removes it, a `Close` for the one on screen takes it down, and **neither**
  sends anything: the portal already answered cancel;
- `a_close_that_arrives_before_its_open_is_remembered` — the reordered case is dropped
  once and not swallowed for ever;
- `the_idle_clock_runs_while_nothing_is_showing` — a request resets it, an answer is the
  last activity;
- `the_title_falls_back_to_the_question`;
- `only_a_choice_becomes_an_answer` — a `Chosen` outcome is the only one that reaches the
  bus, and its filter is echoed only when the request offered a list;
- `x11::a_portal_x11_handle_is_the_window_id_in_hex` and
  `x11::handles_this_process_cannot_use_are_not_parents` — `x11:0x…` with and without
  `0x`, uppercase hex, and the handles that are not a window: `wayland:…`, empty, `x11:0`,
  junk.

**The window itself is verified under Xvfb** (§9.7): the daemon runs with the capture hook,
`examples/picker_call.rs` sends a real `OpenFile` with a real X11 parent, and the PNG plus
the two X11 properties are the evidence.

## 8. What is deliberately not here

- **`choices`.** No widget in the picker, and the interface lets a portal ignore them.
- **Several folders for `directory` + `multiple`** — one folder, as above.
- **A read-only toggle for `OpenFile`** — `writable` is always false for opens.
- **The Documents portal and write access.** The backend returns paths; granting an
  application access outside a sandbox is `xdg-desktop-portal`'s job with the Documents
  portal, and `writable: false` is the honest answer for a picker that reads.
- **The desktop's own palette.** The picker's skin is Adwaita — dark by default,
  `DTR_PORTAL_SCHEME=light` for the other — not the machine's real GTK palette: the reader
  that does that (`crates/dtr/src/system`) is in the terminal crate and needs a running
  desktop. That is a fallback with a documented switch, and the reader moving into a crate
  both frontends can use is the follow-up (F4 needs the same answer for the manager
  window).
- **Remote folders in a picker.** A portal request names a *path* (`current_folder` is
  bytes with no host), so `OpenFile` can only mean this machine: the window's listing is
  `dtr-vfs`'s `Local`, and `Ssh` is not reachable from the interface. The manager window
  (F4) has no such limit and needs the SFTP pump dtr's `App` has (`Browser::take_request`
  → worker → `apply_listing`), which is why that seam is `dtr-vfs`'s and not this crate's.
- **`org.dtr.Files.OpenWindow`** (the manager window's private interface) is F4, and it
  shares this daemon — one more interface, not a second process. The `window` module is
  shaped for that reuse; §9.8 says what it can take.
- **Wayland transient windows.** `parent_window` is an opaque xdg-foreign token there and
  winit exposes no way to attach one, so the picker is not transient on Wayland. Written
  down, not lied about with a flag.
- **Windows.** The crate is Linux's; it compiles elsewhere but there is no portal there.

## 9. The window, as built

`src/window.rs` is the `Host` §9 was a checklist for; `src/x11.rs` is the hint winit cannot
set; `examples/picker_call.rs` is the frontend that drives it under Xvfb. The shape, point
by point:

### 9.1 The loop is the main thread

winit 0.30 panics when its `EventLoop` is created off the main thread, and points at the
X11/Wayland `with_any_thread` extensions as the escape hatch it exists to talk you out of.
So the process is the other way round from the sketch: `main` builds the picker loop, hands
the D-Bus side a `WindowHost`, waits for the bus handshake, and runs the loop. The bus is a
thread of its own (`dtr-portal-bus`), which is the half that does not care where it runs.
`Host::open` is `EventLoopProxy::send_event(Event::Open(call))` and `Host::close` is the
same with `Event::Close(id)` — posting, never blocking — and the answer travels back through
the `Call`'s own `Reply` from the window's `about_to_wait`.

If the loop cannot be built at all (no display, no X/Wayland backend), `main` takes the
documented no-window path: serve the interface, answer every request cancel with the reason
in the log. A dialog that never appears with no explanation is the failure mode this
refuses to have.

### 9.2 One warm window, one picker at a time

`resumed` creates the window, immediately hides it, and installs the skin — so the adapter,
device, egui context and fonts are paid for once, before the first dialog rather than during
it. One seam `dtr-frame` does not expose: `WindowOptions` has no `visible`, so the window is
created and then hidden two calls later; on a real window manager that is one map that can
flash at daemon startup. Adding `visible` to `WindowOptions` belongs in the frame crate, not
here. `Desk` holds the queue; `about_to_wait` (i.e. after the whole event batch, never inside
`user_event`) shows the head when nothing is on screen. That ordering is what makes an
`Open` and the `Close` that raced it never paint a window, and it is what the unit tests
assert.

A second request while one is up is **queued**; the frontend's method call waits, which is
what an async portal method is for. `Host::close` for a queued request removes it, for the
one on screen hides it without answering (the portal answered cancel when it saw the close),
and for an id it has never heard of remembers it for five seconds so a reordered open is
dropped rather than painted — the wire cannot produce one today, and the guard is there so
that a shifted queue cannot.

### 9.3 What is drawn, and in what skin

`Browser::new(start)` where `start` is `request.folder` or else `dirs::home_dir()` — the
daemon's working directory after D-Bus activation is not a place anybody chose — then
`browser.set_pick(request.clone())`, then `Pick::set_filter(call.filter)`. The client area
is the pane: `dtr_files::pane_ui(ui, id, ui.max_rect(), &browser, &skin, true)` paints the
header, breadcrumb, grid/list, places popup, sort control, close cross and the picker's own
footer (`docs/manager.md` §4). The window adds no chrome of its own; the caption is
`Window.title`, or "Open a file" / "Choose a folder" / "Save a file" when the application
gave none.

The skin is `dtr_widgets::Skin::adwaita(dark, ADWAITA_ACCENT)`: GNOME's chrome is what a
desktop dialog looks like, `DTR_PORTAL_SCHEME=light|dark` settles the scheme (default dark),
and §8 says why it is not the machine's actual palette yet.

### 9.4 Input

The window is the only place a picker's keys exist, so it maps them itself, once, and the
browser's own rules do the rest:

- `Escape` cancels — closing a save's `replace` question first and the picker second, the
  browser's one-button-two-depths rule; the footer's cancel button reports the same thing
  through `PaneOutcome::pick_cancel`;
- `Enter` accepts, exactly the footer's button (`Browser::accept_pick`);
- arrows, `PageUp`/`PageDown`, `Home`/`End`, `Backspace`, `Alt+Up` and `h j k l r g G` are
  `dtr_files::Key`s, the same mapping dtr's file panes use — **unless an egui widget has the
  keyboard**, which is a save's name field, so typing a name does not also move a listing
  behind it;
- a left press is `Browser::clicked` with dtr's double-click counter (400 ms, four points),
  and the wheel is `Browser::scroll`;
- key **releases** are ignored: the release of the keystroke that opened the window arrives
  after it is open, and acting on both would cancel twice (`AGENTS.md`).

### 9.5 The answer

Only two outcomes reach the bus:

- `Outcome::Chosen(paths)` → `Answer::Chosen(Chosen { paths, filter })`, where `filter` is
  `pick.filter()` **only when the request offered filters** — an index into an empty list
  is not a filter, and `current_filter` exists because the application asked for a list
  (`docs/portal.md` §2);
- `Cancelled` for `Escape`, the footer's cancel, the header's cross and the window
  manager's close button.

`Refused` and `Replace` stay in the footer (it says why and the button stays armed), and
`Navigate` calls `Browser::navigate` so a folder in a file picker is a way in, not an
answer. `Reply::send` returning false means the frontend closed first — the log says so and
the window comes down without a second answer.

### 9.6 Transiency, modality, and the Wayland gap

`x11:0x…` becomes `WM_TRANSIENT_FOR`, and `Window.modal` adds
`_NET_WM_STATE = [_NET_WM_STATE_MODAL]`, written in `src/x11.rs` before the window is mapped
— EWMH's window for a window's initial state, and the only one that works for a window that
is reused by the next application. winit 0.30 has no API for either: `WindowExtX11` is empty
and the only parent-shaped attribute, `with_embed_parent_window`, would reparent the picker
*into* the application's window. So the properties are written over the X protocol with
`x11rb`, using the id `raw-window-handle` hands out — two dependencies that were already in
the tree through winit, and one function that fails quietly (a debug log) when the window is
not X11.

A Wayland `parent_window` is an xdg-foreign token, not a window id, and winit exposes no
way to attach one; the picker is not transient there. That is the documented gap, not a
flag that lies. A request with no `parent_window` clears the previous hints before the
window is shown again, so a reused window never inherits the last application's parent.

### 9.7 The listing

The picker does not own a listing thread, and should not: `Browser` already streams it.
`Browser::new`/`navigate`/`refresh` start `dtr-vfs`'s `Local::list`, which walks on its own
`dtr-list` thread and sends batches over a channel (`crates/dtr-vfs/src/local.rs` names the
picker as a caller with no runtime to borrow); the window drains them in `Browser::poll()`
once per frame and re-`stat`s the directory with `refresh_if_changed()` twice a second while
a picker is up. That is the same pump dtr's `App` uses, one `poll()` and no second
implementation, and the D-Bus task keeps its promise of never doing I/O.

There is no compio runtime in this crate and no `Vfs` handle, because a portal request
cannot name a machine (§8): `current_folder` is bytes with no host, so the answer is always
this machine's `Local`. When F4 grows a remote manager window it needs the SFTP pump — the
`Browser::take_request` → worker → `apply_listing` handshake — and *that* is where
`dtr_vfs::Vfs` and an alias live.

### 9.8 Idle exit, and what F4 can reuse

With nothing on screen and no request for `DTR_PORTAL_IDLE_TIMEOUT_MS` (default 60 s;
environment only, like the other hooks) the loop calls `event_loop.exit()`. `main` returns,
the process — and with it the bus connection and the service name — go away, and
`xdg-desktop-portal` activates a fresh one for the next request. The first picker of a
session pays for a process; every later one within the timeout is a frame.

F4 (the manager window) shares this daemon and can take:

- `window::start`/`Run`/`WindowHost`'s split — one loop, one event type, one place a second
  window scheme plugs in — but **not `WindowHost` as a `Host`**: the manager window is a
  private `org.dtr.Files.OpenWindow` call, so it needs its own host or a host that posts a
  different enum;
- the `Desk` shape (what is queued, what is shown, the idle clock), which generalises;
- `x11::parent_window` and `x11::set_transient_for` unchanged, and `Options::skin`;
- the capture hook, which is how any window in this process gets photographed for real.

What F4 must solve differently: **two windows at once**. `Windows` is a `Vec<Frame>`, so a
manager window beside a picker is the collection's job, not a second event loop — but this
module's `PickerApp` is written around exactly one window id and one `Picker`, so the app
type needs per-window state (a `HashMap<WindowId, …>`, and `Desk` per window or one desk
with a window id) rather than a second copy of it.

### 9.9 Verification

The gate is still the devShell one (§7). The *window* is verified under Xvfb, because the
thing being checked is that a real mapped X11 window, a lavapipe surface, the skin and the
listing agree — and because the X11 hints are facts about the window, not about our code:

```sh
nix develop .#default --command bash -c 'cargo build -p dtr-portal --example picker_call'
xvfb=$(nix build nixpkgs#xvfb --no-link --print-out-paths)/bin/Xvfb
$xvfb :171 -screen 0 1280x800x24 -nolisten tcp -ac &
export DISPLAY=:171

dbus-run-session -- bash -c '
  DTR_PORTAL_CAPTURE=/tmp/dtr-portal-picker.png \
  DTR_PORTAL_CAPTURE_AFTER_MS=800 \
  DTR_PORTAL_IDLE_TIMEOUT_MS=5000 \
    ./target/debug/dtr-portal > /tmp/portal.log 2>&1 &
  daemon=$!
  sleep 3
  ./target/debug/examples/picker_call --capture /tmp/dtr-portal-picker.png --parent
  wait $daemon
'
```

`examples/picker_call.rs` prints what it saw; a good run is:

```text
captured 900 × 620 at /tmp/dtr-portal-picker.png
window 0x200002 is transient for 0x600000
_NET_WM_STATE = ["_NET_WM_STATE_MODAL"]
request answered response 1 (cancel)
```

and the daemon's own log ends:

```text
DEBUG dtr_portal::window: photographing the picker window path=/tmp/dtr-portal-picker.png
 INFO dtr_frame: captured the window path=/tmp/dtr-portal-picker.png width=900 height=620
DEBUG dtr_portal::portal: answered the file chooser request response=1 uris=false
 INFO dtr_portal::window: no picker for the idle timeout; exiting so D-Bus activation can start afresh idle_s=5
```

The PNG is the window's client area at 1× (900 × 620, the requested inner size): header with
the places button and breadcrumb, the grid of the scratch directory the client passed as
`current_folder`, the filter control, and `cancel`/`Open` in the footer. `magick identify`
reads it and `read_image` looks at it. The same run twice, one request after the other, is the warm-window check: both
`set the picker's X11 hints` lines name the same window (`2097154` here), so the second
picker reused the window and the device the first one left hidden, and the daemon then
exited 0 at its idle timeout. `WINIT_X11_SCALE_FACTOR=2` re-runs the whole thing at
1800 × 1240 with the same point measurements, which is the DPI half of the frame's job:
`pane_ui` is handed points and the window is the only thing that knows the factor.

The two hooks that make this possible are
`DTR_PORTAL_CAPTURE` (write the first shown picker's surface once, through
`dtr_frame`'s `COPY_SRC` path — the same mechanism `dtr --capture` uses) and
`DTR_PORTAL_IDLE_TIMEOUT_MS`, so a verification run's daemon exits by itself and *proves*
the idle path at the same time.

Two environment notes from the run above: there is no window manager on Xvfb, so the window
has no decorations and the close button in the capture is the pane's own cross; and the
icon theme is not installed there, so the grid's file icons are egui's missing-image
placeholder — dtr's file pane draws identically in that environment, and the picker's own
chrome is what the capture is for. The `first frame elapsed_ms=3015` line is mostly the
client's deliberate three-second wait before it asked, not the window: the frame itself was
drawn 20 ms after the request, which is what "warm" means. The daemon's exit status is 0
because the idle timeout ended the loop — the same run proves the idle policy.

## 10. The manager window, as built (F4)

F4 puts a second window kind in this process, and a second binary beside the
daemon. Neither is a second daemon: the point of the warm process is one wgpu
device, one font set and one icon cache for the picker and the manager, so the
manager's window is a `Frame` on the same loop and its D-Bus interface is on the
same connection.

### 10.1 Per-window screens

`src/window.rs`'s app was written around one window id and one `Picker`. It is
now a `HashMap<WindowId, Screen>` where a `Screen` is `ScreenKind::Picker` or
`ScreenKind::Manager` plus that window's input state (pointer, click count,
modifiers, the manager's last layout, its refresh deadline). `dtr_frame::Windows`
was already a `Vec<Frame>` with one `egui::Context` per window, so the event
dispatch is `frame.id()` → the screen → the picker's or the manager's handler.

The two kinds have different lifecycles, and the map is about the *visible* ones:

- **Picker** — one warm window, created hidden in `resumed` and reused. It is not
  in the map while hidden, so the idle policy and the capture hook ask about
  windows a person can see. `Desk` still queues requests, still answers a
  `Close` that arrives first, and its unit tests are unchanged.
- **Manager** — a `Frame` and a `Screen` per `OpenWindow`, created visible and
  closed by the person (or by `CloseWindow`). A manager window *is* a visible
  window, so the process stays alive while one is open: the memo's "idle-exit
  when it owns no visible window" is `idle_exit(screens.len(), last_activity,
  idle)` and nothing else. Closing a manager hides it in `window_event` and drops
  the `Frame` in `about_to_wait` (`Windows::close` needs the collection, which the
  event callback does not have). The run that verified F4 also proves the path:
  `CloseWindow` → window gone → idle timeout → `event_loop.exit()`.

The capture hook (`DTR_PORTAL_CAPTURE` / `DTR_PORTAL_CAPTURE_AFTER_MS`) now
photographs **the first window shown**, whichever kind it is: a picker run is a
picker capture as before, and a `dtr-files` run is the manager's.

### 10.2 `org.dtr.Files` and `dtr-files`

```
org.dtr.Files                      at /org/dtr/Files, on the chooser's connection
  OpenWindow  (s path, a{sv} options) -> (s handle)
  CloseWindow (s handle)             -> ()
```

`src/files.rs` is the interface. The options are `title`, `parent_window` and
`modal`; an unknown key is ignored (the interface may grow) and a known one of
the wrong type is `InvalidArgs`. An empty `path` means the person's home — the
interface says so rather than a launcher guessing in another process — and a path
that names a file opens its parent with the file selected, which is the
terminal's `--files some/file` rule.

`OpenWindow` **waits for the loop's answer**: the D-Bus method posts an
`Event::OpenManager(OpenWindowRequest)` and waits on the request's own ack
channel, which the window loop fills when `Windows::open` succeeded (the handle)
or failed (the reason). A launcher therefore learns that the window exists
instead of a handle for nothing, and a machine with no display is an error the
launcher prints. `CloseWindow` is the same shape and answers whether a window had
that handle.

Two files are needed for activation and both are installed, substituted and
checked: the chooser's service file (unchanged) and
`assets/org.dtr.Files.service`, both `Exec=$out/bin/dtr-portal`. The daemon owns
both names: the builder owns `org.freedesktop.impl.portal.desktop.dtr`, and after
registering the object at `/org/dtr/Files` it calls
`Connection::request_name("org.dtr.Files")` — zbus's order for a name registered
outside the builder.

`dtr-files` is the second binary of the package (`src/bin/dtr-files.rs`). It
takes an optional path, resolves it against the shell's directory, calls both
`OpenWindow` and (after the capture, in the verification client) `CloseWindow`,
has a 30 s timeout because a hung daemon must not hang a desktop, and exits
non-zero with a sentence on no session bus, no activation, or a refusal. It does
not link a window: a launcher that drew one would be the second process this
crate exists to avoid.

### 10.3 The manager screen, its data, and its worker

`src/manager.rs` is the window half of `docs/manager.md` §6: `ManagerLayout::new`
is computed once per frame and shared by `manager_ui` and `pane_ui`, the chrome's
`ManagerAnswer` and the pane's `PaneOutcome` are folded into the `Browser`, and a
browser on a host gets `Ssh`'s caps so its menu offers the verbs it can run.
History is a stack of `(machine, path)` — a sidebar host row replaces the browser
and `back` returns to the other machine. The editable location keeps a draft
(the chrome draws the field from `ManagerState.location`), understands `~` and a
`host:` prefix, and resolves a relative path against the directory on screen.

`src/desktop.rs` reads the sidebar's data: `Places`, `/proc/mounts` filtered to
real mounts (pseudo filesystems, the root and duplicate mount points dropped,
octal escapes decoded) and `~/.ssh/config` through `dtr-sshconfig`. Every reader
has a `from text` half, so the filters are unit tests without a machine. A host
row opens at `.`; `dtr-vfs::Ssh` has no `home()` and the window will not invent
one.

`src/worker.rs` is the "UI thread asks for slow work" rule for this process: one
thread, one compio runtime, three jobs. A remote listing through
`dtr_vfs::Ssh::list` collected into one piece for `Browser::apply_listing`; a
local count for the status bar; and the browser's `WriteRequest`s executed
through the VFS verbs, with `replace` clearing the destination with that removal
first. The conflict case is why the worker has its own result type: `dtr-files`
chooses its overwrite card by downcasting to `dtr_vfs::Conflict`, and an
`anyhow::Error` cannot cross a thread — so the worker carries the destination and
the window rebuilds the typed error. Local browsing needs none of this: the
browser streams `Local::list` itself, as in the picker.

### 10.4 Tests and verification

The devShell gate is unchanged and green:

```sh
nix develop .#default --command bash -c \
  'cargo fmt --all && cargo test -p dtr-portal && cargo build -p dtr-portal'
```

New tests, all without a window: the mount reader (pseudo/root/duplicate
filtering, octal escapes), the ssh-config reader (aliases, wildcards and
negations, in file order once), the screen registry (visible count and routing
by manager id), the idle policy, the manager's history across machines, its
sidebar tick, its count plumbing, a typed relative location, and the worker's
counts, its five write verbs through the real backends, and the conflict
boundary. The private-bus suite serves **both** interfaces on one private
`dbus-daemon`: the new half asserts the `org.dtr.Files` introspection signature,
that an `OpenWindow` reaches the headless opener with its path/title/parent/modal
and answers the opener's handle, that `CloseWindow` reaches the same window, that
an empty path is home and an unknown option is ignored, that a refused window is
an `error` (with the reason), that a wrong-typed option is `InvalidArgs` before
the opener, and that the shipped `dtr-files` binary exits non-zero on a bus with
no daemon. `DTR_PORTAL_CAPTURE` is how the window itself is photographed; the
Xvfb run is §10.5.

The package grew by one binary and one activation file, and `nix build
.#dtr-portal .#checks.x86_64-linux.dtr-portal-packaging` is the evidence that
both binaries are installed and both service files name the installed daemon:

```
$ ls $out/bin
dtr-files  dtr-portal
```

The packaging check now also asserts that `org.dtr.Files.service`'s `Name` is
`org.dtr.Files`, that its `Exec=` is the same installed binary as the chooser's,
and that `bin/dtr-files` exists.

### 10.5 Verification, and what F4 does not do

The window is verified under Xvfb through **D-Bus activation**, not a
pre-started daemon: a private `dbus-run-session` bus is started with
`XDG_DATA_DIRS` pointing at a directory holding both `.service` files whose
`Exec=` is `target/debug/dtr-portal`; `examples/manager_call.rs` calls
`OpenWindow`, which is the call that activates the daemon. The daemon maps a
1000 × 700 manager window on a scratch directory whose `pictures/` holds
`inside.png`; the client sets X input focus and sends `Right`, `Enter` with
**XTEST** — the X server's own input path, so the window's `KeyboardInput`
handler is what moves the listing — and the daemon's capture (4000 ms after the
window appeared) shows `/tmp/f4-scratch/pictures`, `inside.png`, `1 item` and the
status bar's path. The log's `manager key` lines were the check that the keys
were *the window's*, and the PNG is the check that the surface, the skin, the
sidebar, the grid and the bar agree. `dtr-files` itself is run the same way: its
handle is printed, the window captured, `CloseWindow` sent with `dbus-send`, and
the daemon exits at its idle timeout (exit 0). The picker's capture and the
picker half of the private-bus suite still pass unchanged.

What is *not* done, exactly:

- **The pane's header is drawn inside the content.** `pane_ui` has no
  body-only seam, so the manager window shows the window toolbar's breadcrumb and
  the pane's own header breadcrumb (the capture has both). A `pane_body_ui` in
  `dtr-files` is the fix, and that crate was out of scope.
- **The status count is a second listing.** `Browser` has no public entry count,
  so the worker counts the local directory in its own walk. The count can lag a
  frame, and the search box stores text without filtering because `dtr-files`
  exposes no search API either — the status bar's `shown of total` can
  consequently never appear.
- **The selection's total size is `None`.** `selected_paths()` gives the count
  but not the sizes; the bar shows the count and drops the total, which the
  chrome documents as the "size unknown" state.
- **Host rows open at `.`** (`Ssh` has no `home()`), **drives are Linux-only**
  (`/proc/mounts`; Windows has no drives section), and **sidebar rows are text** —
  no theme icon resolver lives in this process.
- **No write chords.** The browser's verbs are reachable through the pane's
  right-click menu only; mapping `F2`/`Delete`/`Ctrl+Shift+N` is the terminal's
  `[keys]` business (F6), not this window's.
- **A remote file does not open.** `Enter`/the menu on a remote file sets the
  browser's download request, which this window does not pump (downloads are
  F5/F7); local files go to `xdg-open`. `CopyPath` uses egui's in-app clipboard,
  so it does not reach other applications.
- **Remote browsing has no live-server evidence.** The pump is wired to
  `dtr_vfs::Ssh` (the same backend the terminal's panes use) but the verification
  run had no ssh host; its list path is the one the VFS's own scripted-wire tests
  cover.
- **The warm picker window is still created visible and hidden two calls later.**
  `dtr-frame::WindowOptions` has no `visible`, and adding it would have required
  editing `crates/dtr`'s exhaustive struct literal, which this task could not
  touch; the flash risk on a real window manager stays F3's documented gap.
- **One manager window per call, no tabs, no splitter.** Several `dtr-files`
  calls open several windows (the map and the worker route by id), but the
  milestone's window has one content pane.
