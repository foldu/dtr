//! The transfer queue's card, and the one question it can still ask.
//!
//! `docs/manager.md` §8: "a card listing jobs with a progress stripe, cancel and
//! retry, and the tab's own stripe driven by the same figure". The painting is
//! `dtr_widgets::Queue` inside a `dtr_widgets::Card` (`docs/transfers.md` §8's
//! "What the card draws"); this module is the mapping from the worker's rows to
//! that widget, and the answers back, so neither `app.rs` nor `dtr-widgets` has
//! to know about the other's types.
//!
//! **The card is drawn only while it is open** (`App::transfer_card_open`). A
//! queue with jobs does not open it by itself: a copy that starts and finishes
//! in the background is what the tab's stripe is for, and a card appearing over a
//! shell someone is typing in is how a terminal app becomes a file manager people
//! avoid. The one exception is a **question**: an escaped conflict opens the
//! browser's own card, not this one, so nothing here appears uninvited.
//!
//! **Closing the card acknowledges the finished rows.** The queue is the only
//! place the completed jobs exist, and a card that reopened with yesterday's
//! failures would be a list of things that are no longer true; the app sends
//! `ClearFinished` on the close. A running job is untouched either way.

use dtr_transfer::{
    Conflict, Direction as TransferDirection, JobId, JobView, State as TransferState,
};
use dtr_widgets::{
    Card, Conflict as ConflictCard, ConflictAnswer, ConflictDecision, Job as QueueJob,
    JobDirection, JobState, Queue,
};
use egui::Ui;

use crate::ui::Look;

/// What the card's rows and buttons reported this frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The cross, Escape or the toggle chord: the app clears the finished rows
    /// and stops drawing it.
    Close,
    Cancel(JobId),
    Retry(JobId),
    CancelAll,
}

/// The one question the queue itself can still ask.
///
/// It exists for the case the pre-answer cannot cover: a destination that
/// appeared between the listing and the copy, with no browser decision behind
/// it. That case normally becomes the browser's conflict card (the same
/// `dtr_vfs::Conflict` a direct verb returns), so in practice this is only
/// reached if a question arrives for a job the app already resolved — the card
/// exists so the queue does not park that job forever, and so the words are on
/// screen rather than only in the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ask {
    pub job: JobId,
    pub source: String,
    pub destination: String,
    pub detail: String,
}

impl Ask {
    /// The question a queue `Conflict` asks, in the widget's words.
    pub fn of(conflict: &Conflict) -> Self {
        let size = conflict.existing.len;
        Self {
            job: conflict.job,
            source: conflict.source.describe(),
            destination: conflict.destination.describe(),
            detail: format!("the existing file is {size} bytes"),
        }
    }
}

/// What the conflict card reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AskEvent {
    Answer(ConflictAnswer),
    Cancel,
}

/// Draw the queue card and report the one thing pressed on it.
///
/// `rows` is the worker's last snapshot, in the queue's creation order; a row's
/// index is how its action finds the job, exactly as [`Queue`] reports it. An
/// empty queue is a card that says so rather than a blank sheet.
#[allow(deprecated)] // one fixed overlay: the id is a name, not a hash of state
pub fn card(ui: &mut Ui, look: &Look, rows: &[JobView]) -> Option<Event> {
    let skin = look.skin();
    let jobs: Vec<QueueJob<'_>> = rows
        .iter()
        .map(|row| {
            QueueJob::new(&row.name, direction(row.direction), state(row.state))
                .bytes(row.done, row.total)
        })
        .collect();
    let queue = Queue::new(&jobs);
    let mut cancel_all = false;
    let mut event = None;
    let out = Card::new(egui::Id::new("dtr-transfers"), "transfers")
        .size(560.0, 380.0)
        .min(380.0, 220.0)
        .body_min(320.0)
        .note("copy and move")
        .close("close transfers")
        .footer(|ui| cancel_all = queue.footer(ui, &skin))
        .show(ui, &skin, |ui, _width| {
            event = action(rows, queue.show(ui, &skin));
        });
    if out.close {
        return Some(Event::Close);
    }
    if cancel_all {
        return Some(Event::CancelAll);
    }
    event
}

/// Map a row action to the job it belongs to. The index is the caller's slice,
/// which is the queue's own order, so a row cannot act on another job.
fn action(rows: &[JobView], pressed: dtr_widgets::QueueRows) -> Option<Event> {
    if let Some(index) = pressed.cancel {
        return rows.get(index).map(|row| Event::Cancel(row.id));
    }
    if let Some(index) = pressed.retry {
        return rows.get(index).map(|row| Event::Retry(row.id));
    }
    None
}

/// Draw the queue's leftover conflict question, if one is up.
pub fn ask(ui: &mut Ui, look: &Look, ask: &Ask) -> Option<AskEvent> {
    let skin = look.skin();
    let card = ConflictCard::new(&ask.source, &ask.destination)
        .title("replace existing file?")
        .id(egui::Id::unique("dtr-transfer-ask"))
        .detail(&ask.detail)
        .close("cancel the transfer");
    match card.show(ui, &skin) {
        Some(ConflictDecision::Answer(answer)) => Some(AskEvent::Answer(answer)),
        Some(ConflictDecision::Cancel) => Some(AskEvent::Cancel),
        None => None,
    }
}

