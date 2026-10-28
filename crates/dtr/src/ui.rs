//! The chrome around the grid: the status strip, and the colours both it and
//! the renderer use.
//!
//! The grid itself is drawn by `render.rs` with wgpu; this module is what is
//! left of the M0 egui path — the parts that are genuinely small enough not to
//! need a renderer of their own.

use std::path::PathBuf;

use egui::{Color32, RichText, Ui};

/// How the grid is drawn: the font size, and an optional font file to load in
/// place of the embedded default.
#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    /// Installed family the grid should use, before the preferred list.
    pub family: Option<String>,
    pub size: f32,
    pub path: Option<PathBuf>,
    /// The terminal's own background, used behind the grid and as the base from
    /// which the chrome is mixed.
    pub background: [u8; 3],
    pub foreground: [u8; 3],
    /// Cursor/selection accent; the theme's cursor colour when it has one.
    pub accent: [u8; 3],
    /// Status bar and palette background: the terminal background lifted just
    /// enough to be a separate surface.
    pub surface: [u8; 3],
    /// Secondary text.
    pub dim: [u8; 3],
}

impl Default for Look {
    fn default() -> Self {
        Self {
            family: None,
            size: 14.0,
            path: None,
            background: [0x0b, 0x0f, 0x0e],
            foreground: [0xdc, 0xdf, 0xdd],
            accent: [0xff, 0xb0, 0x00],
            surface: [0x14, 0x18, 0x16],
            dim: [0x6b, 0x77, 0x72],
        }
    }
}

impl Look {
    /// Build the chrome colours from a theme, if there is one.
    ///
    /// The chrome is not themed by any format in the wild, so it is derived:
    /// a surface slightly lighter than the background, secondary text between
    /// background and foreground, and the theme's cursor colour as the accent.
    pub fn themed(
        size: f32,
        family: Option<String>,
        path: Option<PathBuf>,
        theme: Option<&crate::theme::Theme>,
    ) -> Self {
        let mut look = Self {
            size,
            family,
            path,
            ..Self::default()
        };
        let Some(theme) = theme else { return look };

        if let Some(bg) = theme.background {
            look.background = bg;
        }
        if let Some(fg) = theme.foreground {
            look.foreground = fg;
        }
        if let Some(cursor) = theme.cursor {
            look.accent = cursor;
        } else if let Some(yellow) = theme.palette[3] {
            look.accent = yellow;
        }
        look.surface = mix(look.background, look.foreground, 0.10);
        look.dim = mix(look.background, look.foreground, 0.55);
        look
    }

    pub fn colour(rgb: [u8; 3]) -> Color32 {
        Color32::from_rgb(rgb[0], rgb[1], rgb[2])
    }
}

/// Linear blend of two colours; `t` of 0 is `a`, 1 is `b`.
fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let blend = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    [blend(a[0], b[0]), blend(a[1], b[1]), blend(a[2], b[2])]
}

impl Look {
    /// Register the configured font with egui, if there is one.
    ///
    /// Fonts are installed once, when the context is created: egui rebuilds its
    /// glyph atlas when the set changes, so doing this per frame would be a
    /// stutter on every repaint.
    pub fn install(&self, ctx: &egui::Context) {
        let Some(path) = self.path.as_ref() else { return };
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::warn!(path = %path.display(), %err, "cannot read the configured font; keeping the default");
                return;
            }
        };
        let mut definitions = egui::FontDefinitions::default();
        definitions
            .font_data
            .insert("dtr-mono".to_owned(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
        definitions
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .insert(0, "dtr-mono".to_owned());
        ctx.set_fonts(definitions);
    }

}

/// Give an area a known rectangle before it is shown.
///
/// An area whose size egui has not seen yet spends its first frame in a
/// sizing pass, and a sizing pass paints nothing. That is invisible in a live
/// window (the second frame draws), but `--screenshot` renders exactly one
/// frame, so an unprimed area would be missing from every capture. Seeding the
/// state with the rectangle we already know skips that pass.
pub(crate) fn prime_area(
    ctx: &egui::Context,
    id: egui::Id,
    order: egui::Order,
    rect: egui::Rect,
    interactable: bool,
) {
    let mut state = egui::AreaState::default();
    state.pivot_pos = Some(rect.min);
    state.size = Some(rect.size());
    state.interactable = interactable;
    ctx.memory_mut(|memory| {
        memory
            .areas_mut()
            .set_state(egui::LayerId::new(order, id), state);
    });
}

