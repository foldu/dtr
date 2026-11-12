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
//! control that opened it, and [`point_at`] is where the body goes so the tip can
//! point at its middle.
//!
//! A row may also carry a mark before its label — the system's own picture of the
//! program it leads to. [`menu_row_icon`] takes the pixels as a texture, because
//! they come from the platform at run time rather than from an asset in the
//! binary, and [`menu_width`] is told which rows have one so the extra column is
//! measured rather than discovered after the body has been drawn too narrow.

use egui::{Sense, Ui};

use crate::skin::Skin;

use super::check::tick;

/// Draw one menu row and report whether the pointer chose it.
pub fn menu_row(ui: &mut Ui, skin: &Skin, label: &str, selected: bool) -> bool {
    row(ui, skin, label, selected, None, None, false)
}

/// The picture a row may lead with.
///
/// Two sources, because there are two: the platform's own picture of a program,
/// which arrives as pixels the caller uploaded, and an embedded picture, which
/// egui's loader decodes. Both are drawn as they are — a brand mark keeps its
/// colour, and so does a system icon — so a row asks for no tint.
pub enum Mark<'a> {
    /// Pixels the caller uploaded: the platform's icon of a program.
    Texture(egui::TextureId),
    /// An embedded picture: a shell's own logo.
    Image(egui::ImageSource<'a>),
}

impl<'a> From<egui::TextureId> for Mark<'a> {
    fn from(texture: egui::TextureId) -> Self {
        Self::Texture(texture)
    }
}

impl<'a> From<egui::ImageSource<'a>> for Mark<'a> {
    fn from(source: egui::ImageSource<'a>) -> Self {
        Self::Image(source)
    }
}

/// A row that leads with the picture of what it is, before the label.
///
/// The mark is a [`Mark`]: pixels the platform handed back at run time — so
/// nothing is embedded — or an embedded picture the loader decodes. `None` keeps
/// the column and draws nothing in it, which is the point: the shells of one
/// machine do not all have a logo, and a group whose labels start at two different
/// x positions reads as a formatting mistake rather than as two kinds of row. A
/// group with no marks at all is [`menu_row`]s, and does not pay for the column.
pub fn menu_row_icon<'a>(
    ui: &mut Ui,
    skin: &Skin,
    mark: impl Into<Option<Mark<'a>>>,
    label: &str,
    selected: bool,
) -> bool {
    row(ui, skin, label, selected, None, mark.into(), true)
}

/// The same row with the chord that does it without the menu, at its right.
///
/// libadwaita's `popover.menu accelerator` is the item's own font at
/// `--dim-opacity`, and that is why the chord is dim and right-aligned rather
/// than a second accent: it is the same sentence as the label, said more
/// quietly.
pub fn menu_row_chord(ui: &mut Ui, skin: &Skin, label: &str, chord: &str) -> bool {
    row(ui, skin, label, false, Some(chord), None, false)
}

/// The side of a row's mark, in points.
///
/// The size of a menu icon on both desktops, and the largest that fits a dtr row
/// (23 points) with air above and below it. The picture is drawn down into this
/// box rather than the box grown to fit it: the shell answers with a 32-pixel
/// icon, which is exactly this at 2×, so a HiDPI window draws it at full
/// resolution, and an embedded logo has a viewBox rather than a size.
const MARK: f32 = 16.0;

