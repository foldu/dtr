//! The transfer queue's card body: one row a job, a stripe that says how far.
//!
//! `docs/manager.md` §8: what Nautilus calls its file-operations window is a
//! copy/retry/cancel feature, not a mount feature, and the card is "a list of
//! jobs with a progress stripe, cancel and retry". This is the painted half of
//! that sentence. It is a **body** for a [`crate::Card`], not a card: the
//! queue's window is the caller's, with its own title, cross and footer band,
//! and the card's footer is where [`Queue::footer`]'s figure and "cancel all"
//! belong — a way out and a count must not scroll away with a long list.
//!
//! **A row is a view, not a job.** [`Job`] is what the caller knows at the moment
//! it draws — a name, a direction, two byte counts, a state — and this module
//! keeps none of it: no queue, no worker, no clock, not even the rate in the
//! footer. That is what lets the F6/F7 window own the state machine and the
//! widget be tested with three literals, and it is the same division every
//! primitive in this crate keeps.
//!
//! **The stripe is the tab strip's control.** `crate::stripe` already draws the
//! three things a protocol can say — a measured fraction, a band that slides
//! when the work is happening with no measurement, and a whole stripe for a
//! state that is not progress — so a queue job wearing one cannot disagree with
//! the tab that started it about what "40%" looks like. The colour is the
//! palette's: the accent while it runs, `text` when it finished, `danger` when
//! it failed, `disabled` when it was cancelled. "Done" is a *state* stripe rather
//! than a full fraction, for the reason the tab strip gives: a failed transfer
//! at 100% of what it managed is not 100% of the file.
//!
//! **Cancel and retry are offers, not decoration.** Only a running job has
//! `cancel`; only a failed one has `retry`; a done or cancelled job has neither,
//! because both would be a lie about what the queue can do. The pointer answers
//! with the row's index — the caller's array is the identity, exactly like
//! [`crate::ContextMenu`]'s rows — and a button already being drawn is the only
//! thing that changes.

use egui::text::{LayoutJob, TextWrapping};
use egui::{Align, Color32, Layout, Rect, Sense, Ui, UiBuilder, Vec2};

use crate::{Button, Skin, Stripe, human_size as bytes, stripe};

/// How far the words of one job sit above the bottom stripe, in rows.
///
/// A row is two control rows tall: a name with the state word on the first line,
/// the direction and the byte count on the second, and the stripe under the
/// second. The fractions are of `Style::row`, so one density token decides the
/// whole row and a 34-point Adwaita control does not get a 23-point row's text.
const LINE_ONE: f32 = 0.55;
const LINE_TWO: f32 = 1.45;

/// Which way a job moves its bytes.
///
/// The four the manager names in `docs/manager.md` §8; the word is also the
/// accessibility name, so a screen reader says `upload` and not "an arrow".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobDirection {
    Upload,
    Download,
    Copy,
    Move,
}

impl JobDirection {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Upload => "upload",
            Self::Download => "download",
            Self::Copy => "copy",
            Self::Move => "move",
        }
    }
}

/// Where a job is in its life.
///
/// Four states, because they are the four a person can act on: a running job can
/// be cancelled, a failed one retried, and the other two are finished with. A
/// paused state would be a fifth only when the queue can pause, which is the
/// caller's decision to make, not this view's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobState {
    Running,
    Done,
    Failed,
    Cancelled,
}

impl JobState {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// The word on the one row action this state offers, if it offers one.
    const fn offer(self) -> Option<&'static str> {
        match self {
            Self::Running => Some("cancel"),
            Self::Failed => Some("retry"),
            Self::Done | Self::Cancelled => None,
        }
    }
}

/// One job, as the caller knows it at this frame.
///
/// `total` is `None` when `stat` could not answer — a remote directory's size,
/// a stream of unknown length — and that is what turns the stripe into the
/// sliding band instead of a fraction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Job<'a> {
    pub name: &'a str,
    pub direction: JobDirection,
    pub done: u64,
    pub total: Option<u64>,
    pub state: JobState,
}

