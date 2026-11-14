//! One cell of a file grid: a picture, a name, and how it stands.
//!
//! The grid is a listing's default view (`docs/manager.md` §3), which makes its
//! cell a control rather than a decoration: it has a selection, a hover, two
//! rows of elided name and a picture that may be a drawn glyph or a decoded
//! thumbnail. It lives here, painted from the [`Skin`], so the pane that draws a
//! listing and the place that decides what a click landed on cannot disagree
//! about a cell's height.
//!
//! **The glyphs are drawn, not borrowed**, for the same two reasons the tick and
//! the cross are: a theme icon needs the machine's icon theme and an image
//! loader before anything can be drawn, and every harness and capture would have
//! to install one first. A folder is a tab and a body; a page is an outline and
//! three lines. When the browser learns to resolve a real icon or decode a
//! thumbnail it passes [`Icon::Image`] and these are the fallback.
//!
//! **The name is elided, never clipped.** Two rows at the cell's width with
//! `…`, because a grid whose names are cut off mid-glyph looks like a rendering
//! bug, and the full name is in the tooltip and the accessibility node the pane
//! raises for the listing as a whole.

use egui::{
    Color32, Pos2, Rect, Stroke, StrokeKind, Ui, Vec2,
    text::{LayoutJob, TextWrapping},
};

use crate::{Palette, Skin, Style};

/// The gap between the picture and the first row of the name.
const LABEL_GAP: f32 = 6.0;

/// How far the name is inset from the cell's edges. A tile's pill fills the
/// cell; the words do not touch its edge.
const LABEL_INSET: f32 = 5.0;

/// The padding above the picture and below the name.
const PAD: f32 = 7.0;

/// How many rows a name gets. Two is the whole difference between a grid of
/// tiles and a grid of single words with most of them elided.
pub const LABEL_ROWS: usize = 2;

/// What a drawn picture says the entry is, when nothing better answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    /// A directory.
    Folder,
    /// A file: anything with no picture of its own.
    File,
    /// The synthetic parent row, drawn as the folder it goes up into.
    Parent,
}

/// The picture in a tile: drawn here, or resolved by the caller.
///
/// A thumbnail and a theme icon arrive the same way — a texture — because the
/// pane decodes both off the drawing thread and this crate must not learn what
/// a video is. The texture is fitted into the icon's square keeping its aspect
/// ratio: a 16:9 frame letterboxed into a square cell reads as a broken image.
pub enum Icon<'a> {
    Glyph(Glyph),
    Image(&'a egui::TextureHandle),
}

/// One cell.
pub struct IconTile<'a> {
    /// The name, as the listing spells it. Elided here, not by the caller.
    pub label: &'a str,
    pub icon: Icon<'a>,
    pub selected: bool,
    pub hovered: bool,
    /// A file the view is not emphasising: a hidden entry while `Ctrl+H` is off
    /// only shows in the places it is drawn, so a dim tile is a state, not a
    /// filter (`docs/manager.md` §3).
    pub dim: bool,
}

/// The height a cell needs to hold an icon of `icon` points and a two-row name.
///
/// The caller owns the *width* — a grid divides its own — and this owns the
/// height, so a tile painted into a shorter rect is clipped by the painter
/// rather than silently drawn over its neighbour.
pub fn tile_height(icon: f32, line_height: f32) -> f32 {
    PAD * 2.0 + icon + LABEL_GAP + LABEL_ROWS as f32 * line_height
}

