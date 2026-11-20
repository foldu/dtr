//! The image layer: what dtr draws for the Kitty graphics protocol.
//!
//! The pinned VT core parses the whole protocol — transmit and display, delete,
//! PNG and raw pixels, zlib, chunking, the protocol's own queries — and keeps
//! the decoded images in a bounded storage. What it cannot do is put them on a
//! screen: that needs the cell size in pixels and a GPU, and they belong to the
//! frontend. This module is the frontend half, in two pieces that can be tested
//! apart:
//!
//!   - **Geometry** (`placement_rect`, `image_quad`, `order`): a placement from
//!     the core's render-state list, the pane's rectangle, the cell size and the
//!     framebuffer size in, one clipped quad in physical pixels with texture
//!     coordinates out. Pure functions, no `wgpu`.
//!   - **Drawing** (`ImageRenderer`): one textured-quad pipeline, one texture
//!     per (image id, generation), and a least-recently-used byte budget. The
//!     pane thread copies pixels out of the core once per generation (see
//!     `pane::ImageCache`); this side uploads them once and then draws the same
//!     texture for as long as the frame keeps naming that generation.
//!
//! ## Which coordinates come from where
//!
//! The core's list is *viewport*-relative, so scrollback needs no arithmetic
//! here: row 0 is the top row of the grid whatever the scrollback offset is, and
//! a negative row is an image that has scrolled up and must be clipped.
//! `viewport_col`/`viewport_row` are cells, `cell_offset_x`/`cell_offset_y` are
//! pixels inside the top-left cell, and `pixel_width`/`pixel_height` are the
//! size to draw at. The cell size is dtr's (`render::Metrics`, physical pixels,
//! the same one the grid is drawn at); the core measured the image against the
//! cell it was resized to, which is the pane divided by the cell count, so the
//! two can disagree by less than a pixel. That is invisible at image scale; it
//! is why the image's *destination* rectangle is computed here and never taken
//! from the core.
//!
//! ## Layers
//!
//! The core gives three: below the cell backgrounds, below the text, above the
//! text. dtr's grid pass draws backgrounds and glyphs in one sequence, so the
//! pass is split into a background range and a foreground range and the image
//! groups are drawn between them: below-background images first, then the grid's
//! backgrounds, then below-text images, then the grid's glyphs. Above-text
//! images are drawn in the following pass, before egui, so a menu is still over
//! them. Within a group they are drawn by `z`, ascending, with the image id as a
//! tie-break so the order is deterministic.
//!
//! A below-background image is drawn, but the pane's own default background is
//! opaque and covers it — that is what "below the background" means; a theme
//! with an opaque background will never show one, and dtr has no background
//! image feature for it to composite against.
//!
//! ## Colour
//!
//! The surface is a non-sRGB `Bgra8Unorm`/`Rgba8Unorm` target (egui prefers
//! one, see `Gpu::new`), so an sRGB-encoded PNG byte must reach the framebuffer
//! unchanged. The texture is `Rgba8Unorm` (not `Rgba8UnormSrgb`) and the shader
//! does no conversion: byte in, byte out. Letting wgpu decode sRGB on sample and
//! encode on write would round-trip correctly in theory, but only if the target
//! is also sRGB; mixing the two conventions is how images come out gamma-wrong.

use std::collections::HashMap;
use std::sync::Arc;

use crate::layout::Rect;
use crate::render::PaneView;

/// How much decoded-pixel memory one terminal may hold, in bytes.
///
/// The core keeps Kitty graphics **off** until a limit is set, so this is both
/// the feature switch and the bound — the same shape as the scrollback limit in
/// `pane::new_terminal`, and for the same reason: a terminal with no bound is a
/// memory leak with a schedule. 256 MiB is roughly eight 4K RGBA frames
/// (3840 × 2160 × 4 = 33 MiB each), which covers a photo viewer or a boot
/// splash several times over, and is small enough that a runaway program cannot
/// take a 4 GB machine with it. The core applies the limit to the main and the
/// alternate screen separately, so the terminal's own worst case is twice this.
pub const KITTY_IMAGE_STORAGE_LIMIT: u64 = 256 * 1024 * 1024;

