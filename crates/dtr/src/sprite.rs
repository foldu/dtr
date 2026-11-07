//! The sprite face: box drawing, block elements and braille, drawn as geometry
//! instead of taken from a font.
//!
//! A port of Ghostty's built-in "sprite face" (`src/font/sprite/draw/*.zig`,
//! MIT, revision `5813c94`), for the same reason Ghostty has one. These ranges
//! are *diagrams*: a font draws them at its own thickness and its own side
//! bearings, so `─` in DejaVu is 19 px of ink in a 17 px cell and two of them
//! overlap, while `█` leaves a pixel of seam against its neighbour. Here the
//! cell *is* the canvas, so an edge of a box is the edge of the cell by
//! construction, at any cell size and on any display.
//!
//! Not ported, and left to the font: the four arc corners (`╭╮╯╰`), the
//! diagonals (`╱╲╳`), the legacy-computing blocks and the geometric shapes. They
//! are decorative rather than structural, and a font's version of `╳` is better
//! than a rectangle I would draw instead (`docs/cjk.md` §5, M6).

use crate::render::Metrics;

/// A drawn glyph: an 8-bit mask the size of one cell, and where it goes.
///
/// The placement is measured the way the rasteriser measures one — `top` is the
/// distance from the baseline up to the top of the mask — so the rest of the
/// pipeline cannot tell a sprite from a font glyph.
pub(crate) struct Sprite {
    pub width: u32,
    pub height: u32,
    pub left: i32,
    pub top: i32,
    pub data: Vec<u8>,
}

/// The cell, as a mask to draw into.
struct Canvas {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

impl Canvas {
    fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            data: vec![0; (width * height) as usize],
        }
    }

    /// Fill an axis-aligned box, clipped to the canvas. `x1`/`y1` are exclusive.
    fn box_(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, alpha: u8) {
        let x0 = x0.clamp(0, self.width as i32);
        let y0 = y0.clamp(0, self.height as i32);
        let x1 = x1.clamp(0, self.width as i32);
        let y1 = y1.clamp(0, self.height as i32);
        for y in y0..y1 {
            let row = (y as u32 * self.width) as usize;
            for x in x0..x1 {
                let at = row + x as usize;
                self.data[at] = self.data[at].max(alpha);
            }
        }
    }
}

/// A fraction of the cell, in eighths. Every block and box fraction is a
/// multiple of one eighth, and integers keep the packing and the rounding
/// exact.
type Eighths = u8;

/// The right/bottom edge of a fraction of `size`.
///
/// Rounded the way Ghostty rounds (`Fraction.max`): a 7 px cell's half is at 4,
/// so the block covers 4 px and the next one starts there — the two halves are
/// one cell between them with no gap and no overlap.
fn frac_max(eighths: Eighths, size: u32) -> i32 {
    (f64::from(eighths) / 8.0 * f64::from(size)).round() as i32
}

/// How thick a light line is, doubled for a heavy one.
///
/// Ghostty's `Thickness`: `light` is the face's box thickness, `heavy` twice
/// that, `super_light` half. A cell of 9 px with a 1 px line is the usual case;
/// a 28 pt cell wants 2 px or a box drawing character disappears.
fn thickness(metrics: &Metrics) -> (u32, u32) {
    let light = metrics.box_thickness.round().max(1.0);
    let light = light as u32;
    (light, light.saturating_mul(2))
}

/// Which edges a line/box character has, and of what style.
///
/// Packed like Ghostty's `Lines`: two bits per edge, in the order up, right,
/// down, left, so the generated table below is a single byte per codepoint.
const NONE: u8 = 0;
const LIGHT: u8 = 1;
const HEAVY: u8 = 2;
const DOUBLE: u8 = 3;

fn edge(lines: u8, index: u32) -> u8 {
    (lines >> (2 * index)) & 0b11
}

const UP: u32 = 0;
const RIGHT: u32 = 1;
const DOWN: u32 = 2;
const LEFT: u32 = 3;

