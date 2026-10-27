//! Painting a pane frame with egui.
//!
//! M0 uses egui's own text drawing: a monospace font, one rect per coloured
//! cell, one text call per non-empty cell. That is fine at 80×24 and keeps the
//! critical path short; the wgpu cell renderer replaces this behind the same
//! call site later (PLAN §3, M2).

use egui::{Align2, Color32, FontId, Rect, Sense, Ui, Vec2};

use crate::pane::Frame;

const FONT_SIZE: f32 = 14.0;

pub fn draw(ui: &mut Ui, frame: &Frame) {
    let font = FontId::monospace(FONT_SIZE);
    let (cell_w, cell_h) = metrics(ui, &font);

    let cols = f32::from(frame.cols);
    let rows = f32::from(frame.rows);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(cell_w * cols, cell_h * rows), Sense::hover());

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, rgb(frame.default_bg));

    let cell_size = Vec2::new(cell_w, cell_h);
    for (i, cell) in frame.cells.iter().enumerate() {
        let Some(pos) = cell_origin(rect, i, frame.cols, cell_w, cell_h) else {
            break;
        };

        if cell.has_bg {
            painter.rect_filled(Rect::from_min_size(pos, cell_size), 0.0, rgb(cell.bg));
        }

        if !cell.text.is_empty() {
            let mut text = cell.text.clone();
            if cell.bold {
                // egui has no bold monospace face at this size; doubling the
                // stroke would blur the grid, so leave it be for now.
                let _ = &mut text;
            }
            painter.text(pos, Align2::LEFT_TOP, text, font.clone(), rgb(cell.fg));
        }
    }

    if let Some((x, y)) = frame.cursor {
        if let Some(pos) = cell_origin(rect, usize::from(y) * usize::from(frame.cols) + usize::from(x), frame.cols, cell_w, cell_h) {
            painter.rect_filled(
                Rect::from_min_size(pos, cell_size),
                0.0,
                Color32::from_rgba_unmultiplied(
                    frame.default_fg[0],
                    frame.default_fg[1],
                    frame.default_fg[2],
                    96,
                ),
            );
        }
    }
}

fn cell_origin(rect: Rect, index: usize, cols: u16, cell_w: f32, cell_h: f32) -> Option<egui::Pos2> {
    let cols = usize::from(cols);
    if cols == 0 {
        return None;
    }
    let x = index % cols;
    let y = index / cols;
    Some(rect.min + Vec2::new(x as f32 * cell_w, y as f32 * cell_h))
}

fn metrics(ui: &Ui, font: &FontId) -> (f32, f32) {
    // One layout for the reference glyph gives both the advance width and the
    // line height, which is all a cell grid needs.
    let galley = ui
        .painter()
        .layout_no_wrap("M".to_owned(), font.clone(), Color32::WHITE);
    (galley.size().x.max(1.0), galley.size().y.max(1.0))
}

fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}