/// Draw a full-screen overlay in a layer above the panes.
///
/// Panes are egui `Area`s in the background layer; the palette has to be above
/// them, or a file listing would paint over the command line it is feeding.
pub fn overlay(ui: &mut egui::Ui, id: &str, add: impl FnOnce(&mut egui::Ui)) {
    let screen = ui.max_rect();
    let ctx = ui.ctx().clone();
    let id = egui::Id::unique(id);
    prime_area(&ctx, id, egui::Order::Foreground, screen, true);
    egui::Area::new(id)
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .constrain(false)
        .fade_in(false)
        .show(&ctx, |ui| {
            ui.set_min_size(screen.size());
            ui.set_max_size(screen.size());
            ui.set_clip_rect(screen);
            add(ui);
        });
}

/// Height of the status strip in logical points. The panes stop above it, so
/// this number is load-bearing: it is what keeps a split from running under
/// the chrome.
pub const STATUS_HEIGHT: f32 = 24.0;
/// The tab strip across the top, shown only when there is more than one tab.
/// Tabs existed before this and were invisible, which made `ctrl+tab` look
/// broken rather than unused.
pub const TAB_HEIGHT: f32 = 30.0;
/// The chip inside the strip. The strip is taller than this by design: a chip
/// that fills its panel makes any rounding in egui's layout paint over the first
/// terminal row.
pub const TAB_CHIP_HEIGHT: f32 = 20.0;
/// Breathing room between the last row of text and the status strip. Without
/// it the grid ends exactly on the strip's edge, and a prompt with a descender
/// looks like it is behind the bar rather than above it.
pub const GRID_GAP: f32 = 4.0;

/// Gap between split panes, in logical points.
pub const SPLIT_GAP: f32 = 3.0;

/// What the status bar shows. Totals come from the pane; rates are the UI's own
/// measurement, because it is the UI's clock that decides when to repaint.
pub struct Status<'a> {
    pub label: &'a str,
    /// `(index, count)` of the session tabs; `(1, 1)` is drawn without a tab
    /// marker, because one session is not a tab bar.
    pub tabs: (usize, usize),
    pub rx: u64,
    pub tx: u64,
    pub rx_rate: f64,
    pub tx_rate: f64,
    /// False while the transport is gone and something is trying to get it back.
    pub connected: bool,
    /// DTR and RTS levels, when the transport has modem lines at all.
    pub modem: Option<(bool, bool)>,
    /// Rows of scrollback below the viewport; 0 is the live screen.
    pub scrollback: u64,
    /// Whether byte counters mean anything for this transport. A shell in a pty
    /// is not a serial line: its byte totals are noise, and the strip is scarce
    /// space next to a filename or a path.
    pub counters: bool,
}

/// A short-lived note in the corner: "copied 1.2 kB".
///
/// Deliberately small and out of the way. A toast is the answer to "did that
/// work?", asked at the moment the answer is still useful; anything larger
/// gets in the way of the thing you just copied from.
#[allow(deprecated)] // one fixed overlay: the id is a name, not a hash of state
pub fn toast(ui: &mut Ui, look: &Look, text: &str, alpha: f32) {
    let corner = ui.ctx().content_rect().right_bottom();
    let id = egui::Id::new("dtr-toast");
    let rect = egui::Rect::from_min_size(
        egui::pos2(corner.x - 260.0, corner.y - 64.0),
        egui::vec2(240.0, 26.0),
    );
    // A fresh Area spends its first frame measuring and paints nothing, which
    // makes a one-frame capture of it blank. Prime the state with the rect.
    prime_area(ui.ctx(), id, egui::Order::Foreground, rect, false);
    egui::Area::new(id)
        .order(egui::Order::Foreground)
        .fixed_pos(egui::pos2(corner.x - 260.0, corner.y - 64.0))
        .interactable(false)
        .show(ui.ctx(), |ui| {
            egui::Frame::new()
                // Mixed between the theme's surface and its text, so the note
                // reads as chrome rather than as terminal output — and with
                // enough contrast to be read at a glance, which is the only
                // way a toast is ever read.
                .fill(Color32::from_rgba_unmultiplied(
                    look.foreground[0],
                    look.foreground[1],
                    look.foreground[2],
                    (36.0 * alpha) as u8,
                ))
                .stroke(egui::Stroke::new(
                    1.0,
                    Color32::from_rgba_unmultiplied(
                        look.accent[0],
                        look.accent[1],
                        look.accent[2],
                        (200.0 * alpha) as u8,
                    ),
                ))
                .corner_radius(6)
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(text)
                            .monospace()
                            .size(13.0)
                            .strong()
                            .color(Color32::from_rgba_unmultiplied(
                                look.foreground[0],
                                look.foreground[1],
                                look.foreground[2],
                                (255.0 * alpha) as u8,
                            )),
                    );
                });
        });
}

