//! Pictures of pictures: the decode, its caps, and the small textures a grid
//! keeps for the images in view.
//!
//! Three rules, and they are the same three the hover preview in the terminal
//! follows (`docs/files.md` §6):
//!
//! - **Decoding happens on a thread, never on the drawing thread.** A
//!   12-megapixel JPEG is tens of milliseconds, and a dropped frame in a grid
//!   somebody is scrolling is the same stutter as one in a shell.
//! - **It is capped before it is read.** A preview of a 400 MB scan serves
//!   nobody, and neither does decoding a 20000×20000 PNG because a tile came
//!   into view.
//! - **A file is asked about once.** The answer — including "no" — is kept, so
//!   scrolling past a row of names does not re-read them.
//!
//! The terminal's *hover* preview lives beside this in `dtr` because it draws at
//! the pointer with a terminal's look; the decode is here so the two cannot
//! disagree about what a preview is or how big a file may be.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use egui::TextureHandle;

/// The biggest source file a preview will read.
pub const MAX_BYTES: u64 = 16 << 20;

/// The biggest source image, in pixels per side: a 40000×100 limit is still a
/// decode nobody asked for.
const MAX_SOURCE: u32 = 8192;

/// What a single decode may allocate. The decoded image is the expensive half,
/// and it exists before any downscale can happen.
const MAX_ALLOC: u64 = 256 << 20;

/// What a hover preview is scaled to: 2× its largest draw size, so it is sharp on
/// a HiDPI display without being a texture per megapixel.
pub const MAX_WIDTH: u32 = 480;
pub const MAX_HEIGHT: u32 = 360;

/// The extensions worth opening. Only a hint — the decoder sniffs the bytes —
/// but it is what keeps every non-image in a directory from being read.
///
/// The list is exactly what the workspace's `image` features decode (PNG and
/// JPEG). A `.gif` on it would be a claim the decoder cannot keep: the read would
/// fail and the refusal would be cached, which is the same answer with a wasted
/// file read on the way to it.
const EXTENSIONS: &[&str] = &["png", "jpg", "jpeg"];

/// A decoded image, already small enough to be a texture.
#[derive(Clone, Debug)]
pub struct Thumb {
    pub width: u32,
    pub height: u32,
    /// RGBA8, straight alpha.
    pub rgba: Vec<u8>,
}