/// How much GPU texture memory the image cache may hold, in bytes.
///
/// Decoded RGBA is the same size as its texture, so this is one budget's worth
/// of visible images. A cache entry that is evicted is simply uploaded again if
/// a later frame still names it; the pane's `ImageCache` keeps the pixels, so a
/// re-upload costs a queue write, not a decode. The budget bounds the cache, not
/// one image: a single image bigger than the budget is still drawn, alone.
pub const TEXTURE_BUDGET: u64 = 256 * 1024 * 1024;

/// Install the process-global PNG decoder the Kitty graphics protocol needs.
///
/// `libghostty` calls one decoder hook for every terminal in the process, and
/// it does not synchronize the hook's installation: the core's own safety note
/// says to set it once at startup, before any terminal is in use. So this is
/// called from `main` before the window exists, not lazily from a pane — a
/// second tab opened while another pane thread is inside `vt_write` is exactly
/// the race the note warns about. The `Once` makes a second call harmless.
pub fn install_png_decoder() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // SAFETY: called before any pane thread exists (`main`, before the app);
        // the decoder itself is `Send` and serialized by the core.
        let installed = unsafe {
            libghostty_vt::kitty::graphics::set_png_decoder(Some(Box::new(
                libghostty_vt::kitty::graphics::RustPngDecoder::new(),
            )))
        };
        match installed {
            Ok(()) => tracing::info!("kitty graphics: PNG decoder installed"),
            Err(err) => tracing::warn!(%err, "could not install the PNG decoder: PNG images will be refused"),
        }
    });
}

/// Decoded pixels of one image, in the layout a texture wants.
///
/// Always 8-bit RGBA, whatever the protocol sent: the raw-pixel paths allow
/// 24-bit RGB and grayscale, and a PNG is decoded to RGBA by the core's decoder
/// hook. The expansion happens once per generation, on the pane thread, which
/// is why the drawing side never has to know a format.
#[derive(Debug)]
pub struct ImagePixels {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, row-major, no padding.
    pub data: Vec<u8>,
}

/// Where an image is drawn relative to the text, as `render::ImageLayer` says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    BelowBackground,
    BelowText,
    AboveText,
}

/// One image placement to draw: what, where, how big, and the pixels.
///
/// This is what a pane publishes in its [`crate::pane::Frame`]. The pixels are
/// an `Arc` so a frame costs a refcount, not an image; the pane replaces the
/// `Arc` only when the core says the image's generation changed.
#[derive(Clone, Debug)]
pub struct Placement {
    /// The Kitty image id, unique within the protocol. Part of the cache key.
    pub image_id: u32,
    /// The image's pixel generation, part of the cache key. It changes when the
    /// program re-sends the image, and — if animations are ever driven — on
    /// every frame.
    pub generation: u64,
    pub layer: Layer,
    pub z: i32,
    /// The top-left cell, viewport-relative. Negative when clipped at the edge.
    pub col: i32,
    pub row: i32,
    /// Pixel offset inside that cell.
    pub offset: [u32; 2],
    /// The size to draw at, in physical pixels.
    pub size: [u32; 2],
    /// The part of the image to draw, in image pixels: x, y, width, height.
    pub source: [u32; 4],
    pub pixels: Arc<ImagePixels>,
}

/// The rectangle a placement covers in physical pixels, before clipping.
///
/// The pane's rectangle is the grid's origin: with no overscan row there is
/// nothing above row 0 inside the pane, so (0, 0) in viewport cells is the
/// pane's top-left pixel. `cell` is the drawn cell in physical pixels.
pub fn placement_rect(pane: Rect, cell: (f32, f32), placement: &Placement) -> Rect {
    Rect::new(
        pane.x + placement.col as f32 * cell.0 + placement.offset[0] as f32,
        pane.y + placement.row as f32 * cell.1 + placement.offset[1] as f32,
        placement.size[0] as f32,
        placement.size[1] as f32,
    )
}