/// The tab strip: one chip per tab, the active one lit, each clickable.
///
/// Returns the index the pointer clicked, if any. Drawing and hit-testing are
/// the same rectangle on purpose — a click that misses the chip the eye sees is
/// the classic way to make tabs feel broken.
pub fn tab_bar(ui: &mut Ui, labels: &[String], active: usize, look: &Look) -> Option<usize> {
    let mut clicked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (index, label) in labels.iter().enumerate() {
            let is_active = index == active;
            let text = format!("{} {}", index + 1, label);
            let galley = ui.painter().layout_no_wrap(
                text,
                egui::FontId::monospace(12.0),
                if is_active {
                    Look::colour(look.background)
                } else {
                    Look::colour(look.foreground)
                },
            );
            // A keyboard-focusable widget here would be activated by Enter and by
            // Space — pressing Enter in the shell would switch tabs. A chip is a
            // hit target, not a control: it takes the pointer and nothing else.
            let size = egui::vec2(galley.size().x + 18.0, TAB_CHIP_HEIGHT);
            let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
            let painter = ui.painter();
            painter.rect_filled(
                rect,
                4.0,
                if is_active {
                    Look::colour(look.accent)
                } else {
                    Look::colour(look.surface)
                },
            );
            painter.galley(
                rect.center() - galley.size() / 2.0,
                galley,
                if is_active {
                    Look::colour(look.background)
                } else {
                    Look::colour(look.foreground)
                },
            );
            if response.clicked() {
                clicked = Some(index);
            }
        }
    });
    clicked
}

pub fn status_bar(ui: &mut Ui, status: &Status, look: &Look) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        if status.tabs.1 > 1 {
            ui.label(
                RichText::new(format!("{}/{}", status.tabs.0, status.tabs.1))
                    .monospace()
                    .size(11.0)
                    .color(Look::colour(look.accent)),
            );
        }
        ui.label(
            RichText::new(status.label)
                .monospace()
                .size(12.0)
                .color(Look::colour(look.foreground).gamma_multiply(0.8)),
        );
        if let Some((dtr, rts)) = status.modem {
            for (label, level) in [("DTR", dtr), ("RTS", rts)] {
                ui.label(
                    RichText::new(format!("{label}{}", if level { "●" } else { "○" }))
                        .monospace()
                        .size(11.0)
                        .color(if level {
                            Look::colour(look.accent)
                        } else {
                            Look::colour(look.dim)
                        }),
                );
            }
        }
        if status.scrollback > 0 {
            ui.label(
                RichText::new(format!("↑ {} lines", status.scrollback))
                    .monospace()
                    .size(11.0)
                    .color(Look::colour(look.accent)),
            );
        }
        if !status.connected {
            ui.label(
                RichText::new("reconnecting…")
                    .monospace()
                    .size(12.0)
                    .color(Look::colour(look.accent)),
            );
        }
        if !status.counters {
            return;
        }
        ui.label(
            RichText::new(format!(
                "RX {} ({}/s)   TX {} ({}/s)",
                human_bytes(status.rx as f64),
                human_bytes(status.rx_rate),
                human_bytes(status.tx as f64),
                human_bytes(status.tx_rate),
            ))
            .monospace()
            .size(12.0)
            .color(Look::colour(look.dim)),
        );
    });
}

fn human_bytes(value: f64) -> String {
    const K: f64 = 1024.0;
    if !value.is_finite() || value < 0.0 {
        return "—".to_owned();
    }
    if value < K {
        format!("{value:.0} B")
    } else if value < K * K {
        format!("{:.1} kB", value / K)
    } else {
        format!("{:.1} MB", value / (K * K))
    }
}
