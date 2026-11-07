//! The cell renderer: a glyph atlas and one instanced draw call.
//!
//! M0 painted every cell with egui, one text call per non-empty cell. It worked
//! at 80×24 and it could not scale: egui lays out text per call, so a full
//! screen was 1920 layout passes per frame, and bold/italic were whatever
//! egui's single bundled face could fake.
//!
//! This replaces it with what a terminal actually is: a grid of quads. Rasterise
//! each glyph once into an R8 atlas, then draw everything — backgrounds, glyphs,
//! cursor — as instances of one pipeline. The atlas texture holds a single white
//! pixel, so a solid rectangle is just a glyph sample of that pixel; that keeps
//! one pipeline and one draw call for the whole screen.

use std::collections::HashMap;

use crate::pane::{Cursor, CursorShape, Frame};
use crate::ui::Look;

/// A glyph as it sits in the atlas, plus where it goes relative to the cell.
#[derive(Clone, Copy, Debug)]
struct Glyph {
    uv: [f32; 4],
    /// Offset from the cell's left/top, in pixels.
    left: f32,
    top: f32,
    width: f32,
    height: f32,
}

// The fields are consumed by `bytemuck_cast`, which the compiler cannot see.
#[allow(dead_code)]
#[derive(Clone, Copy)]
struct Instance {
    /// x, y, width, height in physical pixels.
    rect: [f32; 4],
    uv: [f32; 4],
    colour: [f32; 4],
}

/// A negative uv is the shader's "no texture, use the colour" flag. Sampling a
/// white texel would work too, but it makes every solid element depend on one
/// byte of the atlas being right — and when that byte is wrong the failure is
/// silent: invisible rectangles, no validation error, nothing to grep for.
const SOLID: [f32; 4] = [-1.0, -1.0, -1.0, -1.0];

impl Instance {
    fn solid(rect: [f32; 4], colour: [u8; 3], alpha: f32) -> Self {
        let mut colour = rgb(colour);
        colour[3] = alpha;
        Self {
            rect,
            uv: SOLID,
            colour,
        }
    }
}

fn rgb(colour: [u8; 3]) -> [f32; 4] {
    [
        f32::from(colour[0]) / 255.0,
        f32::from(colour[1]) / 255.0,
        f32::from(colour[2]) / 255.0,
        1.0,
    ]
}

/// Cell metrics in physical pixels, derived once per size change.
///
/// Everything here belongs to the *grid* face, not to whichever face a glyph
/// came from. That is the point: a fallback face places its glyphs on the
/// grid's baseline, so a Han character and the Latin beside it sit on one line
/// however different their own ascents are.
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    pub cell_width: f32,
    pub cell_height: f32,
    /// Distance from the cell's top edge to the baseline every glyph sits on.
    ///
    /// Computed the way Ghostty computes `cell_baseline` (`font/Metrics.zig`):
    /// the line gap is split in half and put above and below the face, then the
    /// baseline is nudged so the face is centred in the whole-pixel cell.
    pub baseline: f32,
    /// The face's own advance, before it was rounded up to a cell.
    ///
    /// A constraint is measured against the *face* box, not the pixel cell:
    /// that is the difference between an icon that fills the room the type
    /// thinks it has and one that fills the room the grid happens to give it.
    pub face_width: f32,
    /// The face's own line height, before it was rounded up to a cell.
    pub face_height: f32,
    /// Distance from the cell's top edge to the top of the face box.
    ///
    /// The cell is a whole number of pixels and the face is not, so the face
    /// sits an asymmetric distance inside it. Vertical constraints are measured
    /// from the face, not from the cell, for the same reason the baseline is
    /// nudged rather than the face being centred.
    pub face_top: f32,
    /// Cap height, and the icon heights Ghostty derives from it.
    ///
    /// `icon_height_single` is `(2 * cap + face_height) / 3` — the same
    /// heuristic the Nerd Font patcher uses, so an icon in a one-cell
    /// constraint matches what the font designer drew for one cell.
    pub icon_height: f32,
    pub icon_height_single: f32,
}

/// Where a glyph's bitmap sits inside its cell, in pixels from the cell's
/// top-left corner.
///
/// The bearings come from the rasteriser and are relative to the pen on the
/// baseline; the cell's top edge is `baseline` above that. Nothing is scaled:
/// a wide character is 1 em of type inside two cells, and 2 × cell_width is
/// normally wider than the em, so the cell pair is the room it needs rather
/// than a box to fill.
///
/// Two callers need this and must agree: the renderer, to emit an instance, and
/// `--font-report`, to say what the renderer emitted.
fn placement(metrics: &Metrics, image: &swash::scale::image::Image) -> (f32, f32, f32, f32) {
    (
        image.placement.left as f32,
        metrics.baseline - image.placement.top as f32,
        image.placement.width as f32,
        image.placement.height as f32,
    )
}

/// A glyph's box relative to the cell's top-left corner, in pixels.
#[derive(Clone, Copy, Debug)]
struct Ink {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

/// How a glyph is fitted into the cells it may occupy.
///
/// A port of Ghostty's `RenderOptions.Constraint` (`src/font/Glyph.zig`), down
/// to the field names. The rules are the accumulated answer to "why is this
/// icon off-centre and that one overlapping its neighbour", and re-deriving
/// them by eye is how you get a powerline arrow with a seam beside it.
///
/// The pads are thousandths of the face box rather than fractions so the whole
/// constraint can be a `Hash + Eq` atlas key; nothing in the range a font
/// needs is finer than a thousandth of a cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
struct Constraint {
    size: Fit,
    align_h: Align,
    align_v: Align,
    pad_top: u16,
    pad_right: u16,
    pad_bottom: u16,
    pad_left: u16,
    /// The most cells the glyph may spread into.
    max_width: u8,
    height: Height,
}

/// The size rule.
///
/// `Fit` and `Cover` are part of the ported rule set and are matched in
/// `factors`; nothing constructs them yet. Colour emoji want `Cover` with a
/// pad and a centre (`docs/cjk.md` §5, M5) and a user-configured icon height is
/// the other candidate, so they stay as the shape of the thing rather than
/// being deleted and re-derived.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
enum Fit {
    /// Leave the glyph the size its designer drew it.
    #[default]
    None,
    /// Scale down if needed to fit the box, preserving the aspect ratio.
    Fit,
    /// Scale up or down to cover the box, preserving the aspect ratio.
    Cover,
    /// Scale down to fit, and up to cover one cell when smaller — but never
    /// shrink because a second cell was offered. Ghostty's Nerd Font rule:
    /// without it an icon gets smaller when you type a space after it.
    Cover1,
    /// Fill the box in both directions, ignoring the aspect ratio.
    Stretch,
}

/// The alignment rule. `Start` and `End` are the leading and trailing edge of
/// the axis — left and right horizontally, **bottom and top** vertically.
///
/// `Start`, `End` and `Center` are matched in `place` but not constructed: the
/// two rules dtr needs are `Center1` for icons and no alignment at all for
/// everything else. A vendored Nerd Font table is what would use the rest.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
enum Align {
    /// Do not move the glyph, but keep it inside the padded box.
    #[default]
    None,
    Start,
    End,
    Center,
    /// Centred on the *first* cell even when several are available, which is
    /// the patcher's rule for icons: a lone icon must not jump sideways when a
    /// space opens after it.
    Center1,
}

/// Which height a vertical size rule measures against.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
enum Height {
    /// The face's line height: what a box-drawing character needs to tile.
    #[default]
    Cell,
    /// The icon height, which depends on whether the glyph has one cell or two.
    Icon,
}

impl Constraint {
    /// Draw the glyph exactly as the font drew it.
    const NONE: Self = Self {
        size: Fit::None,
        align_h: Align::None,
        align_v: Align::None,
        pad_top: 0,
        pad_right: 0,
        pad_bottom: 0,
        pad_left: 0,
        max_width: 2,
        height: Height::Cell,
    };

    /// A Nerd Font icon: fit or cover one cell, centred on the first cell it
    /// has. Ghostty's generated table uses this for the bulk of the PUA.
    const ICON: Self = Self {
        size: Fit::Cover1,
        align_h: Align::Center1,
        align_v: Align::Center1,
        height: Height::Icon,
        ..Self::NONE
    };

    /// A graphics element: two blocks side by side must not leave a seam, and
    /// a box-drawing line must meet the one beside it, so the glyph fills its
    /// cell exactly.
    const TILE: Self = Self {
        size: Fit::Stretch,
        ..Self::NONE
    };

    /// A powerline arrow: stretched to the cell it belongs to.
    ///
    /// An arrow is a solid shape meant to abut the one beside it, so scaling it
    /// into the cell is the fix for a 36 px arrow in a 33 px cell — and unlike a
    /// box-drawing line, whose three pixels of ink *are* the line, a triangle
    /// stretched by ten percent is still a triangle. One cell wide: an arrow is
    /// one segment of a bar, not an icon with room to spill into.
    const POWERLINE: Self = Self {
        size: Fit::Stretch,
        max_width: 1,
        ..Self::NONE
    };

    fn does_anything(&self) -> bool {
        self.size != Fit::None || self.align_h != Align::None || self.align_v != Align::None
    }

    /// The metrics this constraint measures against.
    ///
    /// A stretched glyph is meant to meet its neighbour at a cell boundary, so
    /// it is measured against the *cell*: measured against the face, `█` would
    /// leave a gap the width of its side bearing, and every row of blocks would
    /// be striped.
    fn box_metrics(&self, metrics: Metrics) -> Metrics {
        if self.size != Fit::Stretch {
            return metrics;
        }
        Metrics {
            face_width: metrics.cell_width,
            face_height: metrics.cell_height,
            face_top: 0.0,
            ..metrics
        }
    }

    /// How many cells this glyph may actually use.
    ///
    /// A face whose advance is nearly its line height is already square, and
    /// stretching it across two cells would draw a rectangle over two cells of
    /// type — the same guard the font patcher has.
    fn cells(&self, offered: u8, metrics: &Metrics) -> u8 {
        if self.size == Fit::Stretch && metrics.face_width > 0.9 * metrics.face_height {
            return 1;
        }
        self.max_width.min(offered)
    }

