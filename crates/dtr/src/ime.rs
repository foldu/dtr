//! IME composition: the text being composed, and the box it is drawn in.
//!
//! A terminal is not a text field. There is no widget to hand the composition
//! to, so the platform sends `Ime::Preedit` (what is being composed) and
//! `Ime::Commit` (what was decided), and the terminal has to do three things
//! with them: show the preedit so the person can see what they are composing,
//! tell the platform *where* the caret is so the candidate window lands next to
//! it instead of in a corner, and send only the committed text to the pty.
//!
//! That last one is the subtle one. A composition is not a keystroke: while a
//! preedit is active the keys are part of the composition, so their text must
//! not also reach the shell — the commit will carry it. `KeyStroke::composing`
//! is that flag, and `input::fill` hands it to the encoder, which suppresses the
//! text for exactly this reason.
//!
//! The box sits above the caret and inside the pane (see [`place`]). Above
//! rather than below because the row being typed stays visible that way, and a
//! box hanging over the next line of output would cover the line a person is
//! reading while they type over it.

use egui::{Color32, Rect, Stroke, Vec2};

use crate::ui::Look;

/// How far the box is kept from the caret, in points.
const GAP: f32 = 2.0;
/// Inner padding, in points.
const PADDING: Vec2 = Vec2::new(6.0, 3.0);

/// The composition the platform is currently in, if any.
#[derive(Debug, Default, Clone)]
pub struct Preedit {
    text: String,
    /// The platform's caret range, in **bytes** of `text` (`Ime::Preedit` is
    /// byte-indexed); converted to chars only when it is drawn, because that is
    /// the only place egui's unit matters.
    caret: Option<(usize, usize)>,
}

impl Preedit {
    /// Whether a composition is in progress, which is what suppresses the
    /// keystrokes that are part of it.
    pub fn is_composing(&self) -> bool {
        !self.text.is_empty()
    }

    /// The caret, as a char offset into [`Preedit::text`], for drawing.
    pub fn caret_chars(&self) -> Option<usize> {
        let (begin, _) = self.caret?;
        // A byte offset from the platform can land inside a character that was
        // composed since; clamping to a boundary is cheaper than trusting it.
        let mut end = begin.min(self.text.len());
        while end > 0 && !self.text.is_char_boundary(end) {
            end -= 1;
        }
        Some(self.text[..end].chars().count())
    }

    /// Set the composition. Returns whether anything changed, so a platform that
    /// repeats itself does not make the UI repaint.
    pub fn set(&mut self, text: String, caret: Option<(usize, usize)>) -> bool {
        if self.text == text && self.caret == caret {
            return false;
        }
        self.text = text;
        self.caret = caret;
        true
    }

    /// Clear the composition. Returns whether there was one.
    pub fn clear(&mut self) -> bool {
        !std::mem::take(self).text.is_empty()
    }

    /// Draw the box, if there is anything to draw.
    ///
    /// `anchor` is the caret's cell and the pane it belongs to, both in points:
    /// the grid is laid out in physical pixels, so the caller converts — and
    /// exactly once, or the box drifts by the DPI factor.
    pub fn ui(&self, ui: &mut egui::Ui, anchor: Anchor, look: &Look) {
        if !self.is_composing() {
            return;
        }
        // Monospace at the terminal's point size, because it belongs to the
        // grid even though it is not drawn by it — egui's own monospace is the
        // closest thing a non-grid surface has to the terminal's face.
        let font = egui::FontId::monospace(look.size);
        let colour = Color32::from_rgb(look.foreground[0], look.foreground[1], look.foreground[2]);
        let galley = ui
            .ctx()
            .fonts_mut(|fonts| fonts.layout_no_wrap(self.text.clone(), font, colour));
        let size = galley.size() + PADDING * 2.0;
        let rect = place(anchor.caret, size, anchor.pane);

        // Painted through the layer's painter rather than in an `Area`: the box
        // has no state to keep, and an `Area` spends its first frame measuring,
        // which in a one-frame capture is an invisible composition.
        let painter = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::unique("dtr-preedit"),
        ));
        painter.rect(
            rect,
            3,
            // The terminal's own background, so the preedit reads as text in the
            // grid rather than as chrome on top of it.
            Color32::from_rgb(look.background[0], look.background[1], look.background[2]),
            Stroke::new(
                1.0,
                Color32::from_rgb(look.accent[0], look.accent[1], look.accent[2]),
            ),
            egui::StrokeKind::Inside,
        );
        let origin = rect.min + PADDING;
        let caret = self
            .caret_chars()
            .unwrap_or_else(|| self.text.chars().count());
        let caret_x = origin.x
            + galley
                .pos_from_cursor(egui::text::CCursor::new(caret))
                .min
                .x;
        painter.galley(origin, galley.clone(), colour);
        painter.line_segment(
            [
                egui::pos2(caret_x, origin.y),
                egui::pos2(caret_x, origin.y + galley.size().y),
            ],
            Stroke::new(
                1.0,
                Color32::from_rgb(look.accent[0], look.accent[1], look.accent[2]),
            ),
        );
    }
}

/// Where the composition box hangs: the caret's cell, and the pane it is in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Anchor {
    pub caret: Rect,
    pub pane: Rect,
}

