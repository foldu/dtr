//! The overwrite question: one card, source, destination, three answers.
//!
//! `docs/manager.md` §7: a paste that would replace, a rename onto an existing
//! name and a copy whose destination exists are *one* card with the same three
//! answers — replace, keep both, skip — not three dialogs that happen to agree.
//! This is that card, and only that: it knows the two paths it was handed, draws
//! them, and reports the answer. What "replace" means to a filesystem is the
//! caller's business; there is no I/O here and no operation state.
//!
//! **It uses [`Card`]'s shell**, rather than being a body a screen drops into
//! one. The argument is the one that made `Card` exist: the shell is the scrim,
//! the clamped and centered rectangle, the primed area, the header and the band
//! the footer is drawn outside of, and a second copy of it is a second place a
//! margin or a scrim can drift. A conflict is a modal with a title and one row
//! of answers, so it *is* the card — the caller writes one call, not a card, a
//! body closure and a footer closure that has to hand its answer back through a
//! cell. The body may still be scrolled by the card on a window too small for
//! it; the answers cannot, because they are the footer.
//!
//! **The answers are `Button`s and the words are the accessibility names**:
//! `replace`, `keep both`, `skip`, exactly as they read. Cancel is the card's
//! own cross (`close("cancel")` unless the caller names it otherwise) and
//! Escape; it is not a fourth button, because a dialog whose every row is an
//! answer plus a cancel is a dialog with four answers. There is no separate
//! `Cancel` in [`ConflictAnswer`] for the same reason: an answer is something
//! done to the file, and cancelling is something done to the dialog.
//!
//! **The destructive answer is marked with `Palette::danger`**, never with a
//! literal: the row is three `Button`s, so the one answer that destroys data
//! gets its own edge repainted over the button's — the crate's token for
//! "danger", at the skin's border width and radius. If a confirmation elsewhere
//! ever needs a destructive *button* rather than a destructive *answer*, the
//! place for that is `Button::danger()` and this edge should become a call to
//! it; until then this is a decoration of the existing control, not a second
//! one.
//!
//! **Enter answers the focused answer, Escape cancels, and everything else is
//! the caller's.** The widget reports keys it can honestly read; it does not
//! own a cursor, so it cannot move one with the arrows — `focus` is the
//! caller's, and a caller that wants arrow keys owns that state and passes the
//! new answer next frame. That is the same division [`crate::Button`] keeps:
//! the painted control reports, the screen decides. The focused answer is
//! drawn as the answer Enter will report, not as an egui keyboard focus, which
//! is why every skin marks it: the skin's two-tone ring where it has one, and
//! an accent rule under the label where its focus token is `None` (dtr's own
//! chrome), because a default that nobody can see is a key that overwrites a
//! file by surprise.

use std::cell::Cell;

use egui::text::{LayoutJob, TextWrapping};
use egui::{Align, Id, Layout, Rect, Stroke, StrokeKind, Ui, UiBuilder, Vec2};

use crate::{Button, Card, Focus, Footer, Skin, Style, row};

/// One answer to "a file is already there".
///
/// The manager's three, in the words the buttons carry. The variant order is the
/// order of an empty answer row's *meaning*, not its drawing: [`Self::IN_ORDER`]
/// is the safe-first order a screen reader walks, and the skin's footer token
/// decides which end the answer the dialog is about sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictAnswer {
    Replace,
    KeepBoth,
    Skip,
}

impl ConflictAnswer {
    /// Every answer, safest first: skipping destroys nothing, keeping both
    /// creates nothing over the destination, and replacing writes over it.
    ///
    /// This is also the order a test walks, so it is a statement about the
    /// answers rather than about one skin's footer.
    pub const IN_ORDER: [Self; 3] = [Self::Skip, Self::KeepBoth, Self::Replace];

    /// The word on the button and in the accessibility node.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Replace => "replace",
            Self::KeepBoth => "keep both",
            Self::Skip => "skip",
        }
    }

    /// Whether choosing this answer destroys the destination's bytes.
    ///
    /// Only `replace` does: "keep both" writes beside the destination and "skip"
    /// writes nothing.
    pub const fn destructive(self) -> bool {
        matches!(self, Self::Replace)
    }
}

/// What the dialog reported: an answer, or that it was closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictDecision {
    /// The pointer pressed this answer, or Enter reported the focused one.
    Answer(ConflictAnswer),
    /// Escape, or the cross. Nothing should be written, and the caller decides
    /// what "nothing" means for the operation it was asking about (the whole
    /// paste, or just this one file).
    Cancel,
}