    /// The horizontal and vertical scale factors for an ink box.
    fn factors(&self, metrics: &Metrics, ink: Ink, cells: u8) -> (f32, f32) {
        if self.size == Fit::None {
            return (1.0, 1.0);
        }
        let m = self.box_metrics(*metrics);
        let multi = cells > 1;
        let pads = |left: u16, right: u16| (pad(left), pad(right));
        let (pad_left, pad_right) = pads(self.pad_left, self.pad_right);
        let target_width = (f32::from(cells) - pad_left - pad_right) * m.face_width;
        let target_height = (1.0 - pad(self.pad_top) - pad(self.pad_bottom))
            * match self.height {
                Height::Cell => m.face_height,
                Height::Icon if multi => m.icon_height,
                Height::Icon => m.icon_height_single,
            };
        let mut width_factor = target_width / ink.width.max(0.01);
        let mut height_factor = target_height / ink.height.max(0.01);
        match self.size {
            Fit::None => unreachable!("handled above"),
            // `.min(1)`: a glyph that already fits is not enlarged, which is
            // what keeps text-looking symbols at their design size.
            Fit::Fit => {
                height_factor = height_factor.min(1.0).min(width_factor);
                width_factor = height_factor;
            }
            Fit::Cover => {
                height_factor = height_factor.min(width_factor);
                width_factor = height_factor;
            }
            Fit::Cover1 => {
                height_factor = height_factor.min(width_factor);
                if multi && height_factor > 1.0 {
                    // The single-cell rule, so an icon does not shrink as the
                    // constraint gets wider. Recursion depth is one: `cells` is
                    // 1 there, so `multi` is false.
                    let (_, single) = self.factors(metrics, ink, 1);
                    height_factor = single.max(1.0);
                }
                width_factor = height_factor;
            }
            Fit::Stretch => {}
        }
        (width_factor, height_factor)
    }

    /// Where the already-scaled ink box goes, relative to the cell.
    fn place(&self, metrics: &Metrics, ink: Ink, cells: u8) -> (f32, f32) {
        if !self.does_anything() {
            return (ink.x, ink.y);
        }
        let m = self.box_metrics(*metrics);
        let mut x = ink.x;
        let mut y = ink.y;
        if self.size != Fit::None || self.align_h != Align::None {
            let pad_left = pad(self.pad_left) * m.face_width;
            let pad_right = pad(self.pad_right) * m.face_width;
            // The face cells are laid out from the left edge of the grid cell,
            // so a two-cell span is one face plus one more cell.
            let span = m.face_width + f32::from(cells - 1) * m.cell_width;
            let start = pad_left;
            let end = span - ink.width - pad_right;
            x = match self.align_h {
                // Not aligned, but still kept inside the padded box: every
                // size rule implies that.
                Align::None => x.max(start).min(end),
                Align::Start => start,
                Align::End => end.max(start),
                Align::Center => ((start + end) / 2.0).max(start),
                Align::Center1 => {
                    let end1 = m.face_width - ink.width - pad_right;
                    ((start + end1) / 2.0).max(start)
                }
            };
        }
        if self.size != Fit::None || self.align_v != Align::None {
            let pad_top = pad(self.pad_top) * m.face_height;
            let pad_bottom = pad(self.pad_bottom) * m.face_height;
            let start = m.face_top + pad_top;
            let end = m.face_top + m.face_height - pad_bottom - ink.height;
            y = match self.align_v {
                Align::None if end < start => (start + end) / 2.0,
                Align::None => y.max(start).min(end),
                // The axis runs downwards, so its start is the bottom edge.
                Align::Start => end,
                Align::End => start,
                Align::Center | Align::Center1 => (start + end) / 2.0,
            };
        }
        (x, y)
    }
}

fn pad(thousandths: u16) -> f32 {
    f32::from(thousandths) / 1000.0
}

/// Does this face draw anything at all for `ch`?
fn has_ink(
    font: swash::FontRef<'_>,
    context: &mut swash::scale::ScaleContext,
    size: f32,
    ch: char,
) -> bool {
    let id = font.charmap().map(ch);
    if id == 0 {
        return false;
    }
    let mut scaler = context.builder(font).size(size).hint(true).build();
    let mut image = swash::scale::image::Image::new();
    swash::scale::Render::new(&[swash::scale::Source::Outline]).render_into(
        &mut scaler,
        id,
        &mut image,
    );
    image.placement.width > 0 && image.placement.height > 0
}

/// The width a face gives an ideograph, in pixels at `size`.
///
/// This is Ghostty's `SizeAdjustment::ic_width` — its default — and the reason
/// it is not x-height or cap height: a CJK face has a *small* x-height by
/// design (the em box is full of kanji), and its capital letters are not the
/// size of its Han characters either, so normalising on either shrinks every
/// Han character by 8–11 %. Measured on this machine: x-height gave 0.92, cap
/// height 0.89, and the same point size is what those faces were drawn for, so
/// the answer is 0.96 — close enough to 1.0 to be the rounding of a measurement
/// rather than a decision.
///
/// A face declares its ideographic width only if it has CJK, so this asks for
/// the advance of `一` first and falls back to Ghostty's estimate:
/// `min(ascii height, two cells)`, the ink box of ASCII on one axis and the
/// width of a Han character on the other.
fn ic_width(font: swash::FontRef<'_>, context: &mut swash::scale::ScaleContext, size: f32) -> f32 {
    let units = f32::from(font.metrics(&[]).units_per_em).max(1.0);
    let px = |value: f32| value * size / units;
    let charmap = font.charmap();
    let ideograph = charmap.map('\u{4e00}');
    if ideograph != 0 {
        return px(font.glyph_metrics(&[]).advance_width(ideograph));
    }
    // The widest advance in ASCII is the cell this face draws text in.
    let cell = (0x20u32..=0x7e)
        .filter_map(char::from_u32)
        .map(|ch| px(font.glyph_metrics(&[]).advance_width(charmap.map(ch))))
        .fold(0.0f32, f32::max);
    ascii_height(font, context, size).min(2.0 * cell)
}

/// The height of the ink box around printable ASCII, in pixels.
///
/// Measured rather than declared, because the point of the number is that `$`
/// and `@` overshoot the lines the face declares and a declared ascent does not
/// know that. One pass over 95 glyphs, once per loaded face.
fn ascii_height(
    font: swash::FontRef<'_>,
    context: &mut swash::scale::ScaleContext,
    size: f32,
) -> f32 {
    let charmap = font.charmap();
    let mut scaler = context.builder(font).size(size).hint(true).build();
    let mut image = swash::scale::image::Image::new();
    let mut rise = 0.0f32;
    let mut drop = 0.0f32;
    for codepoint in 0x20u32..=0x7e {
        let Some(ch) = char::from_u32(codepoint) else {
            continue;
        };
        let id = charmap.map(ch);
        if id == 0 {
            continue;
        }
        swash::scale::Render::new(&[swash::scale::Source::Outline]).render_into(
            &mut scaler,
            id,
            &mut image,
        );
        if image.placement.width == 0 || image.placement.height == 0 {
            continue;
        }
        rise = rise.max(image.placement.top as f32);
        drop = drop.max(image.placement.height as f32 - image.placement.top as f32);
    }
    rise + drop
}

/// The script a character belongs to, for the platform's fallback table.
///
/// `swash::text::Script` and `fontique::Script` (parlance's) are different
/// enums, and swash does not expose the ISO 15924 tag its data is keyed by, so
/// this is the bridge: a name for the scripts a terminal is actually asked to
/// draw. Everything else answers `None` and goes to the family walk — which is
/// the right answer for the private use area and the symbol blocks, since no
/// script table has an opinion about an icon.
fn script_of(ch: char) -> Option<fontique::Script> {
    use swash::text::{Codepoint, Script as Swash};
    let tag = match ch.script() {
        Swash::Han => "Hani",
        Swash::Hiragana => "Hira",
        Swash::Katakana => "Kana",
        Swash::Hangul => "Hang",
        Swash::Arabic => "Arab",
        Swash::Hebrew => "Hebr",
        Swash::Thai => "Thai",
        Swash::Devanagari => "Deva",
        Swash::Bengali => "Beng",
        Swash::Tamil => "Taml",
        Swash::Telugu => "Telu",
        Swash::Kannada => "Knda",
        Swash::Malayalam => "Mlym",
        Swash::Gujarati => "Gujr",
        Swash::Gurmukhi => "Guru",
        Swash::Oriya => "Orya",
        Swash::Sinhala => "Sinh",
        Swash::Khmer => "Khmr",
        Swash::Lao => "Laoo",
        Swash::Myanmar => "Mymr",
        Swash::Georgian => "Geor",
        Swash::Armenian => "Armn",
        Swash::Ethiopic => "Ethi",
        Swash::Cherokee => "Cher",
        _ => return None,
    };
    fontique::Script::parse(tag).ok()
}

/// The constraint a codepoint asks for.
///
/// The full answer is a generated table derived from the Nerd Font patcher's
/// attributes (Ghostty ships one, ~270 distinct variants). dtr uses the same
/// three rules for the whole PUA instead: it is a page of code and gets an icon
/// to the right size and in the right place, where today it leaks out of its
/// cell. The table is the next step if a specific icon still looks wrong
/// (`docs/cjk.md` §5, M3).
fn constraint_for(ch: char) -> Constraint {
    let cp = ch as u32;
    if matches!(cp, 0x2800..=0x28FF) {
        // Braille is a dot grid drawn to fill its cell, so a proportional scale
        // inside the cell keeps the pattern and closes the seams. A braille
        // canvas with a gap between cells is not a canvas.
        return Constraint::TILE;
    }
    if matches!(cp, 0xE0B0..=0xE0D7) {
        return Constraint::POWERLINE;
    }
    // Box drawing and block elements are meant to tile too, and stretching
    // their ink is *not* the fix: `─` is three pixels of ink that would become a
    // solid block, and `▀` would lose the half that makes it a half. Ghostty
    // draws these itself, from a sprite face, for exactly this reason
    // (`docs/cjk.md` §5, M6).
    if is_graphics_element(ch) {
        return Constraint::NONE;
    }
    if is_symbol(ch) {
        Constraint::ICON
    } else {
        Constraint::NONE
    }
}

/// Is this a "symbol-like" codepoint: something that may borrow the cell to its
/// right and wants to be fitted rather than set?
///
/// Private use (where every Nerd Font icon lives), the blocks Ghostty's
/// `isSymbol` names, and the legacy-computing symbols — a block of drawings
/// that a terminal is asked to render and that no text font has.
fn is_symbol(ch: char) -> bool {
    matches!(
        ch as u32,
        0x2460..=0x24FF     // enclosed alphanumerics
        | 0x2190..=0x21FF   // arrows
        | 0x2600..=0x26FF   // miscellaneous symbols
        | 0x2700..=0x27FF   // dingbats
        | 0xE000..=0xF8FF   // private use, the BMP (Nerd Fonts)
        | 0x1F100..=0x1F1FF // enclosed alphanumeric supplement
        | 0x1F300..=0x1F5FF // miscellaneous symbols and pictographs
        | 0x1F600..=0x1F64F // emoticons
        | 0x1F680..=0x1F6FF // transport and map symbols
        | 0x1FB00..=0x1FBFF // symbols for legacy computing
        | 0x1CC00..=0x1CEBF // ... and its Unicode 16 supplement
        | 0xF0000..=0xFFFFD // private use, supplementary
        | 0x100000..=0x10FFFD // private use, supplementary
    )
}

/// Is this a glyph meant to tile: to meet its neighbour with no gap?
///
/// Ghostty's `isGraphicsElement` is box drawing, block elements, legacy
/// computing and powerline — and it does not stretch them (it draws several of
/// them itself, from a sprite face). Braille is here because a braille canvas
/// is the same promise: dots that stop short of the cell edge leave a grid of
/// gaps where the picture should be.
fn is_graphics_element(ch: char) -> bool {
    matches!(
        ch as u32,
        0x2500..=0x257F     // box drawing
        | 0x2580..=0x259F   // block elements
        | 0x2800..=0x28FF   // braille patterns
        | 0xE0B0..=0xE0D7   // powerline, in the private use area
        | 0x1FB00..=0x1FBFF // symbols for legacy computing
        | 0x1CC00..=0x1CEBF
    )
}

pub struct GridRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    /// Kept alive: the bind group references these, and the atlas would need
    /// rebuilding with them if it ever has to grow.
    #[allow(dead_code)]
    bind_group_layout: wgpu::BindGroupLayout,
    #[allow(dead_code)]
    sampler: wgpu::Sampler,
    atlas: wgpu::Texture,
    atlas_size: u32,
    /// Next free row in the atlas packing (simple shelf packing).
    shelf_y: u32,
    shelf_height: u32,
    shelf_x: u32,
    /// Keyed by (face, glyph id, size×4, constraint, cells): the same character
    /// is a different bitmap in the bold face, a different size is a different
    /// raster, and the same icon is a different bitmap one cell wide than two.
    glyphs: HashMap<(u32, u16, u32, Constraint, u8), Glyph>,
    fonts: Fonts,
    instances: Vec<Instance>,
    instance_buffer: Option<wgpu::Buffer>,
    instance_capacity: usize,
    uniform: wgpu::Buffer,
    screen: [f32; 2],
}

