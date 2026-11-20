# `dtr-frame`: the window, the surface, and the loop

`crates/dtr-frame` is the window plumbing the terminal used to keep inside
`app.rs`: winit's event loop and window, the wgpu instance/adapter/device/queue
and the configured surface, one `egui::Context` and `egui_winit` state per
window, the `egui_wgpu` renderer, the render passes and the present. It exists
because the file picker is going to be a second process (`dtr-portal`) and the
manager a second window (`docs/manager.md` §2), and neither may re-solve the
traps below — DPI, surface loss, viewport commands, repaint carrying, drop
order — differently from the terminal.

**One `Frame` is one window. One `FrameLoop` is the event loop and every window
it owns.** Multi-window is the shape from the start: `Windows` is a `Vec<Frame>`
over a single `EventLoop`, with an `egui::Context` each, even though `dtr` opens
one.

## The smallest consumer

A frontend implements `dtr_frame::App` — winit's `ApplicationHandler` with the
windows handed to it and the control flow collected rather than set — and hands
it to `FrameLoop::run`:

```rust,ignore
use std::sync::Arc;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;

struct Picker {
    // the frontend's own state: a listing, a query, a selection
}

impl dtr_frame::App for Picker {
    type Event = MyWorkerEvent;

    fn resumed(&mut self, event_loop: &ActiveEventLoop, windows: &mut dtr_frame::Windows) {
        if !windows.is_empty() {
            return; // resumed again after a suspend
        }
        let frame = windows
            .open(event_loop, dtr_frame::WindowOptions {
                title: "dtr-picker".to_owned(),
                inner_size: (720.0, 480.0),
                app_id: Some(("dtr-picker".to_owned(), "dtr-picker".to_owned())),
                ..Default::default()
            })
            .expect("a window");
        frame.request_redraw();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        frame: &mut dtr_frame::Frame,
        event: WindowEvent,
    ) {
        // Feed every event to egui first: `Frame::feed_event` drops
        // `RedrawRequested` itself, so feeding it cannot make the loop spin.
        if frame.feed_event(&event) || matches!(event, WindowEvent::CursorMoved { .. }) {
            frame.request_redraw();
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                frame.resize(size);
                frame.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                // Whatever the scale factor is *after* a moved window: ask the
                // frame, never divide two physical sizes.
                let scale = frame.scale();
                let size = frame.size();
                let mut scene = NoScene { clear: wgpu::Color::BLACK };
                let _ = frame.render(
                    &mut scene,
                    dtr_frame::RenderOptions {
                        frames_drawn: self.frames_drawn,
                        started: self.started,
                        capture: None,
                        measure_present: false,
                    },
                    |ui| {
                        egui::CentralPanel::default().show(ui, |ui| {
                            ui.label(format!("{size:?} at {scale}×"));
                        });
                    },
                );
                self.frames_drawn += 1;
            }
            _ => {}
        }
    }

    fn about_to_wait(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _windows: &mut dtr_frame::Windows,
        deadlines: &mut dtr_frame::Deadlines,
    ) {
        // Say when the next thing is due; never call `set_control_flow`.
        if let Some(at) = self.next_deadline() {
            deadlines.soon(at);
        }
    }
}
```

and `FrameLoop::run` returns when the loop ends, after calling
`App::finished()` — the place to report a fatal error as the process's failure:

```rust,ignore
let frames = dtr_frame::FrameLoop::<MyWorkerEvent>::new()?;
let proxy = frames.proxy(); // workers send events through this
frames.run(Picker::new(proxy))?;
```

`Frame` exposes what the frontend needs outside the draw closure:

| method | what |
| --- | --- |
| `context() -> &egui::Context` | the window's context, one per window |
| `window() -> &Arc<winit::window::Window>` | native dialog parent, IME cursor area, pointer, monitor |
| `device()`, `queue()` | for the frontend's own wgpu resources (dtr's grid) |
| `format()` | the surface's texture format, which a pipeline must match |
| `size()`, `scale()` | the configured surface, in physical pixels, and the DPI factor |
| `adapter_name()` | the GPU that produced a frame, for an overlay |
| `feed_event`, `resize`, `configure_pending_resize` | the event half |
| `render(scene, options, draw)` | the frame half |
| `set_title`, `request_redraw` | the window half |

## The seam: a scene under egui

`render` takes two things per frame:

* a **`Scene`** — what is drawn into the surface underneath egui. `prepare`
  uploads the frame's data, `clear_color`/`draw` are the first pass and
  `draw_above` is the same pass after the clear with `Load`, before egui's own
  draw. dtr's terminal grid is a `Scene`
  ([`GridScene`](../crates/dtr/src/app.rs)); a frontend with no underlay
  implements the trait with empty bodies.
* a **draw closure**, `FnMut(&mut egui::Ui)` — the root of this window's egui
  pass. Panels, cards, overlays; the application's business.

The `Frame` does the rest, in this order, so no frontend repeats it:
`get_current_texture` (reconfigure and skip on `Lost`/`Outdated`; a timeout or
an occluded window is not an error), `update_viewport_info`, `take_egui_input`,
`Context::run_ui`, `handle_platform_output`, `process_viewport_commands`,
tessellation, texture upload, `update_buffers`, the underlay pass, the egui
pass, `submit`, an optional surface capture, `present`, then freeing textures
and clearing `textures_delta` (egui panics if a delta is dropped unapplied).