impl<'a> Job<'a> {
    /// A job with no bytes accounted for yet, which is what a row looks like
    /// the frame the queue accepts it.
    pub const fn new(name: &'a str, direction: JobDirection, state: JobState) -> Self {
        Self {
            name,
            direction,
            done: 0,
            total: None,
            state,
        }
    }

    /// The byte counts the progress stripe is a fraction of; `total` may be
    /// unknown even while bytes are moving, which is the indeterminate case.
    pub const fn bytes(mut self, done: u64, total: Option<u64>) -> Self {
        self.done = done;
        self.total = total;
        self
    }

    /// The fraction of the destination written, when the total is known.
    fn fraction(self) -> Option<f32> {
        match self.total {
            Some(total) if total > 0 => Some((self.done.min(total) as f32) / (total as f32)),
            _ => None,
        }
    }

    /// The stripe a job asks for: a fraction while running, a slide without a
    /// total, a state once it is no longer progress.
    fn stripe(self) -> Stripe {
        match self.state {
            JobState::Running => match self.fraction() {
                Some(fraction) => Stripe::Fraction(fraction),
                None => Stripe::Indeterminate,
            },
            JobState::Done | JobState::Failed | JobState::Cancelled => Stripe::State,
        }
    }

    /// The stripe's colour: the palette's own slots, one per state.
    ///
    /// A finished job keeps the accent — it is the bar it was, now full — because
    /// a white rule across the row reads as a separator rather than as "this one
    /// arrived". Only the two states that are not progress get their own colour:
    /// `danger` for a failure and `disabled` for a cancellation.
    fn colour(self, skin: &Skin) -> Color32 {
        let palette = skin.palette;
        match self.state {
            JobState::Running | JobState::Done => palette.accent,
            JobState::Failed => palette.danger,
            JobState::Cancelled => palette.disabled,
        }
    }

    /// The second line's progress words: `upload · 42% of 29.4 MB`.
    ///
    /// The percentage is rounded to a whole one because it is read at a glance,
    /// and the byte counts are humanised here rather than by the caller so two
    /// rows cannot spell a megabyte differently. `so far` is the indeterminate
    /// wording: a number that is not a fraction of anything is still worth
    /// knowing.
    fn progress(self) -> String {
        let direction = self.direction.label();
        match (self.total, self.state) {
            (Some(total), JobState::Running) if total > 0 => {
                let percent = (self.fraction().unwrap_or(0.0) * 100.0).round();
                format!("{direction}, {percent:.0}% of {}", bytes(total))
            }
            (Some(total), _) => format!("{direction}, {} of {}", bytes(self.done), bytes(total)),
            (None, JobState::Running) => format!("{direction}, {} so far", bytes(self.done)),
            (None, _) => format!("{direction}, {}", bytes(self.done)),
        }
    }

    /// The row's accessibility name: the three facts a screen reader needs in
    /// one sentence, and the same facts the row draws.
    fn name(self) -> String {
        format!(
            "{} {} {}",
            self.name,
            self.direction.label(),
            self.state.label()
        )
    }
}

/// A transfer queue's rows, for the body of a card.
///
/// The caller owns the jobs and the card; this borrows the slice for one frame
/// and returns what the pointer pressed.
#[derive(Clone, Copy, Debug)]
pub struct Queue<'a> {
    jobs: &'a [Job<'a>],
    rate: Option<&'a str>,
}

