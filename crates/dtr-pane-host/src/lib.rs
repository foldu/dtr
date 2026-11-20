//! The glue between a window frontend and [`dtr_files`]' browser.
//!
//! One browser, three frontends: the terminal's file pane, the desktop's manager
//! window and its picker all drive the same [`dtr_files::Browser`]. What differs
//! between them is the window, the thread and the event loop — not what a key
//! means or when a press is a double-click. Those are written once, here, because
//! two spellings of either is how the picker and the manager learned to disagree
//! about the chords they accept.
//!
//! What is here:
//!
//! - [`input::pane_key`] — the winit key to [`dtr_files::Key`] table, gated by
//!   [`input::Scope`] so a picker (or a read-only backend) is never offered a
//!   write verb.
//! - [`Gestures`] — the click-run counter: one press, two, three, within 400 ms
//!   and four pixels.
//!
//! What is not here, deliberately: a window, an event loop, a runtime or a
//! channel to a thread. Each frontend owns its own.

pub mod gestures;
pub mod input;

pub use gestures::{CLICK_SLOP, DOUBLE_CLICK, Gestures};
pub use input::{Scope, pane_key};
