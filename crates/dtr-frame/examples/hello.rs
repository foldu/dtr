//! The smallest standalone consumer of `dtr-frame`.
//!
//! Run it with `cargo run -p dtr-frame --example hello` (a display or Xvfb is
//! needed). It opens one window, draws an egui label over an empty scene, and
//! closes after three seconds or when the window is closed — the whole public
//! surface a second frontend (`dtr-portal`) needs, in one file.
//!
//! `docs/frame.md` explains the shapes; the comments here are the parts of that
//! guide that are easiest to get wrong.

use std::time::{Duration, Instant};

use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;

/// The event type workers can send back through [`dtr_frame::FrameLoop::proxy`].
#[derive(Debug)]
struct WorkerEvent;

struct Hello {
    started: Instant,
    frames_drawn: u64,
}

/// The underlay: this example has none, so it clears to the skin's background
/// and draws nothing. `dtr` implements the same trait with its terminal grid.
struct Empty;

impl dtr_frame::Scene for Empty {
    fn clear_color(&self) -> wgpu::Color {
        wgpu::Color {
            r: 0.10,
            g: 0.10,
            b: 0.12,
            a: 1.0,
        }
    }

    fn prepare(&mut self, _device: &wgpu::Device, _queue: &wgpu::Queue, _size: (u32, u32)) {}

    fn draw(&mut self, _pass: &mut wgpu::RenderPass<'_>) {}

    fn draw_above(&mut self, _pass: &mut wgpu::RenderPass<'_>) {}
}

impl dtr_frame::App for Hello {
    type Event = WorkerEvent;

    fn resumed(&mut self, event_loop: &ActiveEventLoop, windows: &mut dtr_frame::Windows) {
        // `resumed` is called again after a suspend; a window already open is
        // the whole answer.
        if !windows.is_empty() {
            return;
        }
        let frame = match windows.open(
            event_loop,
            dtr_frame::WindowOptions {
                title: "dtr-frame hello".to_owned(),
                inner_size: (640.0, 400.0),
                app_id: Some(("dtr-frame-hello".to_owned(), "dtr-frame-hello".to_owned())),
                ..dtr_frame::WindowOptions::default()
            },
        ) {
            Ok(frame) => frame,
            Err(err) => {
                eprintln!("could not open a window: {err}");
                event_loop.exit();
                return;
            }
        };
        frame.request_redraw();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        frame: &mut dtr_frame::Frame,
        event: WindowEvent,
    ) {
        // Feed every event to egui first. `feed_event` drops `RedrawRequested`
        // itself, so this cannot reintroduce the refresh-rate spin. egui asks
        // for a frame on a click but not on a hover, so a pointer move is worth
        // one too.
        let egui_wants = frame.feed_event(&event);
        let pointer = matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
        );
        if egui_wants || pointer {
            frame.request_redraw();
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                // Recorded, not applied: the frame configures the swapchain once,
                // at the top of the frame that draws the new size.
                frame.resize(size);
                frame.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                let scale = frame.scale();
                let (width, height) = frame.size();
                let mut scene = Empty;
                let _ = frame.render(
                    &mut scene,
                    dtr_frame::RenderOptions {
                        frames_drawn: self.frames_drawn,
                        started: self.started,
                        capture: None,
                        measure_present: false,
                    },
                    |ui| {
                        egui::CentralPanel::default()
                            .frame(egui::Frame::new().fill(egui::Color32::TRANSPARENT))
                            .show(ui, |ui| {
                                ui.label(format!(
                                    "frame {} — {width}×{height} at {scale}×",
                                    self.frames_drawn
                                ));
                            });
                    },
                );
                self.frames_drawn += 1;
                // egui asks for another frame while anything animates (here:
                // nothing), but a caller that ignores the question leaves a
                // hover highlight frozen.
                if frame.context().has_requested_repaint() {
                    frame.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(
        &mut self,
        event_loop: &ActiveEventLoop,
        _windows: &mut dtr_frame::Windows,
        deadlines: &mut dtr_frame::Deadlines,
    ) {
        // This is the whole control-flow contract: say when the next thing is
        // due, never call `set_control_flow` (the loop does it once, from the
        // earliest of these, or `Wait`).
        if self.started.elapsed() > Duration::from_secs(3) {
            event_loop.exit();
            return;
        }
        deadlines.soon(Instant::now() + Duration::from_millis(100));
    }
}

fn main() -> anyhow::Result<()> {
    let frames = dtr_frame::FrameLoop::<WorkerEvent>::new()?;
    // A worker would keep this proxy and `send_event` through it; the loop
    // delivers those to `App::user_event`.
    let _proxy: winit::event_loop::EventLoopProxy<WorkerEvent> = frames.proxy();
    frames.run(Hello {
        started: Instant::now(),
        frames_drawn: 0,
    })
}
