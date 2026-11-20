//! The debug overlay: frame time, fps, and the wait the `slow frame` log cannot see.
//!
//! `docs/windows.md` §5.1: the frame's CPU time (`dtr_frame::FrameTiming::cpu_ms`)
//! is taken *before* `present`, because a
//! vsynced frame waiting for the compositor is not work — so the existing
//! `slow frame` line can never show a vsync wait, and that wait is exactly what
//! "it feels laggy" is suspected to be. This module keeps the present duration
//! beside the CPU one and draws both in the window, while the thing being
//! measured is what a person is looking at.
//!
//! It is off unless asked: `--stats` at startup, or the palette's `overlay`
//! command. Off means no sample is taken, no `Instant` is read and no area is
//! drawn; the only per-frame work left is the `if` at the two call sites. On
//! means one label of chrome text, in the skin's colours and font, drawn in
//! [`egui::Order::Middle`]: above the panes (and a file pane next door), below
//! every card, so a preferences sheet still covers it. It takes no pointer
//! input — an overlay that swallowed a click aimed at the terminal under it
//! would be a bug — and it is placed in the bottom-right of the pane area, above
//! the status row, so the tab strip and the status bar are never covered.
//!
//! **It refreshes at [`REFRESH_INTERVAL`], not once per frame.** An instrument
//! that forced a frame per refresh would measure itself: fps would equal the
//! display's rate whether the app was idle or busy, and the fan would run for a
//! number. The box's own refresh counts as a frame, so a quiet window reads
//! about ten a second — the observer's cost is in the number it reports — while
//! typing or an animation pushes it up to whatever dtr really draws.
//!
//! **The numbers are one frame stale.** A frame's own cost is not known until
//! it has been submitted and presented, so the box shows the ring as it stood
//! when that frame began. That is normal for an on-screen counter, and it is why
//! the box does not claim to describe the frame it is painted in.
//!
//! It is not in `--screenshot`: that path renders offscreen from injected state,
//! at 1× with no window and no present, so it has neither the loop's per-second
//! rates nor the present wait to show. `--capture` (the live window) does show
//! it, which is the point of that flag.

use std::fmt::Write as _;

/// How often the box refreshes its readout while it is on.
///
/// Ten frames a second is enough for a number a person reads, and cheap enough
/// to leave on. **It is deliberately not every frame**: an instrument that forced
/// a frame per refresh would measure itself — it would pin fps to the display's
/// rate, keep a laptop's fan on, and answer neither question the box exists for
/// ("is dtr idle?" and "what does a frame cost?"). The overlay's own refresh is
/// included in the fps it shows, so a quiet window reads about ten frames a
/// second rather than zero; typing and animation push it to the real rate.
pub const REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// How many frames the rolling statistics keep: two seconds at 60 Hz.
pub const WINDOW: usize = 120;

/// What the event loop did in the last second, from `LoopReport`.
///
/// The loop already counts these for the `-v` line (`iterations_per_s`,
/// `frames_per_s`, `wakes`); the overlay reads the same report rather than
/// keeping a second set of counters that could disagree with the log.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LoopRates {
    pub iterations_per_s: f32,
    pub frames_per_s: f32,
    pub wakes_per_s: f32,
}

/// What only the event loop knows: whether to measure, its rates, and the grid.
#[derive(Clone, Copy, Debug, Default)]
pub struct Input {
    pub enabled: bool,
    pub rates: LoopRates,
    /// Columns × rows of the focused pane's grid, as the last fit left it.
    pub grid: (u16, u16),
}

/// Everything the box prints. Built per enabled frame; `Copy` is not possible
/// (`adapter` is a `&str`) but nothing here allocates.
pub struct Live<'a> {
    pub input: Input,
    /// Glyphs the last frame rasterised: the split between font work and drawing.
    pub glyphs: usize,
    /// Cell size in physical pixels, from the grid's metrics.
    pub cell: (f32, f32),
    /// Backing-store pixels per logical point.
    pub scale: f32,
    pub adapter: &'a str,
    /// Whether the status row is drawn, so the box sits above it rather than on
    /// it. A shell session has no status bar and the box goes lower.
    pub status_visible: bool,
}

/// Last, mean and 99th percentile of a rolling window, in milliseconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Summary {
    pub last: f32,
    pub avg: f32,
    pub p99: f32,
}

/// A fixed-size ring of samples, summarised in one pass.
///
/// The array is the whole buffer, so nothing here allocates: a debug counter
/// that allocated per frame would be measuring its own noise.
#[derive(Clone, Copy, Debug)]
pub struct Rolling {
    values: [f32; WINDOW],
    /// Where the next sample goes.
    next: usize,
    /// How many of `values` are live, saturating at `WINDOW`.
    len: usize,
}

