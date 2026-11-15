//! The desktop's own answers, as the grid wants them.
//!
//! [`dtr_shell`] is the Win32 half and knows nothing about egui: it answers an
//! [`Icon`](dtr_shell::Icon) — pixels and a size — and a list of [`Place`]s. This is
//! the adapter that turns pixels into a [`Picture`] (bytes and a URI, which is what
//! egui's loader takes) and places into menu labels. The icon *theme* half lives in
//! [`crate::theme`], which asks this first and its own theme second.
//!
//! On a freedesktop machine there is no shell to ask and every function here is
//! `None` or empty; `docs/windows.md` §11 has the account.

use std::path::{Path, PathBuf};

use crate::theme::Picture;

/// The shell's icon for an entry, encoded as the PNG egui's loader takes.
///
/// `None` where there is no shell, and where the shell would not produce a picture:
/// a broken association, a file that vanished between the listing and the draw, a
/// monochrome icon with no colour bitmap. The caller's next question — the theme,
/// then its own drawn glyph — is what a person sees then.
pub fn picture(path: &Path, is_dir: bool, px: u32) -> Option<Picture> {
    let icon = dtr_shell::icon(path, is_dir, px)?;
    let png = png_of(&icon.rgba, icon.width, icon.height)?;
    Some(Picture::from_png(uri(path, px), png))
}

/// The platform's places: the user's known folders and the machine's drives on
/// Windows, nothing on a machine whose places are its desktop's decorations.
pub fn places() -> Vec<(String, PathBuf)> {
    dtr_shell::places()
        .into_iter()
        .map(|place| (place.label, place.path))
        .collect()
}

/// RGBA as a PNG, which is the only thing egui's loader takes besides a file.
///
/// Encoding on the way in, rather than holding the pixels, is what keeps this out
/// of the texture manager: a `Picture` is bytes and a URI, and the theme's are
/// exactly that.
fn png_of(rgba: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    use image::{ExtendedColorType, ImageEncoder};
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(rgba, width, height, ExtendedColorType::Rgba8)
        .ok()?;
    Some(png)
}

/// A `bytes://` URI that is the entry and the size, so egui's texture cache keeps
/// this picture while the tile is on screen and two tiles cannot share one.
///
/// The path is hashed rather than spelled out. A picture URI has to be stable across
/// frames and unique per entry, and a *readable* one cannot be both: the loader
/// decides by the last extension, so a path may not be used as it stands, and a
/// flattening of it (`\` and a space both becoming `_`) collides for two names that
/// differ in exactly those characters — which would draw one entry's icon on
/// another. The hash of the path has neither problem, and the size is appended
/// because a 48-point tile and a 96-point one cannot share a texture.
fn uri(path: &Path, px: u32) -> String {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    format!("bytes://dtr-shell/{:016x}.{px}.png", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of encoding on the way in: what goes to egui is a PNG that
    /// reads back as the pixels the shell gave, channels and alpha included.
    #[test]
    fn an_icon_goes_to_the_loader_as_a_png_of_its_pixels() {
        let rgba = [0u8, 0, 255, 255, 255, 0, 0, 128];
        let png = png_of(&rgba, 2, 1).expect("a png");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let decoded = image::load_from_memory(&png).expect("a picture").to_rgba8();
        assert_eq!(decoded.dimensions(), (2, 1));
        assert_eq!(decoded.into_raw(), rgba);
    }

    /// A URI is the entry and the size, and ends in `.png` exactly once — the loader
    /// picks its decoder by that extension. Two sizes of one file are two pictures,
    /// because a 48-point tile and a 96-point one cannot share a texture, and two
    /// names that flatten alike are still two pictures.
    #[test]
    fn a_picture_uri_is_the_entry_and_the_size() {
        let notes = Path::new(r"C:\fixture\notes.txt");
        assert_eq!(uri(notes, 48), uri(notes, 48));
        assert_ne!(uri(notes, 48), uri(notes, 96));
        let small = uri(notes, 48);
        assert!(small.ends_with(".png"), "{small}");
        assert_eq!(small.matches(".png").count(), 1, "{small}");
        // The pair a flattened path would have confused: a space against a
        // separator.
        assert_ne!(uri(Path::new("/a/b c"), 48), uri(Path::new("/a/b_c"), 48));
    }
}
