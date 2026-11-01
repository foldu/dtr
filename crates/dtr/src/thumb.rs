//! Thumbnails for the image under the pointer.
//!
//! A terminal that prints `photo.jpg` is one click from opening it; this is the
//! step before that click, for when the name is not enough and opening the file
//! is more than you wanted. Hovering a link whose path ends in `.png`, `.jpg` or
//! `.jpeg` shows a small copy of it next to the pointer.
//!
//! Three rules hold it together:
//!
//! - **Decoding happens on the files worker, never on the drawing thread.** A
//!   12-megapixel JPEG is tens of milliseconds and a dropped frame is a stutter
//!   in a shell somebody is typing into.
//! - **It is capped before it is read.** A preview of a 400 MB scan is not a
//!   service to anyone, and neither is decoding a 20000x20000 PNG into 1.6 GB of
//!   pixels because the pointer brushed past its name.
//! - **A file is asked about once.** The answer — including "no" — is cached, so
//!   moving the mouse along a row of names does not re-read them all.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use egui::{Pos2, Rect, Stroke, TextureHandle, Vec2};

use crate::ui::Look;

/// What the pointer has to land on to be worth a preview at all. The extension is
/// only a hint — the decoder sniffs the bytes — but it is what keeps every
/// non-image link from being opened and read.
const EXTENSIONS: &[&str] = &["png", "jpg", "jpeg"];

/// The biggest source file a hover will read.
pub const MAX_BYTES: u64 = 16 << 20;
/// The biggest source image, in pixels per side: a 40000x100 limit is still a
/// decode nobody asked for.
const MAX_SOURCE: u32 = 8192;
/// What a single decode may allocate. The decoded image is the expensive half,
/// and it exists before any downscale can happen.
const MAX_ALLOC: u64 = 256 << 20;
/// The size of the preview that is stored: 2× the largest draw size, so it is
/// sharp on a HiDPI display without being a texture per megapixel.
pub const MAX_WIDTH: u32 = 480;
pub const MAX_HEIGHT: u32 = 360;
/// How many previews are kept. A directory listing is longer than this, and the
/// point of the cache is the pointer resting on a row of names, not remembering
/// yesterday.
const CAPACITY: usize = 16;

/// A decoded image, already small enough to be a texture.
#[derive(Clone, Debug)]
pub struct Thumb {
    pub width: u32,
    pub height: u32,
    /// RGBA8, straight alpha.
    pub rgba: Vec<u8>,
}

/// Is this a path a hover should even try?
pub fn looks_like_image(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            EXTENSIONS
                .iter()
                .any(|wanted| ext.eq_ignore_ascii_case(wanted))
        })
}

