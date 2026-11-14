//! A grid's geometry, as functions of its numbers.
//!
//! The pane paints a cell and the pane decides what a click landed on, and those
//! two must not compute the columns separately: that is exactly the shape of the
//! `pane_area` bug, where the divider was drawn in one place and hit-tested in
//! another (`AGENTS.md`, panes). So the arithmetic lives here, both callers ask
//! it, and it is tested as a pure function rather than through a picture.
//!
//! Three units and one rule:
//!
//! - a **cell** is `cell` points, gap included separately;
//! - a **row** is a line of `columns` cells;
//! - `scroll` is a row index, never a pixel offset. Two views of the same
//!   listing — the drawer and the hit test — must agree about what "the first
//!   visible row" means while batches are still arriving, and a pixel offset
//!   accumulated per frame drifts.
//!
//! Everything here is in logical points, like the rest of the pane (`dtr-files`
//! is handed an `egui::Rect` and divides nowhere).

use egui::{Pos2, Rect, Vec2};

/// How many cells fit across `width`, with `gap` between them and none outside.
///
/// Never zero for a positive width: a pane narrower than one cell still shows
/// one column of narrower cells (a tile clamps its picture to the cell), because
/// a listing that renders nothing is indistinguishable from an empty directory.
pub fn columns(width: f32, cell: f32, gap: f32) -> usize {
    if width <= 0.0 || cell <= 0.0 {
        return 0;
    }
    let pitch = cell + gap;
    (((width + gap) / pitch).floor() as usize).max(1)
}

/// How many rows a listing of `count` cells takes, `..` included by the caller.
pub fn rows(count: usize, columns: usize) -> usize {
    if columns == 0 {
        return 0;
    }
    count.div_ceil(columns)
}

/// How many rows fit in `height`, as whole cells plus the gaps between them.
pub fn visible_rows(height: f32, cell: f32, gap: f32) -> usize {
    if height <= 0.0 || cell <= 0.0 {
        return 0;
    }
    (((height + gap) / (cell + gap)).floor() as usize).max(0)
}

/// Which row a cell is on. The one place an index is divided by the column
/// count, so `first_row` and the drawer cannot disagree about a wrapping.
pub fn row_of(index: usize, columns: usize) -> usize {
    if columns == 0 {
        return 0;
    }
    index / columns
}

/// The rect of the cell at `index`, at `scroll` rows down.
///
/// `origin` is the top-left of the grid's content, in the same space as the
/// returned rect — a point inside the pane, not the screen.
pub fn cell_rect(
    index: usize,
    columns: usize,
    cell: Vec2,
    gap: f32,
    origin: Pos2,
    scroll: usize,
) -> Rect {
    if columns == 0 {
        return Rect::from_min_size(origin, Vec2::ZERO);
    }
    let column = index % columns;
    // The row is signed on purpose: a cell scrolled off the top of the grid must
    // be *above* the origin so the painter clips it, not folded back onto the
    // first visible row where it would paint a duplicate.
    let row = row_of(index, columns) as f32 - scroll as f32;
    let pitch = cell + Vec2::splat(gap);
    Rect::from_min_size(
        Pos2::new(origin.x + column as f32 * pitch.x, origin.y + row * pitch.y),
        cell,
    )
}

/// Which cell a point is over, or `None` for a gap, a margin or past the end.
///
/// "Over a cell" is the cell's own rect, not its slot: the gap between two tiles
/// belongs to nobody, so a press there does nothing rather than selecting the
/// neighbour of whichever way the division rounded.
pub fn index_at(
    point: Pos2,
    columns: usize,
    cell: Vec2,
    gap: f32,
    origin: Pos2,
    scroll: usize,
    count: usize,
) -> Option<usize> {
    if columns == 0 || count == 0 || cell.x <= 0.0 || cell.y <= 0.0 {
        return None;
    }
    let dx = point.x - origin.x;
    let dy = point.y - origin.y;
    if dx < 0.0 || dy < 0.0 {
        return None;
    }
    let pitch_x = cell.x + gap;
    let pitch_y = cell.y + gap;
    let column = (dx / pitch_x).floor() as usize;
    let row = (dy / pitch_y).floor() as usize;
    if column >= columns {
        return None;
    }
    // Inside the cell, or in the gap after it?
    if dx - column as f32 * pitch_x > cell.x || dy - row as f32 * pitch_y > cell.y {
        return None;
    }
    let index = (row + scroll) * columns + column;
    (index < count).then_some(index)
}