/// The one card an overwrite asks.
///
/// Everything it draws is a `&str` the caller owns; it keeps no state between
/// frames and stores nothing, so two frames of one conflict cannot disagree
/// about what it is asking.
#[derive(Clone, Copy, Debug)]
pub struct Conflict<'a> {
    id: Id,
    title: &'a str,
    source: &'a str,
    destination: &'a str,
    detail: Option<&'a str>,
    close: &'a str,
    focused: ConflictAnswer,
    size: Vec2,
    min: Vec2,
}

impl<'a> Conflict<'a> {
    /// A conflict about `source` being written to `destination`.
    ///
    /// The default title is "replace existing file?" and the default focused
    /// answer is [`ConflictAnswer::Skip`]: a card can appear under the pointer
    /// while a chord is being typed, and the key that lands on it must not be
    /// the one that overwrites a file. A caller whose operation cannot skip —
    /// and therefore asks differently — passes [`Self::focus`].
    pub fn new(source: &'a str, destination: &'a str) -> Self {
        Self {
            // One conflict is up at a time, so one id is enough; a harness that
            // stacks two asks for its own.
            id: Id::unique("dtr-widgets-conflict"),
            title: "replace existing file?",
            source,
            destination,
            detail: None,
            close: "cancel",
            focused: ConflictAnswer::Skip,
            size: Vec2::new(440.0, 250.0),
            min: Vec2::new(320.0, 190.0),
        }
    }

    /// The card's own id, when a screen needs to tell two conflicts apart.
    pub fn id(mut self, id: Id) -> Self {
        self.id = id;
        self
    }

    /// What the question is about, e.g. `move over existing file?`.
    pub fn title(mut self, title: &'a str) -> Self {
        self.title = title;
        self
    }

    /// A third line of facts under the two paths — sizes, dates, which one is
    /// newer — in the dim face, wrapped to the card's width.
    pub fn detail(mut self, detail: &'a str) -> Self {
        self.detail = Some(detail);
        self
    }

    /// The cross's accessibility name. Cancel is not an answer button, so this
    /// is the only name the way out has.
    pub fn close(mut self, label: &'a str) -> Self {
        self.close = label;
        self
    }

    /// The answer Enter reports, drawn as the dialog's default.
    pub fn focus(mut self, answer: ConflictAnswer) -> Self {
        self.focused = answer;
        self
    }

    /// The size the card asks for, and the smallest it may get — the same
    /// contract as [`Card::size`] and [`Card::min`].
    pub fn size(mut self, width: f32, height: f32) -> Self {
        self.size = Vec2::new(width, height);
        self
    }

    pub fn min(mut self, width: f32, height: f32) -> Self {
        self.min = Vec2::new(width, height);
        self
    }

    /// Draw the card and report what the person answered, if anything yet.
    ///
    /// `None` is "still asking": the pointer has not pressed an answer, no key
    /// has answered, and the cross has not been used. The caller keeps drawing
    /// the same `Conflict` until it gets a `Some`.
    ///
    /// The keys are read before the card is drawn, so a pointer press in the
    /// same frame wins over the key: a click is a deliberate act on this dialog,
    /// and Enter may have been meant for the program behind it.
    pub fn show(self, ui: &mut Ui, skin: &Skin) -> Option<ConflictDecision> {
        let sink: Cell<Option<ConflictDecision>> = Cell::new(None);
        let pressed = ui.input(|input| {
            if input.key_pressed(egui::Key::Escape) {
                Some(ConflictDecision::Cancel)
            } else if input.key_pressed(egui::Key::Enter) {
                Some(ConflictDecision::Answer(self.focused))
            } else {
                None
            }
        });
        if let Some(decision) = pressed {
            sink.set(Some(decision));
        }

        let answer = &sink;
        let focused = self.focused;
        let body = self;
        let out = Card::new(self.id, self.title)
            .size(self.size.x, self.size.y)
            .min(self.min.x, self.min.y)
            .close(self.close)
            .footer(move |ui| answers(ui, skin, focused, answer))
            .show(ui, skin, move |ui, _width| {
                body.body(ui, skin);
            });
        if out.close {
            sink.set(Some(ConflictDecision::Cancel));
        }
        sink.get()
    }

    /// The two paths and the optional third line, in the card's scrolling body.
    fn body(&self, ui: &mut Ui, skin: &Skin) {
        ui.add_space(skin.style.gap);
        row(ui, skin, "source", |ui| wrapped(ui, skin, self.source, 2));
        row(ui, skin, "destination", |ui| {
            wrapped(ui, skin, self.destination, 2);
        });
        if let Some(detail) = self.detail {
            ui.add_space(skin.style.gap);
            dim_wrapped(ui, skin, detail, 3);
        }
    }
}