/// Read and downscale one image, with every limit applied before the expensive
/// part.
pub fn load(path: &Path) -> anyhow::Result<Thumb> {
    let meta = std::fs::metadata(path)?;
    anyhow::ensure!(meta.is_file(), "not a file");
    anyhow::ensure!(
        meta.len() <= MAX_BYTES,
        "{} bytes is more than a preview is worth",
        meta.len()
    );
    // Guessed, not trusted from the extension: a `.png` that is really a JPEG is
    // a thing `mv` does, and the decoder knows better than the name.
    let mut reader = image::ImageReader::open(path)?.with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SOURCE);
    limits.max_image_height = Some(MAX_SOURCE);
    limits.max_alloc = Some(MAX_ALLOC);
    reader.limits(limits);
    let image = reader.decode()?;
    // `thumbnail` enlarges as happily as it shrinks — a 1x1 icon "fits" a 480x360
    // box at 360x360 — and a preview that invents pixels is worse than none.
    let scaled = if image.width() > MAX_WIDTH || image.height() > MAX_HEIGHT {
        image.thumbnail(MAX_WIDTH, MAX_HEIGHT)
    } else {
        image
    };
    let rgba = scaled.to_rgba8();
    Ok(Thumb {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

/// Which file a preview belongs to.
///
/// The machine is part of the key because the same path on two machines is two
/// files: `/tmp/shot.png` on `jupyter` and on this disk must not hand each other's
/// picture back.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    /// The ssh alias, or `None` for this machine.
    pub machine: Option<String>,
    pub path: PathBuf,
}

impl Key {
    pub fn new(machine: Option<String>, path: PathBuf) -> Self {
        Self { machine, path }
    }
}

/// One answer about one path.
enum Entry {
    /// Decoded off the drawing thread, waiting for a frame to become a texture.
    Pixels(Thumb),
    Ready(TextureHandle),
    /// Asked and refused: too big, unreadable, or not an image after all. Kept so
    /// the answer does not have to be worked out again on the next hover.
    Failed,
}

/// The previews that have been asked about, oldest first.
#[derive(Default)]
pub struct Cache {
    entries: Vec<(Key, Entry)>,
    /// Keys a worker is dealing with right now, so a fast pointer cannot start
    /// the same decode twice.
    loading: HashSet<Key>,
}

impl Cache {
    /// Claim `key` for a load. `false` when it is already known or in flight —
    /// the caller then does nothing at all.
    pub fn begin(&mut self, key: &Key) -> bool {
        if self.entries.iter().any(|(known, _)| known == key) {
            return false;
        }
        self.loading.insert(key.clone())
    }

    /// The worker answered. Nothing is uploaded here — a texture is made during a
    /// frame, by the code that draws it — so a failure is remembered as "no
    /// preview" and a success waits as pixels.
    pub fn finish(&mut self, key: Key, result: anyhow::Result<Thumb>) {
        self.loading.remove(&key);
        let entry = match result {
            Ok(thumb) => Entry::Pixels(thumb),
            Err(err) => {
                tracing::debug!(machine = ?key.machine, path = %key.path.display(), %err, "no thumbnail");
                Entry::Failed
            }
        };
        self.entries.push((key, entry));
        if self.entries.len() > CAPACITY {
            self.entries.remove(0);
        }
    }

    /// The texture for `key`, uploading it on first use.
    ///
    /// The upload is here rather than in the loader because it needs an egui
    /// context, which only exists once the window does; the worker has no business
    /// knowing about textures anyway.
    pub fn texture(&mut self, key: &Key, ctx: &egui::Context) -> Option<&TextureHandle> {
        let at = self.entries.iter().position(|(known, _)| known == key)?;
        if matches!(&self.entries[at].1, Entry::Pixels(_)) {
            let Entry::Pixels(thumb) = std::mem::replace(&mut self.entries[at].1, Entry::Failed)
            else {
                unreachable!("just matched a decoded image")
            };
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [thumb.width as usize, thumb.height as usize],
                &thumb.rgba,
            );
            self.entries[at].1 = Entry::Ready(ctx.load_texture(
                format!("dtr-thumb:{}", key.path.display()),
                image,
                egui::TextureOptions::LINEAR,
            ));
        }
        match &self.entries[at].1 {
            Entry::Ready(texture) => Some(texture),
            _ => None,
        }
    }
}

/// The largest a preview may be drawn, in points.
pub fn fit(size: Vec2) -> Vec2 {
    const MAX: Vec2 = Vec2::new(240.0, 180.0);
    if size.x <= 0.0 || size.y <= 0.0 {
        return Vec2::ZERO;
    }
    let factor = (MAX.x / size.x).min(MAX.y / size.y).min(1.0);
    size * factor
}

/// Where a preview of `size` goes for a pointer at `pointer`.
///
/// Below-right of the pointer, because that is where the eye already is and where
/// the pointer itself does not cover it — flipped when it would leave the window,
/// and clamped last, because flipping can push it off the opposite edge when the
/// window is smaller than the picture.
pub fn place(pointer: Pos2, size: Vec2, screen: Rect) -> Rect {
    const GAP: f32 = 14.0;
    let mut min = egui::pos2(pointer.x + GAP, pointer.y + GAP);
    if min.x + size.x > screen.max.x {
        min.x = pointer.x - GAP - size.x;
    }
    if min.y + size.y > screen.max.y {
        min.y = pointer.y - GAP - size.y;
    }
    min.x = min
        .x
        .clamp(screen.min.x, (screen.max.x - size.x).max(screen.min.x));
    min.y = min
        .y
        .clamp(screen.min.y, (screen.max.y - size.y).max(screen.min.y));
    Rect::from_min_size(min, size)
}

