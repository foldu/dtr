//! A menu that opens at a point: what a right-press asks for.
//!
//! It is not a [`Popup`](egui::Popup) because a popup belongs to a *control* —
//! the sort selector and the places button each hang their menu off their own
//! response — and a right-press has no control, only a pointer. The area is
//! placed where the press was, kept inside the window like every other menu
//! ([`constrain`](egui::Area::constrain)), and dismissed the way the rest of the
//! tree dismisses one: an Escape inside the widget, and any other key or a press
//! outside by the caller, which is the only thing that has the press.
//!
//! The caller keeps the state, like every primitive here: [`ContextMenu`] is a
//! value the screen owns, `open_at` is the right-press, and the answer is an index
//! into the rows. That is what lets both a painted pane and a test open it without
//! a window to anchor it to.
//!
//! Rows are [`Row`]s and draw through [`menu_row`](super::menu_row), so a
//! right-click menu and a dropdown are the same row: a chord at the right in the
//! dim colour, the pointer's row filled with `control_hover`, and a tick on the
//! entry that is already the value.

use crate::skin::Skin;
use egui::{Pos2, Rect, Ui};

use super::menu::{menu_row, menu_row_chord};

/// One line of the menu.
#[derive(Clone, Copy, Debug)]
pub struct Row<'a> {
    pub label: &'a str,
    /// The chord that does the same thing without the menu, drawn at the right.
    ///
    /// Omitted for a row whose chord is the caller's to know: the pane cannot say
    /// what `[keys]` has rebound, and a menu that names the default is a menu that
    /// lies the first time somebody rebinds it.
    pub chord: Option<&'a str>,
}

impl<'a> Row<'a> {
    pub fn new(label: &'a str) -> Self {
        Self { label, chord: None }
    }

    /// The same row with the chord that does it, if the chord is fixed.
    pub fn chord(mut self, chord: &'a str) -> Self {
        self.chord = Some(chord);
        self
    }
}

/// A menu that opens where a press was.
#[derive(Clone, Copy, Debug)]
pub struct ContextMenu {
    open: bool,
    /// Where the press was, in points. The body is placed from here and pushed
    /// back inside the window when it would hang off the edge.
    at: Pos2,
    /// Where the body was last drawn, so a press outside it can be told apart
    /// from the press that chose a row.
    rect: Rect,
}

impl Default for ContextMenu {
    fn default() -> Self {
        Self {
            open: false,
            at: Pos2::ZERO,
            rect: Rect::NOTHING,
        }
    }
}

impl ContextMenu {
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Is `at` inside the menu as it was last drawn?
    pub fn contains(&self, at: Pos2) -> bool {
        self.open && self.rect.contains(at)
    }

    /// Open at a point, and forget where the last one was drawn: a rectangle
    /// from a menu that is gone says nothing about this one.
    pub fn open_at(&mut self, at: Pos2) {
        self.open = true;
        self.at = at;
        self.rect = Rect::NOTHING;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.rect = Rect::NOTHING;
    }

    /// Draw it, and report which row was chosen.
    ///
    /// It takes the context rather than a `Ui` because the body is its own top
    /// level: a menu drawn inside the pane that opened it would be clipped to the
    /// pane, and a right-press near an edge puts the body past one.
    ///
    /// `id` must be unique in the surface and stable across frames — the caller's,
    /// because two menus on one window are two areas.
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        skin: &Skin,
        id: egui::Id,
        rows: &[Row<'_>],
    ) -> Option<usize> {
        if !self.open || rows.is_empty() {
            return None;
        }
        // Escape is the menu's own: it is the only thing that knows it is open,
        // and a modal dismissed somewhere else drifts out of sync with itself
        // (`LinkMenu` learned this first).
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.close();
            return None;
        }
        let palette = skin.palette;
        let style = &skin.style;
        let mut chosen = None;
        let response = egui::Area::new(id)
            // Above the panes, the palette's foreground layer and the notice: a
            // menu is only useful over what it is about.
            .order(egui::Order::Foreground)
            .fixed_pos(self.at)
            // A menu opened near an edge would otherwise put its last row off
            // screen, which is the row a right-press was aiming at.
            .constrain(true)
            .fade_in(false)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(palette.card)
                    .stroke(egui::Stroke::new(style.border, palette.border_strong))
                    .corner_radius(style.control_radius)
                    .inner_margin(egui::Margin::same(6))
                    .show(ui, |ui| {
                        ui.set_min_width(width(ui, skin, rows));
                        for (index, row) in rows.iter().enumerate() {
                            let hit = match row.chord {
                                Some(chord) => menu_row_chord(ui, skin, row.label, chord),
                                None => menu_row(ui, skin, row.label, false),
                            };
                            if hit {
                                chosen = Some(index);
                            }
                        }
                    });
            });
        self.rect = response.response.rect;
        if chosen.is_some() {
            self.close();
        }
        chosen
    }
}

/// The width the body needs: its widest row, a chord's gap included, inside one
/// inset and the frame's margins.
///
/// Measured rather than fixed, for the reason [`menu_width`](super::menu_width)
/// measures a popover: a body left to decide for itself takes the width it is
/// offered, and a menu two words wide would span a screen.
fn width(ui: &Ui, skin: &Skin, rows: &[Row<'_>]) -> f32 {
    let palette = skin.palette;
    let style = &skin.style;
    let widest = rows
        .iter()
        .map(|row| {
            let label = ui
                .painter()
                .layout_no_wrap(row.label.to_owned(), style.value_font.clone(), palette.text)
                .size()
                .x;
            match row.chord {
                Some(chord) => {
                    label
                        + style.gap * 2.0
                        + ui.painter()
                            .layout_no_wrap(chord.to_owned(), style.label_font.clone(), palette.dim)
                            .size()
                            .x
                }
                None => label,
            }
        })
        .fold(0.0_f32, f32::max);
    widest
        + style.padding.x * 2.0
        + f32::from(style.menu_margin.left)
        + f32::from(style.menu_margin.right)
}
