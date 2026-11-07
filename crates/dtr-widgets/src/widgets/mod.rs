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
//!   hit is accepted only through [`pointer_only`], which strips that synthetic
//!   click, and the widget surrenders focus after being drawn.
//! - **Controls must not overlap.** egui gives a press to the topmost widget
//!   that senses clicks, so a chevron drawn on top of a text field would take
//!   half the presses. The field and its dropdown arrow are disjoint rectangles.

mod block;
mod button;
mod check;
mod combo;
mod icons;
mod menu;
mod segmented;

pub use block::{label_cell, row, section};
pub use button::Button;
pub use check::check;
pub use combo::{Edit, Editable};
pub use icons::{cross, cross_at, plus};
pub use menu::menu_row;
pub use segmented::segmented;

/// Make a response pointer-only, so its `clicked()` cannot have come from Enter.
///
/// egui sets `FAKE_PRIMARY_CLICKED` when a focused widget is activated by Space
/// or Enter, and `Response::clicked()` counts it. Enter is the shell's key, so a
/// chip that kept that flag switched tabs when a program at the prompt was
/// waiting for a keystroke.
pub(crate) fn pointer_only(mut response: egui::Response) -> egui::Response {
    response
        .flags
        .remove(egui::response::Flags::FAKE_PRIMARY_CLICKED);
    response
}
