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
mod card;
mod check;
mod combo;
mod context;
mod icons;
mod menu;
mod segmented;
mod stripe;
mod tabs;
mod tile;

pub use block::{label_cell, row, section};
pub use button::Button;
pub use card::{Card, CardOutput, prime_area};
pub use check::{check, check_box};
pub use combo::{Edit, Editable};
pub use context::{ContextMenu, Row};
pub use icons::{cross, cross_at, plus};
pub use menu::{
    Mark, RowSpec, menu_row, menu_row_chord, menu_row_icon, menu_separator, menu_tip, menu_width,
    point_at,
};
pub use segmented::segmented;
pub use stripe::{Stripe, stripe};
pub use tabs::tabs;
pub use tile::{Glyph, Icon, IconTile, LABEL_ROWS, icon_tile, tile_height};

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

/// Whether a layer egui found under the pointer is one of its own surfaces — a
/// card, a popup, a file pane — rather than the window's root.
///
/// `Context::layer_id_at` is never `None` inside the window: the root layer's
/// rect is the whole content area, so it is found under the pointer wherever the
/// pointer is. Asking "is there a layer here" therefore answers *yes* over
/// anything that is painted rather than made of widgets, which made "the pointer
/// is over egui" true everywhere — a link under the pointer never lit its hand,
/// and a wheel over a menu was read as the text behind it. The layer *above* the
/// root is the question, and this is the one spelling of it: the window asks
/// before handing a wheel or a preview to the thing underneath, and a painted
/// pane asks in its own test that its body is click-through.
pub fn above_the_root(layer: egui::LayerId) -> bool {
    layer != egui::LayerId::background()
}

#[cfg(test)]
mod tests {
    use super::above_the_root;

    #[test]
    fn the_root_layer_is_not_a_surface() {
        assert!(!above_the_root(egui::LayerId::background()));
    }

    #[test]
    fn anything_above_the_root_is_a_surface() {
        // A painted pane is a `Background`-order area of its own, which is the
        // case the predicate exists for: same order as the root, different id.
        let area = egui::LayerId::new(egui::Order::Background, egui::Id::unique("file pane"));
        assert!(above_the_root(area));
    }
}