impl Default for Rolling {
    fn default() -> Self {
        Self {
            values: [0.0; WINDOW],
            next: 0,
            len: 0,
        }
    }
}

impl Rolling {
    pub fn push(&mut self, sample: f32) {
        self.values[self.next] = sample;
        self.next = (self.next + 1) % WINDOW;
        self.len = (self.len + 1).min(WINDOW);
    }

    pub fn clear(&mut self) {
        self.next = 0;
        self.len = 0;
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The mean and the 99th percentile of what has been pushed, or `None` for
    /// an empty ring (the first frames after the overlay is switched on).
    ///
    /// The percentile is the nearest-rank one: `ceil(0.99 × n) - 1`, so 100
    /// samples make the p99 the 99th and a single sample is its own p99. Sorting
    /// copies the window into a local array; 120 `f32`s is 480 bytes on the
    /// stack and this only runs while the overlay is on.
    pub fn summary(&self) -> Option<Summary> {
        if self.len == 0 {
            return None;
        }
        let mut sorted = [0.0f32; WINDOW];
        let mut sum = 0.0f64;
        for index in 0..self.len {
            sorted[index] = self.values[index];
            sum += f64::from(self.values[index]);
        }
        sorted[..self.len].sort_unstable_by(|left, right| {
            left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal)
        });
        let rank = (self.len as f64 * 0.99).ceil() as usize;
        let rank = rank.clamp(1, self.len) - 1;
        Some(Summary {
            // The newest sample: the slot before the write cursor.
            last: self.values[(self.next + WINDOW - 1) % WINDOW],
            avg: (sum / self.len as f64) as f32,
            p99: sorted[rank],
        })
    }
}

/// The two rolling windows the box shows: how long the frame took, and how long
/// it then waited in `present`.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameSamples {
    pub cpu: Rolling,
    pub present: Rolling,
}

impl FrameSamples {
    pub fn push(&mut self, cpu_ms: f32, present_ms: f32) {
        self.cpu.push(cpu_ms);
        self.present.push(present_ms);
    }

    pub fn clear(&mut self) {
        self.cpu.clear();
        self.present.clear();
    }
}

/// The box's text, as plain lines. Pure, so the formatting has a test without a
/// window and without a GPU.
///
/// One string rather than a label per row: it is one galley, one allocation, and
/// the numbers keep their columns because each field has a width.
pub fn text(samples: &FrameSamples, live: &Live<'_>) -> String {
    let mut out = String::with_capacity(280);
    match samples.cpu.summary() {
        Some(summary) => {
            let _ = writeln!(
                out,
                "frame   last {:>5.1}  avg {:>5.1}  p99 {:>5.1} ms",
                summary.last, summary.avg, summary.p99
            );
        }
        None => out.push_str("frame   collecting...\n"),
    }
    match samples.present.summary() {
        Some(summary) => {
            let _ = writeln!(
                out,
                "present last {:>5.1}  avg {:>5.1}  p99 {:>5.1} ms",
                summary.last, summary.avg, summary.p99
            );
        }
        None => out.push_str("present collecting...\n"),
    }
    let rates = live.input.rates;
    let _ = writeln!(
        out,
        "fps {:>5.1}  loop {:>5.0}/s  wakes {:>5.0}/s",
        rates.frames_per_s, rates.iterations_per_s, rates.wakes_per_s
    );
    let (cols, rows) = live.input.grid;
    let _ = writeln!(
        out,
        "glyphs {:>6}  grid {}x{}  cell {:.0}x{:.0} px  scale {:.2}",
        live.glyphs, cols, rows, live.cell.0, live.cell.1, live.scale
    );
    let _ = writeln!(out, "adapter {}", live.adapter);
    out
}

