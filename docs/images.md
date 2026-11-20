# Kitty graphics: what the core parses, and what dtr draws

Status: 2026-11-20. The protocol is parsed by the pinned `libghostty-vt` (rev
`84373f8`); everything below is the frontend half — turning the render state's
placement list into textured quads — plus the two switches that turn the feature
on at all. Read this before changing `crates/dtr/src/kitty.rs`,
`pane::publish` or the image ranges in `render::prepare`.

## 1. The split of duties

The core already does all of the protocol work: transmit and display, delete,
PNG and raw RGBA/RGB/gray, zlib, chunked payloads (`m=1` continuations), query
replies, the storage limit, and it works out where every placement goes — cell
placements, Unicode placeholders (`U+10EEEE`), and placements relative to
another image. Its render state hands out one list of
`render::ImageIteration`s, already resolved to viewport cells, pixel offsets,
destination size, source rectangle, layer and z-index.

What it cannot do is put bytes on a screen: that needs the cell size in pixels
and a GPU, both of which are the frontend's. So the missing last mile was:

1. **Switch the feature on.** Kitty graphics are *off* until
   `Terminal::set_kitty_image_storage_limit` is called (zero means disabled),
   and PNG is refused until a `DecodePng` is installed process-wide. Neither was
   ever called, which is why the bytes were parsed, stored and dropped.
2. **Carry the list and the pixels across the thread boundary.** A pane owns a
   `!Send` terminal; the UI thread draws.
3. **Draw them**, in the right place, in the right order, clipped to the pane.

## 2. Enabling it

* `pane::new_terminal` sets the storage limit to
  `kitty::KITTY_IMAGE_STORAGE_LIMIT` (256 MiB, one constant with its reasoning
  beside it). The core applies the limit to the main and alternate screen
  separately, so the terminal's own worst case is twice that. This is the same
  shape as the scrollback limit: a bound is part of the type, not a preference,
  because a program with no bound is a memory leak with a schedule.
* `main` calls `kitty::install_png_decoder()` before the window exists. The
  decoder hook is process-global and libghostty does not synchronize installing
  it, so the one safe moment is startup — not "the first pane that needs it",
  which could race a second tab's `vt_write` on another thread. The decoder is
  the core's own `RustPngDecoder` (the `png` crate): it decodes straight into the
  allocator the core caps, so a tiny PNG claiming gigabytes cannot get past the
  storage limit. The `kitty-graphics` and `png` features are enabled on the
  workspace's `libghostty-vt` dependency in `crates/dtr/Cargo.toml`.

## 3. The frame

`pane::ImageCache` is the pane thread's copy of the render state's list:

* It re-reads the list only when `Snapshot::image_placements_generation`
  changes. That generation moves when an image is added, deleted or moved —
  including when text scrolls it — but not when only text changes (unless the
  changed rows had placeholder cells). An ordinary frame therefore does no image
  work at all.
* It copies an image's pixels out of the terminal only when that image's own
  `image_generation` changes. Pixels are normalised to 8-bit RGBA once (the raw
  paths also allow RGB, gray and gray+alpha), because the drawing side then has
  one format to upload.
* The pixels live behind an `Arc<ImagePixels>`; the published `Frame` carries
  `Arc<[Placement]>`, so a frame that did not change is refcounts, not image
  copies. The pane drops cache entries for images the new list no longer names.

`Frame::images` is the carrier. It is empty (a shared, allocation-free empty
slice) for every terminal that never receives an image.

The frame gate had to learn about images: an image arriving, moving with a
scroll or being deleted changes no cell, exactly like a cursor or a title, so
`publish` compares the placements generation with the last published one and
publishes when it differs. Without that, a delete would leave the picture
showing an image the program has already removed.

