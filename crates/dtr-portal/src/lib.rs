//! The desktop's file chooser, answered by dtr's browser.
//!
//! An application that asks the desktop for a file does not get a dialog from
//! the desktop: `xdg-desktop-portal` receives the request and forwards it to a
//! **backend** — `org.freedesktop.impl.portal.FileChooser` — which is the thing
//! that actually shows a window. GNOME's backend links GTK and builds a chooser
//! per request; this crate is the other half of that same interface, so that the
//! picker is dtr's browser and its process is already warm (`docs/manager.md`
//! §2, §5).
//!
//! What is here:
//!
//! - [`Portal`], serving `OpenFile`, `SaveFile` and `SaveFiles` on the session
//!   bus as `org.freedesktop.impl.portal.desktop.dtr`, with the signatures read
//!   from the installed `xdg-desktop-portal` 1.22.1 XML rather than remembered;
//! - [`options`], which decodes the `a{sv}` a frontend sends onto
//!   [`dtr_files::pick::Request`] and holds the parts the picker has no field
//!   for (the raw filter list, so `current_filter` can be echoed back);
//! - [`uri`], which turns a path's **bytes** into a `file://` URI — a path is
//!   not promised to be UTF-8, so `to_string_lossy` is not an encoder;
//! - [`Host`], the one seam the window plugs into: a host receives a [`Call`] and
//!   answers through its [`Reply`];
//! - [`window`], the real host: one `dtr-frame` window, hidden and warm between
//!   requests, drawing the F2 picker (`dtr_files::pane_ui` in `Mode::Pick`) and
//!   answering through the same `Reply`. The binary's `main` runs that loop.
//!
//! ## Threads
//!
//! The D-Bus task must never block on I/O. zbus dispatches each method call on a
//! task of its own, so a pending request is a future waiting on a channel and
//! `Request.Close()` from the frontend runs while it waits. A [`Host`] is called
//! on that task under a mutex, so [`Host::open`] must **post** the work and
//! answer later through the [`Reply`]. A host that read a directory, or waited
//! for a person, inside [`Host::open`] would stall every other method on the
//! connection.
//!
//! **The window loop is the process's main thread.** winit refuses to create its
//! event loop anywhere else, so the split is: `main` builds and runs the picker
//! loop, and the D-Bus connection is the thread that moves. The listing is a
//! thread of its own again — `dtr-vfs`'s local backend walks a directory on one
//! and streams batches back, which the window only drains per frame.
//!
//! ## Packaging
//!
//! Three text files make a backend loadable; `crates/dtr-portal/assets/` holds
//! all three and `docs/portal.md` is the account:
//!
//! - `dtr.portal` — the `share/xdg-desktop-portal/portals/` entry that names the
//!   bus name and the interface;
//! - `org.freedesktop.impl.portal.desktop.dtr.service` — the bus-activation file
//!   whose `Exec=` is the installed binary (substituted at build time);
//! - `portals.conf` — the user's `[preferred]` mapping, an example rather than
//!   something a package should install.
//!
//! `flake.nix` installs the first two and grows a `nixosModules.dtr-portal` that
//! sets `xdg.portal.config` and `xdg.portal.extraPortals`.

mod host;
pub mod options;
mod paths;
mod portal;
mod uri;
pub mod window;
mod x11;

pub use host::{Answer, Call, Chosen, Host, Reply, RequestId, WindowContext};
pub use options::{DecodeError, Decoded, Kind, Options, PortalFilter};
pub use paths::unique_path;
pub use portal::{INTERFACE, OBJECT_PATH, Portal, REQUEST_INTERFACE, SERVICE_NAME};
pub use uri::file_uri;

// The browser's own vocabulary, re-exported because a host cannot implement
// anything without it: the request it is handed is a `Request`, the answer is a
// list of paths, and the picker it will draw is fed `Filter`s.
pub use dtr_files::pick::{Filter, Mode, Pick, Request, Select};