/// Every box-drawing codepoint Ghostty's `linesChar` answers for, and its edges.
///
/// Generated from `box.zig`'s switch (`0x… => linesChar(…, .{ … })`); the
/// codepoints it does not list — the arcs, the diagonals, the dashes — fall
/// through to the font.
const LINES: &[(u16, u8)] = &[
    (0x2500, 0b01000100),
    (0x2501, 0b10001000),
    (0x2502, 0b00010001),
    (0x2503, 0b00100010),
    (0x250C, 0b00010100),
    (0x250D, 0b00011000),
    (0x250E, 0b00100100),
    (0x250F, 0b00101000),
    (0x2510, 0b01010000),
    (0x2511, 0b10010000),
    (0x2512, 0b01100000),
    (0x2513, 0b10100000),
    (0x2514, 0b00000101),
    (0x2515, 0b00001001),
    (0x2516, 0b00000110),
    (0x2517, 0b00001010),
    (0x2518, 0b01000001),
    (0x2519, 0b10000001),
    (0x251A, 0b01000010),
    (0x251B, 0b10000010),
    (0x251C, 0b00010101),
    (0x251D, 0b00011001),
    (0x251E, 0b00010110),
    (0x251F, 0b00100101),
    (0x2520, 0b00100110),
    (0x2521, 0b00011010),
    (0x2522, 0b00101001),
    (0x2523, 0b00101010),
    (0x2524, 0b01010001),
    (0x2525, 0b10010001),
    (0x2526, 0b01010010),
    (0x2527, 0b01100001),
    (0x2528, 0b01100010),
    (0x2529, 0b10010010),
    (0x252A, 0b10100001),
    (0x252B, 0b10100010),
    (0x252C, 0b01010100),
    (0x252D, 0b10010100),
    (0x252E, 0b01011000),
    (0x252F, 0b10011000),
    (0x2530, 0b01100100),
    (0x2531, 0b10100100),
    (0x2532, 0b01101000),
    (0x2533, 0b10101000),
    (0x2534, 0b01000101),
    (0x2535, 0b10000101),
    (0x2536, 0b01001001),
    (0x2537, 0b10001001),
    (0x2538, 0b01000110),
    (0x2539, 0b10000110),
    (0x253A, 0b01001010),
    (0x253B, 0b10001010),
    (0x253C, 0b01010101),
    (0x253D, 0b10010101),
    (0x253E, 0b01011001),
    (0x253F, 0b10011001),
    (0x2540, 0b01010110),
    (0x2541, 0b01100101),
    (0x2542, 0b01100110),
    (0x2543, 0b10010110),
    (0x2544, 0b01011010),
    (0x2545, 0b10100101),
    (0x2546, 0b01101001),
    (0x2547, 0b10011010),
    (0x2548, 0b10101001),
    (0x2549, 0b10100110),
    (0x254A, 0b01101010),
    (0x254B, 0b10101010),
    (0x2550, 0b11001100),
    (0x2551, 0b00110011),
    (0x2552, 0b00011100),
    (0x2553, 0b00110100),
    (0x2554, 0b00111100),
    (0x2555, 0b11010000),
    (0x2556, 0b01110000),
    (0x2557, 0b11110000),
    (0x2558, 0b00001101),
    (0x2559, 0b00000111),
    (0x255A, 0b00001111),
    (0x255B, 0b11000001),
    (0x255C, 0b01000011),
    (0x255D, 0b11000011),
    (0x255E, 0b00011101),
    (0x255F, 0b00110111),
    (0x2560, 0b00111111),
    (0x2561, 0b11010001),
    (0x2562, 0b01110011),
    (0x2563, 0b11110011),
    (0x2564, 0b11011100),
    (0x2565, 0b01110100),
    (0x2566, 0b11111100),
    (0x2567, 0b11001101),
    (0x2568, 0b01000111),
    (0x2569, 0b11001111),
    (0x256A, 0b11011101),
    (0x256B, 0b01110111),
    (0x256C, 0b11111111),
    (0x2574, 0b01000000),
    (0x2575, 0b00000001),
    (0x2576, 0b00000100),
    (0x2577, 0b00010000),
    (0x2578, 0b10000000),
    (0x2579, 0b00000010),
    (0x257A, 0b00001000),
    (0x257B, 0b00100000),
    (0x257C, 0b01001000),
    (0x257D, 0b00100001),
    (0x257E, 0b10000100),
    (0x257F, 0b00010010),
];

