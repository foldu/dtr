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
) -> anyhow::Result<()> {
    let instance = crate::app::instance();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
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

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
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
        rx: stats.rx(),
        tx: stats.tx(),
        rx_rate: 0.0,
        tx_rate: 0.0,
    };

    let mut full_output = ctx.run_ui(raw_input, |ui| {
        egui::Panel::bottom("dtr-status")
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(0x14, 0x18, 0x16))
                    .inner_margin(egui::Margin::symmetric(8, 4)),
            )
            .show(ui, |ui| crate::ui::status_bar(ui, &status));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(egui::Color32::BLACK))
            .show(ui, |ui| match frame {
                Some(frame) => crate::ui::draw(ui, frame),
                None => {
                    ui.label("no frame yet");
                }
            });
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
    {
        let mut pass = encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
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
        pixels.extend_from_slice(&data[start..start + unpadded as usize]);
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