/// The overlap of two rectangles, or `None` when they do not touch.
pub fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.width).min(b.x + b.width);
    let y1 = (a.y + a.height).min(b.y + b.height);
    (x1 > x0 && y1 > y0).then(|| Rect::new(x0, y0, x1 - x0, y1 - y0))
}

/// One clipped quad in physical pixels plus its texture coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quad {
    pub rect: Rect,
    /// u0, v0, u1, v1 into the image texture.
    pub uv: [f32; 4],
}

/// The quad to draw for one placement, clipped to its pane and the framebuffer.
///
/// Clipping happens by shrinking the quad and moving the texture coordinates
/// with it, not with a scissor: one image can be half outside its pane and half
/// outside the window, and a quad built from the clipped rectangle is the same
/// arithmetic in both tests and the draw.
///
/// The texture coordinates are normalised against the *image* size, not the
/// texture size: when an image is too large for the device it is downscaled at
/// upload time by the same factor in both axes, so the source rectangle maps to
/// the same fraction of the texture either way.
pub fn image_quad(
    pane: Rect,
    cell: (f32, f32),
    screen: (f32, f32),
    placement: &Placement,
) -> Option<Quad> {
    let image = &placement.pixels;
    if image.width == 0 || image.height == 0 {
        return None;
    }
    let full = placement_rect(pane, cell, placement);
    if full.width <= 0.0 || full.height <= 0.0 {
        return None;
    }
    // The pane is the clip *and* the framebuffer; the window is only needed for
    // a pane rectangle that overhangs it (the layout never makes one, but a
    // capture with a stale frame can).
    let target = Rect::new(0.0, 0.0, screen.0.max(0.0), screen.1.max(0.0));
    let clipped = intersect(intersect(full, pane)?, target)?;

    // Inside the drawn rectangle, dest pixel (x, y) samples source pixel
    // (source.x + (x - full.x) * source.width / full.width, ...). The clipped
    // rectangle is a sub-rectangle of that mapping.
    let source = placement.source;
    let u = |dx: f32| -> f32 {
        let scaled = source[0] as f32 + dx * source[2] as f32 / full.width;
        (scaled / image.width as f32).clamp(0.0, 1.0)
    };
    let v = |dy: f32| -> f32 {
        let scaled = source[1] as f32 + dy * source[3] as f32 / full.height;
        (scaled / image.height as f32).clamp(0.0, 1.0)
    };
    Some(Quad {
        rect: clipped,
        uv: [
            u(clipped.x - full.x),
            v(clipped.y - full.y),
            u(clipped.x + clipped.width - full.x),
            v(clipped.y + clipped.height - full.y),
        ],
    })
}

/// The order to draw these placements in: layer, then z, then image id.
///
/// The core does not promise an order for its list («the order of placements is
/// not the order to draw them in»), so this is the one place the order is
/// decided, and `ImageRenderer::prepare` iterates through it rather than keeping
/// a second copy of the policy. The image id tie-break is not protocol-visible;
/// it makes two placements with equal z draw the same way on every frame.
pub fn order(placements: &[Placement]) -> Vec<usize> {
    let mut indices: Vec<usize> = (0..placements.len()).collect();
    indices.sort_by(|a, b| {
        let (a, b) = (&placements[*a], &placements[*b]);
        (a.layer, a.z, a.image_id).cmp(&(b.layer, b.z, b.image_id))
    });
    indices
}

/// Which animation frame of an image a texture belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextureKey {
    pub image_id: u32,
    pub generation: u64,
}

/// A byte-budgeted least-recently-used cache.
///
/// Generic over what a cache entry is so the policy — keying, touching and
/// eviction — is tested without a GPU; the renderer instantiates it with a
/// texture and its bind group. "Used" is per prepared frame: every entry a frame
/// names is touched, and when an insert would go over the budget the entries
/// touched longest ago go first.
#[derive(Debug)]
pub struct Cache<V> {
    entries: HashMap<TextureKey, Entry<V>>,
    budget: u64,
    bytes: u64,
    /// A counter rather than a clock: the only thing that matters is the order
    /// of touches, and a frame's touches must not all compare equal.
    tick: u64,
}