/// One pane to draw: where it goes, what it shows, and whether it has focus.
pub struct PaneView<'a> {
    pub rect: crate::layout::Rect,
    pub frame: Option<&'a Frame>,
    pub selection: Option<(usize, usize)>,
    /// Only the focused pane draws a cursor.
    pub cursor: bool,
}

const ATLAS_SIZE: u32 = 1024;
const WHITE_PX: u32 = 1;

impl GridRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, look: &Look) -> Self {
        let atlas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dtr glyph atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = atlas.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("dtr glyph sampler"),
            // 1:1 texel-to-pixel sampling: linear filtering would smear every
            // glyph edge for no benefit.
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dtr grid screen"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dtr grid"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dtr grid"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dtr grid"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("dtr grid"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("dtr grid"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Instance>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x4, // rect
                        1 => Float32x4, // uv
                        2 => Float32x4, // colour
                    ],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let renderer = Self {
            pipeline,
            bind_group,
            bind_group_layout,
            sampler,
            atlas,
            atlas_size: ATLAS_SIZE,
            shelf_y: 0,
            shelf_height: 0,
            shelf_x: WHITE_PX,
            glyphs: HashMap::new(),
            fonts: Fonts::load(look),
            instances: Vec::new(),
            instance_buffer: None,
            instance_capacity: 0,
            uniform,
            screen: [1.0, 1.0],
        };
        renderer
    }

    /// How many instances the last `prepare` produced, for diagnostics.
    pub fn instances(&self) -> usize {
        self.instances.len()
    }

    pub fn metrics(&self) -> Metrics {
        self.fonts.metrics
    }

    /// Build one frame's instances for every visible pane and upload whatever
    /// glyphs are new.
    ///
    /// Panes share one instance buffer and one draw: a per-pane draw would need
    /// the buffer rewritten between them, and the queue writes all land before
    /// the command buffer is submitted, so the last pane would win.
    /// How many glyphs this frame rasterised: a large number means the frame is
    /// paying for font work, not for drawing.
    pub fn glyphs(&self) -> usize {
        self.fonts.glyphs
    }

    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        panes: &[PaneView<'_>],
        look: &Look,
        screen: (f32, f32),
    ) {
        // What a frame actually costs, split where it can differ: glyph work
        // (font resolution and rasterising) against everything else. A slow
        // switch between tabs and a slow redraw look identical from outside.
        self.fonts.glyphs = 0;
        self.screen = [screen.0, screen.1];
        queue.write_buffer(
            &self.uniform,
            0,
            bytemuck_cast(&[screen.0, screen.1, 0.0, 0.0]),
        );

        self.instances.clear();

        let metrics = self.fonts.metrics;
        let (cell_w, cell_h) = (metrics.cell_width, metrics.cell_height);

        for pane in panes {
            self.prepare_pane(device, queue, pane, look, cell_w, cell_h);
        }

        self.upload_instances(device, queue);
        tracing::debug!(
            instances = self.instances.len(),
            glyphs = self.glyphs.len(),
            "grid prepared"
        );
    }

    fn prepare_pane(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pane: &PaneView<'_>,
        look: &Look,
        cell_w: f32,
        cell_h: f32,
    ) {
        let Some(frame) = pane.frame else { return };
        let (origin_x, origin_y) = (pane.rect.x, pane.rect.y);
        // Cells outside the pane are simply not emitted: cheaper than a scissor
        // rect and it cannot smear into a neighbour.
        let cols_fit = (pane.rect.width / cell_w).floor().max(1.0) as usize;
        let rows_fit = (pane.rect.height / cell_h).floor().max(1.0) as usize;
        let visible = |index: usize| -> bool {
            let cols = usize::from(frame.cols).max(1);
            (index / cols) < rows_fit && (index % cols) < cols_fit
        };

        // Backgrounds first: a glyph must never sit under a later rectangle.
        for (index, cell) in frame.cells.iter().enumerate() {
            if !cell.has_bg || !visible(index) {
                continue;
            }
            let (x, y) = cell_origin(index, frame.cols, cell_w, cell_h);
            let (x, y) = (x + origin_x, y + origin_y);
            self.instances
                .push(Instance::solid([x, y, cell_w, cell_h], cell.bg, 1.0));
        }

        // Then the glyphs.
        for (index, cell) in frame.cells.iter().enumerate() {
            // A concealed cell keeps its characters and loses its ink: xterm
            // and Ghostty skip the glyph and still draw the background, which
            // is what makes a password field a field and not an absence. The
            // text stays in the frame, because a copy still has to have it.
            if cell.concealed || cell.text.trim().is_empty() || !visible(index) {
                continue;
            }
            let (x, y) = cell_origin(index, frame.cols, cell_w, cell_h);
            let (x, y) = (x + origin_x, y + origin_y);
            let style = face_for(cell.bold, cell.italic);
            // A wide character is drawn across the two cells it occupies, at
            // its design size: the terminal's `Wide` cell and the empty cell
            // after it are the room it needs. Fitting it into the pair is what
            // made Han 21% too large and turned a full stop into a square — the
            // glyph's own bearings already say where it goes.
            //
            // A symbol or a graphics element is a different question, and
            // `constraint_width` answers it from the neighbours.
            let cells = constraint_width(frame, index);
            for ch in cell.text.chars() {
                let Some(glyph) = self.glyph(device, queue, ch, style, look.size, cells) else {
                    continue;
                };
                self.instances.push(Instance {
                    rect: [
                        (x + glyph.left).round(),
                        (y + glyph.top).round(),
                        glyph.width,
                        glyph.height,
                    ],
                    uv: glyph.uv,
                    colour: rgb(cell.fg),
                });
            }
        }

        // The selection sits above the text, as a wash rather than a solid
        // block: you still have to be able to read what you selected.
        if let Some((first, last)) = pane.selection {
            let (start, end) = if first <= last {
                (first, last)
            } else {
                (last, first)
            };
            for index in start..=end.min(frame.cells.len().saturating_sub(1)) {
                if !visible(index) {
                    continue;
                }
                let (x, y) = cell_origin(index, frame.cols, cell_w, cell_h);
                let (x, y) = (x + origin_x, y + origin_y);
                self.instances
                    .push(Instance::solid([x, y, cell_w, cell_h], look.accent, 0.35));
            }
        }

        if let (true, Some(cursor)) = (pane.cursor, frame.cursor) {
            let (x, y) = cell_origin(
                usize::from(cursor.y) * usize::from(frame.cols) + usize::from(cursor.x),
                frame.cols,
                cell_w,
                cell_h,
            );
            // A cursor on the tail of a two-cell character belongs to the
            // character, not to the invisible half: move it back one cell so a
            // block covers both, and a bar sits at the character's own left
            // edge. Ghostty does the same (`renderer/generic.zig`, `addCursor`).
            let (dx, width) = cursor_cell(&cursor, cell_w);
            // Not the whole cell unless the program asked for a block: an insert
            // cursor is a bar against the left edge, which is how a terminal says
            // "typing goes in here" without covering the character it sits on.
            let mark = cursor.mark(crate::layout::Rect::new(
                x + origin_x + dx,
                y + origin_y,
                width,
                cell_h,
            ));
            self.instances.push(Instance::solid(
                [mark.x, mark.y, mark.width, mark.height],
                look.accent,
                1.0,
            ));
        }
    }

    fn upload_instances(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.instances.is_empty() {
            return;
        }
        if self.instance_capacity < self.instances.len() {
            self.instance_capacity = self.instances.len().next_power_of_two().max(1024);
            self.instance_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("dtr grid instances"),
                size: (self.instance_capacity * std::mem::size_of::<Instance>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        if let Some(buffer) = self.instance_buffer.as_ref() {
            queue.write_buffer(buffer, 0, bytemuck_cast(&self.instances));
        }
    }

    pub fn draw<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        let Some(buffer) = self.instance_buffer.as_ref() else {
            return;
        };
        if self.instances.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, buffer.slice(..));
        pass.draw(0..6, 0..self.instances.len() as u32);
    }