impl<'a> Queue<'a> {
    pub fn new(jobs: &'a [Job<'a>]) -> Self {
        Self { jobs, rate: None }
    }

    /// The rate the footer shows beside the count, e.g. `41 MB/s`.
    ///
    /// A rate is a measurement over time, and this widget has no clock: the
    /// caller computes it (or the queue worker does), and passes nothing while
    /// it cannot yet.
    pub fn rate(mut self, rate: &'a str) -> Self {
        self.rate = Some(rate);
        self
    }

    /// Draw one row per job and report which row's action the pointer pressed.
    ///
    /// An empty queue draws one dim line rather than nothing: a card that is up
    /// says why it is up.
    pub fn show(&self, ui: &mut Ui, skin: &Skin) -> QueueRows {
        let style = &skin.style;
        let palette = skin.palette;
        let height = style.row * 2.0;
        let mut rows = QueueRows::default();
        if self.jobs.is_empty() {
            let (rect, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), style.row), Sense::hover());
            ui.painter().text(
                rect.left_center(),
                egui::Align2::LEFT_CENTER,
                "no transfers",
                style.label_font.clone(),
                palette.dim,
            );
            return rows;
        }
        let top = ui.cursor().min;
        for (index, job) in self.jobs.iter().enumerate() {
            let (rect, response) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
            let action = job.state.offer();
            let action_width = action.map_or(0.0, |label| {
                Button::measured_width(ui, skin, label) + style.gap
            });

            // The state word is right-aligned on the first line, left of the
            // row's action; the name gets whatever is left and is elided rather
            // than clipped, because a name cut mid-glyph reads as a bug.
            let state_colour = match job.state {
                JobState::Failed => palette.danger,
                JobState::Cancelled => palette.disabled,
                JobState::Running | JobState::Done => palette.dim,
            };
            let state = ui.painter().layout_no_wrap(
                job.state.label().to_owned(),
                style.label_font.clone(),
                state_colour,
            );
            let state_x = rect.right() - style.padding.x - action_width - state.size().x;
            ui.painter().galley(
                egui::pos2(
                    state_x,
                    rect.top() + style.row * LINE_ONE - state.size().y / 2.0,
                ),
                state,
                state_colour,
            );

            let name_width = (state_x - style.gap - rect.left() - style.padding.x).max(1.0);
            let mut job_text = LayoutJob::simple(
                job.name.to_owned(),
                style.value_font.clone(),
                palette.text,
                name_width,
            );
            job_text.wrap = TextWrapping {
                max_width: name_width,
                max_rows: 1,
                break_anywhere: true,
                overflow_character: Some('…'),
            };
            let name = ui.painter().layout_job(job_text);
            let name_colour = if job.state == JobState::Cancelled {
                palette.disabled
            } else {
                palette.text
            };
            ui.painter().galley(
                egui::pos2(
                    rect.left() + style.padding.x,
                    rect.top() + style.row * LINE_ONE - name.size().y / 2.0,
                ),
                name,
                name_colour,
            );
            ui.painter().text(
                egui::pos2(
                    rect.left() + style.padding.x,
                    rect.top() + style.row * LINE_TWO,
                ),
                egui::Align2::LEFT_CENTER,
                job.progress(),
                style.label_font.clone(),
                palette.dim,
            );

            if let Some(label) = action {
                let width = Button::measured_width(ui, skin, label);
                let hit = action_rect(rect, style, width);
                // The stripe belongs to the row's words, not to its button: a
                // bar that ran on under `cancel` would read as the button's
                // underline, and a press on it would look like a press on the
                // progress.
                let words =
                    Rect::from_min_max(rect.min, egui::pos2(hit.left() - style.gap, rect.max.y));
                stripe(
                    ui.painter(),
                    words,
                    job.stripe(),
                    job.colour(skin),
                    ui.input(|input| input.time),
                );
                let mut button = ui.new_child(
                    UiBuilder::new()
                        .max_rect(hit)
                        .layout(Layout::left_to_right(Align::Center)),
                );
                if Button::new(label).show(&mut button, skin) {
                    match job.state {
                        JobState::Running => rows.cancel = Some(index),
                        JobState::Failed => rows.retry = Some(index),
                        JobState::Done | JobState::Cancelled => {}
                    }
                }
            } else {
                // A row with nothing to press gives the stripe the whole width.
                stripe(
                    ui.painter(),
                    rect,
                    job.stripe(),
                    job.colour(skin),
                    ui.input(|input| input.time),
                );
            }
            response
                .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ListItem, true, job.name()));
        }

        // The list itself, so "which of these is the queue" is answerable from a
        // reader or a test: the rows are children of it, and the node is created
        // after them only because a painted widget has no node unless it says so.
        let list = Rect::from_min_max(top, egui::pos2(ui.max_rect().right(), ui.cursor().min.y));
        let response = ui.interact(
            list,
            ui.make_persistent_id("dtr-widgets-queue-list"),
            Sense::hover(),
        );
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::List, true, "transfer queue"));
        rows
    }

    /// The footer band: `2 of 3 · 41 MB/s` at the left, `cancel all` at the
    /// right, on the card's footer row — or `false` when nothing can be
    /// cancelled.
    ///
    /// It is a separate call because a footer sits *outside* the card's
    /// scrolling body: a count and the way to stop everything must not leave
    /// with the first page of a long list. The caller passes this to
    /// [`crate::Card::footer`] and owns the answer, which is a `bool` — there is
    /// no queue here to update.
    pub fn footer(&self, ui: &mut Ui, skin: &Skin) -> bool {
        let style = &skin.style;
        let palette = skin.palette;
        let finished = self
            .jobs
            .iter()
            .filter(|job| job.state == JobState::Done)
            .count();
        let figure = match self.rate {
            Some(rate) => format!("{finished} of {}, {rate}", self.jobs.len()),
            None => format!("{finished} of {}", self.jobs.len()),
        };
        let mut cancel_all = false;
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(figure)
                    .font(style.label_font.clone())
                    .color(palette.dim),
            );
            if self.jobs.iter().any(|job| job.state == JobState::Running) {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if Button::new("cancel all").show(ui, skin) {
                        cancel_all = true;
                    }
                });
            }
        });
        cancel_all
    }
}