#[derive(Debug)]
struct Entry<V> {
    value: V,
    bytes: u64,
    last_used: u64,
}

impl<V> Cache<V> {
    pub fn new(budget: u64) -> Self {
        Self {
            entries: HashMap::new(),
            budget,
            bytes: 0,
            tick: 0,
        }
    }

    pub fn get(&self, key: TextureKey) -> Option<&V> {
        self.entries.get(&key).map(|entry| &entry.value)
    }

    pub fn contains(&self, key: TextureKey) -> bool {
        self.entries.contains_key(&key)
    }

    /// Mark an entry as used by the frame being prepared.
    pub fn touch(&mut self, key: TextureKey) {
        self.tick += 1;
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.last_used = self.tick;
        }
    }

    /// Insert `value`, evicting least-recently-used entries to stay within the
    /// budget. Returns the keys it dropped, for logging.
    ///
    /// An entry larger than the whole budget is still inserted — refusing to
    /// draw the image the user asked for would be worse than the cache being
    /// briefly over its bound — and every other entry is dropped for it.
    pub fn insert(&mut self, key: TextureKey, value: V, bytes: u64) -> Vec<TextureKey> {
        let mut evicted = Vec::new();
        if let Some(previous) = self.entries.remove(&key) {
            self.bytes = self.bytes.saturating_sub(previous.bytes);
        }
        self.tick += 1;
        let needed = self.bytes.saturating_add(bytes).saturating_sub(self.budget);
        if needed > 0 {
            // Least recently used first; the new entry has the highest tick and
            // is not in the map yet, so it cannot evict itself.
            let mut oldest: Vec<(u64, TextureKey)> = self
                .entries
                .iter()
                .map(|(key, entry)| (entry.last_used, *key))
                .collect();
            oldest.sort_unstable_by_key(|(used, _)| *used);
            let mut freed = 0u64;
            for (_, key) in oldest {
                if freed >= needed {
                    break;
                }
                if let Some(entry) = self.entries.remove(&key) {
                    freed = freed.saturating_add(entry.bytes);
                    self.bytes = self.bytes.saturating_sub(entry.bytes);
                    evicted.push(key);
                }
            }
        }
        self.entries.insert(
            key,
            Entry {
                value,
                bytes,
                last_used: self.tick,
            },
        );
        self.bytes = self.bytes.saturating_add(bytes);
        evicted
    }

    /// Drop every entry: a new device or a new surface format forgets textures.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

// --------------------------------------------------------------------------
// Geometry tests: no GPU, no window, no fonts.
// --------------------------------------------------------------------------

#[cfg(test)]
mod geometry_tests {
    use super::*;

    fn pixels(width: u32, height: u32) -> Arc<ImagePixels> {
        Arc::new(ImagePixels {
            width,
            height,
            data: vec![0; (width * height * 4) as usize],
        })
    }

    fn placement() -> Placement {
        Placement {
            image_id: 7,
            generation: 3,
            layer: Layer::AboveText,
            z: 0,
            col: 2,
            row: 1,
            offset: [3, 4],
            size: [10, 12],
            source: [0, 0, 10, 12],
            pixels: pixels(10, 12),
        }
    }

    fn pane() -> Rect {
        Rect::new(100.0, 50.0, 200.0, 100.0)
    }

    #[test]
    fn a_placement_starts_at_its_cell_plus_the_offset() {
        let rect = placement_rect(pane(), (8.0, 16.0), &placement());
        assert_eq!(rect, Rect::new(119.0, 70.0, 10.0, 12.0));
    }

    #[test]
    fn a_scrolled_up_image_is_clipped_to_the_pane() {
        let mut p = placement();
        p.row = -1;
        p.size = [16, 32];
        p.pixels = pixels(16, 32);
        p.source = [0, 0, 16, 32];
        let rect = placement_rect(pane(), (8.0, 16.0), &p);
        assert_eq!(rect.y, 38.0);
        let quad = image_quad(pane(), (8.0, 16.0), (400.0, 300.0), &p).unwrap();
        assert_eq!(quad.rect.y, 50.0);
        // The image starts 12 px above the pane, so 20 of its 32 rows show.
        assert_eq!(quad.rect.height, 20.0);
        assert_eq!(quad.uv[1], 12.0 / 32.0);
        assert_eq!(quad.uv[3], 1.0);
    }

