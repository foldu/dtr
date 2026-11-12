//! A row in a menu: a full-width target, an optional chord at its right, and a
//! tick on the entry that is already the value.
//!
//! egui's own menu rows are `SelectableLabel`s, which paint the *text selection*
//! colour — the same blue a dragged-over word gets, in a list where nothing is
//! being selected. A menu row is a control: the pointer's row is `control_hover`,
//! and the row a value came from is marked with a tick rather than a fill,
//! because a menu where the chosen entry looks like a pressed button is louder
//! than the choice it is trying to show.
//!
//! [`menu_tip`] is the other half of a popover: the triangle pointing back at the
//! control that opened it.

use egui::{Sense, Ui};

use crate::skin::Skin;

use super::check::tick;

/// Draw one menu row and report whether the pointer chose it.
pub fn menu_row(ui: &mut Ui, skin: &Skin, label: &str, selected: bool) -> bool {
    row(ui, skin, label, selected, None)
}

/// The same row with the chord that does it without the menu, at its right.
///
/// libadwaita's `popover.menu accelerator` is the item's own font at
/// `--dim-opacity`, and that is why the chord is dim and right-aligned rather
/// than a second accent: it is the same sentence as the label, said more
/// quietly.
pub fn menu_row_chord(ui: &mut Ui, skin: &Skin, label: &str, chord: &str) -> bool {
    row(ui, skin, label, false, Some(chord))
}

fn row(ui: &mut Ui, skin: &Skin, label: &str, selected: bool, chord: Option<&str>) -> bool {
    let palette = skin.palette;
    let style = &skin.style;
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_owned(), style.value_font.clone(), palette.text);
    let chord = chord.map(|chord| {
        ui.painter()
            .layout_no_wrap(chord.to_owned(), style.label_font.clone(), palette.dim)
    });
    // What the row needs before the menu gives it a width: the label, the chord,
    // and the mark a chosen value carries.
    let tick_side = (style.row * 0.5).min(14.0);
    let mark = if selected { tick_side + style.gap } else { 0.0 };
    let mut needed = galley.size().x + style.padding.x * 2.0 + mark;
    if let Some(chord) = &chord {
        needed += chord.size().x + style.gap * 2.0;
    }
    let width = ui.available_width().max(needed);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, style.row), Sense::click());

    let painter = ui.painter();
    // Only the pointer's row is filled: a menu's own background is the frame's,
    // and painting every row with the card colour bands the list instead.
    if response.hovered() {
        painter.rect_filled(rect, style.control_radius, palette.control_hover);
    }
    // The label starts at the row's own padding. It used to start a column
    // further in, a tick gutter reserved on every row of every menu for a mark
    // that the app menu and the new-tab menu never draw — which is what made a
    // short menu read as a column of indented words.
    painter.galley(
        egui::pos2(
            rect.left() + style.padding.x,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        palette.text,
    );
    if let Some(chord) = chord {
        painter.galley(
            egui::pos2(
                rect.right() - style.padding.x - mark - chord.size().x,
                rect.center().y - chord.size().y / 2.0,
            ),
            chord,
            palette.dim,
        );
    }
    if selected {
        // On the right, where a dropdown puts its check, rather than in a column
        // on the left: `popover.menu check.right` is libadwaita's own spelling
        // for the list under a field, and a left tick pushed every label in to
        // make room for a mark only one row had.
        let centre = egui::pos2(
            rect.right() - style.padding.x - tick_side / 2.0,
            rect.center().y,
        );
        tick(
            painter,
            egui::Rect::from_center_size(centre, egui::vec2(tick_side, tick_side)),
            palette.accent,
        );
    }

    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected, label));
    if response.has_focus() {
        response.surrender_focus();
    }
    super::pointer_only(response).clicked()
}

/// A rule between groups in a menu.
///
/// Not `ui.separator()`: egui's takes the whole width it is offered, which is
/// the popup's area rather than the row's, so the line crossed the menu's
/// rounded corners. Here the rule is as wide as a row, which is what
/// libadwaita's `popover.menu separator` is — the content's width, inside the
/// popover's own inset — and the air around it is [`Style::gap`], the same gap
/// that separates two rows.
pub fn menu_separator(ui: &mut Ui, skin: &Skin) {
    let style = &skin.style;
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), style.gap), Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        egui::Stroke::new(style.border, skin.palette.border),
    );
}