    /// Rasterise a character if the atlas does not have it yet.
    ///
    /// `cells` is how many cells the glyph may be constrained into
    /// (`constraint_width`), not how many the character occupies: the grid does
    /// not move, this is room for the glyph inside the cells it already has.
    fn glyph(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        ch: char,
        style: Face,
        size: f32,
        cells: u8,
    ) -> Option<Glyph> {
        let face = self.fonts.face_for(style, ch)?;
        let glyph_id = self.fonts.glyph_id(face, ch)?;
        let constraint = constraint_for(ch);
        let key = (
            face as u32,
            glyph_id,
            (size * 4.0) as u32,
            constraint,
            cells,
        );
        if let Some(glyph) = self.glyphs.get(&key) {
            return Some(*glyph);
        }

        let laid = lay_out(&mut self.fonts, face, glyph_id, ch, size, cells)?;
        let image = &laid.image;
        let (width, height) = (image.placement.width, image.placement.height);
        if width == 0 || height == 0 {
            let glyph = Glyph {
                uv: SOLID,
                left: 0.0,
                top: 0.0,
                width: 0.0,
                height: 0.0,
            };
            self.glyphs.insert(key, glyph);
            return Some(glyph);
        }

        // Shelf packing: fill the current row, then start a new one.
        if self.shelf_x + width > self.atlas_size {
            self.shelf_y += self.shelf_height + 1;
            self.shelf_x = 0;
            self.shelf_height = 0;
        }
        if self.shelf_y + height > self.atlas_size {
            // A full atlas would need a rebuild; at 1024² and one size this is
            // thousands of glyphs, so report and skip instead of panicking.
            tracing::warn!("glyph atlas is full");
            return None;
        }
        let x = self.shelf_x;
        let y = self.shelf_y;
        self.shelf_x += width + 1;
        self.shelf_height = self.shelf_height.max(height);

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.atlas,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &image.data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let _ = device;

        let size_px = self.atlas_size as f32;
        let glyph = Glyph {
            uv: [
                x as f32 / size_px,
                y as f32 / size_px,
                (x + width) as f32 / size_px,
                (y + height) as f32 / size_px,
            ],
            left: laid.ink.x,
            top: laid.ink.y,
            width: laid.ink.width,
            height: laid.ink.height,
        };
        self.glyphs.insert(key, glyph);
        Some(glyph)
    }
}

/// One glyph, rasterised and constrained: the bitmap to copy into the atlas and
/// the box to draw it in.
///
/// The renderer and `--font-report` both come through here. That is the point:
/// a report that recomputed the arithmetic would be a second opinion about a
/// frame nobody draws, which is how the numbers in a bug report stop matching
/// the picture.
struct Laid {
    image: swash::scale::image::Image,
    /// Where the bitmap is drawn, relative to the cell's top-left corner.
    ink: Ink,
    constraint: Constraint,
    /// The cells the constraint was given, after `max_width`.
    cells: u8,
}

fn lay_out(
    fonts: &mut Fonts,
    face: usize,
    glyph_id: u16,
    ch: char,
    size: f32,
    cells: u8,
) -> Option<Laid> {
    let metrics = fonts.metrics;
    let constraint = constraint_for(ch);
    let cells = constraint.cells(cells, &metrics);
    let base = fonts.rasterise(face, glyph_id, size)?;
    let (left, top, width, height) = placement(&metrics, &base);
    let (sx, sy) = constraint.factors(
        &metrics,
        Ink {
            x: left,
            y: top,
            width,
            height,
        },
        cells,
    );
    // A uniform scale is applied to the *outlines*, not to the bitmap: an icon
    // rasterised at 1.6× is crisp, and the same bitmap stretched to 1.6× is
    // grey mush. Only `stretch` changes the aspect ratio, and that one has no
    // outline equivalent, so it is the one drawn into a different rectangle.
    let uniform = (sx - sy).abs() < 1e-3;
    let image = if uniform && (sx - 1.0).abs() > 0.002 {
        let target = ((size * sx * 4.0).round() / 4.0).max(1.0);
        fonts.rasterise(face, glyph_id, target)?
    } else {
        base
    };
    let (left, top, width, height) = placement(&metrics, &image);
    let (scale_x, scale_y) = if uniform { (1.0, 1.0) } else { (sx, sy) };
    let mut ink = Ink {
        x: left,
        y: top,
        width: width * scale_x,
        height: height * scale_y,
    };
    (ink.x, ink.y) = constraint.place(&metrics, ink, cells);
    Some(Laid {
        image,
        ink,
        constraint,
        cells,
    })
}

/// How many cells the glyph in cell `index` may be constrained into.
///
/// Ghostty's `constraintWidth` (`renderer/cell.zig`). A symbol may use the cell
/// beside it when that cell is blank and the glyph to its left was not also a
/// symbol — the second half is what keeps a row of icons on one advance instead
/// of every other one drifting right.
fn constraint_width(frame: &Frame, index: usize) -> u8 {
    let Some(cell) = frame.cells.get(index) else {
        return 1;
    };
    // A wide character already has two cells, so its constraint is the pair.
    if cell.wide {
        return 2;
    }
    let Some(ch) = cell.text.chars().next() else {
        return 1;
    };
    if !is_symbol(ch) {
        return 1;
    }
    let cols = usize::from(frame.cols).max(1);
    // The end of a row is not the end of the buffer: the next cell is the row
    // below, which is never the glyph's to borrow.
    if (index + 1) % cols == 0 {
        return 1;
    }
    if index > 0 {
        let previous = frame
            .cells
            .get(index - 1)
            .and_then(|c| c.text.chars().next());
        if previous.is_some_and(|p| is_symbol(p) && !is_graphics_element(p)) {
            return 1;
        }
    }
    match frame
        .cells
        .get(index + 1)
        .and_then(|c| c.text.chars().next())
    {
        None => 2,
        Some(next) if next.is_whitespace() => 2,
        Some(_) => 1,
    }
}

fn cell_origin(index: usize, cols: u16, cell_w: f32, cell_h: f32) -> (f32, f32) {
    let cols = usize::from(cols).max(1);
    let (row, col) = (index / cols, index % cols);
    (col as f32 * cell_w, row as f32 * cell_h)
}

/// The rectangle a cursor's mark is measured against: `(offset from the
/// reported cell, width)`.
///
/// A cursor on the second half of a two-cell character is reported on the
/// invisible half, so a block has to move back one cell and cover both. A bar
/// or an underline stays at the character's own left edge — a bar marks where
/// typing begins, and the middle of a glyph is not that — but the cell it is
/// measured against is still the character's, so the bar lands on the character
/// and not on the spacer. Ghostty's `addCursor` does the same with
/// `cursor_wide`.
fn cursor_cell(cursor: &Cursor, cell_w: f32) -> (f32, f32) {
    if !cursor.wide_tail {
        return (0.0, cell_w);
    }
    let width = if cursor.shape == CursorShape::Block {
        cell_w * 2.0
    } else {
        cell_w
    };
    (-cell_w, width)
}

fn bytemuck_cast<T: Copy>(slice: &[T]) -> &[u8] {
    // SAFETY: `Instance` is plain old data (f32 arrays only); this is the
    // standard transmute pattern for wgpu vertex buffers.
    unsafe { std::slice::from_raw_parts(slice.as_ptr().cast::<u8>(), std::mem::size_of_val(slice)) }
}

const SHADER: &str = r#"
struct Screen {
    size: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> screen: Screen;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) colour: vec4<f32>,
};

struct Instance {
    @location(0) rect: vec4<f32>,
    @location(1) uv: vec4<f32>,
    @location(2) colour: vec4<f32>,
};

@vertex
fn vs(@builtin(vertex_index) index: u32, instance: Instance) -> VertexOut {
    // Two triangles: 0,1,2 and 2,1,3 over the four corners.
    var corners = array<vec2<f32>, 6>(
        vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0),
        vec2(0.0, 1.0), vec2(1.0, 0.0), vec2(1.0, 1.0),
    );
    let corner = corners[index];
    let pixel = instance.rect.xy + corner * instance.rect.zw;
    // Screen pixels to clip space, with y pointing down.
    let clip = vec2(
        pixel.x / screen.size.x * 2.0 - 1.0,
        1.0 - pixel.y / screen.size.y * 2.0,
    );
    var out: VertexOut;
    out.position = vec4(clip, 0.0, 1.0);
    out.uv = mix(instance.uv.xy, instance.uv.zw, corner);
    out.colour = instance.colour;
    return out;
}

@fragment
fn fs(in: VertexOut) -> @location(0) vec4<f32> {
    // Negative uv means a solid rectangle: backgrounds, the cursor, a
    // selection. Nothing to sample, so alpha comes from the instance.
    var alpha = 1.0;
    if (in.uv.x >= 0.0) {
        alpha = textureSample(atlas, atlas_sampler, in.uv).r;
    }
    return vec4(in.colour.rgb, in.colour.a * alpha);
}
"#;

/// Which face a cell wants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Face {
    Regular = 0,
    Bold = 1,
    Italic = 2,
    BoldItalic = 3,
}

fn face_for(bold: bool, italic: bool) -> Face {
    match (bold, italic) {
        (false, false) => Face::Regular,
        (true, false) => Face::Bold,
        (false, true) => Face::Italic,
        (true, true) => Face::BoldItalic,
    }
}

/// One loaded face: bytes plus the index within the file (a `.ttc` holds more
/// than one).
struct Loaded {
    bytes: Vec<u8>,
    index: usize,
    /// The size this face is rasterised at, relative to the grid's point size.
    ///
    /// A fallback face is chosen for its *coverage*, not its proportions: Noto
    /// CJK at 14 pt has a smaller x-height than DejaVu at 14 pt, so a Han
    /// character beside Latin looks like it came from a smaller size even
    /// though both are correct. Scaling by the ratio of the two x-heights — the
    /// metric Ghostty uses, `SizeAdjustment` and the collection's
    /// `scaleFactor` — closes the optical gap; the baseline is untouched.
    adjust: f32,
}