    #[test]
    fn one_panes_image_cannot_spill_into_its_neighbour() {
        let left = Rect::new(0.0, 0.0, 100.0, 100.0);
        let mut p = placement();
        p.col = 11; // 88 px into the pane, then clipped at 100
        p.size = [32, 12];
        p.pixels = pixels(32, 12);
        p.source = [0, 0, 32, 12];
        let quad = image_quad(left, (8.0, 16.0), (400.0, 300.0), &p).unwrap();
        assert_eq!(quad.rect.x, 91.0);
        assert_eq!(quad.rect.width, 9.0);
        assert_eq!(quad.uv[0], 0.0);
        assert_eq!(quad.uv[2], 9.0 / 32.0);
    }

    #[test]
    fn an_image_entirely_outside_its_pane_draws_nothing() {
        let mut p = placement();
        p.col = 100;
        assert_eq!(image_quad(pane(), (8.0, 16.0), (400.0, 300.0), &p), None);
    }

    #[test]
    fn a_source_rect_maps_to_its_share_of_the_texture() {
        let mut p = placement();
        p.pixels = pixels(20, 20);
        p.source = [10, 5, 10, 10];
        p.size = [10, 10];
        let quad = image_quad(pane(), (8.0, 16.0), (400.0, 300.0), &p).unwrap();
        assert_eq!(quad.uv, [0.5, 0.25, 1.0, 0.75]);
    }