/// Draw the preview next to the pointer.
///
/// An `Area` in the tooltip layer rather than a painter call: a file pane is an
/// `Area` too, and a preview that slid underneath one would be a preview nobody
/// could see. It does not interact, so the pointer keeps asking the terminal what
/// is under it — and hovering the picture itself is not a thing.
pub fn draw(ui: &mut egui::Ui, texture: &TextureHandle, size: Vec2, look: &Look) {
    let Some(pointer) = ui.ctx().pointer_latest_pos() else {
        return;
    };
    let screen = ui.max_rect().shrink(6.0);
    let size = fit(size);
    if size.x < 2.0 || size.y < 2.0 {
        return;
    }
    let rect = place(pointer, size, screen);
    egui::Area::new(egui::Id::unique("dtr-thumb"))
        .order(egui::Order::Tooltip)
        .interactable(false)
        .fixed_pos(rect.min)
        .show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(Look::colour(look.surface))
                .stroke(Stroke::new(1.0, Look::colour(look.dim).gamma_multiply(0.7)))
                .corner_radius(4.0)
                .inner_margin(3)
                .show(ui, |ui| {
                    ui.add(egui::Image::new((texture.id(), size)));
                });
        });
}

#[cfg(test)]
mod tests {
    use super::{
        CAPACITY, Cache, Key, MAX_HEIGHT, MAX_WIDTH, Thumb, fit, load, looks_like_image, place,
    };
    use egui::{Rect, Vec2, pos2};
    use std::path::{Path, PathBuf};

    fn one_pixel() -> Thumb {
        Thumb {
            width: 1,
            height: 1,
            rgba: vec![0x20, 0x40, 0x60, 0xff],
        }
    }

    #[test]
    fn only_names_that_promise_a_picture_are_tried() {
        assert!(looks_like_image(Path::new("a.png")));
        assert!(looks_like_image(Path::new("a.JPG")), "case is the shell's");
        assert!(looks_like_image(Path::new("dir/photo.jpeg")));
        assert!(
            !looks_like_image(Path::new("a.gif")),
            "not a format this reads"
        );
        assert!(!looks_like_image(Path::new("notes.txt")));
        assert!(!looks_like_image(Path::new("README")));
        assert!(!looks_like_image(Path::new("a.png.txt")));
    }

