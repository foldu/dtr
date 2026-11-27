//! The picture viewer: a press on a Kitty image opens it, filling the pane area.
//!
//! `docs/images.md` §10 is the account. This module is the frontend's last
//! missing verb: the renderer already puts an image on the screen, and this is
//! what a person gets when they point at one and press. Two pieces, kept apart
//! so the geometry is testable without a window:
//!
//! - **`hit_test`** answers "which picture is under this pixel". It is the
//!   renderer's own geometry — `kitty::placement_rect`, the drawn cell, the pane
//!   rectangle, the window — plus the one clip the renderer gets for free by
//!   drawing inside a pane: the part of the image the grid actually fills. An
//!   image scrolled mostly out of view is clickable only where it is visible.
//! - **`Viewer`** is the modal state: the pixels it holds, the one egui texture
//!   it makes, the key and the press that dismiss it. It mirrors `crate::pager`:
//!   `App` hands it the keyboard and the pointer while it is up, draws it in the
//!   foreground overlay layer, and closes it through `Action::Close`.
//!
//! ## Why the pixels are an `Arc`
//!
//! A `Placement` carries `Arc<ImagePixels>` because a frame costs a refcount, not
//! an image. That is also what makes the viewer survive a program deleting the
//! image while it is open: the overlay holds one reference of its own, so the
//! pixels outlive the terminal's cache entry and the picture does not turn into
//! a blank screen under a `a=d` the person never saw coming.
//!
//! ## The priority a press obeys
//!
//! The app decides, in `App::pointer_down`, and the order is deliberate:
//!
//! 1. the program, when it asked for the mouse (`?1000h` and friends) — a
//!    click in vim is vim's;
//! 2. an OSC 8 link, because a link is an explicit, small target printed *over*
//!    the picture and is otherwise unreachable;
//! 3. a picture;
//! 4. the terminal's own selection.
//!
//! Only a left press opens the viewer. Right and middle keep their meanings, and
//! a press that opens the viewer starts no selection, no word and no drag.

use std::sync::Arc;

use crate::kitty::{self, ImagePixels, Placement};
use crate::layout::Rect;
use crate::pane::Frame;
use crate::ui::Look;

/// The one line under a picture, said once so the screen and the docs agree.
pub const CLOSE_HINT: &str = "Esc or click to close";

/// What a keystroke asked the viewer to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// The user is done.
    Close,
}

/// The topmost *visible* placement under a point in physical pixels.
///
/// `pane` is the pane under the pointer, `cell` the drawn cell in physical
/// pixels (`render::Metrics`), `screen` the framebuffer, and `frame` the pane's
/// latest publish. `point` is the platform's pointer position, also physical.
///
/// The order is `kitty::order` — layer, then z, then image id — because that is
/// the order the image pass draws in, and the last placement drawn at a pixel is
/// the one a person sees there. Walking the list and keeping the *last* match is
/// therefore the whole z-order rule; nothing here re-sorts and nothing keeps a
/// second opinion.
///
/// A `BelowBackground` placement is not a candidate. It is drawn, and then the
/// pane's own opaque fill is drawn over it, so what the person sees there is
/// background rather than a picture (`docs/images.md` §5); a click that opened it
/// would open something invisible.
pub fn hit_test(
    pane: Rect,
    cell: (f32, f32),
    screen: (f32, f32),
    frame: &Frame,
    point: (f32, f32),
) -> Option<Placement> {
    if frame.images.is_empty() || cell.0 <= 0.0 || cell.1 <= 0.0 {
        return None;
    }
    if pane.width <= 0.0 || pane.height <= 0.0 {
        return None;
    }
    // What the renderer can draw: the pane, the window, and — because a pane is
    // sized by its grid — the cells that fit inside it. `visible_cells` in
    // `render.rs` is the same rule for glyphs; an image is clipped by the pane,
    // so the grid's extent is the pane's, less whatever surplus a pane keeps
    // when the window is not a whole number of cells.
    let grid = grid_rect(pane, cell, frame);
    let clip = kitty::intersect(
        kitty::intersect(
            pane,
            Rect::new(0.0, 0.0, screen.0.max(0.0), screen.1.max(0.0)),
        )?,
        grid,
    )?;

    let mut hit = None;
    for index in kitty::order(&frame.images) {
        let placement = &frame.images[index];
        if placement.layer == kitty::Layer::BelowBackground {
            continue;
        }
        let drawn = kitty::placement_rect(pane, cell, placement);
        if kitty::intersect(drawn, clip).is_some_and(|visible| visible.contains(point.0, point.1)) {
            // Later in `order` is drawn over earlier, so the last match wins.
            hit = Some(placement.clone());
        }
    }
    hit
}