/// Draw `ch` if it is one of the ranges this module owns.
pub(crate) fn draw(ch: char, metrics: &Metrics) -> Option<Sprite> {
    let cp = ch as u32;
    let width = metrics.cell_width.round().max(1.0) as u32;
    let height = metrics.cell_height.round().max(1.0) as u32;
    let mut canvas = Canvas::new(width, height);
    if let Some(&(_, lines)) = LINES.iter().find(|(cp_, _)| u32::from(*cp_) == cp) {
        lines_char(&mut canvas, metrics, lines, width, height);
    } else if let Some(kind) = dash(cp) {
        dash_char(&mut canvas, metrics, kind, width, height);
    } else if (0x2580..=0x259f).contains(&cp) {
        block(&mut canvas, cp, width, height);
    } else if (0x2800..=0x28ff).contains(&cp) {
        braille(&mut canvas, cp, width, height);
    } else {
        return None;
    }
    Some(Sprite {
        width,
        height,
        left: 0,
        // The mask starts at the cell's top edge, which is `baseline` above the
        // baseline every other glyph is placed on.
        top: metrics.baseline.round() as i32,
        data: canvas.data,
    })
}

/// Is this a codepoint the sprite face draws?
pub(crate) fn supported(ch: char) -> bool {
    let cp = ch as u32;
    LINES.iter().any(|(c, _)| u32::from(*c) == cp)
        || dash(cp).is_some()
        || (0x2580..=0x259f).contains(&cp)
        || (0x2800..=0x28ff).contains(&cp)
}

/// A dashed line: how many dashes, whether they are heavy, and the axis.
fn dash(cp: u32) -> Option<(u32, bool, bool)> {
    // (count, heavy, horizontal)
    Some(match cp {
        0x2504 => (3, false, true),
        0x2505 => (3, true, true),
        0x2506 => (3, false, false),
        0x2507 => (3, true, false),
        0x2508 => (4, false, true),
        0x2509 => (4, true, true),
        0x250a => (4, false, false),
        0x250b => (4, true, false),
        0x254c => (2, false, true),
        0x254d => (2, true, true),
        0x254e => (2, false, false),
        0x254f => (2, true, false),
        _ => return None,
    })
}

fn dash_char(
    canvas: &mut Canvas,
    metrics: &Metrics,
    (count, heavy, horizontal): (u32, bool, bool),
    width: u32,
    height: u32,
) {
    let (light, heavy_px) = thickness(metrics);
    let thick = if heavy { heavy_px } else { light };
    // `2n - 1` slots, of which the odd ones are ink: that is what makes the
    // dashes and the gaps the same width.
    let slots = count.saturating_mul(2).saturating_sub(1);
    if slots == 0 {
        return;
    }
    if horizontal {
        let top = ((height.saturating_sub(thick)) / 2) as i32;
        for index in (0..count).map(|index| index * 2) {
            let x0 = (u64::from(index) * u64::from(width) / u64::from(slots)) as i32;
            let x1 = (u64::from(index + 1) * u64::from(width) / u64::from(slots)) as i32;
            canvas.box_(x0, top, x1, top + thick as i32, 0xff);
        }
    } else {
        let left = ((width.saturating_sub(thick)) / 2) as i32;
        for index in (0..count).map(|index| index * 2) {
            let y0 = (u64::from(index) * u64::from(height) / u64::from(slots)) as i32;
            let y1 = (u64::from(index + 1) * u64::from(height) / u64::from(slots)) as i32;
            canvas.box_(left, y0, left + thick as i32, y1, 0xff);
        }
    }
}