/// The font family: four faces, loaded once, with swash doing the rasterising.
struct Fonts {
    /// Index 0..4 are the grid face in its four styles; anything above that is a
    /// fallback loaded on demand, indexed by the order it was discovered.
    faces: Vec<Loaded>,
    context: swash::scale::ScaleContext,
    metrics: Metrics,
    /// The point size every face is rasterised at.
    size: f32,
    /// The family that answered as the grid face, for the report and the log.
    grid_name: String,
    /// The width the grid face gives an ideograph, which every fallback is
    /// scaled against.
    ic_width: f32,
    /// Which face has each character in each style. `None` is cached too: a
    /// character nobody covers must not be searched for twice.
    resolved: HashMap<(u8, char), Option<usize>>,
    /// Families to try before the walk, in order — the configured `[font]
    /// fallback` list, then dtr's own — each with where the opinion came from.
    /// Built on the first missing character.
    preferred: Vec<(fontique::FamilyId, &'static str)>,
    /// Everything else, Nerd Font families first. The walk over this one
    /// prefers a monospaced family, which is what stops a proportional face
    /// from answering for a private-use codepoint.
    rest: Vec<fontique::FamilyId>,
    /// Which families pass the monospace probe. Probed during the walk and
    /// remembered: `fonts::works` parses the file, and a character that needs
    /// the walk is rare after the first one of its script.
    monospaced: HashMap<fontique::FamilyId, bool>,
    /// The configured fallback families, kept so the log and the report can
    /// say where an answer came from.
    configured: Vec<String>,
    /// Family name of every face in `faces`, for logging and for tests that need
    /// to know *which* family answered.
    names: Vec<String>,
    /// Fallback faces already loaded, in the order they were found. A second
    /// Han character is found here instead of walking every family again.
    fallback_order: Vec<usize>,
    collection: Option<fontique::Collection>,
    /// Glyphs rasterised in the current frame, for the frame log.
    glyphs: usize,
    source_cache: fontique::SourceCache,
}

impl Fonts {
    fn load(look: &Look) -> Self {
        let (faces, grid_name) = load_faces(look);

        let mut context = swash::scale::ScaleContext::new();
        let mut grid_ic = look.size;
        let metrics = {
            let font = swash::FontRef::from_index(
                &faces[Face::Regular as usize].bytes,
                faces[Face::Regular as usize].index,
            );
            match font {
                Some(font) => {
                    let units = f32::from(font.metrics(&[]).units_per_em).max(1.0);
                    let scale = |value: f32| value * look.size / units;
                    let ascent = scale(font.metrics(&[]).ascent);
                    let advance = scale(
                        font.glyph_metrics(&[])
                            .advance_width(font.charmap().map('M')),
                    );
                    // swash reports descent as a positive distance *below* the
                    // baseline (unlike FreeType, which gives it negative), so
                    // negating it here shrinks the cell and the lines collapse
                    // into each other.
                    let descent = scale(font.metrics(&[]).descent);
                    let line_gap = scale(font.metrics(&[]).leading);
                    // Whole-pixel cells: a fractional advance puts every glyph
                    // on a half-pixel and the whole screen looks out of focus.
                    // Round *up*: rounding down makes the cell narrower than the
                    // glyph advance, so neighbours overlap ("llll" renders as a
                    // smear).
                    let cell_width = advance.max(1.0).ceil();
                    // The line gap is split either side of the face rather than
                    // added below it, and the baseline is then placed inside the
                    // rounded cell the same way — Ghostty's `Metrics.calc`. A
                    // face with a real line gap otherwise sits high in its cell,
                    // and every fallback glyph follows it.
                    let face_height = ascent + descent + line_gap;
                    let cell_height = face_height.max(1.0).ceil();
                    let baseline =
                        (ascent + line_gap / 2.0 + (cell_height - face_height) / 2.0).round();
                    // Cap height decides the single-cell icon height. A font
                    // that does not declare one gets the usual ratio of its
                    // ascent rather than an icon of zero height.
                    let cap_height = {
                        let declared = scale(font.metrics(&[]).cap_height);
                        if declared > 0.0 {
                            declared
                        } else {
                            ascent * 0.72
                        }
                    };
                    // The size every fallback face is scaled against, and it
                    // has to be measured: see `ic_width`.
                    grid_ic = ic_width(font, &mut context, look.size);
                    Metrics {
                        cell_width,
                        cell_height,
                        baseline,
                        face_width: advance.max(1.0),
                        face_height,
                        face_top: baseline - ascent - line_gap / 2.0,
                        icon_height: face_height,
                        icon_height_single: (2.0 * cap_height + face_height) / 3.0,
                    }
                }
                None => Metrics {
                    cell_width: look.size * 0.6,
                    cell_height: look.size * 1.2,
                    baseline: look.size,
                    face_width: look.size * 0.6,
                    face_height: look.size * 1.2,
                    face_top: 0.0,
                    icon_height: look.size * 1.2,
                    icon_height_single: look.size * 1.2,
                },
            }
        };

        Self {
            faces: faces.into_iter().collect(),
            grid_name,
            size: look.size,
            context,
            metrics,
            resolved: HashMap::new(),
            preferred: Vec::new(),
            rest: Vec::new(),
            monospaced: HashMap::new(),
            configured: look.fallback.clone(),
            ic_width: grid_ic,
            names: Vec::new(),
            fallback_order: Vec::new(),
            // Built here, not on the first unmapped character: creating it is a
            // fontconfig scan, and doing that inside `face_for` means a frame
            // that happens to draw a new character pays for a font sort.
            collection: Some(fontique::Collection::new(
                fontique::CollectionOptions::default(),
            )),
            glyphs: 0,
            source_cache: fontique::SourceCache::default(),
        }
    }

    /// The face index that has `ch` in `style`, loading a fallback if needed.
    ///
    /// A terminal that renders boxes for CJK is broken for half its users, and
    /// the grid face is the wrong font to ask: the fallback is whichever
    /// installed family covers the character. Loaded lazily and remembered, so
    /// the search happens once per character, not once per cell.
    ///
    /// The order is a policy, and the policy is the whole of what makes two
    /// runs agree (`docs/cjk.md` §5, M4):
    ///
    /// 1. the faces already loaded, so a second Han character is free;
    /// 2. `[font] fallback` from the config — the person looking at the screen;
    /// 3. `fonts::FALLBACK` — dtr's opinion, which is "a monospaced CJK face";
    /// 4. the platform's own script preference, from fontique's fallback table;
    /// 5. every other installed family: Nerd Fonts first, and monospaced
    ///    before proportional.
    ///
    /// Steps 2–4 are opinions, so the first one that has the glyph wins even if
    /// it is proportional. Step 5 is a *search*, so monospace decides there: a
    /// proportional face with a stray private-use glyph must not answer for an
    /// icon while a patched monospaced family is installed. That rule is the
    /// portable version of fontconfig's `spacing = mono`, and it is what
    /// actually fixes the answer being different on two runs.
    fn face_for(&mut self, style: Face, ch: char) -> Option<usize> {
        let primary = style as usize;
        if self.glyph_id(primary, ch).is_some() {
            return Some(primary);
        }
        if let Some(found) = self.resolved.get(&(style as u8, ch)) {
            return *found;
        }

        // Already-loaded fallbacks first: one CJK family covers most CJK, and
        // re-scanning every family per character would be a font load per cell.
        for index in self.fallback_order.clone() {
            if self.glyph_id(index, ch).is_some() {
                self.resolved.insert((style as u8, ch), Some(index));
                return Some(index);
            }
        }

        self.build_candidates();
        let weight = match style {
            Face::Regular | Face::Italic => fontique::FontWeight::NORMAL,
            Face::Bold | Face::BoldItalic => fontique::FontWeight::BOLD,
        };
        let slant = match style {
            Face::Regular | Face::Bold => fontique::FontStyle::Normal,
            Face::Italic | Face::BoldItalic => fontique::FontStyle::Italic,
        };

        let mut opinions = self.preferred.clone();
        opinions.extend(
            self.script_fallbacks(ch)
                .into_iter()
                .map(|id| (id, "script")),
        );
        for (id, how) in opinions {
            if let Some(index) = self.try_family(id, ch, slant, weight) {
                return Some(self.remember(style, ch, index, how));
            }
        }

        // The search. A proportional answer is remembered and only taken when
        // no monospaced family has the glyph at all.
        let mut proportional = None;
        for id in self.rest.clone() {
            let Some(index) = self.try_family(id, ch, slant, weight) else {
                continue;
            };
            if self.monospaced(id, index) {
                return Some(self.remember(style, ch, index, "walk"));
            }
            if proportional.is_none() {
                proportional = Some(index);
            }
        }
        let found = proportional.map(|index| self.remember(style, ch, index, "walk"));
        self.resolved.insert((style as u8, ch), found);
        found
    }