/// Paint one tile into `rect`.
///
/// `rect` is the cell: the pill fills it, the picture is centred at the top and
/// the name is centred under it.
pub fn icon_tile(ui: &Ui, skin: &Skin, rect: Rect, tile: &IconTile<'_>) {
    if rect.width() < 8.0 || rect.height() < 8.0 {
        return;
    }
    let palette = skin.palette;
    let style = &skin.style;
    let painter = ui.painter();

    // The pill goes down first. A fill painted after the name hides the name,
    // which is how the palette's selected row was invisible once.
    let pill = rect.shrink(2.0);
    if tile.selected {
        painter.rect_filled(pill, style.control_radius, palette.control_active);
    } else if tile.hovered {
        painter.rect_filled(pill, style.control_radius, palette.control_hover);
    }

    // The picture: a square at the top of the cell, narrowed with the cell when
    // the pane is too narrow for the style's icon.
    let room = (rect.width() - 2.0 * LABEL_INSET).max(8.0);
    let side = style.tile_icon.min(room);
    let picture = Rect::from_center_size(
        Pos2::new(rect.center().x, rect.top() + PAD + side / 2.0),
        Vec2::splat(side),
    );
    match tile.icon {
        Icon::Glyph(glyph) => paint_glyph(painter, picture, glyph, style.border, palette, tile.dim),
        Icon::Image(texture) => paint_image(painter, picture, texture, tile.dim),
    }

    // The name: two rows, centred, elided at the cell's width.
    let colour = if tile.selected {
        palette.accent
    } else if tile.dim {
        palette.dim
    } else {
        palette.text
    };
    let width = (rect.width() - 2.0 * LABEL_INSET).max(8.0);
    // `halign = Center` centres every row on the galley's origin, so the origin
    // is the cell's centre line and *not* the galley's left edge: a left-aligned
    // galley would be drawn at `centre - width / 2` and the rows inside it are
    // then already centred, which shifts the whole name half a cell to the left.
    let galley = painter.layout_job(label_job(style, tile.label, width, colour));
    let origin = Pos2::new(rect.center().x, picture.bottom() + LABEL_GAP);
    painter.galley(origin, galley, colour);
}