/// One line/box character: an arm towards each edge it has, meeting in the
/// middle.
///
/// A direct port of Ghostty's `linesChar`, including the rules for where an arm
/// *stops*: a light arm that meets a heavy one stops at the heavy arm's edge, and
/// a doubled arm is drawn as two thin lines that end where the crossing line
/// is. Those rules are why `┼` has an unbroken light centre and `╬` has a hole
/// in it, and they are not obvious enough to re-derive.
fn lines_char(canvas: &mut Canvas, metrics: &Metrics, lines: u8, width: u32, height: u32) {
    let (light, heavy) = thickness(metrics);
    let up = edge(lines, UP);
    let right = edge(lines, RIGHT);
    let down = edge(lines, DOWN);
    let left = edge(lines, LEFT);

    let h_light_top = (height.saturating_sub(light) / 2) as i32;
    let h_light_bottom = h_light_top + light as i32;
    let h_heavy_top = (height.saturating_sub(heavy) / 2) as i32;
    let h_heavy_bottom = h_heavy_top + heavy as i32;
    let h_double_top = h_light_top - light as i32;
    let h_double_bottom = h_light_bottom + light as i32;

    let v_light_left = (width.saturating_sub(light) / 2) as i32;
    let v_light_right = v_light_left + light as i32;
    let v_heavy_left = (width.saturating_sub(heavy) / 2) as i32;
    let v_heavy_right = v_heavy_left + heavy as i32;
    let v_double_left = v_light_left - light as i32;
    let v_double_right = v_light_right + light as i32;

    let up_bottom = if left == HEAVY || right == HEAVY {
        h_heavy_bottom
    } else if left != right || down == up {
        if left == DOUBLE || right == DOUBLE {
            h_double_bottom
        } else {
            h_light_bottom
        }
    } else if left == NONE && right == NONE {
        h_light_bottom
    } else {
        h_light_top
    };
    let down_top = if left == HEAVY || right == HEAVY {
        h_heavy_top
    } else if left != right || up == down {
        if left == DOUBLE || right == DOUBLE {
            h_double_top
        } else {
            h_light_top
        }
    } else if left == NONE && right == NONE {
        h_light_top
    } else {
        h_light_bottom
    };
    let left_right = if up == HEAVY || down == HEAVY {
        v_heavy_right
    } else if up != down || left == right {
        if up == DOUBLE || down == DOUBLE {
            v_double_right
        } else {
            v_light_right
        }
    } else if up == NONE && down == NONE {
        v_light_right
    } else {
        v_light_left
    };
    let right_left = if up == HEAVY || down == HEAVY {
        v_heavy_left
    } else if up != down || right == left {
        if up == DOUBLE || down == DOUBLE {
            v_double_left
        } else {
            v_light_left
        }
    } else if up == NONE && down == NONE {
        v_light_left
    } else {
        v_light_right
    };

    let cell_height = height as i32;
    let cell_width = width as i32;
    match up {
        NONE => {}
        LIGHT => canvas.box_(v_light_left, 0, v_light_right, up_bottom, 0xff),
        HEAVY => canvas.box_(v_heavy_left, 0, v_heavy_right, up_bottom, 0xff),
        DOUBLE => {
            let left_bottom = if left == DOUBLE {
                h_light_top
            } else {
                up_bottom
            };
            let right_bottom = if right == DOUBLE {
                h_light_top
            } else {
                up_bottom
            };
            canvas.box_(v_double_left, 0, v_light_left, left_bottom, 0xff);
            canvas.box_(v_light_right, 0, v_double_right, right_bottom, 0xff);
        }
        _ => {}
    }
    match right {
        NONE => {}
        LIGHT => canvas.box_(right_left, h_light_top, cell_width, h_light_bottom, 0xff),
        HEAVY => canvas.box_(right_left, h_heavy_top, cell_width, h_heavy_bottom, 0xff),
        DOUBLE => {
            let top_left = if up == DOUBLE {
                v_light_right
            } else {
                right_left
            };
            let bottom_left = if down == DOUBLE {
                v_light_right
            } else {
                right_left
            };
            canvas.box_(top_left, h_double_top, cell_width, h_light_top, 0xff);
            canvas.box_(
                bottom_left,
                h_light_bottom,
                cell_width,
                h_double_bottom,
                0xff,
            );
        }
        _ => {}
    }
    match down {
        NONE => {}
        LIGHT => canvas.box_(v_light_left, down_top, v_light_right, cell_height, 0xff),
        HEAVY => canvas.box_(v_heavy_left, down_top, v_heavy_right, cell_height, 0xff),
        DOUBLE => {
            let left_top = if left == DOUBLE {
                h_light_bottom
            } else {
                down_top
            };
            let right_top = if right == DOUBLE {
                h_light_bottom
            } else {
                down_top
            };
            canvas.box_(v_double_left, left_top, v_light_left, cell_height, 0xff);
            canvas.box_(v_light_right, right_top, v_double_right, cell_height, 0xff);
        }
        _ => {}
    }
    match left {
        NONE => {}
        LIGHT => canvas.box_(0, h_light_top, left_right, h_light_bottom, 0xff),
        HEAVY => canvas.box_(0, h_heavy_top, left_right, h_heavy_bottom, 0xff),
        DOUBLE => {
            let top_right = if up == DOUBLE {
                v_light_left
            } else {
                left_right
            };
            let bottom_right = if down == DOUBLE {
                v_light_left
            } else {
                left_right
            };
            canvas.box_(0, h_double_top, top_right, h_light_top, 0xff);
            canvas.box_(0, h_light_bottom, bottom_right, h_double_bottom, 0xff);
        }
        _ => {}
    }
}