/// Draw the box over the panes, in the skin's colours.
///
/// `Order::Middle`, not `Foreground`: the panes are `Background` areas and every
/// modal is `Foreground`, so this is over the terminal and under a card — a
/// preferences sheet the person is reading must not have a counter painted on
/// it. `interactable(false)` so a click aimed at the cell under the box still
/// reaches the program (and `Context::layer_id_at` skips the layer, so hovering
/// it keeps asking the terminal about links).
///
/// Painted as a `Frame` rather than shapes by hand: `Frame` fills before it lays
/// its content out, which is the ordering a background needs — a fill added
/// after the label hides it.
pub fn overlay(ui: &mut egui::Ui, look: &crate::ui::Look, samples: &FrameSamples, live: &Live<'_>) {
    if !live.input.enabled {
        return;
    }
    let skin = look.skin();
    let palette = skin.palette;
    let style = &skin.style;
    // Above the status row: the row is reserved in points and drawn at the
    // bottom, and an overlay on top of it would cover the counters it is next
    // to. A shell has no status bar, so the box goes lower there.
    let status = if live.status_visible {
        crate::ui::STATUS_HEIGHT + crate::ui::GRID_GAP
    } else {
        0.0
    };
    let margin = 8.0;
    egui::Area::new(egui::Id::unique("dtr-stats"))
        .order(egui::Order::Middle)
        .anchor(
            egui::Align2::RIGHT_BOTTOM,
            egui::vec2(-margin, -(status + margin)),
        )
        .interactable(false)
        // No fade: the box is not decoration, and a capture one frame after it
        // is switched on should show the numbers, not a half-transparent card.
        .fade_in(false)
        .show(ui.ctx(), |ui| {
            // A drag across the numbers is not a text selection; the terminal
            // under the box is what a drag is for.
            ui.style_mut().interaction.selectable_labels = false;
            egui::Frame::new()
                .fill(palette.card)
                .stroke(egui::Stroke::new(style.border, palette.border))
                .corner_radius(style.control_radius)
                .inner_margin(egui::Margin::same(6))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(text(samples, live))
                            .font(style.value_font.clone())
                            .color(palette.text),
                    );
                });
        });
}

#[cfg(test)]
mod tests {
    use super::{FrameSamples, Input, Live, LoopRates, Rolling, Summary, WINDOW, text};

    fn summary_of(values: &[f32]) -> Option<Summary> {
        let mut rolling = Rolling::default();
        for value in values {
            rolling.push(*value);
        }
        rolling.summary()
    }

    fn live(enabled: bool) -> Live<'static> {
        Live {
            input: Input {
                enabled,
                rates: LoopRates {
                    iterations_per_s: 121.0,
                    frames_per_s: 59.8,
                    wakes_per_s: 60.0,
                },
                grid: (100, 34),
            },
            glyphs: 4210,
            cell: (9.0, 17.0),
            scale: 1.25,
            adapter: "Test Adapter",
            status_visible: true,
        }
    }

    #[test]
    fn an_empty_ring_has_no_summary_rather_than_a_zero() {
        assert_eq!(Rolling::default().summary(), None);
        assert_eq!(Rolling::default().len(), 0);
        // A zero would be a measurement ("0.0 ms frames"), and there is none yet.
        let samples = FrameSamples::default();
        let text = text(&samples, &live(true));
        assert!(text.contains("collecting"), "{text}");
        assert!(!text.contains("NaN"), "{text}");
    }

    #[test]
    fn one_sample_is_its_own_last_average_and_p99() {
        let summary = summary_of(&[7.5]).expect("one sample is a summary");
        assert_eq!(summary.last, 7.5);
        assert_eq!(summary.avg, 7.5);
        assert_eq!(summary.p99, 7.5);
    }

    #[test]
    fn the_percentile_is_nearest_rank() {
        // 1..=100: the mean is 50.5 and the nearest-rank p99 is the 99th value.
        let values: Vec<f32> = (1..=100).map(|value| value as f32).collect();
        let summary = summary_of(&values).expect("a summary");
        assert_eq!(summary.last, 100.0);
        assert_eq!(summary.avg, 50.5);
        assert_eq!(summary.p99, 99.0);

        // Ten samples: ceil(0.99 × 10) = 10, so the p99 is the tenth — not the
        // ninth, which floor-based percentile code gets wrong on small windows.
        let values: Vec<f32> = (1..=10).map(|value| value as f32).collect();
        assert_eq!(summary_of(&values).expect("a summary").p99, 10.0);
    }

    #[test]
    fn the_window_keeps_the_newest_samples_and_drops_the_rest() {
        let values: Vec<f32> = (1..=200).map(|value| value as f32).collect();
        let summary = summary_of(&values).expect("a summary");
        // 200 samples through a 120-slot ring leaves 81..=200.
        assert_eq!(summary.last, 200.0);
        assert_eq!(summary.avg, 140.5);
        assert_eq!(summary.p99, 199.0);
        assert_eq!(WINDOW, 120);
    }

    #[test]
    fn the_present_window_is_separate_from_the_cpu_one() {
        let mut samples = FrameSamples::default();
        samples.push(16.0, 2.0);
        samples.push(8.0, 12.0);
        let cpu = samples.cpu.summary().expect("a cpu summary");
        let present = samples.present.summary().expect("a present summary");
        assert_eq!((cpu.last, cpu.avg, cpu.p99), (8.0, 12.0, 16.0));
        assert_eq!((present.last, present.avg, present.p99), (12.0, 7.0, 12.0));
    }

    #[test]
    fn the_text_names_every_number_and_never_a_nan() {
        let mut samples = FrameSamples::default();
        samples.push(16.7, 1.3);
        let text = text(&samples, &live(true));
        for expected in [
            "frame",
            "last  16.7",
            "avg  16.7",
            "p99  16.7",
            "present",
            "fps  59.8",
            "loop   121/s",
            "wakes    60/s",
            "glyphs   4210",
            "grid 100x34",
            "cell 9x17 px",
            "scale 1.25",
            "adapter Test Adapter",
        ] {
            assert!(
                text.contains(expected),
                "{expected:?} missing from:\n{text}"
            );
        }
        assert!(!text.contains("NaN"), "{text}");
        assert!(!text.contains("inf"), "{text}");
    }

    #[test]
    fn a_failed_frame_is_still_a_number() {
        // Nothing can make `cpu_ms` or the present duration non-finite today,
        // but a formatter that says "NaN" on one is worse than the number: the
        // ring keeps what it is given.
        let mut samples = FrameSamples::default();
        samples.push(0.0, 0.0);
        let text = text(&samples, &live(true));
        assert!(text.contains("last   0.0"), "{text}");
    }

    #[test]
    fn clearing_starts_the_window_again() {
        let mut samples = FrameSamples::default();
        samples.push(16.0, 1.0);
        assert!(samples.cpu.summary().is_some());
        samples.clear();
        assert!(samples.cpu.summary().is_none());
        assert!(samples.present.summary().is_none());
    }
}

