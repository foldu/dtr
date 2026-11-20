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
//! - [`window`], the real hosts: one `dtr-frame` loop, a picker window hidden and
//!   warm between requests, and a manager window per `dtr-files` request. The
//!   binary's `main` runs that loop, and the D-Bus side is the thread that moves.
//! - [`files`], the manager window's **private** interface (`org.dtr.Files`):
//!   `OpenWindow(s path, a{sv} options) -> (s handle)` and `CloseWindow(s handle)`.
//!   It shares the bus connection with the file chooser, so one process owns both
//!   and a manager window costs a `Frame`, not a process (`docs/manager.md` §6).
//! - [`manager`], the manager screen's model: the `Browser`, the history stacks,
//!   the sidebar's data and the answers it folds back into the browser.
//! - [`desktop`], the sidebar's sources: `user-dirs.dirs`, `/proc/mounts` and
//!   `~/.ssh/config`.
//! - [`worker`], one thread with a compio runtime for the manager's remote
//!   listings, local counts and write verbs, so the frame loop never waits on a
//!   directory (`docs/manager.md` §6).
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
//! Four text files make the two interfaces reachable; `crates/dtr-portal/assets/`
//! holds all four and `docs/portal.md` is the account:
//!
//! - `dtr.portal` — the `share/xdg-desktop-portal/portals/` entry that names the
//!   bus name and the interface;
//! - `org.freedesktop.impl.portal.desktop.dtr.service` — the file chooser's
//!   bus-activation entry, whose `Exec=` is the installed binary (substituted at
//!   build time);
//! - `org.dtr.Files.service` — the manager window's, the same binary and the same
//!   substitution, because `dtr-files` is a launcher and not a second daemon;
//! - `portals.conf` — the user's `[preferred]` mapping, an example rather than
//!   something a package should install.
//!
//! `flake.nix` installs the first three and grows a `nixosModules.dtr-portal`
//! that sets `xdg.portal.config` and `xdg.portal.extraPortals`.

pub mod desktop;
pub mod files;
mod host;
pub mod manager;
pub mod options;
mod paths;
mod portal;
mod uri;
pub mod window;
mod worker;
mod x11;

pub use files::{
    CloseWindowAck, FILES_INTERFACE, FILES_OBJECT_PATH, FILES_SERVICE_NAME, FilesService,
    OpenWindowAck, OpenWindowRequest, WindowOpener,
};
pub use host::{Answer, Call, Chosen, Host, Reply, RequestId, WindowContext};
pub use options::{DecodeError, Decoded, Kind, Options, PortalFilter};
pub use paths::unique_path;
pub use portal::{INTERFACE, OBJECT_PATH, Portal, REQUEST_INTERFACE, SERVICE_NAME};
pub use uri::file_uri;

// The browser's own vocabulary, re-exported because a host cannot implement
// anything without it: the request it is handed is a `Request`, the answer is a
// list of paths, and the picker it will draw is fed `Filter`s.
pub use dtr_files::pick::{Filter, Mode, Pick, Request, Select};