/// Block elements: halves, eighths, quadrants and shades.
fn block(canvas: &mut Canvas, cp: u32, width: u32, height: u32) {
    // A shade is a flat alpha: a mask has no pattern to dither with, and at cell
    // size a `░` at 25 % is what a dither looks like from a metre away.
    let shade = match cp {
        0x2591 => 0x40,
        0x2592 => 0x80,
        0x2593 => 0xc0,
        0x2588 => 0xff,
        _ => 0,
    };
    if shade != 0 {
        canvas.box_(0, 0, width as i32, height as i32, shade);
        return;
    }
    // (align_h, align_v, width eighths, height eighths): 0 = start, 1 = centre,
    // 2 = end, so the same table covers `▀`, `▏` and the quadrants' edges.
    let spec = match cp {
        0x2580 => (1, 0, 8, 4),
        0x2581 => (1, 2, 8, 1),
        0x2582 => (1, 2, 8, 2),
        0x2583 => (1, 2, 8, 3),
        0x2584 => (1, 2, 8, 4),
        0x2585 => (1, 2, 8, 5),
        0x2586 => (1, 2, 8, 6),
        0x2587 => (1, 2, 8, 7),
        0x2589 => (0, 1, 7, 8),
        0x258a => (0, 1, 6, 8),
        0x258b => (0, 1, 5, 8),
        0x258c => (0, 1, 4, 8),
        0x258d => (0, 1, 3, 8),
        0x258e => (0, 1, 2, 8),
        0x258f => (0, 1, 1, 8),
        0x2590 => (2, 1, 4, 8),
        0x2594 => (1, 0, 8, 1),
        0x2595 => (2, 1, 1, 8),
        _ => {
            // The quadrants are combinations, not fractions.
            let (tl, tr, bl, br) = match cp {
                0x2596 => (false, false, true, false),
                0x2597 => (false, false, false, true),
                0x2598 => (true, false, false, false),
                0x2599 => (true, false, true, true),
                0x259a => (true, false, false, true),
                0x259b => (true, true, true, false),
                0x259c => (true, true, false, true),
                0x259d => (false, true, false, false),
                0x259e => (false, true, true, false),
                0x259f => (false, true, true, true),
                _ => return,
            };
            let (w, h) = (width as i32, height as i32);
            let (xm, ym) = (w / 2, h / 2);
            if tl {
                canvas.box_(0, 0, xm, ym, 0xff);
            }
            if tr {
                canvas.box_(xm, 0, w, ym, 0xff);
            }
            if bl {
                canvas.box_(0, ym, xm, h, 0xff);
            }
            if br {
                canvas.box_(xm, ym, w, h, 0xff);
            }
            return;
        }
    };
    let (align_h, align_v, w_eighths, h_eighths) = spec;
    let bw = frac_max(w_eighths, width);
    let bh = frac_max(h_eighths, height);
    let x = match align_h {
        0 => 0,
        2 => width as i32 - bw,
        _ => (width as i32 - bw) / 2,
    };
    let y = match align_v {
        0 => 0,
        2 => height as i32 - bh,
        _ => (height as i32 - bh) / 2,
    };
    canvas.box_(x, y, x + bw, y + bh, 0xff);
}

