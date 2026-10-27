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

use crate::pane::Frame;
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
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    pub cell_width: f32,
    pub cell_height: f32,
    pub ascent: f32,
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
    glyphs: HashMap<(u8, u16, u32), Glyph>,
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
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        panes: &[PaneView<'_>],
        look: &Look,
        screen: (f32, f32),
    ) {
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
            if cell.text.trim().is_empty() || !visible(index) {
                continue;
            }
            let (x, y) = cell_origin(index, frame.cols, cell_w, cell_h);
            let (x, y) = (x + origin_x, y + origin_y);
            let style = face_for(cell.bold, cell.italic);
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
                self.instances.push(Instance::solid(
                    [x, y, cell_w, cell_h],
                    look.accent,
                    0.35,
                ));
            }
        }

        if let (true, Some((cx, cy))) = (pane.cursor, frame.cursor) {
            let (x, y) = cell_origin(
                usize::from(cy) * usize::from(frame.cols) + usize::from(cx),
                frame.cols,
                cell_w,
                cell_h,
            );
            let (x, y) = (x + origin_x, y + origin_y);
            self.instances
                .push(Instance::solid([x, y, cell_w, cell_h], look.accent, 1.0));
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
        let glyph_id = self.fonts.glyph_id(style, ch)?;
        let key = (style as u8, glyph_id, (size * 4.0) as u32);
        if let Some(glyph) = self.glyphs.get(&key) {
            return Some(*glyph);
        }

        let image = self.fonts.rasterise(style, glyph_id, size)?;
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
        let glyph = Glyph {
            uv: [
                x as f32 / size_px,
                y as f32 / size_px,
                (x + width) as f32 / size_px,
                (y + height) as f32 / size_px,
            ],
            // `placement.top` is the distance from the baseline up to the
            // bitmap's top edge; the cell origin is the top of the line, so the
            // glyph sits at ascent minus that.
            left: image.placement.left as f32,
            top: self.fonts.metrics.ascent - image.placement.top as f32,
            width: width as f32,
            height: height as f32,
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

fn bytemuck_cast<T: Copy>(slice: &[T]) -> &[u8] {
    // SAFETY: `Instance` is plain old data (f32 arrays only); this is the
    // standard transmute pattern for wgpu vertex buffers.
    unsafe {
        std::slice::from_raw_parts(
            slice.as_ptr().cast::<u8>(),
            std::mem::size_of_val(slice),
        )
    }
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
    faces: [Loaded; 4],
    context: swash::scale::ScaleContext,
    metrics: Metrics,
}

impl Fonts {
    fn load(look: &Look) -> Self {
        let faces = load_faces(look);

        let context = swash::scale::ScaleContext::new();
        let metrics = {
            let font = swash::FontRef::from_index(
                &faces[Face::Regular as usize].bytes,
                faces[Face::Regular as usize].index,
            );
            match font {
                Some(font) => {
                    let units = f32::from(font.metrics(&[]).units_per_em).max(1.0);
                    let ascent = font.metrics(&[]).ascent * look.size / units;
                    let advance = font
                        .glyph_metrics(&[])
                        .advance_width(font.charmap().map('M'))
                        * look.size
                        / units;
                    // swash reports descent as a positive distance *below* the
                    // baseline (unlike FreeType, which gives it negative), so
                    // negating it here shrinks the cell and the lines collapse
                    // into each other.
                    let descent = font.metrics(&[]).descent * look.size / units;
                    // Whole-pixel cells: a fractional advance puts every glyph
                    // on a half-pixel and the whole screen looks out of focus.
                    // Round *up*: rounding down makes the cell narrower than the
                    // glyph advance, so neighbours overlap ("llll" renders as a
                    // smear).
                    Metrics {
                        cell_width: advance.max(1.0).ceil(),
                        cell_height: (ascent + descent).max(1.0).ceil(),
                        ascent: ascent.round(),
                    }
                }
                None => Metrics {
                    cell_width: look.size * 0.6,
                    cell_height: look.size * 1.2,
                    ascent: look.size,
                },
            }
        };

        Self {
            faces,
            context,
            metrics,
        }
    }

    fn font(&self, face: Face) -> Option<swash::FontRef<'_>> {
        let loaded = &self.faces[face as usize];
        swash::FontRef::from_index(&loaded.bytes, loaded.index)
    }

    fn glyph_id(&self, face: Face, ch: char) -> Option<u16> {
        let font = self.font(face)?;
        let id = font.charmap().map(ch);
        (id != 0).then_some(id)
    }

    fn rasterise(
        &mut self,
        face: Face,
        glyph_id: u16,
        size: f32,
    ) -> Option<swash::scale::image::Image> {
        // The font borrows the face's bytes, the context is a separate field:
        // split the borrow so both can be used at once.
        let loaded = &self.faces[face as usize];
        let font = swash::FontRef::from_index(&loaded.bytes, loaded.index)?;
        let context = &mut self.context;
        let mut scaler = context
            .builder(font)
            .size(size)
            .hint(true)
            .build();
        let mut image = swash::scale::image::Image::new();
        swash::scale::Render::new(&[swash::scale::Source::Outline]).render_into(
            &mut scaler,
            glyph_id,
            &mut image,
        );
        Some(image)
    }
}

/// Load the four faces: regular, bold, italic, bold-italic.
///
/// `font.path` wins and can only supply one face, so the other three fall back
/// to it — a user who points at a single file gets a uniform grid rather than a
/// partial one. Otherwise the family is asked for each style and a missing face
/// falls back down the chain: bold-italic → italic → bold → regular, which is
/// what a terminal does when a family ships only two weights.
fn load_faces(look: &Look) -> [Loaded; 4] {
    if let Some(path) = look.path.as_ref() {
        match std::fs::read(path) {
            Ok(bytes) => {
                let loaded = Loaded { bytes, index: 0 };
                return [
                    loaded.clone_face(),
                    loaded.clone_face(),
                    loaded.clone_face(),
                    loaded,
                ];
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
        ["JetBrains Mono", "Hack", "DejaVu Sans Mono", "Liberation Mono", "Fira Code", "Ubuntu Mono", "Noto Sans Mono", "monospace"]
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
            return [
                regular,
                bold.unwrap_or_else(|| empty.clone_face()),
                italic.unwrap_or_else(|| empty.clone_face()),
                bold_italic.unwrap_or(empty),
            ];
        }
    }

    tracing::warn!("no monospace family found; the grid will draw backgrounds only");
    let empty = Loaded {
        bytes: Vec::new(),
        index: 0,
    };
    [
        empty.clone_face(),
        empty.clone_face(),
        empty.clone_face(),
        empty,
    ]
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