**Output waits for the first fit.** A placement is worked out in pixels against
the terminal's cell, and before `fit_panes` has sent its first
`Command::Resize` the terminal has no cell size — the core drops an image
transmitted then. That ordering is not rare: a replay feeds its captured bytes
immediately, a board's boot splash arrives before the first frame, and a
program can print before the window is up. So a pane holds terminal output until
the first resize and then parses it, placing any image at the measured cell
rather than at a guess. `EARLY_OUTPUT_WAIT_IMAGE` (ten seconds) bounds the hold
on the two transports whose first burst can be a picture — a serial line (a
board's boot logo arrives before the window) and a replay (a captured stream) —
because a cold Windows start's first D3D12 frame can take seconds; a shell or
ssh pane uses `EARLY_OUTPUT_WAIT` (one second) instead, since its first bytes are
a prompt and holding those reads as a hung terminal. `EARLY_OUTPUT_MAX` (4 MiB)
bounds the memory. A pane nobody fits — a test, a screenless run — still prints
when the wait expires. This is what makes `--replay` show images; before it, the
payload was parsed and every image in it was dropped for having no size to be
placed against.

## 4. Geometry, in one place

`kitty::placement_rect` and `kitty::image_quad` are pure functions of the
placement, the pane's rectangle, the drawn cell size and the framebuffer size:

* The origin is `pane.top_left + (col × cell_width, row × cell_height) +
  (cell_offset_x, cell_offset_y)`; the size is the core's `pixel_width ×
  pixel_height`. Viewport-relative means scrollback needs no arithmetic — row 0
  is the grid's top row however far the viewport has scrolled, and a negative
  row is an image partly above it.
* Clipping is done by shrinking the quad and moving its texture coordinates
  with it, not with a scissor: the intersection of the placement, the pane and
  the framebuffer is the quad, and its uv range is computed from the
  destination-to-source mapping. One image can be half outside its pane and half
  outside the window, and a split cannot leak one pane's image over the other
  because the pane's rectangle is part of the intersection.
* The cell size is dtr's drawn cell (`render::Metrics`), not the core's
  `resize` argument. They differ by less than a pixel (the core gets the pane
  divided by the cell count, floored); the drawn grid is what the image must
  line up with.
* Texture coordinates are normalised by the *image* size, not the texture size,
  so the device-limit downscale (§5) does not change the mapping.

## 5. Layers, z, and where each group is drawn

The core gives three layers. dtr's grid pass draws backgrounds and glyphs in one
sequence, so `render::prepare` now fills the instance buffer in two ranges —
every pane's backgrounds, then every pane's glyphs — and the draw is:

1. `BelowBackground` images (they land under the pane's opaque fill: drawn, but
   a terminal with an opaque background will never show one — that is what the
   layer means, and dtr has no background-image feature to composite against);
2. the grid's background instances;
3. `BelowText` images (above cell backgrounds, under the glyphs);
4. the grid's glyph instances, selection, cursor mark and bell flash;
5. `AboveText` images, in the egui pass before egui itself, so a menu or a card
   is still over the picture.

Within each group, images are ordered by `z` ascending with the image id as a
tie-break for determinism. `z >= 0` is `AboveText` in the protocol, which is why
a default `a=T` transmission appears over the text; `z < 0` is below it.
`--capture` photographs this same pass, so images appear in it; `--screenshot`
renders injected state that has no pixels, so it draws none (allowed, and the
only difference between the two capture paths here).

## 6. The texture cache

One texture per (image id, generation), `Rgba8Unorm` on the usual non-sRGB
surface (or `Rgba8UnormSrgb` if the backend only offers an sRGB target, so the
sample/decode and the target's encode cancel). The shader does no colour
conversion, matching the grid's own convention. The two-part key is enough
across panes because the core never reuses an image generation — not for
another image, screen or terminal — so two panes cannot alias the same entry.

* The cache is byte-budgeted (`kitty::TEXTURE_BUDGET`, 256 MiB) and
  least-recently-used: every entry a prepared frame names is touched, and an
  insert that would exceed the budget evicts the entries touched longest ago. An
  entry evicted while the core still lists the image is simply uploaded again on
  the next frame — the pane still has the pixels, so it costs a queue write,
  never a decode or a protocol round trip. An image larger than the whole budget
  is still drawn, alone.
* A new generation (re-transmission, or an animation frame if one were ever
  driven) is a new key, so the old texture is not reused; it ages out of the
  cache under the budget.
* A deletion is not an explicit eviction: the texture stays until the budget
  pushes it out. It is unreachable (no placement names it) and bounded by the
  budget; `Graphics::generation` is never reused, so a re-created image cannot
  alias it.

## 7. The device limit

`wgpu`'s `max_texture_dimension_2d` (8192 on the default limits, larger on some
adapters) is a hard cap. An image whose width or height exceeds it is downscaled
once at upload with the `image` crate (triangle filter) to fit, and *drawn at
its intended destination size*. The texture coordinates are fractions of the
original image, so a downscaled texture samples exactly the same part of the
picture; only resolution is lost. Not drawing the image at all would be a worse
answer for the one case — a laptop panel or driver with a small limit — where
the user still wants to see the picture.

## 8. What is deliberately not supported

* **Animations.** The protocol's multi-frame images (`a=f`, `a=a`) are stored by
  the core, but playback needs a clock: calling
  `Terminal::kitty_graphics_animation_tick` and scheduling a redraw at the time
  it returns. dtr has no pane-thread timer for it, and adding one means a pane
  that animates at display rate while the UI must cooperate. The cache is
  already generation-keyed, so the only missing piece is the tick plus a wake at
  the returned deadline; until then an animated image shows its first frame.
* **The file and shared-memory media** (`t=f`/`t=s`, and the temp-file
  directory). They are off in the core until an embedder enables them; a
  serial-console terminal should not read paths or shared memory named by
  whatever program is on the other end. The default (data medium only) is the
  safe one.
* **Below-background images becoming visible.** See §5; they are drawn, then
  covered by the pane's own fill. A real background image would need the window
  composition work that `docs/ghostty.md` §3 lists separately.
* **Sixel and `OSC 1337 File=`.** The core advertises no sixel and parses and
  rejects the iTerm2 file sequence; there is nothing to draw.
* **Anything the bindings do not expose.** The core is the source of truth for
  placement geometry (including Unicode placeholders and relative placements),
  and dtr uses the list as given; it deliberately does not reimplement any of
  it.
* **Kitty graphics through a Windows shell pane.** ConPTY is a terminal
  emulator: it renders a child's output into its own buffer and re-emits VT to
  the client, and it drops the unknown APC sequences that carry kitty graphics.
  A program in a shell tab on Windows therefore cannot deliver an image to dtr at
  all — a raw capture of what dtr receives shows the text and none of the
  `ESC _ G` sequences. Serial panes read the COM port directly (no ConPTY), and
  `--replay` feeds bytes straight into the core, so both draw; a Linux shell (a
  real pty) draws too. This is a Windows transport fact, not a renderer gap.
* **An image in output that overruns the first-fit hold.** If a pane's first fit
  never arrives, or more than `EARLY_OUTPUT_MAX` is printed before it, the hold
  is released and an image in that early output may have been transmitted before
  the cell was known, so the core drops it. In the app the fit is one frame
  away; this is the no-UI case.

## 9. Evidence

Unit tests (no GPU): `cargo test -p dtr --lib kitty::` covers placement → pixel
rectangle, clipping to the pane and to the framebuffer, negative viewport rows,
source-rect → uv maths, layer/z ordering, cache keying, LRU eviction and the
device-limit downscale. `cargo test -p dtr --lib pane::image_tests` runs a real
`Terminal` through the protocol (raw RGBA transmit, re-transmission, delete,
negative z, cursor placement, the storage limit) and asserts what the frame
carries, including that an unchanged frame shares the same `Arc`s.

Live: Xvfb (`:98`, 1280×800) + lavapipe, `--capture` of the real window with a
shell script that base64-encodes an 85 KB PNG and transmits it in 4096-byte
chunks, twice (z=0 and z=-1), plus a raw `f=32` image:

* `/tmp/kitty-evidence/above.png` — the PNG above the text covers the letters
  under it; the copy with z=-1 on the right has the text drawn over it; the raw
  RGBA block is the orange rectangle at the left, three rows lower.
* `/tmp/kitty-evidence/after-delete.png` — the same scene after `a=d`: all
  three images are gone and the text they covered is intact.
* `/tmp/kitty-evidence/split-clip.png` — a 30-column image started at column 30
  of a vertical split's left pane: it is cut at the pane's edge (image pixels up
  to x=584, the window's background from x=590) and the right pane is untouched,
  so one pane's image cannot cover another.
* **Replay.** `dtr --replay /tmp/winverify/kitty-emit.bin` — the same chunked
  PNG and raw block, fed before any fit — now logs `output held for the first
  fit bytes=3072`, then `image placements images=2` and `images prepared
  images=2 textures=2 texture_bytes=163904`, and the capture shows both. Before
  the hold existed, the same replay placed nothing at all: the bytes were parsed
  and the images dropped. `a_replayed_image_reaches_the_frame_after_the_first_fit`
  is the regression test.
* **Windows.** A guest run (winvm, 100 %, D3D12 surface, `Bgra8Unorm` 900×620)
  found both facts §8 documents: ConPTY dropped the APC sequences in a shell
  pane (the raw received-byte log had the text and none of the `ESC _ G`), and
  the replay path lost its images to the no-cell-size ordering above. Shell panes
  are therefore not an instrument for this feature on Windows; `--replay` (and a
  serial pane's direct COM read) is the path that bypasses ConPTY. On the fixed
  build the replay drew both images on D3D12: `image placements images=2`,
  `images prepared images=2 textures=2 texture_bytes=163904` (256×160×4 +
  4×4×4, exactly), the gradient at row 4/col 3 over the text and the raw block at
  row 13 with exact `(255,64,0)` pixels. One recipe note for that guest: its
  ~15.6 ms timer tick stretches the replay feeder's 5 ms chunk sleep, so the
  115 KB payload takes seconds to arrive — capture after 12 s, not 3.5 s.

The script is `/tmp/kitty-evidence/keep.sh` (`keep` or `delete`), run through
`/tmp/kitty-evidence/run.sh`; the artefacts are evidence, not fixtures, and do
not live in the repository.

## 10. Where the code is

* `crates/dtr/src/kitty.rs` — placement geometry, the cache and its budget, the
  wgpu pipeline, the device-limit downscale, the decoder installer.
* `crates/dtr/src/pane.rs` — `new_terminal` (storage limit), `ImageCache`,
  `read_pixels`, `publish` (the frame gate and the carried list).
* `crates/dtr/src/render.rs` — `GridRenderer` owns the image layer and splits
  its instance buffer into backgrounds/foreground; `prepare` builds the quads,
  `draw`/`draw_above` draw the three groups.
* `crates/dtr/src/app.rs` — calls `draw_above` in the egui pass; the render
  pass otherwise unchanged.
* `crates/dtr/src/main.rs` — installs the PNG decoder before any terminal.