/// Which order a skin draws the answers in.
///
/// `Footer::PrimaryFirst` is Windows' order — the action the dialog exists for
/// sits left of the way out — and `PrimaryLast` is GNOME's. The destructive
/// answer takes the primary's place, because the question is *about* replacing;
/// there is no cancel button in the row to be the other end of that pair.
fn order(footer: Footer) -> [ConflictAnswer; 3] {
    match footer {
        Footer::PrimaryLast => [
            ConflictAnswer::Skip,
            ConflictAnswer::KeepBoth,
            ConflictAnswer::Replace,
        ],
        Footer::PrimaryFirst => [
            ConflictAnswer::Replace,
            ConflictAnswer::KeepBoth,
            ConflictAnswer::Skip,
        ],
    }
}

/// The words and the answer row: three buttons, right-aligned, one row tall.
fn answers(
    ui: &mut Ui,
    skin: &Skin,
    focused: ConflictAnswer,
    sink: &Cell<Option<ConflictDecision>>,
) {
    let style = &skin.style;
    // One width for all three, so the row is a row and not three words that
    // happen to be beside each other; the label wins if it is longer.
    let width = ConflictAnswer::IN_ORDER
        .iter()
        .map(|answer| Button::measured_width(ui, skin, answer.label()))
        .fold(0.0_f32, f32::max);
    let (row_rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), style.row),
        egui::Sense::hover(),
    );
    let rects = answer_rects(row_rect, style, width);
    for (answer, rect) in order(style.footer).into_iter().zip(rects) {
        // A child `Ui` at the rect the geometry asked for, so the button and the
        // hit target are the same rectangle whether or not the parent is a
        // horizontal layout with its own idea of centering.
        let mut button = ui.new_child(
            UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::left_to_right(Align::Center)),
        );
        if Button::new(answer.label())
            .width(width)
            .show(&mut button, skin)
        {
            sink.set(Some(ConflictDecision::Answer(answer)));
        }
        emphasis(ui, skin, rect, answer == focused, answer.destructive());
    }
}

/// Where the three buttons go, right-aligned in `row`, in [`order`]'s order.
///
/// Split out so the hit rectangles are arithmetic a test can check without a
/// harness: the buttons must not overlap (egui gives a press to the topmost
/// click-sensing widget, so an overlap is "the answer that only works
/// sometimes"), and the row must stay inside the card however narrow it gets.
fn answer_rects(row: Rect, style: &Style, width: f32) -> [Rect; 3] {
    let total = width * 3.0 + style.gap * 2.0;
    let mut x = (row.right() - total).max(row.left());
    std::array::from_fn(|_| {
        let rect = Rect::from_min_size(egui::pos2(x, row.top()), Vec2::new(width, style.row));
        x += width + style.gap;
        rect
    })
}

/// Mark the destructive answer, and the answer Enter reports.
///
/// Both marks are drawn *after* the button, over its own edge: a `Button` paints
/// itself and reports a bool, so the decoration the conflict needs is the
/// card's, using the same `Skin` the button used. Every colour, radius and
/// stroke width below comes from the skin; the two doubled widths are the token
/// scaled, which is what "thicker" means here.
fn emphasis(ui: &Ui, skin: &Skin, rect: Rect, focused: bool, destructive: bool) {
    let style = &skin.style;
    let palette = skin.palette;
    let painter = ui.painter();
    if destructive {
        painter.rect_stroke(
            rect,
            style.control_radius,
            Stroke::new(style.border, palette.danger),
            StrokeKind::Inside,
        );
    }
    if !focused {
        return;
    }
    match style.focus {
        // A two-tone ring outside the button: the accent band and the card's own
        // colour inside it, so the ring reads as a ring and not as a fat border.
        Focus::Ring => {
            painter.rect_stroke(
                rect.expand(style.border * 2.0),
                style.control_radius.saturating_add(2),
                Stroke::new(style.border * 2.0, palette.focus_outer),
                StrokeKind::Middle,
            );
            painter.rect_stroke(
                rect.expand(style.border * 0.5),
                style.control_radius,
                Stroke::new(style.border.max(1.0), palette.focus_inner),
                StrokeKind::Inside,
            );
        }
        // dtr's chrome has no focus ring — hover is the whole affordance,
        // because a focused chip used to swallow the shell's Enter. A modal's
        // default answer is the one place that argument does not hold: the
        // pointer may be nowhere near the dialog, so the rule under the word is
        // what says which answer the key will choose.
        Focus::None => {
            let y = rect.bottom() - style.border;
            painter.line_segment(
                [
                    egui::pos2(rect.left() + style.padding.x, y),
                    egui::pos2(rect.right() - style.padding.x, y),
                ],
                Stroke::new(style.border.max(1.0) * 1.5, palette.focus_outer),
            );
        }
    }
}