/// Draw the tip of a popover: the little triangle that points back at the
/// control the popover came from.
///
/// `popup` is the response [`egui::Popup::show`] returned. Its rectangle is the
/// popover's own — frame, inset and all — and its layer is the one the frame was
/// painted into, so the tip lands above the body and its shadow rather than
/// under them. `anchor` is the control's rectangle, in the same coordinates: the
/// tip goes on the edge facing it, at the control's centre, clamped so it never
/// runs onto a rounded corner. That clamp is what lets a menu whose body extends
/// past the button it belongs to — the app menu at the end of a tab strip —
/// still point at it.
///
/// The shape is libadwaita's `popover > arrow`: a 45-degree triangle filled with
/// the popover's own background and outlined with its own border, so body and tip
/// read as one outline, with the base a border's width inside the body to cover
/// the body's edge where the two meet.
///
/// The tip is drawn *outside* the body, so the body has to be placed a
/// [`crate::style::Style::tip_height`] clear of the control — `Popup::gap` — or
/// the tip grows into the control instead of into the space between them.
///
/// Nothing is drawn when [`crate::style::Style::tip_height`] is `None`: Windows
/// 11's menus and egui's own popups have no beak, and a tip that is not there is
/// a different shape rather than a smaller one.
pub fn menu_tip(popup: &egui::Response, skin: &Skin, anchor: egui::Rect) {
    let Some(height) = skin.style.tip_height else {
        return;
    };
    let style = skin.egui_style(&popup.ctx);
    let visuals = &style.visuals;
    let body = popup.rect;
    let border = skin.style.border;
    // Half a base each way from the apex: a 45-degree slope a side, as GTK's is,
    // which makes the base twice the height.
    let half = height;
    let radius = visuals
        .menu_corner_radius
        .nw
        .max(visuals.menu_corner_radius.ne) as f32;
    let x = tip_x(body, anchor.center().x, radius, half);
    // A popover that dropped below its control grows its tip on the top edge; one
    // that flipped up because the window ended grows it underneath. `out` points
    // away from the body, which is the only thing that differs between the two.
    let out = if anchor.center().y >= body.center().y {
        1.0
    } else {
        -1.0
    };
    let edge = if out > 0.0 { body.bottom() } else { body.top() };
    let apex = egui::pos2(x, edge + out * height);
    let base = edge - out * border;
    let painter = popup.ctx.layer_painter(popup.layer_id);
    // The outline is the border-coloured triangle *behind* the filled one, not a
    // stroke along the slopes: a one-point stroke at 45 degrees lands on a pixel
    // grid corner at a time and reads as a chain of dots, where a filled band
    // anti-aliases like the body's own edge. The band is `border` wide
    // perpendicular to each slope, which for a 45-degree slope is `border * √2`
    // in each axis.
    if border > 0.0 && visuals.window_stroke.width > 0.0 {
        let flare = border * std::f32::consts::SQRT_2;
        painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(x, edge + out * (height + flare)),
                egui::pos2(x - half - flare, base),
                egui::pos2(x + half + flare, base),
            ],
            visuals.window_stroke.color,
            egui::Stroke::NONE,
        ));
    }
    // Its base reaches a point past the outline's, so the band is covered where
    // the tip meets the body and only the two slopes are left showing.
    painter.add(egui::Shape::convex_polygon(
        vec![
            apex,
            egui::pos2(x - half, base - out * skin.style.border),
            egui::pos2(x + half, base - out * skin.style.border),
        ],
        visuals.window_fill,
        egui::Stroke::NONE,
    ));
}

/// Where along the body's edge the tip sits: over the control it came from, but
/// never so close to an end that the tip's own corner runs onto the body's
/// rounded corner.
///
/// This is the whole of the "left, middle or right" the tip can do. A body
/// centred on its button gets the tip in the middle of the button; one that
/// hangs off to a side — because a centred menu would not fit — still points at
/// the control from as near to the middle as the corner allows.
fn tip_x(body: egui::Rect, anchor_x: f32, radius: f32, half: f32) -> f32 {
    let low = body.left() + radius + half;
    let high = body.right() - radius - half;
    if low <= high {
        anchor_x.clamp(low, high)
    } else if body.width() > half * 2.0 {
        // Narrower than two corners and a tip: there is no straight edge to sit
        // on, so stay inside the body and as near the control as that allows.
        body.center()
            .x
            .max(body.left() + half)
            .min(body.right() - half)
    } else {
        body.center().x
    }
}

#[cfg(test)]
mod tests {
    use super::tip_x;

    fn body(left: f32, right: f32) -> egui::Rect {
        egui::Rect::from_min_max(egui::pos2(left, 0.0), egui::pos2(right, 100.0))
    }

    /// A body wide enough for its corners puts the tip under the control wherever
    /// the control is along it — the left end, the middle, the right end.
    #[test]
    fn the_tip_sits_under_the_control() {
        let body = body(0.0, 200.0);
        assert_eq!(tip_x(body, 100.0, 12.0, 12.0), 100.0);
        assert_eq!(tip_x(body, 60.0, 12.0, 12.0), 60.0);
        assert_eq!(tip_x(body, 140.0, 12.0, 12.0), 140.0);
    }

    /// A control past the body's straight edge pulls the tip only as far as that
    /// edge: a corner's width plus half the base, so the base never pokes out
    /// through the curve.
    #[test]
    fn a_control_past_the_corner_stops_at_the_straight_edge() {
        let body = body(0.0, 200.0);
        assert_eq!(tip_x(body, 190.0, 12.0, 12.0), 176.0);
        assert_eq!(tip_x(body, 10.0, 12.0, 12.0), 24.0);
    }

    /// Whatever it is asked for, the tip stays inside the body — a body narrower
    /// than two corners included.
    #[test]
    fn the_tip_stays_inside_the_body() {
        for width in [200.0, 60.0, 40.0, 20.0] {
            let body = body(30.0, 30.0 + width);
            for anchor in [-100.0, 0.0, 50.0, 400.0] {
                let x = tip_x(body, anchor, 12.0, 12.0);
                assert!(
                    x >= body.left() && x <= body.right(),
                    "width {width}, anchor {anchor}: {x} left the body"
                );
            }
        }
    }
}