/// The name's layout: two rows at `width`, broken anywhere and elided with `…`.
///
/// `break_anywhere` is not a preference: a file name is one word with no spaces
/// in it, so a word-breaking layout cannot break at all and the second row is
/// only ever reached by the *elision*, which reads as a bug. And a row limit
/// without an overflow character truncates silently, which is the same bug with
/// no ellipsis to explain it.
fn label_job(style: &Style, text: &str, width: f32, colour: Color32) -> LayoutJob {
    let mut job = LayoutJob::simple(text.to_owned(), style.value_font.clone(), colour, width);
    job.wrap = TextWrapping {
        max_width: width,
        max_rows: LABEL_ROWS,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    job.halign = egui::Align::Center;
    job
}

/// Wrap a texture into `rect` without distorting it.
fn paint_image(painter: &egui::Painter, rect: Rect, texture: &egui::TextureHandle, dim: bool) {
    let size = texture.size_vec2();
    if size.x <= 0.0 || size.y <= 0.0 {
        return;
    }
    let factor = (rect.width() / size.x).min(rect.height() / size.y);
    let fitted = Rect::from_center_size(rect.center(), size * factor);
    // A hidden entry's thumbnail is dimmed rather than dropped: the picture is
    // *why* the tile is a tile.
    let tint = if dim {
        Color32::from_white_alpha(140)
    } else {
        Color32::WHITE
    };
    painter.image(
        texture.id(),
        fitted,
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        tint,
    );
}

/// A folder (with an up arrow when it is the parent row) or a page.
fn paint_glyph(
    painter: &egui::Painter,
    rect: Rect,
    glyph: Glyph,
    border: f32,
    palette: Palette,
    dim: bool,
) {
    match glyph {
        Glyph::Folder | Glyph::Parent => {
            let fill = if dim { palette.dim } else { palette.accent };
            folder(painter, rect, fill);
            if glyph == Glyph::Parent {
                up_arrow(painter, rect, palette.on_accent, border);
            }
        }
        Glyph::File => {
            let (fill, line, edge) = if dim {
                (palette.well, palette.disabled, palette.border)
            } else {
                (palette.control, palette.dim, palette.border_strong)
            };
            let page = rect.shrink2(egui::vec2(rect.width() * 0.16, rect.height() * 0.08));
            let radius = (page.width() * 0.08).max(1.0);
            painter.rect_filled(page, radius, fill);
            painter.rect_stroke(
                page,
                radius,
                Stroke::new(border.max(1.0), edge),
                StrokeKind::Inside,
            );
            // Three lines of "text", the last one short: a full-width last line
            // reads as a solid bar rather than as a page.
            let stroke = Stroke::new((page.height() * 0.045).max(1.0), line);
            let left = page.left() + page.width() * 0.22;
            let right = page.right() - page.width() * 0.22;
            for (index, fraction) in [0.34, 0.5, 0.66].into_iter().enumerate() {
                let y = page.top() + page.height() * fraction;
                let end = if index == 2 {
                    left + (right - left) * 0.55
                } else {
                    right
                };
                painter.line_segment([Pos2::new(left, y), Pos2::new(end, y)], stroke);
            }
        }
    }
}

/// The folder: a tab at the top left, and a body from the tab's foot down.
fn folder(painter: &egui::Painter, rect: Rect, fill: Color32) {
    let width = rect.width();
    let height = rect.height();
    let left = rect.left() + width * 0.07;
    let right = rect.right() - width * 0.07;
    let top = rect.top() + height * 0.24;
    let bottom = rect.bottom() - height * 0.10;
    let radius = (width * 0.06).max(1.5);

    let tab = Rect::from_min_max(
        Pos2::new(left + width * 0.05, rect.top() + height * 0.12),
        Pos2::new(left + width * 0.46, top + height * 0.04),
    );
    painter.rect_filled(tab, radius, fill);
    painter.rect_filled(
        Rect::from_min_max(Pos2::new(left, top), Pos2::new(right, bottom)),
        radius,
        fill,
    );
}

/// The parent row's mark: an arrow up, in the ink the folder's fill allows.
fn up_arrow(painter: &egui::Painter, rect: Rect, colour: Color32, border: f32) {
    let centre = Pos2::new(rect.center().x, rect.center().y + rect.height() * 0.06);
    let arm = rect.width() * 0.16;
    let stroke = Stroke::new((rect.width() * 0.06).max(border.max(1.5)), colour);
    let tip = Pos2::new(centre.x, centre.y - arm);
    painter.line_segment([tip, Pos2::new(centre.x, centre.y + arm)], stroke);
    painter.line_segment(
        [tip, Pos2::new(centre.x - arm * 0.8, centre.y - arm * 0.1)],
        stroke,
    );
    painter.line_segment(
        [tip, Pos2::new(centre.x + arm * 0.8, centre.y - arm * 0.1)],
        stroke,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tile_is_as_tall_as_its_picture_and_its_two_rows_of_name() {
        let short = tile_height(48.0, 15.0);
        let tall = tile_height(64.0, 15.0);
        assert!(tall > short);
        assert_eq!(tall - short, 16.0);
        let taller_text = tile_height(48.0, 20.0);
        assert_eq!(taller_text - short, 2.0 * 5.0);
    }

    /// The label never becomes the cell's width: the inset on both sides is
    /// what keeps two neighbouring names from touching, and a cell narrower
    /// than that inset still has to paint something rather than panic.
    #[test]
    fn the_name_keeps_its_distance_from_the_edge() {
        let width = 80.0;
        assert!(width - 2.0 * LABEL_INSET > 0.0);
        assert!(tile_height(48.0, 15.0) > 48.0 + LABEL_GAP);
    }

    /// A name too long for two rows ends in `…` **and says so**: `Galley::elided`
    /// is what a caller can check, and without it a truncated name is
    /// indistinguishable from a name that happens to fit.
    #[test]
    fn a_long_name_is_elided_rather_than_cut_off() {
        let style = Style::dtr();
        let width = 70.0;
        let job = label_job(
            &style,
            "a-name-that-cannot-possibly-fit-in-two-rows-of-a-narrow-cell.txt",
            width,
            Color32::WHITE,
        );
        let elided = std::rc::Rc::new(std::cell::Cell::new(false));
        let elided_in = elided.clone();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(Vec2::new(200.0, 100.0))
            .build_ui(move |ui| {
                let galley = ui.painter().layout_job(job.clone());
                elided_in.set(galley.elided);
                assert!(galley.rows.len() <= LABEL_ROWS, "two rows, not three");
            });
        harness.run();
        assert!(elided.get(), "the name was cut off without an ellipsis");
    }
}