/// Braille: eight dots on a 2 × 4 grid, sized so that the dots and the gaps
/// share the cell without ever becoming zero or fractional.
///
/// A port of Ghostty's `draw2800_28FF`, including its order of concessions:
/// make the dot non-zero first, then the margin, then the spacing, then widen
/// the dot. A 9 × 17 cell forces every one of those decisions, which is why the
/// sequence exists.
fn braille(canvas: &mut Canvas, cp: u32, width: u32, height: u32) {
    let mut w = (width / 4).min(height / 8) as i32;
    let mut x_spacing = (width / 4) as i32;
    let mut y_spacing = (height / 8) as i32;
    let mut x_margin = x_spacing / 2;
    let mut y_margin = y_spacing / 2;
    let mut x_left = width as i32 - 2 * x_margin - x_spacing - 2 * w;
    let mut y_left = height as i32 - 2 * y_margin - 3 * y_spacing - 4 * w;

    if x_left >= 2 && y_left >= 4 && w == 0 {
        w += 1;
        x_left -= 2;
        y_left -= 4;
    }
    if x_left >= 2 && x_margin == 0 {
        x_margin = 1;
        x_left -= 2;
    }
    if y_left >= 2 && y_margin == 0 {
        y_margin = 1;
        y_left -= 2;
    }
    if x_left >= 1 {
        x_spacing += 1;
    }
    if y_left >= 3 {
        y_spacing += 1;
    }
    if x_left >= 2 {
        x_margin += 1;
    }
    if y_left >= 2 {
        y_margin += 1;
    }
    if x_left >= 2 && y_left >= 4 {
        w += 1;
    }
    if w < 1 {
        return;
    }

    let x = [x_margin, x_margin + w + x_spacing];
    let mut y = [0i32; 4];
    y[0] = y_margin;
    for index in 1..4 {
        y[index] = y[index - 1] + w + y_spacing;
    }
    // The bit order is the codepoint's: tl, ul, ll, tr, ur, lr, bl, br — note
    // that the left column is the first three and the bottom-left dot is the
    // seventh, which is not where a reader expects it.
    let bits = (cp & 0xff) as u8;
    let dots = [
        (0usize, 0usize, bits & 0b0000_0001 != 0),
        (0, 1, bits & 0b0000_0010 != 0),
        (0, 2, bits & 0b0000_0100 != 0),
        (1, 0, bits & 0b0000_1000 != 0),
        (1, 1, bits & 0b0001_0000 != 0),
        (1, 2, bits & 0b0010_0000 != 0),
        (0, 3, bits & 0b0100_0000 != 0),
        (1, 3, bits & 0b1000_0000 != 0),
    ];
    for (column, row, on) in dots {
        if on {
            canvas.box_(x[column], y[row], x[column] + w, y[row] + w, 0xff);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(cell_width: f32, cell_height: f32, baseline: f32) -> Metrics {
        Metrics {
            cell_width,
            cell_height,
            baseline,
            face_width: cell_width,
            face_height: cell_height,
            face_top: 0.0,
            icon_height: cell_height,
            icon_height_single: cell_height,
            box_thickness: 1.0,
        }
    }

    fn drawn(ch: char, metrics: &Metrics) -> Sprite {
        draw(ch, metrics).expect("a sprite")
    }

    /// The mask is the cell, and it starts at the cell's top edge: the whole
    /// point of drawing these rather than borrowing them.
    #[test]
    fn a_sprite_is_the_cell() {
        let m = metrics(9.0, 17.0, 13.0);
        let sprite = drawn('─', &m);
        assert_eq!((sprite.width, sprite.height), (9, 17));
        assert_eq!((sprite.left, sprite.top), (0, 13));
        assert_eq!(sprite.data.len(), 9 * 17);
    }

    /// A full block is every pixel of the cell at full alpha — the seam between
    /// two of them was the thing this whole milestone was for.
    #[test]
    fn a_full_block_leaves_no_seam() {
        let m = metrics(7.0, 19.0, 15.0);
        let sprite = drawn('█', &m);
        assert!(
            sprite.data.iter().all(|&px| px == 0xff),
            "a block has a hole"
        );
        // And the shade below it is the same block, dimmer.
        let shade = drawn('░', &m);
        assert!(shade.data.iter().all(|&px| px == 0x40));
    }

    /// A light horizontal line runs the full width, one to two pixels thick and
    /// in the middle: that is what joins its neighbours with no gap.
    #[test]
    fn a_line_joins_its_neighbours() {
        for (width, height) in [(9u32, 17u32), (17, 33), (10, 19), (3, 7)] {
            let m = metrics(width as f32, height as f32, 0.0);
            let sprite = drawn('─', &m);
            let inked = |row: usize| {
                (0..width).all(|x| sprite.data[row * width as usize + x as usize] == 0xff)
            };
            let rows: Vec<usize> = (0..height as usize).filter(|row| inked(*row)).collect();
            assert_eq!(rows.len(), 1, "{width}x{height}: {:?} ink rows", rows);
            assert_eq!(
                rows[0],
                (height / 2) as usize,
                "{width}x{height}: not centred"
            );
        }
    }

    /// `┌` is one cell of a box: its arms meet at the cell's *centre* and run to
    /// the right and bottom edges, so the next character's arms meet them. Not
    /// at the cell's corner — that is the mistake this test exists to pin down,
    /// because a corner glyph that dotted the cell's own corner would leave a
    /// gap at every junction.
    #[test]
    fn a_corner_meets_its_arms_in_the_middle() {
        let m = metrics(9.0, 17.0, 0.0);
        let sprite = drawn('┌', &m);
        let at = |x: usize, y: usize| sprite.data[y * 9 + x];
        assert_eq!(at(4, 8), 0xff, "no corner at the centre");
        assert_eq!(at(8, 8), 0xff, "the horizontal arm stops short of the edge");
        assert_eq!(at(4, 16), 0xff, "the vertical arm stops short of the edge");
        assert_eq!(
            at(0, 0),
            0x00,
            "a corner drew into the quadrant it has no arm in"
        );
        assert_eq!(
            at(8, 16),
            0x00,
            "a corner drew into the quadrant it has no arm in"
        );
    }

    /// Braille dots are square, inside the cell, and there are eight places for
    /// them: an all-on pattern inks a plausible fraction of the cell and no
    /// more.
    #[test]
    fn braille_dots_are_inside_the_cell() {
        let m = metrics(9.0, 17.0, 0.0);
        let sprite = drawn('\u{28ff}', &m);
        let inked = sprite.data.iter().filter(|&&px| px == 0xff).count();
        assert!(inked > 8, "too few dots: {inked}");
        assert!(
            inked < 9 * 17 * 3 / 4,
            "braille filled {inked} of {} pixels",
            9 * 17
        );
        // The empty pattern draws nothing at all.
        let blank = drawn('\u{2800}', &m);
        assert!(blank.data.iter().all(|&px| px == 0));
    }

    /// The ranges this module owns, and the ones it deliberately does not.
    #[test]
    fn the_owned_ranges_are_the_structural_ones() {
        for ch in [
            '─', '│', '┌', '┘', '┼', '╬', '╴', '┄', '█', '▀', '▌', '░', '⠿',
        ] {
            assert!(supported(ch), "{ch} should be drawn as geometry");
        }
        for ch in ['╭', '╮', '╯', '╰', '╱', '╲', '╳', 'a', '啊', '\u{1fb00}'] {
            assert!(!supported(ch), "{ch} should come from a font");
        }
    }
}