    #[test]
    fn a_full_image_maps_to_the_whole_texture() {
        let quad = image_quad(pane(), (8.0, 16.0), (400.0, 300.0), &placement()).unwrap();
        assert_eq!(quad.uv, [0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn draw_order_is_layer_then_z_then_image_id() {
        let p = |layer, z, image_id| Placement {
            layer,
            z,
            image_id,
            ..placement()
        };
        let list = vec![
            p(Layer::AboveText, 1, 4),
            p(Layer::BelowText, 5, 3),
            p(Layer::AboveText, -2, 9),
            p(Layer::BelowBackground, 0, 1),
            p(Layer::BelowText, 5, 2),
        ];
        let ordered: Vec<usize> = order(&list);
        assert_eq!(ordered, vec![3, 4, 1, 2, 0]);
    }
}

// --------------------------------------------------------------------------
// Cache tests: a fake value, a small budget.
// --------------------------------------------------------------------------

#[cfg(test)]
mod cache_tests {
    use super::*;

    fn key(image_id: u32, generation: u64) -> TextureKey {
        TextureKey {
            image_id,
            generation,
        }
    }

    #[test]
    fn a_generation_is_reused_until_a_new_one_arrives() {
        let mut cache = Cache::new(100);
        let k = key(1, 5);
        assert!(cache.get(k).is_none());
        cache.insert(k, "first", 10);
        assert_eq!(cache.get(k), Some(&"first"));
        // The same generation is a hit: no second upload.
        assert!(cache.contains(k));
        // A new generation is a different texture.
        let newer = key(1, 6);
        assert!(cache.get(newer).is_none());
        cache.insert(newer, "second", 10);
        assert_eq!(cache.get(newer), Some(&"second"));
        assert_eq!(cache.bytes(), 20);
    }

    #[test]
    fn eviction_takes_the_least_recently_used() {
        let mut cache = Cache::new(100);
        let (a, b, c) = (key(1, 1), key(2, 1), key(3, 1));
        cache.insert(a, "a", 40);
        cache.insert(b, "b", 40);
        cache.touch(a); // a is now newer than b
        let evicted = cache.insert(c, "c", 40);
        // 120 > 100, so one entry goes: b, the one not touched.
        assert_eq!(evicted, vec![b]);
        assert_eq!(cache.get(a), Some(&"a"));
        assert_eq!(cache.get(b), None);
        assert_eq!(cache.get(c), Some(&"c"));
        assert_eq!(cache.bytes(), 80);
    }

    #[test]
    fn an_entry_that_evicts_itself_is_uploaded_and_drawn_alone() {
        // Bigger than the whole budget: still inserted, everything else gone.
        let mut cache = Cache::new(100);
        cache.insert(key(1, 1), "small", 40);
        let evicted = cache.insert(key(2, 1), "huge", 500);
        assert_eq!(evicted, vec![key(1, 1)]);
        assert_eq!(cache.get(key(2, 1)), Some(&"huge"));
        assert_eq!(cache.bytes(), 500);
    }

    #[test]
    fn reinserting_a_key_replaces_its_bytes() {
        let mut cache = Cache::new(100);
        let k = key(1, 1);
        cache.insert(k, "a", 60);
        cache.insert(k, "b", 60);
        assert_eq!(cache.bytes(), 60);
        assert_eq!(cache.get(k), Some(&"b"));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn a_dropped_texture_is_uploaded_again_on_the_next_frame() {
        let mut cache = Cache::new(100);
        let k = key(1, 1);
        cache.insert(k, "a", 80);
        cache.insert(key(2, 1), "b", 80); // evicts a
        assert_eq!(cache.get(k), None);
        // The renderer asks again with the same key and creates a new texture;
        // the pane still holds the pixels, so no decode is involved.
        cache.insert(k, "a2", 80);
        assert_eq!(cache.get(k), Some(&"a2"));
    }
}

// --------------------------------------------------------------------------
// The drawing side.
// --------------------------------------------------------------------------

/// One textured quad pipeline, one texture per (image id, generation).
///
/// It is owned by `render::GridRenderer`, which keeps it before the device in
/// `Gpu`'s field order — a texture dropped after its device is a segfault on
/// close, not an error.
pub struct ImageRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// Six vertices per image, drawn with one bind group each. Six vertices is
    /// a whole quad: small enough that an instance buffer and a range per image
    /// would be more bookkeeping than the vertices themselves.
    vertices: Vec<Vertex>,
    vertex_buffer: Option<wgpu::Buffer>,
    vertex_capacity: usize,
    /// Sorted by layer, z and image id; `base` is the first of its six vertices.
    draws: Vec<Draw>,
    textures: Cache<Texture>,
    /// The texture format to upload in, chosen to match the surface: non-sRGB
    /// for the usual target (the bytes are already what the screen wants),
    /// sRGB when the backend only offers an sRGB target — where sampling must
    /// decode and the target must re-encode, which the hardware does for free.
    texture_format: wgpu::TextureFormat,
    /// The device's limit, in pixels a side. An image larger than this is
    /// downscaled once at upload rather than not drawn.
    max_dimension: u32,
}

#[derive(Clone, Copy)]
struct Vertex {
    /// Clip-space position: x right, y up, both -1..1.
    ///
    /// The fields are read by `wgpu` through `as_bytes`, which the compiler
    /// cannot see; keep the allow to say that deliberately.
    #[allow(dead_code)]
    pos: [f32; 2],
    #[allow(dead_code)]
    uv: [f32; 2],
}

struct Draw {
    layer: Layer,
    key: TextureKey,
    base: u32,
}

struct Texture {
    /// Kept alive with the bind group; the bytes are the cache's accounting.
    _texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

impl ImageRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dtr image"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        // Images are sampled with linear filtering: a bitmap
                        // drawn at its own size never picks a texel it should
                        // not, and a scaled one looks like a photo instead of
                        // like a checkerboard.
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("dtr image sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            // A source rect at the image's edge must not wrap into the pixels
            // on the other side.
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dtr image"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("dtr image"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("dtr image"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2, // position
                        1 => Float32x2, // uv
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
        Self {
            pipeline,
            bind_group_layout,
            sampler,
            vertices: Vec::new(),
            vertex_buffer: None,
            vertex_capacity: 0,
            draws: Vec::new(),
            textures: Cache::new(TEXTURE_BUDGET),
            texture_format: if format.is_srgb() {
                wgpu::TextureFormat::Rgba8UnormSrgb
            } else {
                wgpu::TextureFormat::Rgba8Unorm
            },
            max_dimension: device.limits().max_texture_dimension_2d.max(1),
        }
    }

    /// Whether the last `prepare` found anything to draw.
    pub fn is_empty(&self) -> bool {
        self.draws.is_empty()
    }

    /// How many images the last `prepare` queued, for diagnostics.
    pub fn len(&self) -> usize {
        self.draws.len()
    }

    /// Upload any new textures and build this frame's quads for every pane.
    ///
    /// Qualified images are clipped here — to the pane and to the framebuffer —
    /// so the draw needs no scissor state, and a placement with nothing visible
    /// costs no vertex and no draw.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        panes: &[PaneView<'_>],
        cell: (f32, f32),
        screen: (f32, f32),
    ) {
        self.vertices.clear();
        self.draws.clear();
        for pane in panes {
            let Some(frame) = pane.frame else { continue };
            if frame.images.is_empty() {
                continue;
            }
            for index in order(&frame.images) {
                let placement = &frame.images[index];
                let Some(quad) = image_quad(pane.rect, cell, screen, placement) else {
                    continue;
                };
                let Some(key) = self.texture(device, queue, placement) else {
                    continue;
                };
                let base = u32::try_from(self.vertices.len()).unwrap_or(u32::MAX);
                push_quad(&mut self.vertices, quad, screen);
                self.draws.push(Draw {
                    layer: placement.layer,
                    key,
                    base,
                });
            }
        }
        self.upload_vertices(device, queue);
        if !self.draws.is_empty() {
            tracing::debug!(
                images = self.draws.len(),
                textures = self.textures.len(),
                texture_bytes = self.textures.bytes(),
                "images prepared"
            );
        }
    }

    /// Draw one layer's images into `pass`. A pass that draws none sets no state.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, layer: Layer) {
        if self.draws.iter().all(|draw| draw.layer != layer) {
            return;
        }
        let Some(buffer) = self.vertex_buffer.as_ref() else {
            return;
        };
        pass.set_pipeline(&self.pipeline);
        pass.set_vertex_buffer(0, buffer.slice(..));
        for draw in self.draws.iter().filter(|draw| draw.layer == layer) {
            let Some(texture) = self.textures.get(draw.key) else {
                continue;
            };
            pass.set_bind_group(0, &texture.bind_group, &[]);
            pass.draw(draw.base..draw.base + 6, 0..1);
        }
    }

    /// Look up (and upload, on a miss) the texture for one placement.
    fn texture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        placement: &Placement,
    ) -> Option<TextureKey> {
        let key = TextureKey {
            image_id: placement.image_id,
            generation: placement.generation,
        };
        if self.textures.contains(key) {
            self.textures.touch(key);
            return Some(key);
        }
        let pixels = &placement.pixels;
        let (width, height, data) = fit_to_device(pixels, self.max_dimension)?;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dtr image"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // The format follows the target (see `texture_format`): a non-sRGB
            // surface wants the PNG's bytes unchanged, an sRGB surface wants
            // them decoded on sample and re-encoded on write.
            format: self.texture_format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dtr image"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        let bytes = u64::from(width) * u64::from(height) * 4;
        let evicted = self.textures.insert(
            key,
            Texture {
                _texture: texture,
                bind_group,
            },
            bytes,
        );
        if !evicted.is_empty() {
            tracing::debug!(count = evicted.len(), "image textures evicted");
        }
        Some(key)
    }