`Frame::render` returns `Ok(None)` when nothing was drawn — the caller should
not count it as a frame's picture, and `Ok(Some(FrameTiming))` otherwise, with
CPU time and, if `measure_present`, the present wait.

## The traps, and where they live

- **DPI.** `Frame::scale` is `window.scale_factor()`. `inner_size()` is already
  physical in winit 0.30, so a surface-width ratio is always 1.0 and everything
  measured in points would reserve half the space it needs on HiDPI
  (`docs/windows.md` §2). Units: egui is in points and applies the factor,
  layout constants are points, grid sizes and inset are physical, and the
  factor is applied exactly once.
- **Surface loss and resize.** A `Lost`/`Outdated` swapchain is reconfigured and
  that frame skipped. `Resized` only *records* the size (`Frame::resize`):
  Windows sends one `WM_SIZE` per mouse move and every configure waits for the
  present queue to go idle on DX12, so `render` (and a caller that asks early,
  `configure_pending_resize`) configures once for the frame that draws it.
- **Viewport commands.** `StartDrag`, `BeginResize`, `Maximized` and the rest
  arrive in `FullOutput::viewport_output` and do nothing unless
  `egui_winit::process_viewport_commands` is called; egui knows the maximised
  state only because `update_viewport_info` tells it every frame
  (`docs/chrome.md` §8). Both are inside `render` now.
- **Repaint carrying.** egui asks for a redraw for a click, not for a hover, so
  a caller should request one for pointer moves too; after a frame ask
  `context().has_requested_repaint()`. This crate does not own the app's wake
  flag — that belongs with the app's deadline list, because Wayland makes a
  redraw requested *inside* a redraw wait for the compositor's frame callback,
  so the app turns its wake into a `Deadlines::soon(now)`.
- **Never feed `RedrawRequested` to egui.** `Frame::feed_event` returns `false`
  for it without touching egui, so a caller that feeds every event cannot
  reintroduce the refresh-rate spin.
- **Control flow.** `set_control_flow` is sticky state, not a per-iteration
  timeout: leaving a past `WaitUntil` in place makes every `poll` time out and
  the loop spin (55 000 iterations a second was measured). `App::about_to_wait`
  gets a `Deadlines`; the loop sets exactly one value after it returns — the
  earliest deadline, or `Wait`.
- **Drop order.** Inside `Frame`: egui's renderer, then the surface, then the
  device/queue, and the window last — resources before the device that owns
  them, the surface before the window. Across the whole loop: `Runner` drops the
  `App` before the windows, so the frontend's own GPU resources (dtr holds the
  grid's buffers and textures) are dropped before the device too. Getting this
  wrong segfaults on close instead of failing loudly.
- **`on_created`.** Platform work that needs the native handle before the first
  frame — dtr subclasses the window procedure so Windows 11 offers snap layouts
  over its painted maximise button — runs from `WindowOptions::on_created`,
  which the loop calls between `create_window` and `Frame::new`.

## Windows, and a second one

`Windows::open(event_loop, options)` creates the winit window, enables the IME
if asked, runs `on_created`, constructs the `Frame` and returns it. The
collection answers `len`, `is_empty`, `get_mut(id)`, `primary()`, `iter_mut`
and `close(id)`. The loop routes each `WindowEvent` to
`App::window_event` with that window's `Frame`, so a second window is a second
`Frame` and a second `egui::Context` — not a second event loop (winit allows one
per thread, and two would not share the app state anyway).

A frontend that closes its last window decides what that means: `Windows::close`
drops the surface and device, and the app calls `event_loop.exit()` if the
process should end (dtr's only window uses `CloseRequested` for that).

## What stayed in `dtr`, and why

`crates/dtr/src/app.rs` keeps everything that is *the application*: `App`, the
panes and tabs, the layout, the palette, the file browser, the config, the
keymap, the grid renderer and its fonts. Only two small structs are left on the
window side:

- `Gpu` — the grid renderer plus the two chrome decisions a frame needs
  (`client_frame`, `caption_glyphs`) and the installed skin. It is dtr's
  `Scene` host, not a window.
- `GridScene` — the `Scene` implementation that hands the frame the grid's
  per-frame data.

`--capture` is the frame's (`render` photographs the surface before `present`,
because it configured `COPY_SRC` for it), and the PNG writer moved with it
(`dtr_frame::write_png`, re-exported through `dtr::screenshot::write_png`).
`--screenshot` is still dtr's: it renders offscreen from injected state with no
window and no present, so it is not a frame.

`App` keeps three cheap handles so its methods work between frames the way they
did when they reached into the window: the window (`Arc<Window>` for
`request_redraw`, the monitor, `request_inner_size`, the IME area), the egui
context (a clone; `pixels_per_point`, `layer_id_at`, the thumbnail texture) and
`size`, the configured surface size, refreshed at the top of each redraw. That
is the seam: the frame owns the surface; the app owns what the last frame drew
on it.

## Adding a second window later

`FrameLoop` already owns a `Vec<Frame>`, so a manager window is:

1. `windows.open(event_loop, options)` from `resumed` or a user event;
2. a second screen state, drawn in `App::window_event` for that `Frame`;
3. `windows.close(id)` plus `event_loop.exit()` when it is the last one.

The parts that must not be duplicated are already here: adapter choice
(`adapter_hint`, and `WGPU_BACKEND` through `dtr_frame::instance()`), the
`COPY_SRC` capture path, the drop order, and the deadline discipline.
