//! Painting a pane frame with egui.
//!
//! M0 uses egui's own text drawing: a monospace font, one rect per coloured
//! cell, one text call per non-empty cell. That is fine at 80×24 and keeps the
//! critical path short; the wgpu cell renderer replaces this behind the same
//! call site later (PLAN §3, M2).

use std::collections::HashMap;

use egui::{Align2, Color32, FontId, Rect, RichText, Sense, Ui, pos2, vec2};

use crate::pane::Frame;

const FONT_SIZE: f32 = 14.0;

/// What the status bar shows. Totals come from the pane; rates are the UI's own
/// measurement, because it is the UI's clock that decides when to repaint.
pub struct Status<'a> {
    pub label: &'a str,
    pub rx: u64,
    pub tx: u64,
    pub rx_rate: f64,
    pub tx_rate: f64,
}

pub fn draw(ui: &mut Ui, frame: &Frame) {
    let font = FontId::monospace(FONT_SIZE);
    let (cell_w, cell_h) = metrics(ui, &font);
    let size = vec2(
        f32::from(frame.cols) * cell_w,
        f32::from(frame.rows) * cell_h,
    );

    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, colour(frame.default_bg));

    // Backgrounds first, so a neighbouring cell's text never sits under a
    // rectangle painted after it.
    for (index, cell) in frame.cells.iter().enumerate() {
        if !cell.has_bg {
            continue;
        }
        let (x, y) = cell_origin(frame, index, rect, cell_w, cell_h);
        painter.rect_filled(
            Rect::from_min_size(pos2(x, y), vec2(cell_w, cell_h)),
            0.0,
            colour(cell.bg),
        );
    }

    let mut fonts: HashMap<(bool, bool), FontId> = HashMap::new();
    for (index, cell) in frame.cells.iter().enumerate() {
        if cell.text.trim().is_empty() {
            continue;
        }
        let (x, y) = cell_origin(frame, index, rect, cell_w, cell_h);
        let font = fonts
            .entry((cell.bold, cell.italic))
            .or_insert_with(|| styled_font(cell.bold, cell.italic))
            .clone();
        painter.text(
            pos2(x, y),
            Align2::LEFT_TOP,
            &cell.text,
            font,
            colour(cell.fg),
        );
    }

    if let Some((x, y)) = frame.cursor {
        let origin = pos2(
            rect.min.x + f32::from(x) * cell_w,
            rect.min.y + f32::from(y) * cell_h,
        );
        let cursor = Rect::from_min_size(origin, vec2(cell_w, cell_h));
        painter.rect_filled(cursor, 0.0, colour(frame.default_fg).gamma_multiply(0.45));
    }
}

pub fn status_bar(ui: &mut Ui, status: &Status) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        ui.label(
            RichText::new(status.label)
                .monospace()
                .size(12.0)
                .color(Color32::from_rgb(0x9a, 0xa5, 0xa0)),
        );
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
            .color(Color32::from_rgb(0x6b, 0x77, 0x72)),
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

fn cell_origin(frame: &Frame, index: usize, rect: Rect, cell_w: f32, cell_h: f32) -> (f32, f32) {
    let cols = usize::from(frame.cols).max(1);
    let (row, col) = (index / cols, index % cols);
    (
        rect.min.x + col as f32 * cell_w,
        rect.min.y + row as f32 * cell_h,
    )
}

fn styled_font(bold: bool, italic: bool) -> FontId {
    // egui's bundled fonts have no bold or italic face; the flags are carried
    // through the frame so the real renderer (M2) can use them.
    let _ = (bold, italic);
    FontId::monospace(FONT_SIZE)
}

fn colour(rgb: [u8; 3]) -> Color32 {
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

fn metrics(ui: &Ui, font: &FontId) -> (f32, f32) {
    // One layout for the reference glyph gives both the advance width and the
    // line height, which is all a cell grid needs.
    let galley = ui
        .painter()
        .layout_no_wrap("M".to_owned(), font.clone(), Color32::WHITE);
    (galley.size().x.max(1.0), galley.size().y.max(1.0))
}