    fn upload_vertices(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.vertices.is_empty() {
            return;
        }
        if self.vertex_capacity < self.vertices.len() {
            self.vertex_capacity = self.vertices.len().next_power_of_two().max(64);
            self.vertex_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("dtr image vertices"),
                size: (self.vertex_capacity * std::mem::size_of::<Vertex>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        if let Some(buffer) = self.vertex_buffer.as_ref() {
            queue.write_buffer(buffer, 0, as_bytes(&self.vertices));
        }
    }
}

/// Turn a clipped quad into six vertices in clip space.
fn push_quad(vertices: &mut Vec<Vertex>, quad: Quad, screen: (f32, f32)) {
    let (w, h) = (screen.0.max(1.0), screen.1.max(1.0));
    let x0 = quad.rect.x / w * 2.0 - 1.0;
    let x1 = (quad.rect.x + quad.rect.width) / w * 2.0 - 1.0;
    // The framebuffer's y grows down and clip space's grows up.
    let y0 = 1.0 - quad.rect.y / h * 2.0;
    let y1 = 1.0 - (quad.rect.y + quad.rect.height) / h * 2.0;
    let [u0, v0, u1, v1] = quad.uv;
    let corners = [
        ([x0, y0], [u0, v0]),
        ([x1, y0], [u1, v0]),
        ([x1, y1], [u1, v1]),
        ([x0, y0], [u0, v0]),
        ([x1, y1], [u1, v1]),
        ([x0, y1], [u0, v1]),
    ];
    vertices.extend(corners.into_iter().map(|(pos, uv)| Vertex { pos, uv }));
}

/// The pixels to upload, downscaled when either side is over the device limit.
///
/// The destination size is fixed by the placement, so this only makes the
/// *texture* smaller; the quad is still the size the protocol asked for, and the
/// texture coordinates are normalised against the original image size, so the
/// mapping does not change. A width or height that is still zero is refused by
/// the caller before it gets here.
fn fit_to_device(pixels: &ImagePixels, max: u32) -> Option<(u32, u32, std::borrow::Cow<'_, [u8]>)> {
    let (width, height) = (pixels.width, pixels.height);
    if width == 0 || height == 0 {
        return None;
    }
    if width <= max && height <= max {
        return Some((width, height, std::borrow::Cow::Borrowed(&pixels.data)));
    }
    let scale = max as f32 / width.max(height) as f32;
    let new_width = ((width as f32 * scale).round() as u32).clamp(1, max);
    let new_height = ((height as f32 * scale).round() as u32).clamp(1, max);
    tracing::info!(
        width,
        height,
        new_width,
        new_height,
        limit = max,
        "image over the device limit; downscaled for the texture"
    );
    let image = image::RgbaImage::from_raw(width, height, pixels.data.clone())?;
    let resized = image::imageops::resize(
        &image,
        new_width,
        new_height,
        image::imageops::FilterType::Triangle,
    );
    Some((
        new_width,
        new_height,
        std::borrow::Cow::Owned(resized.into_raw()),
    ))
}

/// The vertex bytes for a slice of our own plain-old-data vertices.
fn as_bytes<T: Copy>(slice: &[T]) -> &[u8] {
    // SAFETY: `Vertex` is two `f32` arrays with no padding and no drop glue, and
    // the caller hands it straight to `queue.write_buffer`.
    unsafe { std::slice::from_raw_parts(slice.as_ptr().cast::<u8>(), std::mem::size_of_val(slice)) }
}

const SHADER: &str = r#"
struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@group(0) @binding(0) var image_texture: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;

@vertex
fn vs(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip = vec4<f32>(in.position, 0.0, 1.0);
    out.uv = in.uv;
    return out;
}

// No colour conversion: the texture and the surface are both non-sRGB, so the
// PNG's bytes are already the sRGB values the framebuffer wants.
@fragment
fn fs(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(image_texture, image_sampler, in.uv);
}
"#;

#[cfg(test)]
mod upload_tests {
    use super::*;

    #[test]
    fn an_image_that_fits_is_uploaded_as_it_is() {
        let pixels = ImagePixels {
            width: 4,
            height: 3,
            data: vec![7; 4 * 3 * 4],
        };
        let (width, height, data) = fit_to_device(&pixels, 16).unwrap();
        assert_eq!((width, height), (4, 3));
        assert!(matches!(data, std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn an_image_over_the_device_limit_is_downscaled_in_proportion() {
        let pixels = ImagePixels {
            width: 8,
            height: 4,
            data: vec![255; 8 * 4 * 4],
        };
        let (width, height, data) = fit_to_device(&pixels, 4).unwrap();
        assert_eq!((width, height), (4, 2));
        // The bytes are RGBA at the new size.
        assert_eq!(data.len(), 4 * 2 * 4);
    }
}