    /// A real PNG, encoded here rather than committed as a fixture: the point is
    /// the whole path — read, sniff, decode, downscale — and a fixture only ever
    /// tests the decoder.
    #[test]
    fn a_png_is_read_and_scaled_down() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wide.png");
        let (width, height) = (1600u32, 400u32);
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                rgba.extend_from_slice(&[(x % 256) as u8, (y % 256) as u8, 0x40, 0xff]);
            }
        }
        write_png(&path, width, height, &rgba);

        let thumb = load(&path).expect("decode");
        assert!(thumb.width <= MAX_WIDTH, "{thumb:?}");
        assert!(thumb.height <= MAX_HEIGHT, "{thumb:?}");
        // Aspect ratio survives: a 4:1 source stays 4:1.
        let ratio = thumb.width as f32 / thumb.height as f32;
        assert!((ratio - 4.0).abs() < 0.05, "ratio {ratio}");
        assert_eq!(
            thumb.rgba.len(),
            (thumb.width * thumb.height * 4) as usize,
            "RGBA8, four bytes a pixel"
        );
    }

    /// The extension is a hint, not a promise: a JPEG named `.png` still decodes,
    /// because the reader sniffs the bytes.
    #[test]
    fn the_bytes_decide_the_format() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("actually-a-jpeg.png");
        let (width, height) = (64u32, 48u32);
        let rgba: Vec<u8> = (0..width * height)
            .flat_map(|i| [i as u8, 0x20, 0x80, 0xff])
            .collect();
        write_jpeg(&path, width, height, &rgba);

        let thumb = load(&path).expect("decode");
        assert_eq!((thumb.width, thumb.height), (width, height));
    }

    /// A file too big to be worth previewing is refused *before* it is read.
    /// Sparse, so the test does not write 16 MB to the disk.
    #[test]
    fn a_file_too_big_to_preview_is_refused_unread() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("huge.png");
        let file = std::fs::File::create(&path).expect("create");
        file.set_len(super::MAX_BYTES + 1).expect("sparse");
        drop(file);

        let err = load(&path).expect_err("too big");
        assert!(err.to_string().contains("bytes"), "{err}");
    }

    #[test]
    fn a_file_that_is_not_an_image_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("notes.png");
        std::fs::write(&path, b"this is not a picture").expect("write");
        assert!(load(&path).is_err());
    }

    #[test]
    fn a_preview_is_capped_but_never_stretched() {
        assert_eq!(fit(Vec2::new(100.0, 50.0)), Vec2::new(100.0, 50.0));
        let wide = fit(Vec2::new(960.0, 120.0));
        assert!((wide.x - 240.0).abs() < 0.01, "{wide:?}");
        assert!((wide.y - 30.0).abs() < 0.01, "{wide:?}");
        assert_eq!(fit(Vec2::ZERO), Vec2::ZERO);
    }

    /// Below-right by default, flipped at the edges, and never outside the window.
    #[test]
    fn the_picture_stays_inside_the_window() {
        let screen = Rect::from_min_size(pos2(0.0, 0.0), Vec2::new(800.0, 600.0));
        let size = Vec2::new(200.0, 150.0);

        let middle = place(pos2(300.0, 200.0), size, screen);
        assert_eq!(middle.min, pos2(314.0, 214.0));

        // Near the right edge it moves to the pointer's left, still on screen.
        let right = place(pos2(790.0, 200.0), size, screen);
        assert!(right.min.x + size.x <= screen.max.x, "{right:?}");
        assert!(right.min.x < 790.0, "flipped left of the pointer");

        // Near the bottom-right corner both flips happen.
        let corner = place(pos2(795.0, 595.0), size, screen);
        assert!(corner.max.x <= screen.max.x && corner.max.y <= screen.max.y);

        // A window smaller than the picture clamps rather than escaping through
        // the opposite edge.
        let tiny = Rect::from_min_size(pos2(0.0, 0.0), Vec2::new(100.0, 80.0));
        let clamped = place(pos2(60.0, 50.0), size, tiny);
        assert_eq!(clamped.min, pos2(0.0, 0.0));
    }

    /// The thumbnail a test compares against is the bytes that went in, scaled.
    #[test]
    fn a_one_pixel_image_is_one_pixel() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dot.png");
        write_png(&path, 1, 1, &[0x10, 0x20, 0x30, 0xff]);
        let thumb = load(&path).expect("decode");
        assert_eq!((thumb.width, thumb.height), (1, 1));
        assert_eq!(thumb.rgba, vec![0x10, 0x20, 0x30, 0xff]);
    }

    fn key(path: &str) -> Key {
        Key::new(None, PathBuf::from(path))
    }

    /// Each file is opened, decoded and uploaded at most once: the answer — even
    /// "no" — is kept, so a pointer moving along a row of names does not re-read
    /// them all.
    #[test]
    fn each_file_is_asked_about_once() {
        let ctx = egui::Context::default();
        let mut cache = Cache::default();
        let picture = key("/tmp/picture.png");

        assert!(cache.begin(&picture), "the first hover asks");
        assert!(
            !cache.begin(&picture),
            "the second does not, it is in flight"
        );
        assert!(
            cache.texture(&picture, &ctx).is_none(),
            "nothing to draw yet"
        );

        cache.finish(picture.clone(), Ok(one_pixel()));
        assert!(!cache.begin(&picture), "answered, so never asked again");
        assert!(
            cache.texture(&picture, &ctx).is_some(),
            "and it can be drawn"
        );
    }

    /// A refusal is an answer. Without this, every hover on a 2 GB scan would try
    /// to read it again.
    #[test]
    fn a_refusal_is_remembered_too() {
        let ctx = egui::Context::default();
        let mut cache = Cache::default();
        let enormous = key("/tmp/enormous.png");

        assert!(cache.begin(&enormous));
        cache.finish(enormous.clone(), Err(anyhow::anyhow!("too big")));
        assert!(!cache.begin(&enormous), "known bad, not asked twice");
        assert!(
            cache.texture(&enormous, &ctx).is_none(),
            "and nothing to draw"
        );
    }

    /// The same path on two machines is two files: a remote `/tmp/shot.png` must
    /// not be answered with this disk's picture of the same name.
    #[test]
    fn a_remote_file_is_not_the_local_one_with_its_name() {
        let ctx = egui::Context::default();
        let mut cache = Cache::default();
        let local = Key::new(None, PathBuf::from("/tmp/shot.png"));
        let remote = Key::new(Some("jupyter".to_owned()), PathBuf::from("/tmp/shot.png"));

        assert!(cache.begin(&local));
        cache.finish(local.clone(), Ok(one_pixel()));

        assert!(cache.begin(&remote), "another machine, another file");
        assert!(cache.texture(&remote, &ctx).is_none(), "not fetched yet");
        assert!(
            cache.texture(&local, &ctx).is_some(),
            "the local one stands"
        );
    }

    /// The cache is a window over what the pointer has been near, not a library:
    /// it forgets the oldest, and asking again is allowed.
    #[test]
    fn the_oldest_preview_is_forgotten_first() {
        let ctx = egui::Context::default();
        let mut cache = Cache::default();
        for index in 0..=CAPACITY {
            let picture = key(&format!("/tmp/{index}.png"));
            assert!(cache.begin(&picture));
            cache.finish(picture, Ok(one_pixel()));
        }

        assert!(
            cache.texture(&key("/tmp/0.png"), &ctx).is_none(),
            "the first one is gone"
        );
        assert!(
            cache
                .texture(&key(&format!("/tmp/{CAPACITY}.png")), &ctx)
                .is_some(),
            "the newest one is here"
        );
        assert!(cache.begin(&key("/tmp/0.png")), "and can be asked again");
    }

    fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) {
        let file = std::fs::File::create(path).expect("create");
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("header");
        writer.write_image_data(rgba).expect("data");
    }

    fn write_jpeg(path: &Path, width: u32, height: u32, rgba: &[u8]) {
        use image::ImageEncoder;
        // JPEG has no alpha: an RGBA buffer is refused, and dropping the alpha
        // channel is what "save this as a JPEG" means everywhere else too.
        let rgb: Vec<u8> = rgba
            .chunks_exact(4)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect();
        let file = std::fs::File::create(path).expect("create");
        let writer = std::io::BufWriter::new(file);
        image::codecs::jpeg::JpegEncoder::new_with_quality(writer, 95)
            .write_image(&rgb, width, height, image::ExtendedColorType::Rgb8)
            .expect("encode");
    }
}