#[cfg(test)]
mod kittest_tests {
    use super::{FrameSamples, Input, Live, LoopRates, overlay};
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    fn live(enabled: bool) -> Live<'static> {
        Live {
            input: Input {
                enabled,
                rates: LoopRates {
                    iterations_per_s: 121.0,
                    frames_per_s: 59.8,
                    wakes_per_s: 60.0,
                },
                grid: (100, 34),
            },
            glyphs: 4210,
            cell: (9.0, 17.0),
            scale: 1.25,
            adapter: "Test Adapter",
            status_visible: true,
        }
    }

    fn harness(enabled: bool) -> Harness<'static> {
        let look = crate::ui::Look::default();
        let mut samples = FrameSamples::default();
        samples.push(16.7, 1.3);
        let live = live(enabled);
        Harness::builder()
            .with_size(egui::vec2(900.0, 620.0))
            .build_ui(move |ui| overlay(ui, &look, &samples, &live))
    }

    /// The box exists only when it is asked for: `--stats` starts it and the
    /// palette's `overlay` toggles it, but a window nobody asked is a window
    /// with no counter on it.
    #[test]
    fn nothing_is_drawn_while_the_overlay_is_off() {
        let mut harness = harness(false);
        harness.run_steps(2);
        assert!(
            harness.query_by_label_contains("fps").is_none(),
            "the overlay drew while it was off"
        );
    }

    /// Switched on, it shows the numbers, sits in the pane area (clear of the
    /// status row) and *does not take the pointer*: it is not an input surface,
    /// so `layer_id_at` does not answer with it and a click goes through to the
    /// cell underneath.
    #[test]
    fn the_overlay_shows_the_numbers_and_takes_no_pointer() {
        let mut harness = harness(true);
        harness.run_steps(3);
        let node = harness.get_by_label_contains("fps");
        let rect = node.rect();
        assert!(rect.width() > 100.0, "the box is a box: {rect:?}");
        assert!(rect.right() <= 900.0, "inside the window: {rect:?}");
        // Above the status reservation: STATUS_HEIGHT + GRID_GAP + an 8pt margin.
        let status = crate::ui::STATUS_HEIGHT + crate::ui::GRID_GAP + 8.0;
        assert!(
            rect.bottom() <= 620.0 - status + 0.5,
            "the box covers the status row: {rect:?}"
        );
        // Not a surface: the layer at the box's own centre is the root layer, or
        // nothing at all — never an area above it.
        let at = rect.center();
        if let Some(layer) = harness.ctx.layer_id_at(at) {
            assert!(
                !dtr_widgets::above_the_root(layer),
                "the overlay took the pointer: {layer:?}"
            );
        }
    }
}
