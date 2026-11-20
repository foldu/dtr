//! The two cards the file manager's writes and transfers put up, in the states
//! worth looking at.
//!
//! The conflict is the one card `docs/manager.md` §7 allows for an overwrite —
//! replace, keep both, skip — and the queue is §8's list of jobs. Both are
//! `dtr-widgets`' rather than a screen's, so the pictures here are the contract:
//! the card's own shell and header, the three answers with the destructive one
//! marked from `Palette::danger`, and a row per job with the tab strip's stripe
//! under it.
//!
//! Rendering only: `dtr_widgets` has no file I/O and no queue, which is exactly
//! why three literals can show every state.

use dtr_widgets::{
    Card, Conflict, ConflictAnswer, ConflictDecision, Job, JobDirection, JobState, Palette, Queue,
    Skin,
};
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use std::cell::RefCell;
use std::rc::Rc;

fn dtr_skin() -> Skin {
    Skin::dtr(Palette::dtr(
        [0x0b, 0x0f, 0x0e],
        [0xdc, 0xdf, 0xdd],
        [0xff, 0xb0, 0x00],
    ))
}

/// The window behind the dialog, so the card's edge and its scrim are visible:
/// the harness's own panel takes the card's colour and would merge with it.
fn window(ui: &mut egui::Ui, skin: &Skin) {
    skin.apply(ui.ctx());
    ui.painter()
        .rect_filled(ui.max_rect(), 0.0, skin.palette.window);
}

/// One conflict, over a collision of a real shape: two paths and the sizes and
/// dates that make "replace" a decision rather than a reflex.
fn conflict(
    skin: Skin,
    focus: ConflictAnswer,
    answer: Option<Rc<RefCell<Option<ConflictDecision>>>>,
) -> Harness<'static> {
    let mut harness = Harness::builder()
        .with_size(vec2(640.0, 460.0))
        .build_ui(move |ui| {
            window(ui, &skin);
            let decided = Conflict::new("reports/2026-11/summary.pdf", "/srv/archive/summary.pdf")
                .detail("12.4 MB (14 November 2026 09:12), 29.4 MB (20 November 2026 18:03)")
                .focus(focus)
                .show(ui, &skin);
            if let (Some(sink), Some(decision)) = (&answer, decided) {
                *sink.borrow_mut() = Some(decision);
            }
        });
    harness.run();
    harness
}

/// Three answers, one of them under the pointer, so the hover a person sees is
/// in the picture: a painted control with no hover state is a control nobody
/// believes is pressable.
#[test]
fn the_conflict_asks_with_three_answers() {
    let mut harness = conflict(dtr_skin(), ConflictAnswer::Skip, None);
    harness.get_by_label("keep both").hover();
    harness.run();
    harness.snapshot("conflict_answers");
}

/// The answer Enter reports is the one the dialog marks, and because it is also
/// the one that destroys, both marks are on it at once: `danger`'s edge and the
/// accent rule that says "this is the default".
#[test]
fn the_destructive_answer_can_be_the_one_focused() {
    let mut harness = conflict(dtr_skin(), ConflictAnswer::Replace, None);
    harness.run();
    harness.snapshot("conflict_focused");
}

/// The other half of the focused answer: a skin with a focus ring gets the
/// two-tone one around the button, which is the branch dtr's chrome never takes.
/// Rendered on Linux, like every other Windows-skin picture in this crate.
#[test]
fn the_focused_answer_wears_the_skins_ring() {
    let mut harness = conflict(Skin::windows_light(), ConflictAnswer::Replace, None);
    harness.run();
    harness.snapshot("conflict_ring");
}

fn jobs() -> [Job<'static>; 3] {
    [
        // Mid-progress, with a total: the stripe is a fraction, and 42% is
        // visibly neither empty nor full.
        Job::new(
            "photos/IMG_2041.heic",
            JobDirection::Upload,
            JobState::Running,
        )
        .bytes(14_300_000, Some(34_000_000)),
        // Failed, so its one offer is `retry` — and the stripe is a state, not
        // "60% of a job that did not finish".
        Job::new("srv/db/dump.sql", JobDirection::Download, JobState::Failed)
            .bytes(1_200_000, Some(8_000_000)),
        Job::new("notes.txt", JobDirection::Copy, JobState::Done).bytes(4_096, Some(4_096)),
    ]
}

