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
    /// Keyed by (face, glyph id, size×4): the same character is a different
    /// bitmap in the bold face, and a different size is a different raster.
    glyphs: HashMap<(u32, u16, u32), Glyph>,
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
            for ch in cell.text.chars() {
                let Some(glyph) = self.glyph(device, queue, ch, style, look.size) else {
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
    fn glyph(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        ch: char,
        style: Face,
        size: f32,
    ) -> Option<Glyph> {
        let face = self.fonts.face_for(style, ch)?;
        let glyph_id = self.fonts.glyph_id(face, ch)?;
        let key = (face as u32, glyph_id, (size * 4.0) as u32);
        if let Some(glyph) = self.glyphs.get(&key) {
            return Some(*glyph);
        }

        let image = self.fonts.rasterise(face, glyph_id, size)?;
        let width = image.placement.width;
        let height = image.placement.height;
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
        let (left, top, width, height) = placement(&self.fonts.metrics, &image);
        let glyph = Glyph {
            uv: [
                x as f32 / size_px,
                y as f32 / size_px,
                (x + image.placement.width) as f32 / size_px,
                (y + image.placement.height) as f32 / size_px,
            ],
            left,
            top,
            width,
            height,
        };
        self.glyphs.insert(key, glyph);
        Some(glyph)
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
    /// Which face has each character in each style. `None` is cached too: a
    /// character nobody covers must not be searched for twice.
    resolved: HashMap<(u8, char), Option<usize>>,
    /// Families to try, discovered on the first missing character.
    candidates: Vec<fontique::FamilyId>,
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

        let context = swash::scale::ScaleContext::new();
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
                    Metrics {
                        cell_width,
                        cell_height,
                        baseline,
                    }
                }
                None => Metrics {
                    cell_width: look.size * 0.6,
                    cell_height: look.size * 1.2,
                    baseline: look.size,
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
            candidates: Vec::new(),
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
            if let Some(id) = self.glyph_id(index, ch) {
                let _ = id;
                self.resolved.insert((style as u8, ch), Some(index));
                return Some(index);
            }
        }

        if self.candidates.is_empty() {
            if let Some(collection) = self.collection.as_mut() {
                // The preference list first, then everything else: a terminal
                // wants a monospaced CJK face, so the wide characters line up
                // with the Latin grid instead of carrying their own advance.
                let mut ordered: Vec<fontique::FamilyId> = Vec::new();
                for name in crate::fonts::FALLBACK {
                    if let Some(id) = collection.family_id(name) {
                        ordered.push(id);
                    }
                }
                ordered.extend(
                    collection
                        .family_ids()
                        .filter(|id| !ordered.contains(id))
                        .collect::<Vec<_>>(),
                );
                self.candidates = ordered;
            }
        }

        let mut found = None;
        let weight = match style {
            Face::Regular | Face::Italic => fontique::FontWeight::NORMAL,
            Face::Bold | Face::BoldItalic => fontique::FontWeight::BOLD,
        };
        let slant = match style {
            Face::Regular | Face::Bold => fontique::FontStyle::Normal,
            Face::Italic | Face::BoldItalic => fontique::FontStyle::Italic,
        };
        // A fallback face is usually regular weight and upright even when the
        // grid is bold or italic; asking for the exact style would miss it.
        for (slant, weight) in [
            (slant, weight),
            (fontique::FontStyle::Normal, fontique::FontWeight::NORMAL),
        ] {
            for position in 0..self.candidates.len() {
                let Some(collection) = self.collection.as_mut() else {
                    break;
                };
                let Some(&id) = self.candidates.get(position) else {
                    break;
                };
                let Some(family) = collection.family(id) else {
                    continue;
                };
                let Some(font) =
                    family.match_font(fontique::FontWidth::NORMAL, slant, weight, false)
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
                // The face's own ascent, so `-v` shows why its glyphs sit
                // where they do relative to the grid's baseline. Past this
                // point the ascent must never move that baseline.
                let ascent = {
                    let metrics = reference.metrics(&[]);
                    let units = f32::from(metrics.units_per_em).max(1.0);
                    metrics.ascent * self.size / units
                };
                drop(bytes);
                // Reload to own the bytes: `font.load` borrows from the cache.
                let Some(bytes) = font.load(None) else {
                    continue;
                };
                let family_name = family.name().to_owned();
                self.names.push(family_name);
                self.faces.push(Loaded {
                    bytes: bytes.data().to_vec(),
                    index,
                });
                found = Some(self.faces.len() - 1);
                self.fallback_order.push(self.faces.len() - 1);
                tracing::debug!(
                    character = %ch,
                    family = self.names.last().map(String::as_str),
                    ascent,
                    "fallback face"
                );
                break;
            }
            if found.is_some() {
                break;
            }
        }

        self.resolved.insert((style as u8, ch), found);
        found
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
        let Some(image) = fonts.rasterise(face, glyph_id, look.size) else {
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
        let (left, top, width, height) = placement(&metrics, &image);
        println!(
            "U+{:04X} {ch}  face {face}  {name:<26} asc {:>6.2}  adv {:>6.2}  ink {:>3}x{:<3}  at ({:+.0},{:+.0}) {:.0}x{:.0}",
            ch as u32,
            ascent,
            advance,
            image.placement.width,
            image.placement.height,
            left,
            top,
            width,
            height,
        );
    }

    println!();
    println!(
        "fallbacks {}",
        if fonts.names.is_empty() {
            "(none loaded)".to_owned()
        } else {
            fonts.names.join(", ")
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
                let loaded = Loaded { bytes, index: 0 };
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
        }
    }
}

#[cfg(test)]
mod font_tests {
    use super::*;

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
        let image = fonts.rasterise(face, glyph_id, look.size)?;
        let rect = placement(&fonts.metrics, &image);
        Some(Placed {
            face,
            image,
            rect,
            metrics: fonts.metrics,
        })
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

    #[test]
    fn latin_text_never_leaves_the_grid_face() {
        let mut fonts = Fonts::load(&Look::default());
        assert_eq!(fonts.face_for(Face::Regular, 'M'), Some(0));
        assert_eq!(fonts.faces.len(), 4, "no fallback for plain text");
    }
}