/// Put a box of `size` next to the caret, inside the pane.
///
/// Above the caret when there is room, below it when there is not (a caret on
/// the first row of the pane), and then clamped into the pane in both axes: a
/// composition near the right edge or in a pane shorter than the box has to stay
/// readable rather than hang out of the window.
pub fn place(caret: Rect, size: Vec2, pane: Rect) -> Rect {
    let above = Rect::from_min_size(egui::pos2(caret.min.x, caret.min.y - size.y - GAP), size);
    let below = Rect::from_min_size(egui::pos2(caret.min.x, caret.max.y + GAP), size);
    let mut rect = if above.min.y >= pane.min.y {
        above
    } else {
        below
    };
    if rect.max.x > pane.max.x {
        rect = rect.translate(egui::vec2(pane.max.x - rect.max.x, 0.0));
    }
    if rect.min.x < pane.min.x {
        rect = rect.translate(egui::vec2(pane.min.x - rect.min.x, 0.0));
    }
    if rect.max.y > pane.max.y {
        rect = rect.translate(egui::vec2(0.0, pane.max.y - rect.max.y));
    }
    if rect.min.y < pane.min.y {
        rect = rect.translate(egui::vec2(0.0, pane.min.y - rect.min.y));
    }
    rect
}

#[cfg(test)]
mod tests {
    use super::{Anchor, Preedit, place};
    use crate::ui::Look;
    use egui::{Rect, pos2, vec2};
    use egui_kittest::Harness;

    fn caret(x: f32, y: f32) -> Rect {
        Rect::from_min_size(pos2(x, y), vec2(8.0, 16.0))
    }

    fn pane() -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))
    }

    /// The default: the box is above the caret, so the row being typed stays
    /// visible.
    #[test]
    fn the_box_hangs_above_the_caret() {
        let placed = place(caret(100.0, 300.0), vec2(60.0, 20.0), pane());
        assert_eq!(placed.max.y, 300.0 - super::GAP);
        assert_eq!(placed.min.x, 100.0);
    }

    /// A caret on the first row has no room above it, so the box goes below.
    #[test]
    fn a_caret_on_the_first_row_puts_the_box_below() {
        let placed = place(caret(10.0, 0.0), vec2(60.0, 20.0), pane());
        assert_eq!(placed.min.y, 16.0 + super::GAP);
    }

    /// Near the right edge the box slides left rather than hanging out of the
    /// window, and it stays inside the pane.
    #[test]
    fn the_box_is_kept_inside_the_pane() {
        let placed = place(caret(790.0, 300.0), vec2(60.0, 20.0), pane());
        assert_eq!(placed.max.x, 800.0);
        assert!(placed.min.x >= 0.0);

        // A pane shorter than the box: neither above nor below fits, so the
        // clamp is what decides, and the box still starts inside the pane.
        let small = Rect::from_min_size(pos2(0.0, 0.0), vec2(200.0, 20.0));
        let placed = place(caret(10.0, 4.0), vec2(60.0, 40.0), small);
        assert_eq!(placed.min.y, 0.0);
    }

    /// The composition, and the caret inside it, come from the platform as
    /// bytes; egui counts chars, and a composed character is several bytes.
    #[test]
    fn the_caret_is_a_char_offset_even_though_the_platform_sends_bytes() {
        let mut preedit = Preedit::default();
        assert!(!preedit.is_composing());
        assert!(preedit.set("aé".to_owned(), Some((3, 3))));
        assert!(preedit.is_composing());
        assert_eq!(preedit.caret_chars(), Some(2));
        // Repeating the same event is not a change: it would repaint for
        // nothing.
        assert!(!preedit.set("aé".to_owned(), Some((3, 3))));
        assert!(preedit.clear());
        assert!(!preedit.is_composing());
        assert!(!preedit.clear());
    }

    /// A caret offset the platform measured before the text changed must not
    /// panic on a byte that is not a char boundary.
    #[test]
    fn a_caret_inside_a_character_is_clamped() {
        let mut preedit = Preedit::default();
        preedit.set("é".to_owned(), Some((1, 1)));
        assert_eq!(preedit.caret_chars(), Some(0));
    }

    /// The box has to be *painted* where the caret is, not merely computed:
    /// a mesh that never reaches the screen still passes every geometry test.
    /// The accent border is the sharpest signal — nothing else in an empty
    /// harness is amber.
    #[test]
    fn the_box_is_painted_above_the_caret() {
        let look = Look::default();
        let mut preedit = Preedit::default();
        // Latin on purpose: this test is about *where* the box is painted, and a
        // harness has no CJK font unless the app installed one. That the UI can
        // draw CJK at all is `fonts::ui_fallback`'s test, and the screenshot in
        // the commit notes shows it in the real window.
        preedit.set("nihon".to_owned(), Some((2, 2)));
        let mut harness = Harness::new_ui(move |ui| {
            let anchor = Anchor {
                caret: Rect::from_min_size(pos2(100.0, 200.0), vec2(8.0, 16.0)),
                pane: Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 300.0)),
            };
            preedit.ui(ui, anchor, &look);
        });
        harness.set_size(vec2(400.0, 300.0));
        harness.run();
        let rendered = harness.render().expect("a frame");

        let accent = |x: usize, y: usize| {
            let p = rendered.get_pixel(x as u32, y as u32);
            // #ffb000, with room for the antialiased edge of a 1-point line.
            p[0] > 180 && (100..=220).contains(&p[1]) && p[2] < 90
        };
        // Somewhere in the strip the box occupies above the caret.
        let inside = (100..150).any(|x| (170..200).any(|y| accent(x, y)));
        assert!(inside, "nothing was painted above the caret");

        // ...and nothing far away, where there is only the empty window.
        let away = (250..320).any(|x| (240..280).any(|y| accent(x, y)));
        assert!(!away, "the box was painted away from the caret");
    }

    /// An anchor is just two rects; this pins that a caller cannot accidentally
    /// hand over the caret and the pane the other way round.
    #[test]
    fn the_anchor_names_both_rects() {
        let anchor = Anchor {
            caret: caret(1.0, 2.0),
            pane: pane(),
        };
        assert_eq!(anchor.caret.min, pos2(1.0, 2.0));
        assert_eq!(anchor.pane.width(), 800.0);
    }
}