/// The queue's direction in the widget's words.
///
/// `CrossHost` is a copy whose two ends are different machines; the card's four
/// words are §8's, and "copy" is the honest one for it (the bytes are streamed,
/// not mounted).
fn direction(direction: TransferDirection) -> JobDirection {
    match direction {
        TransferDirection::Copy | TransferDirection::CrossHost => JobDirection::Copy,
        TransferDirection::Move => JobDirection::Move,
        TransferDirection::Upload => JobDirection::Upload,
        TransferDirection::Download => JobDirection::Download,
    }
}

/// The queue's five states in the widget's four.
///
/// `Queued` is drawn as running because the card's question is "is this under
/// way"; a job waiting behind another machine is still the job a person asked
/// for, and a fifth word for it would need a fifth action ("why is this not
/// moving?") that nothing here can answer.
fn state(state: TransferState) -> JobState {
    match state {
        TransferState::Queued | TransferState::Running => JobState::Running,
        TransferState::Done => JobState::Done,
        TransferState::Failed => JobState::Failed,
        TransferState::Cancelled => JobState::Cancelled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dtr_transfer::Direction;

    fn row(id: u64, state: TransferState) -> JobView {
        JobView {
            id: JobId::new(id),
            name: format!("file-{id}.bin"),
            direction: Direction::Copy,
            state,
            done: 0,
            total: None,
            error: None,
            waiting: false,
            attempt: 1,
            can_cancel: !state.is_terminal(),
            can_retry: matches!(state, TransferState::Failed | TransferState::Cancelled),
        }
    }

    /// Every direction has a word the card can draw, and a cross-host stream is
    /// a copy.
    #[test]
    fn every_direction_maps_to_a_word() {
        assert_eq!(direction(Direction::Copy), JobDirection::Copy);
        assert_eq!(direction(Direction::Move), JobDirection::Move);
        assert_eq!(direction(Direction::Upload), JobDirection::Upload);
        assert_eq!(direction(Direction::Download), JobDirection::Download);
        assert_eq!(direction(Direction::CrossHost), JobDirection::Copy);
    }

    /// A queued job is drawn as running (it is under way, just behind a
    /// machine), and the three terminal states keep their words.
    #[test]
    fn the_five_states_fit_the_four_words() {
        assert_eq!(state(TransferState::Queued), JobState::Running);
        assert_eq!(state(TransferState::Running), JobState::Running);
        assert_eq!(state(TransferState::Done), JobState::Done);
        assert_eq!(state(TransferState::Failed), JobState::Failed);
        assert_eq!(state(TransferState::Cancelled), JobState::Cancelled);
    }

    /// A pressed row acts on the job at that index, and nothing else.
    #[test]
    fn a_row_action_names_its_own_job() {
        let rows = [
            row(10, TransferState::Running),
            row(11, TransferState::Failed),
        ];
        assert_eq!(
            action(
                &rows,
                dtr_widgets::QueueRows {
                    cancel: Some(0),
                    retry: None
                }
            ),
            Some(Event::Cancel(JobId::new(10)))
        );
        assert_eq!(
            action(
                &rows,
                dtr_widgets::QueueRows {
                    cancel: None,
                    retry: Some(1)
                }
            ),
            Some(Event::Retry(JobId::new(11)))
        );
        assert_eq!(action(&rows, dtr_widgets::QueueRows::default()), None);
        assert_eq!(
            action(
                &[],
                dtr_widgets::QueueRows {
                    cancel: Some(3),
                    retry: None
                }
            ),
            None,
            "a stale index is dropped, not a panic"
        );
    }

    /// The question's words are the two machines and the size, and the job id
    /// is the one the answer must be sent for.
    #[test]
    fn a_question_names_the_job_and_both_paths() {
        use dtr_transfer::{Answer, Conflict};
        use dtr_vfs::{Kind, Location, Meta};
        let conflict = Conflict {
            job: JobId::new(4),
            source: Location::local("/a/x.bin"),
            destination: Location::local("/b/x.bin"),
            existing: Meta {
                kind: Kind::File,
                len: 4096,
                modified: None,
            },
        };
        let ask = Ask::of(&conflict);
        assert_eq!(ask.job, JobId::new(4));
        assert!(ask.source.ends_with("/a/x.bin"), "{}", ask.source);
        assert!(ask.destination.ends_with("/b/x.bin"), "{}", ask.destination);
        assert!(ask.detail.contains("4096"), "{}", ask.detail);
        // The answer type is the widget's, so one card serves both mechanisms.
        let _: Answer = match AskEvent::Answer(ConflictAnswer::Replace) {
            AskEvent::Answer(answer) => match answer {
                ConflictAnswer::Replace => Answer::Replace,
                ConflictAnswer::KeepBoth => Answer::KeepBoth,
                ConflictAnswer::Skip => Answer::Skip,
            },
            AskEvent::Cancel => Answer::Skip,
        };
    }
}