/// The part of the pane the grid fills: whole cells, bounded by the frame.
fn grid_rect(pane: Rect, cell: (f32, f32), frame: &Frame) -> Rect {
    let cols = (pane.width / cell.0)
        .floor()
        .max(1.0)
        .min(f32::from(frame.cols.max(1)));
    let rows = (pane.height / cell.1)
        .floor()
        .max(1.0)
        .min(f32::from(frame.rows.max(1)));
    Rect::new(pane.x, pane.y, cols * cell.0, rows * cell.1)
}

/// An image's natural size in points: one image pixel is one physical pixel.
///
/// egui lays out in points and multiplies by the display's scale, so dividing by
/// that same scale is what makes an icon stay the size it was drawn instead of
/// growing on a HiDPI screen.
pub fn natural_size(width: u32, height: u32, scale: f32) -> egui::Vec2 {
    let scale = if scale > 0.0 { scale } else { 1.0 };
    egui::vec2(width as f32 / scale, height as f32 / scale)
}

/// Fit `natural` into `room`, enlarging or shrinking it with one factor.
///
/// The viewer *is* the picture, so the measurement is the window's rather than
/// the image's: a 256×160 screenshot fills a maximised window instead of sitting
/// small in the middle of a scrim, and a 4000-pixel photo is reduced. Both axes
/// take the same factor, or the picture would be stretched.
pub fn fitted(natural: egui::Vec2, room: egui::Vec2) -> egui::Vec2 {
    if natural.x <= 0.0 || natural.y <= 0.0 || room.x <= 0.0 || room.y <= 0.0 {
        return egui::Vec2::ZERO;
    }
    let factor = (room.x / natural.x).min(room.y / natural.y);
    natural * factor
}

/// The modal picture: what is open, its pixels, and the texture they became.
///
/// The texture is made at draw time, because `load_texture` needs an egui
/// context and a viewer can be opened before the frame that draws it. It lives
/// in the viewer rather than in a cache: there is exactly one picture on screen,
/// so `(image id, generation)` is all the identity it needs, and dropping the
/// handle when the viewer closes is what frees the texture.
#[derive(Default)]
pub struct Viewer {
    open: bool,
    image_id: u32,
    generation: u64,
    /// One reference of our own, which is why a delete cannot take the picture
    /// away from under the viewer.
    pixels: Option<Arc<ImagePixels>>,
    texture: Option<egui::TextureHandle>,
}

impl Viewer {
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open the viewer on one placement's whole image.
    ///
    /// The placement's *source* rectangle is not the picture: a clipped
    /// placement shows part of an image in the grid, and the viewer shows the
    /// image. So the arguments are the pixels and an identity, and the renderer's
    /// clipping stays the renderer's.
    pub fn show(&mut self, placement: &Placement) {
        let same = self.image_id == placement.image_id
            && self.generation == placement.generation
            && self
                .pixels
                .as_ref()
                .is_some_and(|pixels| Arc::ptr_eq(pixels, &placement.pixels));
        if !same {
            // A new generation is a new picture: the old texture is not it, and
            // keeping it would draw the re-sent image as the one it replaced.
            self.texture = None;
        }
        self.image_id = placement.image_id;
        self.generation = placement.generation;
        self.pixels = Some(Arc::clone(&placement.pixels));
        self.open = true;
    }

    /// Let go of the picture and its texture. Idempotent.
    pub fn close(&mut self) {
        self.open = false;
        self.pixels = None;
        // Dropping the handle is the free: egui frees a texture when the last
        // `TextureHandle` naming it goes, and nothing else holds this one.
        self.texture = None;
    }