fn row<'a>(
    ui: &mut Ui,
    skin: &Skin,
    label: &str,
    selected: bool,
    chord: Option<&str>,
    mark: Option<Mark<'a>>,
    lead: bool,
) -> bool {
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
    // the mark a chosen value carries, and the picture a row may lead with.
    let tick_side = (style.row * 0.5).min(14.0);
    let tick_room = if selected { tick_side + style.gap } else { 0.0 };
    // The column is reserved by `lead`, not by the picture: a shell with no logo
    // keeps the indent, so a group of shells lines up.
    let leading = if lead { MARK + style.gap } else { 0.0 };
    let mut needed = galley.size().x + style.padding.x * 2.0 + tick_room + leading;
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
    if let Some(mark) = mark {
        // Untinted, and the quad is the row's rather than the picture's: a brand
        // mark keeps its colour, a 32-pixel answer from the shell is drawn at 16
        // points, and neither is a shade of the text.
        let picture = egui::Rect::from_center_size(
            egui::pos2(rect.left() + style.padding.x + MARK / 2.0, rect.center().y),
            egui::vec2(MARK, MARK),
        );
        match mark {
            Mark::Texture(texture) => {
                painter.image(
                    texture,
                    picture,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
            // `paint_at` rather than the painter's own `image`: an embedded
            // picture may still be decoding, and egui draws it when it arrives and
            // asks for the frame that will.
            Mark::Image(source) => {
                egui::Image::new(source)
                    .fit_to_exact_size(picture.size())
                    .paint_at(ui, picture);
            }
        }
    }
    // The label starts at the row's own padding. It used to start a column
    // further in, a tick gutter reserved on every row of every menu for a mark
    // that the app menu and the new-tab menu never draw — which is what made a
    // short menu read as a column of indented words.
    painter.galley(
        egui::pos2(
            rect.left() + style.padding.x + leading,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        palette.text,
    );
    if let Some(chord) = chord {
        painter.galley(
            egui::pos2(
                rect.right() - style.padding.x - tick_room - chord.size().x,
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

/// One row as the width measure sees it: its label, and whether it reserves the
/// column a mark goes in.
#[derive(Clone, Copy, Debug)]
pub struct RowSpec<'a> {
    pub label: &'a str,
    pub icon: bool,
}

/// The width a popover needs for its rows, insets and a scroll bar included.
///
/// A row fills the width it is offered — that is what makes the hover fill span
/// the menu rather than the text — so a body left to decide for itself takes
/// everything from the button to the window's edge: the new-tab menu was 600
/// points wide with its words bunched at the left. The caller measures the rows
/// it is about to draw and gives the body that width, which is what every
/// popover on the desktop does: as wide as the widest row, and no wider.
///
/// The bar is *always* counted. A solid scroll bar takes its width out of the
/// rows, and it appears only when the list is taller than the room there is; a
/// body that grows a bar and clips its longest row because nobody reserved the
/// space is worse than one with a few points to spare.
///
/// A row that leads with a mark is measured with it, because the mark is part of
/// the row: the column is only as wide as the rows that use it, so a menu of
/// three shells and a long serial port still hugs the port.
pub fn menu_width(ui: &Ui, skin: &Skin, rows: &[RowSpec<'_>]) -> f32 {
    let style = &skin.style;
    let widest = rows
        .iter()
        .map(|row| {
            let label = ui
                .painter()
                .layout_no_wrap(
                    row.label.to_owned(),
                    style.value_font.clone(),
                    skin.palette.text,
                )
                .size()
                .x;
            if row.icon {
                label + MARK + style.gap
            } else {
                label
            }
        })
        .fold(0.0_f32, f32::max);
    let scroll = &ui.spacing().scroll;
    let bar = scroll.bar_width + 2.0 * scroll.bar_inner_margin;
    widest
        + style.padding.x * 2.0
        + f32::from(style.menu_margin.left)
        + f32::from(style.menu_margin.right)
        + bar
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

/// Aim a menubutton's popover at the middle of its button, with the beak's room
/// under it.
///
/// A beak cannot sit on the body's rounded corner, so a body hung off the
/// button's edge drags its beak inwards by a corner plus half the base and stops
/// pointing at the button: the `+`'s menu, 600 points wide with the `+` near the
/// window's left edge, had its beak 20 points to the right of the `+`. GNOME's
/// rule is the one here — the body centres on the button, and a body too wide to
/// centre is pushed back inside the window rather than hanging off it. Either
/// way the beak keeps the button's own centre to point at; only a body pinned to
/// a window edge, with the button nearer to that edge than a corner, leaves the
/// beak to give way.
///
/// The width is egui's own estimate for the popup, the same number its own
/// alignment is computed from, because the body has not been laid out yet.
///
/// The anchor rect is the button's, moved sideways rather than shortened: the
/// body still opens below the button, and flips above it when it has to.
pub fn point_at<'a>(
    ui: &Ui,
    popup: egui::Popup<'a>,
    button: egui::Rect,
    tip: f32,
) -> egui::Popup<'a> {
    let width = popup.get_expected_size().map_or(0.0, |size| size.x);
    let centre = body_centre(button.center().x, width, ui.ctx().content_rect());
    let anchor = egui::Rect::from_center_size(egui::pos2(centre, button.center().y), button.size());
    popup
        .anchor(egui::PopupAnchor::ParentRect(anchor))
        .align(egui::RectAlign::BOTTOM)
        .gap(tip)
}

/// The x the body's middle wants to be at: the button's, pushed back inside the
/// window by however much a body that wide would stick out.
fn body_centre(button_x: f32, width: f32, content: egui::Rect) -> f32 {
    let half = width / 2.0;
    (button_x - half).clamp(
        content.left(),
        (content.right() - width).max(content.left()),
    ) + half
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
    use super::{body_centre, tip_x};

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

    /// Placement and clamp together. A body too wide to centre on its button is
    /// pushed back to the window's edge, and the tip still has the button's own
    /// centre to point at: the `+` menu, 600 points wide with the `+` at x=252 in
    /// a 900-point window, used to put its beak 20 points to the right of it.
    #[test]
    fn a_body_too_wide_to_centre_still_points_at_the_button() {
        let content = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(900.0, 600.0));
        let button = egui::Rect::from_min_size(egui::pos2(235.0, 0.0), egui::vec2(34.0, 30.0));
        let centre = body_centre(button.center().x, 600.0, content);
        assert_eq!(centre, 300.0, "the body is pinned to the left edge");
        let body =
            egui::Rect::from_center_size(egui::pos2(centre, 130.0), egui::vec2(600.0, 200.0));
        assert_eq!(
            tip_x(body, button.center().x, 12.0, 12.0),
            button.center().x
        );
    }

    /// A body that fits centres on the button, so its tip needs no clamp at all.
    #[test]
    fn a_body_that_fits_centres_on_the_button() {
        let content = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(900.0, 600.0));
        let button = egui::Rect::from_min_size(egui::pos2(774.0, 0.0), egui::vec2(28.0, 30.0));
        let centre = body_centre(button.center().x, 193.0, content);
        assert_eq!(centre, button.center().x);
        let body =
            egui::Rect::from_center_size(egui::pos2(centre, 130.0), egui::vec2(193.0, 200.0));
        assert_eq!(
            tip_x(body, button.center().x, 12.0, 12.0),
            button.center().x
        );
    }
}