/// Whether a file is worth asking about, by its name.
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
///
/// `max` is the box the answer has to fit: a grid cell asks for its own size and
/// a hover preview asks for [`MAX_WIDTH`] × [`MAX_HEIGHT`], and the source caps
/// are the same for both because they are about the *file*, not about who wants
/// the picture.
pub fn load(path: &Path, max: (u32, u32)) -> anyhow::Result<Thumb> {
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
    // `thumbnail` enlarges as happily as it shrinks — a 1×1 icon "fits" a 480×360
    // box at 360×360 — and a preview that invents pixels is worse than none.
    let (width, height) = max;
    let scaled = if image.width() > width || image.height() > height {
        image.thumbnail(width, height)
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

/// How many decodes one grid may have running at once: a directory of
/// photographs must not start two hundred threads.
const IN_FLIGHT: usize = 4;

/// How many textures a grid keeps. A cell is its picture; a directory is longer
/// than this, and the point of the cache is a screenful, not a whole archive.
const CAPACITY: usize = 48;

/// What is known about one path.
enum Entry {
    /// A thread is reading it.
    Pending,
    /// Decoded and uploaded, ready to draw.
    Ready(TextureHandle),
    /// Asked and refused: too big, unreadable, or not a picture after all. Kept
    /// so the answer does not have to be worked out again.
    Failed,
}

struct Done {
    path: PathBuf,
    thumb: Option<Thumb>,
}

/// The pictures of the images in view.
///
/// One cache per browser rather than one per window: a pane's pictures are the
/// pane's, and two panes on two directories would otherwise evict each other's.
/// The cost is that the same photograph in two panes is decoded twice, which is
/// a file read each rather than a shared frame.
pub struct Thumbs {
    sender: Sender<Done>,
    inbox: Receiver<Done>,
    state: HashMap<PathBuf, Entry>,
    running: usize,
}

impl Default for Thumbs {
    fn default() -> Self {
        let (sender, inbox) = std::sync::mpsc::channel();
        Self {
            sender,
            inbox,
            state: HashMap::new(),
            running: 0,
        }
    }
}

impl Thumbs {
    /// Drop every picture: the listing under them is not the same one.
    pub fn clear(&mut self) {
        self.state.clear();
        // The threads already running are not cancelled — they are short — and
        // their answers are dropped by `poll` because the path is unknown.
        self.running = 0;
    }

    /// Turn whatever arrived into textures.
    ///
    /// Returns whether anything did, so the caller knows the picture changed.
    pub fn poll(&mut self, ctx: &egui::Context) -> bool {
        let mut changed = false;
        loop {
            match self.inbox.try_recv() {
                Ok(done) => {
                    self.running = self.running.saturating_sub(1);
                    let entry = match done.thumb {
                        Some(thumb) => Entry::Ready(ctx.load_texture(
                            done.path.to_string_lossy().as_ref(),
                            egui::ColorImage::from_rgba_unmultiplied(
                                [thumb.width as usize, thumb.height as usize],
                                &thumb.rgba,
                            ),
                            egui::TextureOptions::LINEAR,
                        )),
                        None => Entry::Failed,
                    };
                    // A path nobody is looking at any more is dropped rather than
                    // cached: the listing moved on while it was being read.
                    if let Some(slot) = self.state.get_mut(&done.path) {
                        *slot = entry;
                        changed = true;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        changed
    }

    /// The texture for `path`, asking for it the first time it is wanted.
    ///
    /// No context, because nothing is uploaded here: the decode runs on a thread
    /// and [`Thumbs::poll`] is what turns pixels into a texture, from the one
    /// place that has the drawing context.
    ///
    /// `None` means "not yet", not "no": the caller draws the file's type icon
    /// until the picture lands, which is what keeps scrolling smooth and a
    /// directory of photographs readable on the first frame.
    pub fn texture(&mut self, path: &Path, px: u32) -> Option<&TextureHandle> {
        if !self.state.contains_key(path) {
            if self.running >= IN_FLIGHT {
                // The queue is the next frame's problem; a caller that wants one
                // says so.
                return None;
            }
            self.state.insert(path.to_path_buf(), Entry::Pending);
            self.running += 1;
            let sender = self.sender.clone();
            let path = path.to_path_buf();
            let max = (px.max(16), px.max(16));
            std::thread::Builder::new()
                .name("dtr-thumb".to_owned())
                .spawn(move || {
                    let thumb = load(&path, max).ok();
                    // A closed channel means the browser is gone, which is not an
                    // error worth reporting.
                    let _ = sender.send(Done { path, thumb });
                })
                .ok()?;
        }
        // Bound the cache: the oldest entry is not tracked, so the whole map goes
        // when it is full. A grid redraws a screenful in a few frames, and a
        // dictionary of eviction policy for 48 textures is not worth the code.
        if self.state.len() > CAPACITY {
            let keep: Vec<PathBuf> = self.state.keys().take(CAPACITY / 2).cloned().collect();
            self.state.retain(|path, _| keep.contains(path));
        }
        match self.state.get(path) {
            Some(Entry::Ready(texture)) => Some(texture),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_image_is_recognised_by_its_name() {
        assert!(looks_like_image(Path::new("shot.png")));
        assert!(looks_like_image(Path::new("shot.JPG")));
        assert!(
            !looks_like_image(Path::new("scan.tiff")),
            "not a format the decoder has"
        );
        assert!(!looks_like_image(Path::new("notes.txt")));
        assert!(!looks_like_image(Path::new("Makefile")));
        assert!(!looks_like_image(Path::new(".hidden")));
    }

    /// A tiny PNG, written rather than embedded: the decode path is what is being
    /// checked, not the bytes of a fixture.
    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        let rgba = vec![0x40u8; (width * height * 4) as usize];
        let image = image::RgbaImage::from_raw(width, height, rgba).expect("pixels");
        image
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("encode");
        bytes
    }

    #[test]
    fn a_picture_is_downscaled_to_the_box_asked_for() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("big.png");
        std::fs::write(&path, png(600, 400)).expect("write");
        let thumb = load(&path, (64, 64)).expect("decoded");
        assert!(thumb.width <= 64 && thumb.height <= 64, "{thumb:?}");
        assert_eq!(thumb.rgba.len(), (thumb.width * thumb.height * 4) as usize);
        // And a small one is not enlarged: a picture that invents pixels is worse
        // than a small picture.
        let small = dir.path().join("small.png");
        std::fs::write(&small, png(20, 10)).expect("write");
        let thumb = load(&small, (64, 64)).expect("decoded");
        assert_eq!((thumb.width, thumb.height), (20, 10));
    }

    #[test]
    fn a_file_that_is_not_a_picture_is_a_reason_and_not_a_panic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fake.png");
        std::fs::write(&path, b"this is not an image").expect("write");
        assert!(load(&path, (64, 64)).is_err());
        assert!(load(&dir.path().join("absent.png"), (64, 64)).is_err());
    }

    /// Asking for a picture starts one read and not two, and a second ask while
    /// it is running does not start another — which is what stops a grid that
    /// draws the same tile every frame from forking a thread every frame.
    #[test]
    fn a_picture_is_asked_for_once() {
        let ctx = egui::Context::default();
        let mut thumbs = Thumbs::default();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("one.png");
        std::fs::write(&path, png(16, 16)).expect("write");

        assert!(
            thumbs.texture(&path, 32).is_none(),
            "the first ask yields a pending picture, not a texture"
        );
        assert_eq!(thumbs.running, 1, "one decode");
        assert!(thumbs.texture(&path, 32).is_none());
        assert_eq!(thumbs.running, 1, "the second ask starts no second decode");

        // The worker finishes eventually; polling is what turns the pixels into
        // a texture, and until then the caller draws the file's type icon.
        for _ in 0..400 {
            if thumbs.poll(&ctx) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            thumbs.texture(&path, 32).is_some(),
            "the picture arrived and was uploaded"
        );
        assert_eq!(thumbs.running, 0, "and nothing is still running");
    }

    /// A file that is not a picture is remembered as a no, so a grid does not
    /// re-read it every frame.
    #[test]
    fn a_refusal_is_remembered() {
        let ctx = egui::Context::default();
        let mut thumbs = Thumbs::default();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fake.png");
        std::fs::write(&path, b"not a picture").expect("write");

        assert!(thumbs.texture(&path, 32).is_none());
        for _ in 0..400 {
            if thumbs.poll(&ctx) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(thumbs.texture(&path, 32).is_none());
        assert_eq!(thumbs.running, 0, "the refusal was recorded, not retried");
    }
}
