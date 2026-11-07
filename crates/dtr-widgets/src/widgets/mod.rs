//! The control primitives, drawn with a [`crate::Skin`].
//!
//! Each one is a free function or a small builder that *reports* what the user
//! did; none of them owns the value it edits. That keeps the caller's state the
//! only state, which is what makes the surfaces testable: a harness runs the
//! widget, the caller records the answer.
//!
//! The rules that every primitive here has to keep, because they were each a bug
//! once in this tree:
//!
//! - **A painted widget has no accessibility node** unless it declares one, so
//!   every response gets a name and, where it has one, its value.
//! - **`Sense::click()` is not pointer-only**: egui turns Enter into a click for
//!   any focused widget that senses clicks, and Enter belongs to the shell. A
//!   hit is accepted only when the pointer actually clicked, and the widget
//!   surrenders focus after being drawn.
//! - **Controls must not overlap.** egui gives a press to the topmost widget
//!   that senses clicks, so a chevron drawn on top of a text field would take
//!   half the presses. The field and its dropdown arrow are disjoint rectangles.

mod combo;

pub use combo::{Edit, Editable};