/// What a queue's rows reported this frame: the index of the job whose action
/// the pointer pressed, if any.
///
/// Indices, not names: a name can repeat — two copies of one file are a sane
/// queue — and the caller's slice is the only stable identity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QueueRows {
    pub cancel: Option<usize>,
    pub retry: Option<usize>,
}

/// Where a row's action button goes: at the row's right edge, a control's height
/// and centred on the row, clear of the state word by [`crate::Style::gap`].
///
/// Split out so the hit rectangle is arithmetic a test can check: the action must
/// not overlap the row's words, and it must be inside the row the card gave.
fn action_rect(row: Rect, style: &crate::Style, width: f32) -> Rect {
    Rect::from_min_size(
        egui::pos2(
            row.right() - style.padding.x - width,
            row.center().y - style.row / 2.0,
        ),
        Vec2::new(width, style.row),
    )
}

/// A byte count the way `dtr-files` writes one, so a queue row and a status bar
/// do not disagree about a megabyte.
///
/// The format itself is `dtr_widgets::human_size`, the one home at the bottom of
/// the graph; `bytes` here is only the name this module's rows know it by.

#[cfg(test)]
mod tests {
    use super::{Job, JobDirection, JobState, Queue, QueueRows, action_rect, bytes};
    use crate::{Palette, Skin};

    fn skin() -> Skin {
        Skin::dtr(Palette::dtr(
            [0x0b, 0x0f, 0x0e],
            [0xdc, 0xdf, 0xdd],
            [0xff, 0xb0, 0x00],
        ))
    }

