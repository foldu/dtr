//! What the Windows shell knows, in one place.
//!
//! Three questions the rest of the tree has to ask a desktop, and one this
//! platform answers differently from the others:
//!
//! - **What does an entry look like?** [`icon`] — the colour page the registry
//!   associates with `.pdf`, the bitmap an `.exe` carries, the folder the shell
//!   draws for `Downloads`. On a freedesktop machine that answer is a PNG in
//!   `share/icons` and `dtr-files` reads it there; here it is an `HICON` and
//!   there is no file at all.
//! - **Where can a person go?** [`places`] — the known folders, which need not be
//!   where the profile would suggest, and the drives, which are the roots.
//! - **Which entries are hidden?** [`Dir`] — a directory walk whose attributes
//!   arrive with the name, because Windows hides a `FILE_ATTRIBUTE_HIDDEN` file
//!   and a dot means nothing there.
//!
//! **A crate, not a module**, so that the two callers keep their distance from the
//! platform: `dtr-files` asks for pixels and menu rows and never sees an `HICON`,
//! `dtr-vfs` asks for a directory and never sees a find handle, and only this crate
//! links `windows-sys`. It also puts the Win32 code in one file's worth of `unsafe`
//! instead of three.
//!
//! **Everything public compiles on any machine** and answers nothing on one with no
//! shell: [`icon`] is `None`, [`places`] is empty, [`Dir::open`] is an error. That is
//! what lets the rules with a decision in them — the drive labels, the mask-to-alpha
//! conversion, the file time — be tested on a Linux machine, and it is the only
//! reason this crate is not itself `cfg(windows)`.
//!
//! `docs/windows.md` §11 is the account: what the pictures looked like before this
//! existed, and what the shell answered that the theme could not.

mod dir;
mod icon;
mod places;

pub use dir::{Dir, Found};
pub use icon::{Icon, icon};
pub use places::{DriveKind, Place, drive_label, places};