/// The queue as the file-operations card has it: a card body of rows and the
/// card's footer carrying the figure. The rate is the caller's number, so the
/// picture does not depend on a clock.
fn queue(sink: Option<Rc<RefCell<Option<usize>>>>) -> Harness<'static> {
    let skin = dtr_skin();
    let jobs = jobs();
    let mut harness = Harness::builder()
        .with_size(vec2(640.0, 460.0))
        .build_ui(move |ui| {
            window(ui, &skin);
            let queue = Queue::new(&jobs).rate("41 MB/s");
            let footer_skin = skin.clone();
            let rows = Rc::new(RefCell::new(None));
            let rows_for_body = rows.clone();
            let body_skin = &skin;
            let _ = Card::new(egui::Id::unique("test-queue-card"), "transfers")
                .size(540.0, 340.0)
                .min(360.0, 240.0)
                .close("close transfers")
                .footer(move |ui| {
                    let _ = queue.footer(ui, &footer_skin);
                })
                .show(ui, &skin, |ui, _width| {
                    *rows_for_body.borrow_mut() = Some(queue.show(ui, body_skin));
                });
            if let (Some(sink), Some(rows)) = (&sink, *rows.borrow()) {
                if let Some(index) = rows.retry.or(rows.cancel) {
                    *sink.borrow_mut() = Some(index);
                }
            }
        });
    harness.run();
    harness
}

/// Mixed states, one failed job with its `retry`, and a running job at 42%: a
/// picture of the queue that shows every kind of row the caller can supply.
#[test]
fn a_queue_shows_its_rows_stripes_and_answers() {
    let mut harness = queue(None);
    harness.snapshot("queue_mixed");
}

/// A finished job offers nothing, because there is nothing left to ask of it: an
/// action drawn for a state that cannot take it is a button that lies.
#[test]
fn only_the_state_that_offers_an_action_draws_one() {
    let harness = queue(None);
    assert!(harness.query_by_label("cancel").is_some());
    assert!(harness.query_by_label("retry").is_some());
    // One running job and one failed one in the fixture, so one of each; a
    // second `cancel` would mean a done row grew one.
    assert_eq!(harness.get_all_by_label("cancel").count(), 1);
    assert_eq!(harness.get_all_by_label("retry").count(), 1);
}

/// The row's answer is the index in the caller's slice, not a name — two copies
/// of one file are a sane queue.
#[test]
fn a_row_action_reports_the_index_it_came_from() {
    let picked = Rc::new(RefCell::new(None));
    let mut harness = queue(Some(picked.clone()));
    harness.get_by_label("retry").click();
    harness.run();
    assert_eq!(*picked.borrow(), Some(1), "retry is the failed row's offer");
}

/// The conflict's two answers are the two controls: a button, and the cross that
/// cancels. Both report, and neither does anything itself.
#[test]
fn an_answer_is_the_control_the_pointer_pressed() {
    let picked = Rc::new(RefCell::new(None));
    let mut harness = conflict(dtr_skin(), ConflictAnswer::Skip, Some(picked.clone()));
    harness.get_by_label("skip").click();
    harness.run();
    assert_eq!(
        *picked.borrow(),
        Some(ConflictDecision::Answer(ConflictAnswer::Skip))
    );

    let picked = Rc::new(RefCell::new(None));
    let mut harness = conflict(dtr_skin(), ConflictAnswer::Skip, Some(picked.clone()));
    harness.get_by_label("cancel").click();
    harness.run();
    assert_eq!(*picked.borrow(), Some(ConflictDecision::Cancel));
}

/// Enter answers the focused answer — `skip` unless the caller says otherwise —
/// and Escape cancels. Both are the keys the card can honestly read; the arrows
/// are the caller's, because a cursor is state and this widget does not own one.
#[test]
fn enter_reports_the_focused_answer_and_escape_cancels() {
    let picked = Rc::new(RefCell::new(None));
    let mut harness = conflict(dtr_skin(), ConflictAnswer::KeepBoth, Some(picked.clone()));
    harness.key_press(egui::Key::Enter);
    harness.run();
    assert_eq!(
        *picked.borrow(),
        Some(ConflictDecision::Answer(ConflictAnswer::KeepBoth))
    );

    let picked = Rc::new(RefCell::new(None));
    let mut harness = conflict(dtr_skin(), ConflictAnswer::Replace, Some(picked.clone()));
    harness.key_press(egui::Key::Escape);
    harness.run();
    assert_eq!(*picked.borrow(), Some(ConflictDecision::Cancel));
}
