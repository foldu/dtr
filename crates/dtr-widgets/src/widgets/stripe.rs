//! A progress stripe: what a program says it is doing, drawn on a tab chip.
//!
//! It is a painted primitive rather than a widget because it is a decoration —
//! it senses nothing and reports nothing — but it still belongs here: the height,
//! the radius and the inset are the same kind of token as the chip's, and a
//! second screen that wants one should not have to invent them.
//!
//! Three shapes, because the protocols say three different things:
//!
//! - a fraction (`OSC 9;4` with a percentage): a track with that much of it
//!   filled;
//! - no fraction at all (`OSC 9;4` indeterminate): a band that slides, because a
//!   bar that sits still when the work is moving is a lie about the progress;
//! - [`Stripe::State`]: the whole stripe in one colour, for work that is not
//!   progressing (done, paused, blocked, failed) — a *state*, not a measurement.

use egui::{Color32, Painter, Rect};

/// How thick the stripe is, in points: two, so it reads at 1× and does not eat
/// the chip's bottom padding at 2×.
pub const STRIPE_HEIGHT: f32 = 2.0;

/// How far the band is from the chip's edges, in points. The cross lives at the
/// right, and a bar under it would look like part of the button.
pub const STRIPE_INSET: f32 = 5.0;

/// What the stripe is saying.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Stripe {
    /// A measured fraction, clamped to `0.0..=1.0`.
    Fraction(f32),
    /// Work that is happening with no measurement: a sliding band. `time` is the
    /// egui clock, so the slide is a function of the animation and not of any
    /// widget's state.
    Indeterminate,
    /// Not progress: a finished, paused or failed state, filling the stripe.
    State,
}

/// Paint a stripe along the bottom edge of `rect`.
///
/// `fill` is the colour the work has; the track is the same colour at a quarter
/// alpha, so one token covers both and a theme cannot make them clash.
pub fn stripe(painter: &Painter, rect: Rect, what: Stripe, fill: Color32, time: f64) {
    let track = Rect::from_min_max(
        egui::pos2(rect.left() + STRIPE_INSET, rect.bottom() - STRIPE_INSET),
        egui::pos2(
            rect.right() - STRIPE_INSET,
            rect.bottom() - STRIPE_INSET + STRIPE_HEIGHT,
        ),
    );
    if track.width() <= 0.0 {
        return;
    }
    let radius = STRIPE_HEIGHT / 2.0;
    let alpha = fill.a();
    let dim = Color32::from_rgba_unmultiplied(fill.r(), fill.g(), fill.b(), alpha / 4);

    match what {
        Stripe::State => {
            painter.rect_filled(track, radius, fill);
            return;
        }
        Stripe::Fraction(fraction) => {
            painter.rect_filled(track, radius, dim);
            let filled = fraction.clamp(0.0, 1.0);
            if filled > 0.0 {
                let done = Rect::from_min_max(
                    track.min,
                    egui::pos2(track.left() + track.width() * filled, track.max.y),
                );
                painter.rect_filled(done, radius, fill);
            }
        }
        Stripe::Indeterminate => {
            painter.rect_filled(track, radius, dim);
            // A third of the bar, sweeping left to right once a second: slow
            // enough to read as "still going" and not as a second animation
            // competing with the cursor.
            let width = track.width() / 3.0;
            let travel = (time % 1.0) as f32;
            let left = track.left() + (track.width() + width) * travel - width;
            let band = Rect::from_min_max(
                egui::pos2(left.max(track.left()), track.min.y),
                egui::pos2((left + width).min(track.right()), track.max.y),
            );
            if band.width() > 0.0 {
                painter.rect_filled(band, radius, fill);
            }
        }
    }
}
