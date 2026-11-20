//! Counting presses into clicks, because winit does not.
//!
//! A press within [`DOUBLE_CLICK`] and [`CLICK_SLOP`] of the last one continues
//! the run: one selects a row, two opens it (or takes the word, on a terminal's
//! grid), and a fourth starts over. The rule is one rule: a terminal's panes and
//! a manager window's content would otherwise pick their own numbers, and the
//! same hand doing the same thing in two windows would feel different.

use std::time::{Duration, Instant};

/// How soon after a press the next one is still the same run.
pub const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// How far a press may move and still be the same run, in points.
///
/// Four and not zero: a mouse moves a pixel between the two edges of a click,
/// and a hand that is not perfectly still must not turn a double-click into two
/// single ones.
pub const CLICK_SLOP: f64 = 4.0;

/// The click run of one pointer: what the last press was, and how many there
/// have been in a row.
///
/// One per window (or per screen within a window), because the count belongs to
/// the hand doing the pressing rather than to the model being pressed.
#[derive(Clone, Debug, Default)]
pub struct Gestures {
    last: Option<(Instant, (f64, f64))>,
    run: u8,
}

impl Gestures {
    /// A counter that has not seen a press yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a press at `at`, and answer which press of the run it is: `1` for
    /// a fresh one, `2` for a double, `3` for a triple, then back to `1`.
    ///
    /// `now` is the caller's clock rather than `Instant::now()`, so a test can
    /// put two presses inside the window and two outside it without sleeping.
    pub fn press(&mut self, at: (f64, f64), now: Instant) -> u8 {
        let again = self.last.is_some_and(|(when, (x, y))| {
            now.duration_since(when) <= DOUBLE_CLICK
                && (x - at.0).abs() <= CLICK_SLOP
                && (y - at.1).abs() <= CLICK_SLOP
        });
        let run = if again { self.run % 3 + 1 } else { 1 };
        self.run = run;
        self.last = Some((now, at));
        run
    }

    /// [`Self::press`] on the wall clock, which is what a window event uses.
    pub fn press_now(&mut self, at: (f64, f64)) -> u8 {
        self.press(at, Instant::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed origin, so `press` only ever subtracts two instants: nothing here
    /// depends on the wall clock.
    fn origin() -> Instant {
        Instant::now()
    }

    fn at(origin: Instant, millis: u64) -> Instant {
        origin + Duration::from_millis(millis)
    }

    #[test]
    fn a_second_press_within_the_window_and_the_slop_is_a_double() {
        let t0 = origin();
        let mut gestures = Gestures::new();
        assert_eq!(gestures.press((10.0, 20.0), at(t0, 0)), 1);
        assert_eq!(gestures.press((12.0, 22.0), at(t0, 399)), 2, "same run");
        assert_eq!(
            gestures.press((12.0, 22.0), at(t0, 799)),
            3,
            "still the run"
        );
        assert_eq!(
            gestures.press((12.0, 22.0), at(t0, 1199)),
            1,
            "a fourth press starts over"
        );
    }

    #[test]
    fn a_press_that_waited_too_long_or_moved_too_far_starts_over() {
        let t0 = origin();
        let mut gestures = Gestures::new();
        gestures.press((10.0, 20.0), at(t0, 0));
        assert_eq!(
            gestures.press((10.0, 20.0), at(t0, 401)),
            1,
            "past 400 ms is another click"
        );
        gestures.press((10.0, 20.0), at(t0, 1000));
        assert_eq!(
            gestures.press((14.0, 20.0), at(t0, 1100)),
            2,
            "four points is still the same click"
        );
        assert_eq!(
            gestures.press((14.0 + CLICK_SLOP + 0.5, 20.0), at(t0, 1200)),
            1,
            "beyond the slop is a new one"
        );
    }

    #[test]
    fn the_window_is_inclusive_at_the_boundary() {
        // One rule, not two: the app counted `<=` and the portal `<`, so a press
        // exactly 400 ms later was a double in one window and not the other.
        let t0 = origin();
        let mut gestures = Gestures::new();
        gestures.press((0.0, 0.0), at(t0, 0));
        assert_eq!(gestures.press((0.0, 0.0), at(t0, 400)), 2);
    }
}
