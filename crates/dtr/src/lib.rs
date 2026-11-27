//! dtr — a super serial terminal, and a file browser for some reason.
//!
//! The crate is a library plus a thin binary for one reason: the file browser is
//! becoming a manager *and* the desktop's file picker, and the picker is a second
//! process. A binary-only crate cannot hand a listing, a sort or a hit test to
//! another frontend, and `docs/manager.md` §2 is the plan for which of those move
//! out (`dtr-files`, `dtr-frame`) and in what order.
//!
//! Everything here is `pub` because it is all the same application: `publish` is
//! off workspace-wide, so there is no API to promise anyone, and `pub(crate)` on
//! thirty modules would only mean the binary's `use` list grows a re-export per
//! item. The module boundaries are the documentation.

pub mod about;
pub mod app;
pub mod chrome;
pub mod conpty;
pub mod fonts;
pub mod gpu;
pub mod image_overlay;
pub mod ime;
pub mod input;
pub mod keymap;
pub mod keys;
pub mod kitty;
pub mod layout;
pub mod linkmenu;
pub mod logbook;
pub mod modem;
pub mod notify;
pub mod pager;
pub mod palette;
pub mod pane;
pub mod profile;
pub mod pty;
pub mod render;
pub mod screenshot;
pub mod serial;
pub mod serial_dialog;
pub mod settings;
pub mod sources;
pub mod sprite;
pub mod stats;
pub mod system;
pub mod terminfo;
pub mod theme;
pub mod thumb;
pub mod tmux;
pub mod transfer;
pub mod transfer_card;
pub mod ui;
pub mod uri;
pub mod watch;
pub mod worker;