    /// Build the two candidate lists once.
    ///
    /// `fontique::FamilyId` is a cheap handle, so the lists are built once and
    /// the expensive part — loading a file to ask its charmap — happens per
    /// character as the lists are walked.
    fn build_candidates(&mut self) {
        if !self.preferred.is_empty() || !self.rest.is_empty() {
            return;
        }
        let Some(collection) = self.collection.as_mut() else {
            return;
        };
        let mut preferred: Vec<(fontique::FamilyId, &'static str)> = Vec::new();
        let configured = self.configured.clone();
        for name in configured
            .iter()
            .map(String::as_str)
            .chain(crate::fonts::FALLBACK.iter().copied())
        {
            let how = if self.configured.iter().any(|c| c == name) {
                "configured"
            } else {
                "listed"
            };
            let Some(id) = collection.family_id(name) else {
                continue;
            };
            if !preferred.iter().any(|(seen, _)| *seen == id) {
                preferred.push((id, how));
            }
        }
        let ids: Vec<_> = collection.family_ids().collect();
        let mut nerd = Vec::new();
        let mut rest = Vec::new();
        for id in ids {
            if preferred.iter().any(|(seen, _)| *seen == id) {
                continue;
            }
            let name = collection
                .family(id)
                .map(|family| family.name().to_owned())
                .unwrap_or_default();
            if crate::fonts::is_nerd_font(&name) {
                nerd.push(id);
            } else {
                rest.push(id);
            }
        }
        nerd.extend(rest);
        self.preferred = preferred;
        self.rest = nerd;
    }

    /// The platform's preference for the character's script, if it has one.
    ///
    /// fontique's fallback table is the data a browser uses to pick a readable
    /// face when the primary font has no glyph for a script. It sits after
    /// dtr's list, because that list is the terminal-specific opinion — a
    /// *monospaced* CJK face — and before the walk, because it is still
    /// somebody's considered answer rather than an alphabetical accident.
    fn script_fallbacks(&mut self, ch: char) -> Vec<fontique::FamilyId> {
        let Some(script) = script_of(ch) else {
            return Vec::new();
        };
        let Some(collection) = self.collection.as_mut() else {
            return Vec::new();
        };
        collection
            .fallback_families(fontique::FallbackKey::new(script, None))
            .take(4)
            .collect()
    }

    /// Load one family and answer whether it has `ch`.
    ///
    /// Two style attempts, because a fallback face is usually regular weight
    /// and upright even when the grid is bold or italic; asking for the exact
    /// style would miss it entirely.
    ///
    /// The face is *loaded* even when the answer is no, and that is deliberate:
    /// the walk is ordered by a rule, not by the first hit, so a rejected
    /// family cannot be un-loaded without reading the file again. A rejected
    /// face is never named in `fallback_order`, so nothing draws from it.
    fn try_family(
        &mut self,
        id: fontique::FamilyId,
        ch: char,
        slant: fontique::FontStyle,
        weight: fontique::FontWeight,
    ) -> Option<usize> {
        for (slant, weight) in [
            (slant, weight),
            (fontique::FontStyle::Normal, fontique::FontWeight::NORMAL),
        ] {
            let Some(collection) = self.collection.as_mut() else {
                return None;
            };
            let Some(family) = collection.family(id) else {
                return None;
            };
            let Some(font) = family.match_font(fontique::FontWidth::NORMAL, slant, weight, false)
            else {
                continue;
            };
            let Some(bytes) = font.load(Some(&mut self.source_cache)) else {
                continue;
            };
            let index = font.index() as usize;
            let Some(reference) = swash::FontRef::from_index(bytes.data(), index) else {
                continue;
            };
            if reference.charmap().map(ch) == 0 {
                continue;
            }
            // A charmap entry is not ink. Noto Color Emoji maps every emoji and
            // has no outlines — its bitmaps live in a `CBDT` table the outline
            // source cannot see — and a `.notdef` in a face that covers the
            // block is a box with a codepoint. A terminal that draws nothing is
            // worse than one that draws the wrong glyph, so the walk skips a
            // face whose glyph is empty and keeps looking.
            if !has_ink(reference, &mut self.context, self.size, ch) {
                continue;
            }
            // The face's own metrics, for `-v`: its ascent explains why its
            // glyphs sit where they do, and its x-height is the ratio it is
            // rasterised at. Neither moves the grid's baseline.
            let ascent = self.scaled(reference, reference.metrics(&[]).ascent);
            let own_ic = ic_width(reference, &mut self.context, self.size);
            let adjust = if own_ic <= 0.5 || self.ic_width <= 0.5 {
                1.0
            } else {
                // Clamped: a nonsense metric must not turn into a face drawn
                // four times the size.
                (self.ic_width / own_ic).clamp(0.5, 2.0)
            };
            drop(bytes);
            // Reload to own the bytes: `font.load` borrows from the cache.
            let Some(bytes) = font.load(None) else {
                continue;
            };
            self.names.push(family.name().to_owned());
            self.faces.push(Loaded {
                bytes: bytes.data().to_vec(),
                index,
                adjust,
            });
            tracing::trace!(
                family = self.names.last().map(String::as_str),
                ascent,
                adjust
            );
            return Some(self.faces.len() - 1);
        }
        None
    }

    /// Design units to pixels at the grid's point size.
    fn scaled(&self, font: swash::FontRef<'_>, value: f32) -> f32 {
        let units = f32::from(font.metrics(&[]).units_per_em).max(1.0);
        value * self.size / units
    }

    /// Is this family monospaced, by the same probe the settings menu uses?
    fn monospaced(&mut self, id: fontique::FamilyId, face: usize) -> bool {
        if let Some(known) = self.monospaced.get(&id) {
            return *known;
        }
        let mono = self
            .faces
            .get(face)
            .is_some_and(|loaded| crate::fonts::works(&loaded.bytes, loaded.index as u32));
        self.monospaced.insert(id, mono);
        mono
    }

    /// Record an answer: cache it, name the face as loaded, and say where it
    /// came from.
    fn remember(&mut self, style: Face, ch: char, index: usize, how: &'static str) -> usize {
        self.resolved.insert((style as u8, ch), Some(index));
        self.fallback_order.push(index);
        let family = self.names.get(index - 4).cloned().unwrap_or_default();
        let adjust = self.faces.get(index).map_or(1.0, |loaded| loaded.adjust);
        tracing::debug!(character = %ch, family, adjust, how, "fallback face");
        index
    }

    fn font(&self, face: usize) -> Option<swash::FontRef<'_>> {
        let loaded = self.faces.get(face)?;
        swash::FontRef::from_index(&loaded.bytes, loaded.index)
    }

    fn glyph_id(&self, face: usize, ch: char) -> Option<u16> {
        let font = self.font(face)?;
        let id = font.charmap().map(ch);
        (id != 0).then_some(id)
    }

    fn rasterise(
        &mut self,
        face: usize,
        glyph_id: u16,
        size: f32,
    ) -> Option<swash::scale::image::Image> {
        self.glyphs += 1;
        // The font borrows the face's bytes, the context is a separate field:
        // split the borrow so both can be used at once.
        let loaded = self.faces.get(face)?;
        let font = swash::FontRef::from_index(&loaded.bytes, loaded.index)?;
        // A fallback is drawn at its own optical size, not the grid's: its
        // bearings still land on the grid's baseline, so only the ink changes.
        let size = size * loaded.adjust;
        let context = &mut self.context;
        let mut scaler = context.builder(font).size(size).hint(true).build();
        let mut image = swash::scale::image::Image::new();
        swash::scale::Render::new(&[swash::scale::Source::Outline]).render_into(
            &mut scaler,
            glyph_id,
            &mut image,
        );
        Some(image)
    }
}

/// Characters `--font-report` measures: one from each family a terminal is
/// asked to draw.
///
/// `啊` (`U+554A`) is *the* CJK sample in this repository — AGENTS.md says so.
/// It is a full-width Han character with ink in all four quadrants, so a
/// baseline error, a wrong size or a half-drawn cell is visible in its numbers
/// and on screen. The rest are here because they fail differently: kana, a
/// full-width full stop (tiny ink in a wide cell), box drawing and a block
/// (which must tile), a Nerd Font icon, a powerline arrow, and colour emoji.
const FONT_REPORT_SAMPLE: &[char] = &[
    'M', 'g', '啊', 'あ', '。', '─', '█', '\u{f015}', '\u{e0b0}', '😀',
];

/// The constraint's name, for `--font-report`.
fn constraint_name(constraint: Constraint) -> &'static str {
    match constraint.size {
        Fit::None => "none",
        Fit::Fit => "fit",
        Fit::Cover => "cover",
        Fit::Cover1 => "fit_cover1",
        Fit::Stretch => "stretch",
    }
}

