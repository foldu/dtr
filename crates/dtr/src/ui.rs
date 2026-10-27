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
    pub fn themed(size: f32, path: Option<PathBuf>, theme: Option<&crate::theme::Theme>) -> Self {
        let mut look = Self {
            size,
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

/// What the status bar shows. Totals come from the pane; rates are the UI's own
/// measurement, because it is the UI's clock that decides when to repaint.
pub struct Status<'a> {
    pub label: &'a str,
    pub rx: u64,
    pub tx: u64,
    pub rx_rate: f64,
    pub tx_rate: f64,
    /// False while the transport is gone and something is trying to get it back.
    pub connected: bool,
    /// DTR and RTS levels, when the transport has modem lines at all.
    pub modem: Option<(bool, bool)>,
}

pub fn status_bar(ui: &mut Ui, status: &Status, look: &Look) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
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
        if !status.connected {
            ui.label(
                RichText::new("reconnecting…")
                    .monospace()
                    .size(12.0)
                    .color(Look::colour(look.accent)),
            );
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
