//! In-app, offscreen frame capture.
//!
//! `dtr --screenshot out.png` renders the same egui frame the window would show
//! straight into a texture and reads it back. No compositor, no display server,
//! no unlocked session and no external screenshot tool: this is the only way the
//! UI gets reviewed (PLAN §4).

use std::fs::File;
use std::path::Path;

use crate::pane::Frame;

pub fn capture(
    frame: Option<&Frame>,
    path: &Path,
    width: u32,
    height: u32,
    label: &str,
    stats: &crate::pane::Stats,
    show_palette: bool,
    serial: bool,
    look: &crate::ui::Look,
    selection: Option<(usize, usize)>,
    split: Option<crate::layout::Axis>,
    files: Option<&crate::explorer::Explorer>,
) -> anyhow::Result<()> {
    let instance = crate::app::instance();
    // Same preference as the window: a capture from a different adapter than
    // the one the window would use is a capture of a different renderer, and
    // anti-aliasing and colour handling really do differ between RADV and
    // llvmpipe.
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))?;
    tracing::info!(adapter = %adapter.get_info().name, "offscreen adapter");

    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("dtr-screenshot"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
    }))?;

    // Deliberately the format the *window* gets on this platform: Bgra8Unorm
    // is what a swapchain prefers, and capturing in a different format would
    // leave the one path nobody can screenshot untested. The bytes are swapped
    // back to RGBA on readback.
    let format = wgpu::TextureFormat::Bgra8Unorm;
    let extent = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dtr-frame"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    let ctx = egui::Context::default();
    look.install(&ctx);
    ctx.set_visuals(egui::Visuals::dark());
    let mut renderer =
        egui_wgpu::Renderer::new(&device, format, egui_wgpu::RendererOptions::default());

    let raw_input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(width as f32, height as f32),
        )),
        ..Default::default()
    };

    // Totals are real; a single frame has no rate to report.
    let status = crate::ui::Status {
        label,
        tabs: (1, 1),
        rx: stats.rx(),
        tx: stats.tx(),
        rx_rate: 0.0,
        tx_rate: 0.0,
        connected: stats.connected(),
        scrollback: stats.scrollback(),
        modem: serial.then(|| (stats.dtr(), stats.rts())),
    };

    // The layout is decided before the egui pass because the file pane's
    // rectangle comes from the same split tree the grid uses.
    let mut grid = crate::render::GridRenderer::new(&device, format, look);
    let status_height = crate::ui::STATUS_HEIGHT;
    let area = crate::layout::Rect::new(
        0.0,
        0.0,
        width as f32,
        (height as f32 - status_height).max(1.0),
    );
    let mut layout = crate::layout::Layout::leaf(1);
    let (panes, file_rect): (Vec<crate::render::PaneView<'_>>, _) = if files.is_some() {
        // A real listing next to the terminal: the left-hand pane keeps the
        // grid, the right-hand one is painted by egui.
        let _ = layout.split(1, crate::layout::Axis::Horizontal, 2, false);
        let rects = layout.rects(area, crate::ui::SPLIT_GAP);
        let file_rect = rects
            .iter()
            .find(|(id, _)| *id == 2)
            .map(|(_, rect)| *rect);
        let panes = rects
            .into_iter()
            .map(|(id, rect)| crate::render::PaneView {
                rect,
                frame: if id == 1 { frame } else { None },
                selection: if id == 1 { selection } else { None },
                cursor: id == 1,
            })
            .collect();
        (panes, file_rect)
    } else {
        match split {
            // The same frame twice: this is a review of the layout and the
            // renderer's per-pane clipping, not of two live sessions.
            Some(axis) => {
                let _ = layout.split(1, axis, 2, false);
                (
                    layout
                        .rects(area, crate::ui::SPLIT_GAP)
                        .into_iter()
                        .map(|(id, rect)| crate::render::PaneView {
                            rect,
                            frame,
                            selection: if id == 1 { selection } else { None },
                            cursor: id == 1,
                        })
                        .collect(),
                    None,
                )
            }
            None => (
                vec![crate::render::PaneView {
                    rect: area,
                    frame,
                    selection,
                    cursor: true,
                }],
                None,
            ),
        }
    };

    let mut palette = crate::palette::Palette::new(show_palette);
    let mut full_output = ctx.run_ui(raw_input, |ui| {
        egui::Panel::bottom("dtr-status")
            .frame(
                egui::Frame::new()
                    .fill(crate::ui::Look::colour(look.surface))
                    .inner_margin(egui::Margin::symmetric(8, 4)),
            )
            .show(ui, |ui| crate::ui::status_bar(ui, &status, look));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(egui::Color32::TRANSPARENT))
            .frame(egui::Frame::new().fill(egui::Color32::TRANSPARENT))
            .show(ui, |_ui| {});
        if let (Some(explorer), Some(rect)) = (files, file_rect) {
            crate::explorer::pane_ui(
                ui,
                egui::Id::unique("dtr-screenshot-files"),
                rect,
                explorer,
                look,
                true,
            );
        }
        if show_palette {
            crate::ui::overlay(ui, "dtr-screenshot-palette", |ui| {
                palette.ui(ui, look);
            });
        }
    });

    let pixels_per_point = full_output.pixels_per_point;
    let clipped = ctx.tessellate(full_output.shapes, pixels_per_point);
    for (id, deltas) in &full_output.textures_delta.set {
        for delta in deltas {
            renderer.update_texture(&device, &queue, *id, delta);
        }
    }

    let screen = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [width, height],
        pixels_per_point,
    };

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("dtr-screenshot"),
    });
    let user_cmds = renderer.update_buffers(&device, &queue, &mut encoder, &clipped, &screen);

    device.on_uncaptured_error(std::sync::Arc::new(|err: wgpu::Error| {
        tracing::error!(%err, "wgpu validation");
    }));
    grid.prepare(
        &device,
        &queue,
        &panes,
        look,
        (width as f32, height as f32),
    );
    {
        let mut pass = encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("grid"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from(look.background[0]) / 255.0,
                            g: f64::from(look.background[1]) / 255.0,
                            b: f64::from(look.background[2]) / 255.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            })
            .forget_lifetime();
        grid.draw(&mut pass);
    }
    {
        let mut pass = encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            })
            .forget_lifetime();
        renderer.render(&mut pass, &clipped, &screen);
    }

    // Readback needs 256-byte row alignment, so copy into a padded buffer.
    let unpadded = width * 4;
    let padded = unpadded.div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("dtr-readback"),
        size: u64::from(padded) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        extent,
    );

    queue.submit(user_cmds.into_iter().chain(std::iter::once(encoder.finish())));

    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |result| {
        if let Err(err) = result {
            tracing::error!(%err, "map readback buffer");
        }
    });
    device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: None,
    })?;

    let data = slice.get_mapped_range()?;
    let mut pixels = Vec::with_capacity((unpadded * height) as usize);
    for row in 0..height {
        let start = (row * padded) as usize;
        let row = &data[start..start + unpadded as usize];
        // BGRA on the GPU, RGBA in the file.
        for texel in row.chunks_exact(4) {
            pixels.extend_from_slice(&[texel[2], texel[1], texel[0], texel[3]]);
        }
    }
    drop(data);
    buffer.unmap();

    full_output.textures_delta.clear();

    let file = File::create(path)?;
    let mut writer = std::io::BufWriter::new(file);
    let mut png = png::Encoder::new(&mut writer, width, height);
    png.set_color(png::ColorType::Rgba);
    png.set_depth(png::BitDepth::Eight);
    let mut png = png.write_header()?;
    png.write_image_data(&pixels)?;
    png.finish()?;

    tracing::info!(path = %path.display(), width, height, "wrote screenshot");
    Ok(())
}