    /// Handle one key press. `None` when the key is not an answer here — the
    /// caller swallows it all the same, because the terminal behind the viewer
    /// must not see a keystroke aimed at it.
    pub fn key(&mut self, key: &winit::keyboard::Key) -> Option<Action> {
        if !self.open {
            return None;
        }
        match key {
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape) => Some(Action::Close),
            _ => None,
        }
    }

    /// A press, wherever it landed. `true` when the viewer was up and has now
    /// closed, which is the caller's sign that the press goes no further: a
    /// dismiss is not also a selection.
    pub fn press(&mut self) -> bool {
        if !self.open {
            return false;
        }
        self.close();
        true
    }

    /// The texture for the held pixels, uploading it the first time it is drawn.
    fn texture(&mut self, ctx: &egui::Context) -> Option<&egui::TextureHandle> {
        if self.texture.is_none() {
            let pixels = self.pixels.as_ref()?;
            let expected = (pixels.width as usize)
                .saturating_mul(pixels.height as usize)
                .saturating_mul(4);
            if pixels.width == 0 || pixels.height == 0 || pixels.data.len() != expected {
                // The pane normalises everything it copies out of the core, so
                // this is a bug rather than a format: say so and draw nothing
                // instead of letting `ColorImage` panic in the middle of a frame.
                tracing::warn!(
                    width = pixels.width,
                    height = pixels.height,
                    bytes = pixels.data.len(),
                    "an image's pixels are not RGBA8; not drawing it"
                );
                return None;
            }
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [pixels.width as usize, pixels.height as usize],
                &pixels.data,
            );
            self.texture = Some(ctx.load_texture(
                format!("dtr-image:{}:{}", self.image_id, self.generation),
                image,
                egui::TextureOptions::LINEAR,
            ));
        }
        self.texture.as_ref()
    }

    /// Draw the picture, if it is open.
    ///
    /// There is no card: no fill, no hairline, no title row. The picture is the
    /// subject and fills the window, so the only chrome is the scrim that says
    /// the terminal behind it is not what is being looked at, and the one line
    /// that says how to leave. Colour and type come from the resolved skin; the
    /// numbers here are geometry.
    ///
    /// `viewport` is the area the picture may use: the window beyond its
    /// decoration. `App` hands in the central panel's rectangle, so the title
    /// bar (the tab strip, and the client-side frame's caption) and the status
    /// strip are never painted over, and the picture is centred in what is left
    /// rather than in the raw window.
    pub fn ui(&mut self, ui: &mut egui::Ui, look: &Look, viewport: egui::Rect) {
        let Some(pixels) = self.pixels.clone() else {
            return;
        };
        if !self.open || pixels.width == 0 || pixels.height == 0 {
            return;
        }
        if viewport.width() < 1.0 || viewport.height() < 1.0 {
            return;
        }
        let skin = look.skin();
        let palette = skin.palette;
        let style = &skin.style;
        let scale = ui.ctx().pixels_per_point().max(f32::EPSILON);

        /// How far the picture stays from the pane area's edge, in points.
        const INSET: f32 = 24.0;
        /// The air between the picture and its line of instruction, in points.
        const HINT_GAP: f32 = 8.0;

        let hint_font =
            egui::FontId::new(style.label_font.size - 1.5, style.label_font.family.clone());
        let hint_height = ui
            .painter()
            .layout_no_wrap(CLOSE_HINT.to_owned(), hint_font.clone(), palette.dim)
            .size()
            .y;

        let bounds = viewport.shrink(INSET);
        // The line under the picture is measured before the picture gets its
        // room, so a tall image cannot push the only way out off screen.
        let room = egui::vec2(
            bounds.width().max(1.0),
            (bounds.height() - hint_height - HINT_GAP).max(1.0),
        );
        let natural = natural_size(pixels.width, pixels.height, scale);
        let picture = fitted(natural, room);
        if picture.x < 1.0 || picture.y < 1.0 {
            return;
        }

        // The terminal is still there and is not the subject while the picture
        // is up. No frame around the picture: it is the picture's own edge. The
        // scrim stops at the pane area, so the tab strip stays the tab strip.
        ui.painter().rect_filled(viewport, 0.0, palette.scrim);

        // Centre the picture and its line *together* in the pane area, so the
        // hint is inside the same margin the picture keeps and the group sits
        // where the eye expects rather than a little high or low.
        let group = picture.y + HINT_GAP + hint_height;
        let top = bounds.center().y - group / 2.0;
        let image = egui::Rect::from_min_size(
            egui::pos2(bounds.center().x - picture.x / 2.0, top),
            picture,
        );
        if let Some(texture) = self.texture(ui.ctx()) {
            // `paint_at`, not a widget: the picture is what the viewer is *of*,
            // and a press anywhere closes it, so there is nothing to click.
            egui::Image::new((texture.id(), picture)).paint_at(ui, image);
        }

        ui.painter().text(
            egui::pos2(
                bounds.center().x,
                image.bottom() + HINT_GAP + hint_height / 2.0,
            ),
            egui::Align2::CENTER_CENTER,
            CLOSE_HINT,
            hint_font,
            palette.dim,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kitty::Layer;
    use std::sync::Arc;

    fn pixels(width: u32, height: u32) -> Arc<ImagePixels> {
        Arc::new(ImagePixels {
            width,
            height,
            data: vec![0; (width * height * 4) as usize],
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn placement(
        id: u32,
        layer: Layer,
        z: i32,
        col: i32,
        row: i32,
        offset: [u32; 2],
        size: [u32; 2],
    ) -> Placement {
        Placement {
            image_id: id,
            generation: 1,
            layer,
            z,
            col,
            row,
            offset,
            size,
            source: [0, 0, size[0], size[1]],
            pixels: pixels(size[0], size[1]),
        }
    }

    fn published(cols: u16, rows: u16, images: Vec<Placement>) -> Frame {
        Frame {
            cols,
            rows,
            default_fg: [0; 3],
            default_bg: [0; 3],
            cursor: None,
            selection: None,
            cells: Vec::new(),
            title: String::new(),
            images: images.into(),
        }
    }

    /// 800 × 600 at (100, 30), in 10 × 20 cells.
    fn pane() -> Rect {
        Rect::new(100.0, 30.0, 800.0, 600.0)
    }

    fn cell() -> (f32, f32) {
        (10.0, 20.0)
    }

    fn at(col: i32, row: i32) -> (f32, f32) {
        (
            pane().x + col as f32 * cell().0,
            pane().y + row as f32 * cell().1,
        )
    }

    /// The press lands on the picture, its offset and the pane's own position
    /// account for where the rectangle really is, and a point outside the drawn
    /// rectangle is not the picture.
    #[test]
    fn a_point_inside_a_picture_is_the_picture_and_one_outside_is_not() {
        let images = vec![placement(1, Layer::AboveText, 0, 4, 3, [2, 4], [100, 80])];
        let frame = published(80, 30, images);
        // The rectangle starts two pixels right and four below the cell's own
        // corner, so the corner alone is outside it.
        assert!(hit_test(pane(), cell(), (1000.0, 700.0), &frame, at(4, 3)).is_none());
        // Centre of the 100 × 80 picture.
        let centre = (at(4, 3).0 + 52.0, at(4, 3).1 + 44.0);
        let hit = hit_test(pane(), cell(), (1000.0, 700.0), &frame, centre).expect("inside");
        assert_eq!(hit.image_id, 1);
        // Its top-left corner is inside, by the offset.
        let corner = (at(4, 3).0 + 2.0, at(4, 3).1 + 4.0);
        assert!(hit_test(pane(), cell(), (1000.0, 700.0), &frame, corner).is_some());
        // Two pixels past its right edge, still inside the grid.
        let outside = (at(4, 3).0 + 102.0, corner.1);
        assert!(hit_test(pane(), cell(), (1000.0, 700.0), &frame, outside).is_none());
    }

    /// The pane's rectangle is in window pixels, not pane-local ones: a point
    /// that would be inside the picture if the pane were at the origin is not
    /// the picture when it is not.
    #[test]
    fn the_hit_test_uses_the_panes_own_origin() {
        let images = vec![placement(1, Layer::AboveText, 0, 0, 0, [0, 0], [50, 50])];
        let frame = published(80, 30, images);
        assert!(hit_test(pane(), cell(), (1000.0, 700.0), &frame, (110.0, 40.0)).is_some());
        assert!(
            hit_test(pane(), cell(), (1000.0, 700.0), &frame, (10.0, 40.0)).is_none(),
            "the same offset inside the window but left of the pane"
        );
    }

    /// Only the visible part counts: a picture scrolled halfway off the top is
    /// not clickable above the pane, and one that hangs off the bottom is not
    /// clickable below it.
    #[test]
    fn only_the_part_of_a_picture_inside_the_pane_is_clickable() {
        let mut image = placement(1, Layer::AboveText, 0, 0, -2, [0, 0], [100, 120]);
        image.pixels = pixels(100, 120);
        let frame = published(80, 30, vec![image]);
        let (x, y) = at(0, 0);
        assert_eq!(
            hit_test(
                pane(),
                cell(),
                (1000.0, 700.0),
                &frame,
                (x + 10.0, y + 10.0)
            )
            .map(|p| p.image_id),
            Some(1)
        );
        assert!(
            hit_test(
                pane(),
                cell(),
                (1000.0, 700.0),
                &frame,
                (x + 10.0, y - 10.0)
            )
            .is_none(),
            "above the pane is the tab strip, not the picture"
        );
        // 120 px tall from two rows above the top: the last 80 px show.
        assert!(
            hit_test(
                pane(),
                cell(),
                (1000.0, 700.0),
                &frame,
                (x + 10.0, y + 81.0)
            )
            .is_none()
        );
        assert!(
            hit_test(
                pane(),
                cell(),
                (1000.0, 700.0),
                &frame,
                (x + 10.0, y + 79.0)
            )
            .is_some()
        );
    }

    /// A placement that overhangs the grid's last row is clipped by the cells
    /// that exist: the pane keeps a few surplus pixels, and an image is not
    /// clickable in a strip the terminal does not draw to.
    #[test]
    fn the_visible_grid_bounds_the_picture() {
        // Five rows of 20 px in a 100 px pane: the bottom 0 px is not drawn to,
        // but the terminal itself only has five rows.
        let frame = published(
            10,
            5,
            vec![placement(1, Layer::AboveText, 0, 0, 0, [0, 0], [100, 200])],
        );
        assert!(
            hit_test(
                pane(),
                cell(),
                (1000.0, 700.0),
                &frame,
                (at(0, 0).0 + 5.0, at(0, 0).1 + 99.0)
            )
            .is_some(),
            "inside the fifth row"
        );
        assert!(
            hit_test(
                pane(),
                cell(),
                (1000.0, 700.0),
                &frame,
                (at(0, 0).0 + 5.0, at(0, 5).1 + 1.0)
            )
            .is_none(),
            "below the last row the frame has"
        );
    }

    /// Where pictures overlap, the one drawn last wins — the same order the
    /// image pass uses: layer first, then z, then image id.
    #[test]
    fn the_picture_drawn_on_top_wins() {
        let below = placement(1, Layer::BelowText, 5, 0, 0, [0, 0], [100, 100]);
        let above = placement(2, Layer::AboveText, -5, 0, 0, [10, 10], [100, 100]);
        let frame = published(80, 30, vec![below, above]);
        let point = (at(0, 0).0 + 20.0, at(0, 0).1 + 20.0);
        assert_eq!(
            hit_test(pane(), cell(), (1000.0, 700.0), &frame, point).map(|p| p.image_id),
            Some(2),
            "AboveText is drawn after BelowText whatever its z"
        );

        // Within one layer, z decides, and the higher z is drawn later.
        let low = placement(3, Layer::AboveText, 1, 0, 0, [0, 0], [100, 100]);
        let high = placement(4, Layer::AboveText, 2, 0, 0, [10, 10], [100, 100]);
        let frame = published(80, 30, vec![high, low]);
        assert_eq!(
            hit_test(pane(), cell(), (1000.0, 700.0), &frame, point).map(|p| p.image_id),
            Some(4)
        );
    }

    /// The visual order, not the list's order: the core promises no order for
    /// its placement list, so a test that passed by list position would pass on
    /// a frame that draws them the other way.
    #[test]
    fn the_list_order_is_not_the_drawing_order() {
        let front = placement(1, Layer::AboveText, 9, 0, 0, [0, 0], [100, 100]);
        let back = placement(2, Layer::AboveText, -9, 0, 0, [0, 0], [100, 100]);
        let frame = published(80, 30, vec![front, back]);
        assert_eq!(
            hit_test(
                pane(),
                cell(),
                (1000.0, 700.0),
                &frame,
                (at(0, 0).0 + 5.0, at(0, 0).1 + 5.0)
            )
            .map(|p| p.image_id),
            Some(1)
        );
    }

    /// A background image is covered by the pane's own fill, so a press there is
    /// a press on the background rather than on a picture nobody can see.
    #[test]
    fn a_below_background_image_is_not_clickable() {
        let frame = published(
            80,
            30,
            vec![placement(
                1,
                Layer::BelowBackground,
                0,
                0,
                0,
                [0, 0],
                [100, 100],
            )],
        );
        assert!(
            hit_test(
                pane(),
                cell(),
                (1000.0, 700.0),
                &frame,
                (at(0, 0).0 + 5.0, at(0, 0).1 + 5.0)
            )
            .is_none()
        );
    }

    /// Blank cells and spacers are not images: a press over the grid where no
    /// picture was placed selects text, as it always did.
    #[test]
    fn a_point_over_blank_cells_is_not_a_picture() {
        let frame = published(
            80,
            30,
            vec![placement(1, Layer::AboveText, 0, 0, 0, [0, 0], [10, 10])],
        );
        // A cell that holds nothing, well away from the one 10 × 10 image.
        assert!(hit_test(pane(), cell(), (1000.0, 700.0), &frame, at(40, 20)).is_none());
        // A wide character's spacer cell is still a cell, and no image.
        let empty = published(80, 30, Vec::new());
        assert!(hit_test(pane(), cell(), (1000.0, 700.0), &empty, at(0, 0)).is_none());
    }

    /// A picture's natural size is its pixels at the display's scale; fitted,
    /// it takes the room it is given, up or down, without changing shape.
    #[test]
    fn the_picture_fills_the_room_it_is_given() {
        let icon = natural_size(16, 16, 1.0);
        assert_eq!(icon, egui::vec2(16.0, 16.0));
        // Small pictures are enlarged: the viewer is the picture, not a card
        // with a picture in it, so a 16×16 icon becomes the largest square the
        // window holds rather than a speck in the middle of a scrim.
        assert_eq!(
            fitted(icon, egui::vec2(1000.0, 800.0)),
            egui::vec2(800.0, 800.0)
        );
        assert_eq!(
            natural_size(16, 16, 2.0),
            egui::vec2(8.0, 8.0),
            "one image pixel is one physical pixel"
        );

        // A picture bigger than the room is scaled down with its ratio intact.
        let large = fitted(egui::vec2(4000.0, 2000.0), egui::vec2(1000.0, 800.0));
        assert_eq!(large, egui::vec2(1000.0, 500.0));
        // The narrower side is the one that binds for a wide picture.
        assert_eq!(
            fitted(egui::vec2(1000.0, 100.0), egui::vec2(500.0, 500.0)),
            egui::vec2(500.0, 50.0)
        );
        assert_eq!(
            fitted(egui::vec2(10.0, 10.0), egui::Vec2::ZERO),
            egui::Vec2::ZERO
        );
    }

    /// The state machine a press and Escape drive: the picture opens, the pixels
    /// outlive the placement that named them, and both ways out let go of the
    /// texture.
    #[test]
    fn a_press_opens_the_picture_and_escape_or_a_press_closes_it() {
        use winit::keyboard::{Key, NamedKey};

        let mut frame = published(
            80,
            30,
            vec![placement(1, Layer::AboveText, 0, 0, 0, [0, 0], [100, 100])],
        );
        let mut viewer = Viewer::default();
        assert!(!viewer.is_open());
        assert!(!viewer.press(), "a press with nothing up goes on to select");

        let hit = hit_test(
            pane(),
            cell(),
            (1000.0, 700.0),
            &frame,
            (at(0, 0).0 + 5.0, at(0, 0).1 + 5.0),
        )
        .expect("a picture under the press");
        viewer.show(&hit);
        assert!(viewer.is_open());

        // The program deletes the image while the viewer is up: the frame no
        // longer lists it, and the pixels are still here.
        frame.images = Vec::new().into();
        assert!(
            viewer.pixels.is_some(),
            "the viewer holds its own reference"
        );

        assert_eq!(viewer.key(&Key::Character("q".into())), None);
        assert_eq!(
            viewer.key(&Key::Named(NamedKey::Escape)),
            Some(Action::Close)
        );
        viewer.close();
        assert!(!viewer.is_open());

        // Opening it again and pressing anywhere dismisses it rather than
        // falling through to a selection.
        viewer.show(&hit);
        assert!(viewer.is_open());
        assert!(
            viewer.press(),
            "an open viewer takes the press that dismisses it"
        );
        assert!(!viewer.is_open());
        assert!(viewer.pixels.is_none(), "the pixels go with the viewer");
        assert!(viewer.texture.is_none(), "and so does the texture");
    }
}