/// The first row to draw, so the selection is on screen and the window moves as
/// little as it can.
///
/// The same rule the list's `first_visible` applies to rows: a selection just
/// below the window scrolls it by one row rather than jumping the page, which is
/// what makes an arrow key look like a movement instead of a redraw.
pub fn first_row(selected_row: usize, scroll: usize, visible_rows: usize) -> usize {
    if visible_rows == 0 {
        return scroll;
    }
    if selected_row < scroll {
        selected_row
    } else if selected_row >= scroll + visible_rows {
        selected_row + 1 - visible_rows
    } else {
        scroll
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;

    const CELL: Vec2 = Vec2::new(104.0, 88.0);
    const GAP: f32 = 6.0;
    const ORIGIN: Pos2 = Pos2::new(8.0, 30.0);

    #[test]
    fn a_row_fits_as_many_cells_as_its_width_allows() {
        // 450 points is the half-pane the memo keeps talking about: four 104-wide
        // cells and three 6-point gaps are 434, so a fourth gap is not needed.
        assert_eq!(columns(450.0, 104.0, 6.0), 4);
        assert_eq!(columns(434.0, 104.0, 6.0), 4);
        assert_eq!(columns(433.0, 104.0, 6.0), 3);
        // A pane too narrow for a cell still shows one column rather than none.
        assert_eq!(columns(40.0, 104.0, 6.0), 1);
        assert_eq!(columns(0.0, 104.0, 6.0), 0);
    }

    #[test]
    fn rows_round_up_because_a_half_full_row_is_a_row() {
        assert_eq!(rows(0, 4), 0);
        assert_eq!(rows(4, 4), 1);
        assert_eq!(rows(5, 4), 2);
        // A listing with no columns has no rows, rather than dividing by zero.
        assert_eq!(rows(5, 0), 0);
    }

    #[test]
    fn the_visible_rows_are_whole_cells_and_the_gaps_between_them() {
        // Two 88-point cells and the gap between them are 182; the gap after the
        // last one is not needed, which is why the height gets one added.
        assert_eq!(visible_rows(182.0, 88.0, 6.0), 2);
        assert_eq!(visible_rows(181.0, 88.0, 6.0), 1);
        assert_eq!(visible_rows(10.0, 88.0, 6.0), 0);
    }

    /// The hit test and the painter are inverses. This is the test that fails
    /// when one of them counts the trailing gap and the other does not.
    #[test]
    fn every_cell_is_where_a_click_on_it_lands() {
        for scroll in [0, 1, 7] {
            for index in 0..40 {
                let rect = cell_rect(index, 4, CELL, GAP, ORIGIN, scroll);
                let on_screen = rect.top() >= ORIGIN.y;
                for point in [
                    rect.left_top(),
                    rect.center(),
                    rect.right_bottom() - vec2(0.5, 0.5),
                ] {
                    let answer = index_at(point, 4, CELL, GAP, ORIGIN, scroll, 40);
                    assert_eq!(
                        answer,
                        on_screen.then_some(index),
                        "cell {index} at scroll {scroll} is not at {point:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_gap_belongs_to_nobody() {
        // The 6 points between the first and second cell of the first row: over
        // no tile at all, so a press there selects nothing.
        let first = cell_rect(0, 4, CELL, GAP, ORIGIN, 0);
        let gap_point = Pos2::new(first.right() + GAP / 2.0, first.center().y);
        assert_eq!(index_at(gap_point, 4, CELL, GAP, ORIGIN, 0, 40), None);
        // And the margin above and left of the grid.
        assert_eq!(
            index_at(
                Pos2::new(ORIGIN.x - 1.0, ORIGIN.y + 1.0),
                4,
                CELL,
                GAP,
                ORIGIN,
                0,
                40
            ),
            None
        );
    }

    #[test]
    fn a_point_past_the_last_cell_is_not_an_index() {
        // Row 0 has four cells; the fifth slot is empty space.
        let empty = Pos2::new(
            ORIGIN.x + 4.0 * (CELL.x + GAP) + CELL.x / 2.0,
            ORIGIN.y + 4.0,
        );
        assert_eq!(index_at(empty, 4, CELL, GAP, ORIGIN, 0, 4), None);
        // And beyond the count, even where a cell would be.
        let beyond = cell_rect(9, 4, CELL, GAP, ORIGIN, 0).center();
        assert_eq!(index_at(beyond, 4, CELL, GAP, ORIGIN, 0, 4), None);
    }

    #[test]
    fn the_window_follows_the_selection_by_one_row_at_a_time() {
        // Already visible: nothing moves.
        assert_eq!(first_row(3, 2, 4), 2);
        // Just above: up to it.
        assert_eq!(first_row(1, 2, 4), 1);
        // Just below: down until it is the last visible row.
        assert_eq!(first_row(6, 2, 4), 3);
        assert_eq!(first_row(9, 0, 4), 6);
        // A view with no room keeps its scroll rather than jumping.
        assert_eq!(first_row(9, 5, 0), 5);
    }

    /// A row is the column count's business: the index of the first cell of a
    /// row is the row times the columns, and nothing else divides.
    #[test]
    fn a_row_is_a_run_of_columns() {
        assert_eq!(row_of(0, 4), 0);
        assert_eq!(row_of(3, 4), 0);
        assert_eq!(row_of(4, 4), 1);
        assert_eq!(row_of(7, 4), 1);
        assert_eq!(row_of(7, 0), 0);
    }
}