    fn row() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 50.0), egui::vec2(400.0, 46.0))
    }

    /// The action's hit rect is inside the row, on its right edge, and the row's
    /// words stop a gap short of it: two targets on one row would be the
    /// "sometimes unresponsive" bug the crate's rules are made of.
    #[test]
    fn the_action_sits_inside_the_row_and_clear_of_its_words() {
        let style = skin().style;
        let hit = action_rect(row(), &style, 58.0);
        assert!(row().contains_rect(hit));
        assert_eq!(hit.right(), row().right() - style.padding.x);
        assert_eq!(hit.height(), style.row);
        assert!(hit.left() > row().left() + style.padding.x);
        // The state word's right edge is at `hit.left() - gap`, which is where
        // the drawing puts it.
        assert_eq!(
            hit.left() - style.gap,
            row().right() - style.padding.x - 58.0 - style.gap
        );
    }

    /// Only a running job offers cancel and only a failed one offers retry;
    /// "done" and "cancelled" offer nothing, because both would lie.
    #[test]
    fn the_state_offers_the_one_action_it_has() {
        assert_eq!(JobState::Running.offer(), Some("cancel"));
        assert_eq!(JobState::Failed.offer(), Some("retry"));
        assert_eq!(JobState::Done.offer(), None);
        assert_eq!(JobState::Cancelled.offer(), None);
    }

    /// A fraction when the total is known, a slide when it is not, and a state
    /// once the job stopped being progress.
    #[test]
    fn the_stripe_follows_the_state_and_the_counts() {
        let running = Job::new("a", JobDirection::Upload, JobState::Running);
        assert_eq!(running.stripe(), crate::Stripe::Indeterminate);
        assert_eq!(
            running.bytes(42, Some(100)).stripe(),
            crate::Stripe::Fraction(0.42)
        );
        // A total of zero cannot be a fraction of anything, and a job that wrote
        // past its total is clamped rather than drawing a bar past the track.
        assert_eq!(
            Job::new("a", JobDirection::Upload, JobState::Running)
                .bytes(10, Some(0))
                .stripe(),
            crate::Stripe::Indeterminate
        );
        assert_eq!(
            Job::new("a", JobDirection::Upload, JobState::Running)
                .bytes(120, Some(100))
                .stripe(),
            crate::Stripe::Fraction(1.0)
        );
        for state in [JobState::Done, JobState::Failed, JobState::Cancelled] {
            assert_eq!(
                Job::new("a", JobDirection::Upload, state)
                    .bytes(1, Some(2))
                    .stripe(),
                crate::Stripe::State
            );
        }
    }

    /// The second line says the same thing whatever the state, and the
    /// indeterminate case does not invent a denominator.
    #[test]
    fn the_progress_words_are_the_counts_the_row_was_given() {
        let running = Job::new("a", JobDirection::Upload, JobState::Running).bytes(42, Some(100));
        assert_eq!(running.progress(), "upload, 42% of 100 B");
        let unknown = Job::new("a", JobDirection::Download, JobState::Running).bytes(1536, None);
        assert_eq!(unknown.progress(), "download, 1.5 kB so far");
        let failed = Job::new("a", JobDirection::Move, JobState::Failed).bytes(1536, Some(2048));
        assert_eq!(failed.progress(), "move, 1.5 kB of 2.0 kB");
        let done = Job::new("a", JobDirection::Copy, JobState::Done).bytes(2048, Some(2048));
        assert_eq!(done.progress(), "copy, 2.0 kB of 2.0 kB");
    }

    /// The accessibility name is the row's three facts, so a screen reader and a
    /// test ask for the same sentence.
    #[test]
    fn a_row_is_named_for_what_it_says() {
        let job = Job::new("report.pdf", JobDirection::Upload, JobState::Failed).bytes(1, Some(2));
        assert_eq!(job.name(), "report.pdf upload failed");
    }

    /// A rate is the caller's measurement; without one the footer counts only.
    #[test]
    fn the_footer_counts_and_takes_the_rate_it_is_given() {
        let jobs = [
            Job::new("a", JobDirection::Upload, JobState::Done),
            Job::new("b", JobDirection::Upload, JobState::Running),
            Job::new("c", JobDirection::Upload, JobState::Failed),
        ];
        let counted = Queue::new(&jobs);
        assert!(counted.rate.is_none());
        assert_eq!(counted.rate("41 MB/s").rate, Some("41 MB/s"));
        // A cancelled job is not finished: the count is what arrived, which is
        // the number a person is waiting for.
        assert_eq!(
            QueueRows::default(),
            QueueRows {
                cancel: None,
                retry: None
            }
        );
    }

    /// `bytes` is `dtr-files`' spelling; the boundaries are the ones a row's
    /// counts land on.
    #[test]
    fn byte_counts_are_humanised() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1536), "1.5 kB");
        assert_eq!(bytes(2 * 1024 * 1024), "2.0 MB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}