/// Print what the glyph pipeline will do, then let the caller exit.
///
/// The placement arithmetic is the renderer's own (`placement`), so this is not
/// a second opinion: it is the frame the window would draw. `-v` says which
/// family answered; this says what the answer came out as.
pub fn font_report(look: &Look) {
    let mut fonts = Fonts::load(look);
    let metrics = fonts.metrics;
    println!(
        "grid      {} at {}",
        if fonts.grid_name.is_empty() {
            "(none)"
        } else {
            fonts.grid_name.as_str()
        },
        look.size
    );
    println!(
        "cell      {:.0} x {:.0} px, baseline {:.0} px from the top",
        metrics.cell_width, metrics.cell_height, metrics.baseline
    );
    println!();

    for &ch in FONT_REPORT_SAMPLE {
        let Some(face) = fonts.face_for(Face::Regular, ch) else {
            println!("U+{:04X} {ch}  no face has this character", ch as u32);
            continue;
        };
        let name = if face < 4 {
            fonts.grid_name.clone()
        } else {
            fonts.names.get(face - 4).cloned().unwrap_or_default()
        };
        let Some(glyph_id) = fonts.glyph_id(face, ch) else {
            println!("U+{:04X} {ch}  face {face} has no glyph", ch as u32);
            continue;
        };
        // The report shows the room a *lone* glyph gets: the second cell a
        // symbol may borrow when its neighbour is blank. Whether it has one on
        // screen is the neighbours' answer, not the report's.
        let offered = if is_symbol(ch) { 2 } else { 1 };
        let Some(laid) = lay_out(&mut fonts, face, glyph_id, ch, look.size, offered) else {
            continue;
        };
        // The face's own ascent, to show why a fallback used to float: the
        // baseline below is the grid's, not this number.
        let ascent = fonts.font(face).map_or(0.0, |font| {
            let units = f32::from(font.metrics(&[]).units_per_em).max(1.0);
            font.metrics(&[]).ascent * look.size / units
        });
        let advance = fonts.font(face).map_or(0.0, |font| {
            let units = f32::from(font.metrics(&[]).units_per_em).max(1.0);
            font.glyph_metrics(&[]).advance_width(glyph_id) * look.size / units
        });
        let adjust = fonts.faces.get(face).map_or(1.0, |loaded| loaded.adjust);
        println!(
            "U+{:04X} {ch}  face {face}  {name:<26} asc {:>6.2}  adv {:>6.2}  adj {:.2}  ink {:>3}x{:<3}  at ({:+.0},{:+.0}) {:.0}x{:.0}  {} cw{}",
            ch as u32,
            ascent,
            advance,
            adjust,
            laid.image.placement.width,
            laid.image.placement.height,
            laid.ink.x,
            laid.ink.y,
            laid.ink.width,
            laid.ink.height,
            constraint_name(laid.constraint),
            laid.cells,
        );
    }

    println!();
    // Only the faces that answered: `names` holds every family the walk
    // *loaded*, including the ones it rejected, and a report that lists those
    // reads as if they had been used.
    println!(
        "fallbacks {}",
        if fonts.fallback_order.is_empty() {
            "(none loaded)".to_owned()
        } else {
            fonts
                .fallback_order
                .iter()
                .filter_map(|&face| fonts.names.get(face - 4))
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
}

/// Load the four faces: regular, bold, italic, bold-italic, and the family that
/// answered.
///
/// `font.path` wins and can only supply one face, so the other three fall back
/// to it — a user who points at a single file gets a uniform grid rather than a
/// partial one. Otherwise the family is asked for each style and a missing face
/// falls back down the chain: bold-italic → italic → bold → regular, which is
/// what a terminal does when a family ships only two weights.
fn load_faces(look: &Look) -> ([Loaded; 4], String) {
    if let Some(path) = look.path.as_ref() {
        match std::fs::read(path) {
            Ok(bytes) => {
                let loaded = Loaded {
                    bytes,
                    index: 0,
                    adjust: 1.0,
                };
                return (
                    [
                        loaded.clone_face(),
                        loaded.clone_face(),
                        loaded.clone_face(),
                        loaded,
                    ],
                    path.display().to_string(),
                );
            }
            Err(err) => tracing::warn!(path = %path.display(), %err, "configured font"),
        }
    }

    let mut collection = fontique::Collection::new(fontique::CollectionOptions::default());
    // The configured family is tried first; the rest are only fallbacks, so a
    // family that has been uninstalled degrades instead of failing.
    let mut candidates: Vec<String> = Vec::new();
    if let Some(family) = look.family.clone() {
        candidates.push(family);
    }
    candidates.extend(
        [
            "JetBrains Mono",
            "Hack",
            "DejaVu Sans Mono",
            "Liberation Mono",
            "Fira Code",
            "Ubuntu Mono",
            "Noto Sans Mono",
            "monospace",
        ]
        .iter()
        .map(|name| (*name).to_owned()),
    );
    for family in candidates.iter().map(String::as_str) {
        let Some(info) = collection.family_by_name(family) else {
            continue;
        };

        let mut faces: Vec<Option<Loaded>> = Vec::with_capacity(4);
        for (style, weight) in [
            (fontique::FontStyle::Normal, fontique::FontWeight::NORMAL),
            (fontique::FontStyle::Normal, fontique::FontWeight::BOLD),
            (fontique::FontStyle::Italic, fontique::FontWeight::NORMAL),
            (fontique::FontStyle::Italic, fontique::FontWeight::BOLD),
        ] {
            // `synthesize_style: false`: a slanted copy of the regular face is
            // worse than the regular face, and we can tell the difference.
            let font = info.match_font(fontique::FontWidth::NORMAL, style, weight, false);
            let loaded = font.and_then(|font| {
                let blob = font.load(None)?;
                Some(Loaded {
                    bytes: blob.data().to_vec(),
                    index: font.index() as usize,
                    adjust: 1.0,
                })
            });
            faces.push(loaded);
        }

        // Bold without an italic face keeps the bold shape, and vice versa.
        let regular = faces[0].take();
        let clone = |face: &Option<Loaded>| face.as_ref().map(Loaded::clone_face);
        let bold = faces[1].take().or_else(|| clone(&regular));
        let italic = faces[2].take().or_else(|| clone(&regular));
        let bold_italic = faces[3]
            .take()
            .or_else(|| clone(&bold))
            .or_else(|| clone(&italic))
            .or_else(|| clone(&regular));

        if let Some(regular) = regular {
            tracing::info!(
                family,
                bold = bold.is_some(),
                italic = italic.is_some(),
                "grid font"
            );
            let empty = Loaded {
                bytes: Vec::new(),
                index: 0,
                adjust: 1.0,
            };
            return (
                [
                    regular,
                    bold.unwrap_or_else(|| empty.clone_face()),
                    italic.unwrap_or_else(|| empty.clone_face()),
                    bold_italic.unwrap_or(empty),
                ],
                family.to_owned(),
            );
        }
    }

    tracing::warn!("no monospace family found; the grid will draw backgrounds only");
    let empty = Loaded {
        bytes: Vec::new(),
        index: 0,
        adjust: 1.0,
    };
    (
        [
            empty.clone_face(),
            empty.clone_face(),
            empty.clone_face(),
            empty,
        ],
        String::new(),
    )
}

impl Loaded {
    /// `Loaded` is not `Clone` because the bytes are large; callers that need a
    /// second copy of the same face say so explicitly.
    fn clone_face(&self) -> Loaded {
        Loaded {
            bytes: self.bytes.clone(),
            index: self.index,
            adjust: self.adjust,
        }
    }
}

#[cfg(test)]
mod font_tests {
    use super::*;
    use crate::pane::Cell;

    /// One character, measured the way the renderer measures it.
    struct Placed {
        face: usize,
        image: swash::scale::image::Image,
        /// The rectangle the renderer emits, relative to the cell.
        rect: (f32, f32, f32, f32),
        metrics: Metrics,
    }

    /// Place one character at the default look, or `None` when this machine has
    /// no font for it.
    ///
    /// These tests use the renderer's own arithmetic, so a test that passes
    /// here is describing the frame the window draws.
    fn placed(ch: char) -> Option<Placed> {
        let look = Look::default();
        let mut fonts = Fonts::load(&look);
        let face = fonts.face_for(Face::Regular, ch)?;
        let glyph_id = fonts.glyph_id(face, ch)?;
        // Through the renderer's own path — rasterised, constrained, placed —
        // so a passing test describes the frame the window draws.
        let laid = lay_out(&mut fonts, face, glyph_id, ch, look.size, 1)?;
        let rect = (laid.ink.x, laid.ink.y, laid.ink.width, laid.ink.height);
        Some(Placed {
            face,
            image: laid.image,
            rect,
            metrics: fonts.metrics,
        })
    }

    /// One character laid out with a given number of cells offered, or `None`
    /// when this machine has no font for it.
    fn drawn(ch: char, cells: u8) -> Option<(Metrics, Ink, u8)> {
        let look = Look::default();
        let mut fonts = Fonts::load(&look);
        let face = fonts.face_for(Face::Regular, ch)?;
        let glyph_id = fonts.glyph_id(face, ch)?;
        let laid = lay_out(&mut fonts, face, glyph_id, ch, look.size, cells)?;
        Some((fonts.metrics, laid.ink, laid.cells))
    }

    /// A one-row frame, one string per cell.
    fn row(cells: &[(&str, bool)]) -> Frame {
        Frame {
            cols: cells.len() as u16,
            rows: 1,
            default_fg: [0; 3],
            default_bg: [0; 3],
            cursor: None,
            selection: None,
            cells: cells
                .iter()
                .map(|(text, wide)| Cell {
                    text: (*text).to_owned(),
                    wide: *wide,
                    ..Cell::default()
                })
                .collect(),
        }
    }

    /// The codepoint table is the whole of M3's policy, so it is worth pinning
    /// without a font: an icon, a tiling glyph and text each ask for a
    /// different rule.
    #[test]
    fn the_constraint_table_says_what_each_family_gets() {
        // Text is left alone, whatever face it comes from.
        assert_eq!(constraint_for('啊'), Constraint::NONE);
        assert_eq!(constraint_for('M'), Constraint::NONE);
        assert_eq!(constraint_for('。'), Constraint::NONE);
        // Nerd Font icons and the private use area are fitted.
        assert_eq!(constraint_for('\u{f015}'), Constraint::ICON);
        assert_eq!(constraint_for('\u{e000}'), Constraint::ICON);
        // A powerline arrow is fitted to the cell's height, one cell wide.
        assert_eq!(constraint_for('\u{e0b0}'), Constraint::POWERLINE);
        assert_eq!(constraint_for('\u{e0b0}').max_width, 1);
        // Braille tiles by stretching, because its ink *is* a grid.
        assert_eq!(constraint_for('\u{2800}'), Constraint::TILE);
        assert_eq!(constraint_for('\u{28ff}'), Constraint::TILE);
        // Box drawing and blocks are left alone: their ink is a line or a
        // fraction of a cell, and stretching it turns a line into a block.
        assert_eq!(constraint_for('─'), Constraint::NONE);
        assert_eq!(constraint_for('█'), Constraint::NONE);
        assert_eq!(constraint_for('▀'), Constraint::NONE);
        assert!(is_symbol('\u{f015}'));
        assert!(!is_symbol('啊'));
    }

    /// The two-cell rule, which is what stops a row of icons marching to the
    /// right: the first icon may use the blank cell beside it, the second may
    /// not, and neither may borrow a cell that has text in it.
    #[test]
    fn a_symbol_borrows_a_cell_only_when_its_neighbour_is_blank() {
        // Blank both sides: two cells.
        let frame = row(&[("", false), ("\u{f015}", false), ("", false)]);
        assert_eq!(constraint_width(&frame, 1), 2);
        // An icon to the left: aligned with it, so one cell.
        let frame = row(&[("\u{f015}", false), ("\u{f015}", false), ("", false)]);
        assert_eq!(constraint_width(&frame, 1), 1);
        // Text to the right: no room to borrow.
        let frame = row(&[("\u{f015}", false), ("a", false), ("", false)]);
        assert_eq!(constraint_width(&frame, 0), 1);
        // The last column of a row never borrows the next row's first cell.
        let frame = row(&[("", false), ("\u{f015}", false)]);
        assert_eq!(constraint_width(&frame, 1), 1);
        // A wide character already has its pair, and text never borrows.
        let frame = row(&[("啊", true), ("", false)]);
        assert_eq!(constraint_width(&frame, 0), 2);
        let frame = row(&[("a", false), ("", false)]);
        assert_eq!(constraint_width(&frame, 0), 1);
        // A graphics element does not suppress the next symbol's second cell:
        // `█` followed by an icon is not a row of icons.
        let frame = row(&[("█", false), ("\u{f015}", false), ("", false)]);
        assert_eq!(constraint_width(&frame, 1), 2);
    }

    /// A braille cell fills its cell exactly, which is the whole point of
    /// stretching it: measured from the face, every cell of a braille canvas
    /// leaves the same gap and the picture comes out as a grid of tiles.
    #[test]
    fn a_braille_cell_fills_its_cell() {
        let Some((m, ink, _)) = drawn('\u{28ff}', 1) else {
            return; // no font here has braille
        };
        assert!(
            (ink.height - m.cell_height).abs() <= 1.0,
            "a braille cell is {} px tall in a {} px cell",
            ink.height,
            m.cell_height
        );
        assert!(
            (ink.width - m.cell_width).abs() <= 1.0,
            "a braille cell is {} px wide in a {} px cell",
            ink.width,
            m.cell_width
        );
        assert!(
            ink.x.abs() <= 0.5 && ink.y.abs() <= 0.5,
            "a braille cell starts at the cell's corner, not at ({}, {})",
            ink.x,
            ink.y
        );
    }

    /// A powerline arrow is a whole cell tall: drawn at its design size it
    /// overlaps the row above and leaves a notch below.
    #[test]
    fn a_powerline_arrow_fills_the_cell_height() {
        let Some((m, ink, _)) = drawn('\u{e0b0}', 1) else {
            return; // no Nerd Font installed here
        };
        assert!(
            (ink.height - m.cell_height).abs() <= 1.0,
            "a powerline arrow is {} px tall in a {} px cell",
            ink.height,
            m.cell_height
        );
    }

    /// An icon is drawn inside the cells it is given, centred on its own cell,
    /// and never in the row above — which is what it did when the fallback
    /// face's own bearings were used with no constraint at all.
    #[test]
    fn an_icon_fits_and_is_centred() {
        let Some((m, ink, cells)) = drawn('\u{f015}', 2) else {
            return; // no icon font installed here
        };
        assert_eq!(cells, 2, "a lone icon may use the cell beside it");
        assert!(
            ink.x >= -0.5 && ink.x + ink.width <= m.cell_width * 2.0 + 0.5,
            "the icon is {} px wide at x {:+.1}, outside its two {} px cells",
            ink.width,
            ink.x,
            m.cell_width * 2.0
        );
        assert!(
            ink.y >= -0.5 && ink.y + ink.height <= m.cell_height + 0.5,
            "the icon is {} px tall at y {:+.1}, outside its {} px cell",
            ink.height,
            ink.y,
            m.cell_height
        );
        // Centred on the first cell, not on the pair: an icon that jumped when
        // a space opened after it was the bug Ghostty's `center1` fixed. An
        // icon wider than a cell has no room to centre in and goes flush left,
        // which is what `center1`'s clamp does and what it is for.
        let centre = ink.x + ink.width / 2.0;
        if ink.width <= m.face_width {
            assert!(
                (centre - m.face_width / 2.0).abs() <= 1.0,
                "the icon is centred at {centre}, not on the cell's {}",
                m.face_width / 2.0
            );
        } else {
            assert!(
                ink.x.abs() <= 1.0,
                "an icon wider than its cell starts at x {:+.1}, not flush left",
                ink.x
            );
        }
        // And one cell is a smaller box than two.
        let Some((_, narrow, cells)) = drawn('\u{f015}', 1) else {
            return;
        };
        assert_eq!(cells, 1);
        assert!(
            narrow.width <= ink.width + 0.5,
            "an icon with one cell ({} px) came out wider than with two ({} px)",
            narrow.width,
            ink.width
        );
    }

    /// A CJK character must come from another family, and finding it must be
    /// remembered rather than repeated for every cell.
    #[test]
    fn a_han_character_falls_back_to_a_face_that_has_it() {
        let mut fonts = Fonts::load(&Look::default());
        let Some(first) = fonts.face_for(Face::Regular, '啊') else {
            // No CJK font installed here; nothing to prove.
            return;
        };
        assert!(first >= 4, "a fallback must not be one of the grid faces");
        let second = fonts.face_for(Face::Regular, '漢');
        assert_eq!(second, Some(first), "the next character reuses the face");
        assert_eq!(fonts.fallback_order.len(), 1, "one fallback was enough");
    }

    /// The preference list decides which family answers for CJK, not the order
    /// fontconfig happens to report.
    #[test]
    fn a_preferred_fallback_family_is_chosen_over_an_arbitrary_one() {
        let mut fonts = Fonts::load(&Look::default());
        let Some(index) = fonts.face_for(Face::Regular, '啊') else {
            return; // no CJK font installed here
        };
        let name = fonts.names.get(index - 4).cloned().unwrap_or_default();
        // Only assert when a preferred family is actually installed: on a
        // machine without Sarasa or Noto, any family that has the glyph is
        // correct behaviour and there is nothing to check.
        let mut collection = fontique::Collection::new(fontique::CollectionOptions::default());
        let preferred: Vec<&str> = crate::fonts::FALLBACK
            .iter()
            .copied()
            .filter(|family| collection.family_by_name(family).is_some())
            .collect();
        if preferred.is_empty() {
            return;
        }
        assert!(
            preferred.contains(&name.as_str()),
            "picked {name}, but {preferred:?} is installed and preferred"
        );
    }

    /// `啊` is drawn at its design size, on the grid's baseline.
    ///
    /// This is the CJK sample this repository standardises on (AGENTS.md). It
    /// used to be scaled into its two cells and rested on the cell's bottom
    /// edge — 21 % too large and a pixel low, because the cell pair is 18 px
    /// and the glyph is 14 (`docs/cjk.md` §1). The baseline assertion is the
    /// other half: Noto's own ascent is 16.24 against the grid's 13, so a
    /// per-face baseline put every Han glyph 3 px below the Latin beside it.
    #[test]
    fn a_wide_glyph_keeps_its_design_size_and_baseline() {
        let Some(p) = placed('啊') else {
            return; // no CJK font installed here
        };
        let (_, top, width, height) = p.rect;
        assert!(p.face >= 4, "a Han glyph comes from a fallback face");
        assert!(width > 0.0 && height > 0.0, "the sample must have ink");
        assert_eq!(
            (width, height),
            (
                p.image.placement.width as f32,
                p.image.placement.height as f32
            ),
            "the bitmap is drawn at the size it was rasterised at, not fitted"
        );
        assert!(
            width <= p.metrics.cell_width * 2.0 + 0.01,
            "a wide glyph fits its two cells, {width} px wide in {}",
            p.metrics.cell_width * 2.0
        );
        assert!(
            (top + p.image.placement.top as f32 - p.metrics.baseline).abs() < 0.01,
            "the glyph's baseline is the grid's: top {top} + rise {} != {}",
            p.image.placement.top,
            p.metrics.baseline
        );
        assert!(top >= 0.0, "a Han glyph starts below the cell's top edge");
    }

    /// A full-width full stop is a dot, not a cell.
    ///
    /// `。` is 14 px of advance and 5 px of ink: the old wide rule scaled the
    /// *ink* to fill the two cells, so it drew a 17 × 17 square. Nothing about
    /// a wide character says its ink is as big as its advance.
    #[test]
    fn a_fullwidth_stop_is_not_a_square() {
        let Some(p) = placed('。') else { return };
        let (_, _, width, height) = p.rect;
        assert!(
            width <= p.metrics.cell_width && height <= p.metrics.cell_width,
            "a full stop is {width}x{height}, not a cell"
        );
    }

    /// A wide-character cursor moves back onto the character and covers it.
    #[test]
    fn a_block_cursor_on_a_wide_tail_covers_both_cells() {
        let cell_w = 9.0;
        let block = Cursor {
            x: 5,
            y: 0,
            shape: CursorShape::Block,
            wide_tail: true,
        };
        assert_eq!(cursor_cell(&block, cell_w), (-cell_w, 2.0 * cell_w));
        // A bar is a mark against where typing starts, so it stays one cell
        // wide and lands on the character's own left edge.
        let bar = Cursor {
            shape: CursorShape::Bar,
            ..block
        };
        assert_eq!(cursor_cell(&bar, cell_w), (-cell_w, cell_w));
        // And off a wide character there is nothing to move.
        let narrow = Cursor {
            wide_tail: false,
            ..block
        };
        assert_eq!(cursor_cell(&narrow, cell_w), (0.0, cell_w));
    }

    /// The configured list is an *opinion*, and it beats dtr's own: if it did
    /// not, a `[font] fallback` entry for a proportional CJK face would be
    /// silently overridden by the built-in monospaced preference and the key
    /// would look broken.
    #[test]
    fn a_configured_fallback_family_wins_over_the_built_in_list() {
        let mut look = Look::default();
        look.fallback = vec!["Noto Sans CJK SC".to_owned()];
        let mut fonts = Fonts::load(&look);
        let Some(index) = fonts.face_for(Face::Regular, '啊') else {
            return; // no CJK installed here
        };
        let name = fonts.names.get(index - 4).cloned().unwrap_or_default();
        assert_eq!(
            name, "Noto Sans CJK SC",
            "the configured fallback was ignored in favour of {name}"
        );
    }

    /// A fallback is rasterised at its own optical size, and for CJK that means
    /// `adjust ≈ 1.0`: a Han character is about one em tall, so a scale that
    /// differs from 1 by much is the adjustment measuring the wrong thing.
    ///
    /// This is what x-height (0.92) and cap height (0.89) got wrong before
    /// `ic_width`: CJK faces have a small x-height *by design*.
    #[test]
    fn a_cjk_fallback_is_drawn_at_its_own_size() {
        let look = Look::default();
        let mut fonts = Fonts::load(&look);
        let Some(index) = fonts.face_for(Face::Regular, '啊') else {
            return; // no CJK installed here
        };
        let adjust = fonts.faces[index].adjust;
        assert!(
            (0.9..=1.1).contains(&adjust),
            "{} is drawn at {adjust}x the grid's size",
            fonts.names[index - 4]
        );
        let glyph_id = fonts.glyph_id(index, '啊').expect("the face has the glyph");
        let image = fonts
            .rasterise(index, glyph_id, look.size)
            .expect("a raster");
        // One em of ink, within a pixel: the face's own em is what a Han
        // character fills, and the grid's point size is that em.
        let height = image.placement.height as f32;
        assert!(
            (height - look.size).abs() <= look.size * 0.1,
            "the Han glyph is {height} px tall at {} pt",
            look.size
        );
    }

    /// Two loads in one process must agree. The answer used to depend on which
    /// family fontconfig happened to list first, which is the bug M4 exists to
    /// kill; a cache that reorders between loads would bring it back.
    #[test]
    fn the_fallback_answer_is_the_same_twice() {
        let mut answers = Vec::new();
        for _ in 0..2 {
            let mut fonts = Fonts::load(&Look::default());
            answers.push((
                fonts
                    .face_for(Face::Regular, '啊')
                    .map(|index| fonts.names[index - 4].clone()),
                fonts
                    .face_for(Face::Regular, '\u{f015}')
                    .map(|index| fonts.names[index - 4].clone()),
            ));
        }
        assert_eq!(answers[0], answers[1], "two loads disagreed");
    }

    #[test]
    fn latin_text_never_leaves_the_grid_face() {
        let mut fonts = Fonts::load(&Look::default());
        assert_eq!(fonts.face_for(Face::Regular, 'M'), Some(0));
        assert_eq!(fonts.faces.len(), 4, "no fallback for plain text");
    }
}