#[cfg(test)]
mod render_tests {
    use super::draw;
    use crate::ui::Look;
    use egui_kittest::Harness;

    /// The preview is drawn where `place` says, and it is a picture rather than a
    /// frame: the rendered window is checked for the texture's colour, so a mesh
    /// that never reached the screen would fail here.
    ///
    /// The pointer has to move before anything is drawn — the whole feature is
    /// "there is a link under the pointer" — so the harness moves it first.
    #[test]
    fn the_preview_is_painted_next_to_the_pointer() {
        let look = Look::default();
        // A solid colour, so the check does not depend on how a photograph
        // survives the texture path.
        let pixels = vec![200u8, 40, 40, 255].repeat(4 * 4);
        let texture = std::rc::Rc::new(std::cell::RefCell::new(None));
        let sink = texture.clone();
        let mut harness = Harness::new_ui(move |ui| {
            let mut slot = sink.borrow_mut();
            let texture = slot.get_or_insert_with(|| {
                let image = egui::ColorImage::from_rgba_unmultiplied([4, 4], &pixels);
                ui.ctx()
                    .load_texture("test-thumb", image, egui::TextureOptions::LINEAR)
            });
            draw(ui, texture, egui::vec2(100.0, 80.0), &look);
        });
        harness.set_size(egui::vec2(400.0, 300.0));
        harness.hover_at(egui::pos2(40.0, 40.0));
        harness.run();

        let rendered = harness.render().expect("a frame");
        // `place` puts a 100x80 preview at the pointer plus the 14-point gap.
        let inside = rendered.get_pixel(40 + 14 + 50, 40 + 14 + 40);
        assert!(
            inside[0] > 150 && inside[1] < 100,
            "the preview should be the texture's red at {inside:?}"
        );

        // And nothing was painted far away, where there is only the window.
        let away = rendered.get_pixel(360, 260);
        assert!(
            away[0] < 120,
            "a picture where there should be none: {away:?}"
        );
    }
}