/// A path at the row's width: broken anywhere (a path is one word) and elided
/// with `…` after `rows` lines.
fn wrapped(ui: &mut Ui, skin: &Skin, text: &str, rows: usize) {
    let style = &skin.style;
    let width = ui.available_width().max(1.0);
    let mut job = LayoutJob::simple(
        text.to_owned(),
        style.value_font.clone(),
        skin.palette.text,
        width,
    );
    job.wrap = TextWrapping {
        max_width: width,
        max_rows: rows,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = ui.painter().layout_job(job);
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(width, galley.size().y.max(style.row)),
        egui::Sense::hover(),
    );
    ui.painter().galley(rect.min, galley, skin.palette.text);
}

/// The optional third line: dim, and prose, so it wraps on spaces and elides on
/// the lines it is given.
fn dim_wrapped(ui: &mut Ui, skin: &Skin, text: &str, rows: usize) {
    let style = &skin.style;
    let width = ui.available_width().max(1.0);
    let mut job = LayoutJob::simple(
        text.to_owned(),
        style.label_font.clone(),
        skin.palette.dim,
        width,
    );
    job.wrap = TextWrapping {
        max_width: width,
        max_rows: rows,
        break_anywhere: false,
        overflow_character: Some('…'),
    };
    let galley = ui.painter().layout_job(job);
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(width, galley.size().y.max(style.row)),
        egui::Sense::hover(),
    );
    ui.painter().galley(rect.min, galley, skin.palette.dim);
}

#[cfg(test)]
mod tests {
    use super::{ConflictAnswer, answer_rects, order};
    use crate::Style;

    fn row_rect() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(10.0, 100.0), egui::vec2(400.0, 23.0))
    }

    /// The hit rects are disjoint, one gap apart, and end at the row's right
    /// edge: a press belongs to exactly one answer, and the row is where the
    /// card put it.
    #[test]
    fn the_answers_are_disjoint_and_right_aligned() {
        let style = Style::dtr();
        let rects = answer_rects(row_rect(), &style, 70.0);
        assert_eq!(
            rects[0].left(),
            400.0 + 10.0 - (70.0 * 3.0 + style.gap * 2.0)
        );
        assert_eq!(rects[2].right(), row_rect().right());
        for pair in rects.windows(2) {
            assert_eq!(pair[1].left() - pair[0].right(), style.gap);
            assert!(!pair[0].intersects(pair[1]), "the answers overlap");
        }
        for rect in rects {
            assert!(row_rect().contains_rect(rect), "{rect:?} left the row");
            assert_eq!(rect.height(), style.row);
        }
    }

    /// A card narrower than the three buttons keeps them inside it rather than
    /// hanging the destructive one off the edge: the row starts at the card's
    /// left and grows to the right, which clips the last answer's *decoration*
    /// but never its hit rect's left edge.
    #[test]
    fn a_narrow_row_starts_at_the_left_edge() {
        let style = Style::dtr();
        let narrow = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(150.0, 23.0));
        let rects = answer_rects(narrow, &style, 70.0);
        assert_eq!(rects[0].left(), 0.0);
        for pair in rects.windows(2) {
            assert_eq!(pair[1].left() - pair[0].right(), style.gap);
        }
    }

    /// The skin decides which end the answer the dialog is about sits on, and
    /// the other two follow it — Windows first, GNOME last.
    #[test]
    fn the_skin_chooses_which_end_the_answer_sits_on() {
        assert_eq!(
            order(crate::Footer::PrimaryLast),
            [
                ConflictAnswer::Skip,
                ConflictAnswer::KeepBoth,
                ConflictAnswer::Replace
            ]
        );
        assert_eq!(
            order(crate::Footer::PrimaryFirst),
            [
                ConflictAnswer::Replace,
                ConflictAnswer::KeepBoth,
                ConflictAnswer::Skip
            ]
        );
    }

    /// The words are the accessibility names, and the destructive one is the
    /// only one that destroys: a test asserting "the second answer is safe"
    /// should be able to ask rather than know the order by heart.
    #[test]
    fn only_replace_destroys_and_every_answer_has_its_word() {
        for answer in ConflictAnswer::IN_ORDER {
            assert!(!answer.label().is_empty());
        }
        assert_eq!(ConflictAnswer::Replace.label(), "replace");
        assert_eq!(ConflictAnswer::KeepBoth.label(), "keep both");
        assert_eq!(ConflictAnswer::Skip.label(), "skip");
        let destructive: Vec<&str> = ConflictAnswer::IN_ORDER
            .into_iter()
            .filter(|answer| answer.destructive())
            .map(ConflictAnswer::label)
            .collect();
        assert_eq!(destructive, ["replace"]);
    }
}
