//! Where an icon name becomes a file: the icon theme, its sizes, and the two
//! directories a desktop keeps them in.
//!
//! The lookup is the freedesktop icon-theme layout — `icons/<theme>/<size>/
//! <category>/<name>.png` — and it is deliberately free of GTK: a
//! terminal that linked GTK to read a folder's picture would be a terminal that
//! needs a desktop to start. What GTK would answer with is read from the same
//! `settings.ini` the palette's reader already parses.
//!
//! Three rules, each of which is a way this goes wrong:
//!
//! - **Never upscale.** A 48-point tile asks for 96 and takes the smallest size
//!   that is at least that, or the largest there is — a 16 px icon stretched to
//!   48 is a blur, and `scalable/` is SVG, which `image` cannot rasterise, so it
//!   is skipped rather than handed to a decoder that will fail on it.
//! - **`symbolic/` is not the picture.** Those are single-colour glyphs and a
//!   grid that picked one would draw a monochrome folder on a dark desktop,
//!   which looks like a rendering bug rather than a theme.
//! - **A name is a request.** `folder-music` is missing on a machine with only
//!   `hicolor`, so the caller passes a chain and the first file that exists
//!   wins.
//!
//! Reads are cached per theme directory: a grid asks about forty tiles a frame
//! and the size list is a `read_dir` each time otherwise.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::load::Bytes;

use crate::icons::{self, FOLDER, Places};

/// What kind of picture is being asked for. It is the directory name inside the
/// theme, not a semantic of ours: `places/` holds folders, `mimetypes/` holds
/// file types, `apps/` holds applications.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    Places,
    MimeTypes,
    Apps,
}

impl Category {
    fn dir(self) -> &'static str {
        match self {
            Self::Places => "places",
            Self::MimeTypes => "mimetypes",
            Self::Apps => "apps",
        }
    }
}

/// How good a candidate picture is for a request, smaller is better.
///
/// The order is a *rule about pixels*, not a preference about themes: a raster at
/// least as large as the request can be scaled down and stays sharp; a scalable
/// picture can be drawn at any size; a raster smaller than the request is
/// upscaled and soft; and a file with no declared size is the last thing to fall
/// back on.
///
/// Within a class, and between classes, the rule is:
///
/// - the nearest raster *at or above* the request wins, so 64 beats 128 for a 48;
/// - a scalable picture beats any raster that is too small;
/// - the largest raster below the request beats the smaller ones;
/// - a tie is broken by theme order, which is why both indexes are carried —
///   `Adwaita` before `hicolor` matters only when neither size is better.
///
/// Deriving `Ord` would compare the *size* first and get every one of those wrong
/// (16 < 64, so the smallest picture would always win), which is why this is a
/// rank key rather than a struct of fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rank {
    /// A raster at or above the requested size.
    AtLeast { size: u32, order: (usize, usize) },
    /// An SVG, drawn at whatever size is asked for.
    Scalable { order: (usize, usize) },
    /// A raster smaller than the request, which would be upscaled.
    Below { size: u32, order: (usize, usize) },
    /// A picture with no declared size: the legacy `pixmaps/` layout.
    Unsized { order: (usize, usize) },
}

impl Rank {
    /// The comparable key: class, then the size rule, then theme order.
    fn key(self) -> (u8, u32, (usize, usize)) {
        match self {
            // Nearest from above: the smaller of two that are both enough.
            Self::AtLeast { size, order } => (0, size, order),
            Self::Scalable { order } => (1, 0, order),
            // Largest from below: `u32::MAX - size` reverses the comparison.
            Self::Below { size, order } => (2, u32::MAX - size, order),
            Self::Unsized { order } => (3, 0, order),
        }
    }
}

impl Ord for Rank {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.key().cmp(&other.key())
    }
}

impl PartialOrd for Rank {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// The icon themes this machine has, and what sizes each one keeps.
pub struct Theme {
    /// The `share/` directories worth asking, nearest first.
    share: Vec<PathBuf>,
    /// Theme names in the order they are asked.
    names: Vec<String>,
    /// The size directories each `share/icons/<theme>` offers, read once.
    ///
    /// Keyed by `(share index, theme index)` rather than by path so a clone of
    /// the `Theme` cannot disagree with itself about a path it was handed.
    sizes: RefCell<HashMap<(usize, usize), Vec<u32>>>,
}

impl Theme {
    /// What this machine has.
    ///
    /// The theme name comes from `DTR_ICON_THEME` when it is set — which is how
    /// a test asks for a fixture tree, and how a person can say "use the one I
    /// installed" without a config key for it — then from GTK's own
    /// `gtk-icon-theme-name`, then `Adwaita`. `hicolor` is last and always:
    /// freedesktop makes it the fallback every package installs into, so a name
    /// that exists anywhere exists there.
    pub fn system() -> Self {
        let share = share_dirs();
        let mut names = Vec::new();
        if let Some(name) = std::env::var_os("DTR_ICON_THEME")
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string_lossy().into_owned())
        {
            names.push(name);
        }
        if let Some(name) = gtk_icon_theme() {
            if !names.contains(&name) {
                names.push(name);
            }
        }
        for fallback in ["Adwaita", "hicolor"] {
            if !names.iter().any(|name| name == fallback) {
                names.push(fallback.to_owned());
            }
        }
        Self::new(share, names)
    }

    pub fn new(share: Vec<PathBuf>, names: Vec<String>) -> Self {
        Self {
            share,
            names,
            sizes: RefCell::new(HashMap::new()),
        }
    }

    /// The first of `names` that has a file in `category`, at the size nearest
    /// `px` from above.
    ///
    /// The chain is the caller's, because what stands in for a missing picture is
    /// a decision about the screen: a grid's `folder-music` falls back to
    /// `folder`, and its `application-pdf` falls back to `text-x-generic`.
    pub fn find(&self, names: &[&str], category: Category, px: u32) -> Option<PathBuf> {
        for name in names {
            if let Some(path) = self.find_one(name, category, px) {
                return Some(path);
            }
        }
        None
    }

    fn find_one(&self, name: &str, category: Category, px: u32) -> Option<PathBuf> {
        let mut best: Option<(Rank, PathBuf)> = None;
        let mut offer = |rank: Rank, path: PathBuf| {
            let better = match &best {
                Some((known, _)) => rank < *known,
                None => true,
            };
            if better {
                best = Some((rank, path));
            }
        };

        for (share_index, share) in self.share.iter().enumerate() {
            for (theme_index, theme) in self.names.iter().enumerate() {
                let root = share.join("icons").join(theme);
                // A scalable picture is drawn at any size, so it beats any
                // raster that would have to be upscaled — but never a raster
                // that is already big enough.
                let scalable = root
                    .join("scalable")
                    .join(category.dir())
                    .join(format!("{name}.svg"));
                let order = (share_index, theme_index);
                if scalable.is_file() {
                    offer(Rank::Scalable { order }, scalable);
                }
                for size in self.sizes_of(share_index, theme_index, &root) {
                    let path = root
                        .join(format!("{size}x{size}"))
                        .join(category.dir())
                        .join(format!("{name}.png"));
                    if path.is_file() {
                        let order = (share_index, theme_index);
                        let rank = if size >= px {
                            Rank::AtLeast { size, order }
                        } else {
                            Rank::Below { size, order }
                        };
                        offer(rank, path);
                    }
                }
                // Some themes keep a few pictures at the top of the theme.
                let flat = root.join(category.dir()).join(format!("{name}.png"));
                if flat.is_file() {
                    offer(Rank::Unsized { order }, flat);
                }
            }
            // The legacy directory, which is flat and has no theme of its own.
            let pixmap = share.join("pixmaps").join(format!("{name}.png"));
            if pixmap.is_file() {
                offer(
                    Rank::Unsized {
                        order: (share_index, 0),
                    },
                    pixmap,
                );
            }
        }
        best.map(|(_, path)| path)
    }

    /// The sizes this theme actually offers, smallest first.
    fn sizes_of(&self, share: usize, theme: usize, root: &Path) -> Vec<u32> {
        let mut cached = self.sizes.borrow_mut();
        let sizes = cached.entry((share, theme)).or_insert_with(|| {
            let mut sizes = read_sizes(root);
            sizes.sort_unstable();
            sizes.dedup();
            sizes
        });
        sizes.clone()
    }
}

/// A picture ready for egui's own loader: the bytes, and the URI that caches
/// them.
///
/// The URI is the file's own path with a `bytes://` scheme, so it ends in `.png`
/// or `.svg` — which is how `egui_extras`' loaders decide which decoder to use —
/// and it is stable across frames, which is how egui's texture manager knows the
/// picture it already has is this one.
pub struct Picture {
    uri: String,
    bytes: Bytes,
}

impl std::fmt::Debug for Picture {
    /// The URI and the size, never the bytes: a failure message with 40 KiB of
    /// PNG in it is a log nobody reads.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Picture")
            .field("uri", &self.uri)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

impl Picture {
    pub fn uri(&self) -> &str {
        &self.uri
    }

    pub fn bytes(&self) -> &Bytes {
        &self.bytes
    }
}

/// How many theme files one frame may read.
///
/// The drawing thread reads these, so a directory of forty files must not be
/// forty reads in one frame on a cold network mount: the first frames show the
/// drawn glyph and ask for another frame, and the grid fills in. It is the same
/// bargain the listing's batches make, at a different size.
const MAX_READS_PER_FRAME: usize = 12;

/// The pictures a browser has resolved: the theme, the places, and one read per
/// entry.
///
/// The cache is keyed by the entry's path *and* the size asked for, because the
/// same file is a 48-point tile in one pane and a 96-point one in another, and
/// an SVG rasterised for one of those is the wrong texture for the other.
pub struct IconSet {
    theme: Theme,
    places: Places,
    loaded: RefCell<Loaded>,
}

#[derive(Default)]
struct Loaded {
    /// `None` is a name no theme had, remembered so a grid of forty misses is
    /// forty lookups once rather than forty a frame.
    pictures: HashMap<(PathBuf, u32), Option<Arc<Picture>>>,
    /// Reads spent this frame, and whether the frame ran out of budget.
    reads: Cell<usize>,
    exhausted: Cell<bool>,
}

impl IconSet {
    /// This machine's themes and places.
    pub fn system() -> Self {
        Self {
            theme: Theme::system(),
            places: Places::load(),
            loaded: RefCell::new(Loaded::default()),
        }
    }

    /// A set that never asks the filesystem, for a test whose picture must not
    /// depend on which theme the machine running it has.
    pub fn empty() -> Self {
        Self {
            theme: Theme::new(Vec::new(), Vec::new()),
            places: Places::default(),
            loaded: RefCell::new(Loaded::default()),
        }
    }

    /// The set a test can point at a fixture tree.
    pub fn with(theme: Theme, places: Places) -> Self {
        Self {
            theme,
            places,
            loaded: RefCell::new(Loaded::default()),
        }
    }

    /// Begin a frame: the read budget is per frame, not per set.
    pub fn start_frame(&self) {
        let loaded = self.loaded.borrow();
        loaded.reads.set(0);
        loaded.exhausted.set(false);
    }

    /// Whether this frame wanted a picture it could not read yet, so the caller
    /// knows to ask for another frame.
    pub fn wants_a_frame(&self) -> bool {
        self.loaded.borrow().exhausted.get()
    }

    pub fn places(&self) -> &Places {
        &self.places
    }

    /// The picture for an entry, by the three questions [`crate::icons`] asks:
    /// what it is, what its packaging says, and whether the desktop decorated
    /// it.
    ///
    /// `None` is a name the themes do not have — most names on a machine whose
    /// theme stops at 16 px — and the caller draws its own glyph instead.
    pub fn picture_for(
        &self,
        path: &Path,
        name: &str,
        is_dir: bool,
        px: u32,
    ) -> Option<Arc<Picture>> {
        let key = (path.to_path_buf(), px);
        if let Some(hit) = self.loaded.borrow().pictures.get(&key) {
            return hit.clone();
        }
        let mut loaded = self.loaded.borrow_mut();
        // Budget first: a cache miss costs a read, and this frame may have spent
        // its allowance on the tiles above this one.
        if loaded.exhausted.get() {
            return None;
        }
        if loaded.reads.get() >= MAX_READS_PER_FRAME {
            loaded.exhausted.set(true);
            drop(loaded);
            // The work exists and is not done: ask for the frame that will do it.
            return None;
        }
        loaded.reads.set(loaded.reads.get() + 1);

        let found = self.resolve(path, name, is_dir, px);
        loaded.pictures.insert(key, found.clone());
        found
    }

    fn resolve(&self, path: &Path, name: &str, is_dir: bool, px: u32) -> Option<Arc<Picture>> {
        if is_dir {
            // A decorated place first, then the plain folder: `folder-download`
            // is missing from a theme that only carries the generic set.
            let decorated = self.places.of(path);
            let names: Vec<&str> = match decorated {
                Some(icon) => vec![icon, FOLDER],
                None => vec![FOLDER],
            };
            return self.load(&names, Category::Places, px);
        }
        // A `.desktop` file's own `Icon=` is a better answer than "text file",
        // and it is asked before the type's name for that reason.
        let own = icons::desktop_icon(path);
        let kind = icons::type_icon(name);
        let names: Vec<&str> = match own.as_deref() {
            Some(own) => vec![own, kind],
            None => vec![kind],
        };
        self.load(&names, Category::MimeTypes, px)
    }

    fn load(&self, names: &[&str], category: Category, px: u32) -> Option<Arc<Picture>> {
        let path = self.theme.find(names, category, px)?;
        let bytes = std::fs::read(&path).ok()?;
        let uri = format!(
            "bytes://dtr-icons{}",
            path.to_string_lossy().replace('\\', "/")
        );
        Some(Arc::new(Picture {
            uri,
            bytes: Bytes::from(Arc::from(bytes)),
        }))
    }
}

/// The `share/` directories a desktop reads icons from, nearest first.
fn share_dirs() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(data) = std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        push(&mut roots, PathBuf::from(data));
    } else if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        push(&mut roots, PathBuf::from(home).join(".local/share"));
    }
    let dirs = std::env::var_os("XDG_DATA_DIRS")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| std::ffi::OsString::from("/usr/local/share:/usr/share"));
    for dir in std::env::split_paths(&dirs) {
        push(&mut roots, dir);
    }
    // NixOS keeps the merged profile outside the data dirs on a bare system
    // shell, and a machine that has it wants its icons.
    for extra in ["/run/current-system/sw/share", "/usr/share"] {
        push(&mut roots, PathBuf::from(extra));
    }
    roots
}

fn push(roots: &mut Vec<PathBuf>, root: PathBuf) {
    if !roots.contains(&root) {
        roots.push(root);
    }
}

/// The size directories of one theme, `64x64` and `64x64@2` counting as 64.
fn read_sizes(root: &Path) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut sizes = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        // `symbolic/` and `scalable/` are not raster pictures at a size.
        if name == "symbolic" || name == "scalable" {
            continue;
        }
        if let Some(size) = name
            .split(['x', '@'])
            .next()
            .and_then(|width| width.parse::<u32>().ok())
        {
            sizes.push(size);
        }
    }
    sizes
}

/// `gtk-icon-theme-name` from the GTK settings files, if a desktop wrote one.
///
/// The same three files the palette's reader parses, and for the same reason: it
/// is the desktop's own answer and no link to GTK is needed to read a `key=value`
/// line. `[Settings]` only, because that is the group GTK reads.
fn gtk_icon_theme() -> Option<String> {
    let home = dirs::home_dir()?;
    for version in ["gtk-4.0", "gtk-3.0"] {
        let path = home.join(".config").join(version).join("settings.ini");
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Some(name) = setting(&text, "gtk-icon-theme-name") {
            return Some(name);
        }
    }
    None
}

/// One `key=value` from a file's `[Settings]` group.
fn setting(text: &str, key: &str) -> Option<String> {
    let mut in_settings = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(group) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            in_settings = group == "Settings";
            continue;
        }
        if !in_settings {
            continue;
        }
        if let Some((name, value)) = line.split_once('=') {
            if name.trim() == key {
                let value = value.trim();
                if !value.is_empty() {
                    return Some(value.to_owned());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A theme laid out the way a real one is, at two sizes and in two
    /// categories, under a `share/` of its own.
    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let share = dir.path().join("share");
        let theme = share.join("icons").join("Test");
        for (size, category, name) in [
            (16, "places", "folder"),
            (64, "places", "folder"),
            (128, "places", "folder"),
            (64, "places", "folder-download"),
            (64, "mimetypes", "text-x-generic"),
            (64, "mimetypes", "application-pdf"),
        ] {
            let at = theme
                .join(format!("{size}x{size}"))
                .join(category)
                .join(format!("{name}.png"));
            std::fs::create_dir_all(at.parent().expect("parent")).expect("mkdir");
            std::fs::write(&at, b"png").expect("write");
        }
        // The two directory names that are not raster sizes must not be asked
        // for as if they were.
        std::fs::create_dir_all(theme.join("symbolic/places")).expect("mkdir");
        std::fs::create_dir_all(theme.join("scalable/mimetypes")).expect("mkdir");
        // A scalable picture, which is a real answer on a machine whose raster
        // set stops at 16 px — the case this machine is.
        std::fs::write(
            theme.join("scalable/mimetypes/text-x-generic.svg"),
            b"<svg/>",
        )
        .expect("write");
        (dir, share)
    }

    fn theme(share: PathBuf) -> Theme {
        Theme::new(vec![share], vec!["Test".to_owned(), "hicolor".to_owned()])
    }

    #[test]
    fn a_size_at_least_the_one_asked_for_is_the_one_that_answers() {
        let (_dir, share) = fixture();
        let theme = theme(share);
        let found = theme
            .find(&["folder"], Category::Places, 48)
            .expect("a folder");
        assert!(found.ends_with("64x64/places/folder.png"), "{found:?}");
        // Asking for more than the theme has takes the largest, not nothing: a
        // 48 px tile on a 128 px picture is a downscale, not a blur.
        let big = theme
            .find(&["folder"], Category::Places, 96)
            .expect("a folder");
        assert!(big.ends_with("128x128/places/folder.png"), "{big:?}");
        // And a small ask stays small rather than upscaling the 128.
        let small = theme
            .find(&["folder"], Category::Places, 12)
            .expect("a folder");
        assert!(small.ends_with("16x16/places/folder.png"), "{small:?}");
    }

    #[test]
    fn a_name_a_theme_does_not_have_falls_through_the_chain() {
        let (_dir, share) = fixture();
        let theme = theme(share.clone());
        // The decorated name is missing at 128: the generic folder answers.
        let decorated = theme
            .find(&["folder-download"], Category::Places, 128)
            .expect("the download folder");
        assert!(
            decorated.ends_with("64x64/places/folder-download.png"),
            "a missing size falls back to the largest below it: {decorated:?}"
        );
        let chain = theme
            .find(&["folder-nonexistent", "folder"], Category::Places, 64)
            .expect("the chain's second name");
        assert!(chain.ends_with("64x64/places/folder.png"), "{chain:?}");
        assert_eq!(theme.find(&["nothing"], Category::Apps, 64), None);
    }

    /// A theme with no `symbolic/` or `scalable/` entry asked for by size: the
    /// names are not sizes and a decoder handed an SVG fails on it.
    /// An SVG is drawn at any size, so it beats a raster that would have to be
    /// upscaled — and loses to one that is already big enough. Getting this the
    /// wrong way round is what made a 16 px theme icon answer a 96 px request.
    #[test]
    fn a_scalable_picture_beats_a_raster_that_is_too_small() {
        let (_dir, share) = fixture();
        let theme = theme(share);
        let small = theme
            .find(&["text-x-generic"], Category::MimeTypes, 96)
            .expect("a picture");
        assert!(
            small.ends_with("scalable/mimetypes/text-x-generic.svg"),
            "{small:?}"
        );
        // At 48 the 64 px raster is already enough, and it needs no rasterising.
        let enough = theme
            .find(&["text-x-generic"], Category::MimeTypes, 48)
            .expect("a picture");
        assert!(
            enough.ends_with("64x64/mimetypes/text-x-generic.png"),
            "{enough:?}"
        );
    }

    #[test]
    fn the_two_non_raster_directories_are_not_sizes() {
        let (_dir, share) = fixture();
        let root = share.join("icons/Test");
        let mut sizes = read_sizes(&root);
        sizes.sort_unstable();
        assert_eq!(sizes, vec![16, 64, 128]);
    }

    #[test]
    fn a_picture_is_read_once_and_kept_by_its_path() {
        let (_dir, share) = fixture();
        let set = IconSet::with(theme(share), Places::default());
        set.start_frame();
        let folder = set
            .picture_for(Path::new("/somewhere/folder"), "folder", true, 64)
            .expect("the fixture has a folder");
        assert!(
            folder.uri().starts_with("bytes://dtr-icons/"),
            "{}",
            folder.uri()
        );
        assert!(folder.uri().ends_with(".png"), "{}", folder.uri());
        assert!(!folder.bytes().is_empty());

        // A second ask is a cache hit, not a read: the budget it would have
        // spent is still there, which is what a directory of forty tiles needs.
        let again = set
            .picture_for(Path::new("/somewhere/folder"), "folder", true, 64)
            .expect("the same folder");
        assert!(std::sync::Arc::ptr_eq(&folder, &again));

        // And a spent budget is spent on *reads*: a picture already in hand is
        // still answered, which is why a scrolling grid does not go blank.
        for index in 0..MAX_READS_PER_FRAME {
            let _ = set.picture_for(
                Path::new(&format!("/elsewhere/{index}")),
                "text-x-generic",
                false,
                64,
            );
        }
        assert!(set.wants_a_frame());
        assert!(
            set.picture_for(Path::new("/somewhere/folder"), "folder", true, 64)
                .is_some(),
            "a cache hit is not a read"
        );
    }

    /// The desktop's decoration is asked first and the generic folder is the
    /// fallback, in one lookup: `folder-download` exists in the fixture and
    /// `folder-music` does not.
    #[test]
    fn a_decorated_place_asks_for_its_own_picture() {
        let (_dir, share) = fixture();
        let places = Places::parse(
            "XDG_DOWNLOAD_DIR=\"/downloads\"\nXDG_MUSIC_DIR=\"/music\"\n",
            Path::new("/home/ada"),
        );
        let set = IconSet::with(theme(share), places);
        set.start_frame();
        let downloads = set
            .picture_for(Path::new("/downloads"), "downloads", true, 64)
            .expect("the download picture");
        assert!(
            downloads.uri().ends_with("places/folder-download.png"),
            "{}",
            downloads.uri()
        );
        let music = set
            .picture_for(Path::new("/music"), "music", true, 64)
            .expect("the generic folder stands in");
        assert!(
            music.uri().ends_with("places/folder.png"),
            "{}",
            music.uri()
        );
    }

    /// One frame reads a bounded number of files and says so, so the grid can ask
    /// for another frame instead of stalling this one.
    #[test]
    fn a_frame_may_only_read_so_many_files() {
        let (_dir, share) = fixture();
        let set = IconSet::with(theme(share), Places::default());
        set.start_frame();
        for index in 0..MAX_READS_PER_FRAME {
            assert!(
                set.picture_for(
                    Path::new(&format!("/many/{index}")),
                    "text-x-generic",
                    false,
                    64
                )
                .is_some(),
                "read {index} of the budget"
            );
        }
        assert!(!set.wants_a_frame());
        assert!(
            set.picture_for(Path::new("/many/over"), "text-x-generic", false, 64)
                .is_none(),
            "the frame has spent its reads"
        );
        assert!(set.wants_a_frame(), "and it should ask for another frame");
        // The next frame starts with the budget back.
        set.start_frame();
        assert!(
            set.picture_for(Path::new("/many/over"), "text-x-generic", false, 64)
                .is_some()
        );
    }

    /// A set that asks nobody: what a snapshot uses, so a baseline cannot depend
    /// on the theme of the machine that made it.
    #[test]
    fn an_empty_set_never_answers() {
        let set = IconSet::empty();
        set.start_frame();
        assert!(
            set.picture_for(Path::new("/anything"), "folder", true, 64)
                .is_none()
        );
        assert!(
            !set.wants_a_frame(),
            "asking nobody is not a reason to redraw"
        );
    }

    /// `[Settings]` only, and only the key asked for: a `gtk-theme-name` is what
    /// the look reads, and this is the icons' one line beside it.
    #[test]
    fn the_icon_theme_is_read_from_the_settings_file() {
        let text = "\
[Settings]
gtk-theme-name=adw-gtk3-dark
gtk-icon-theme-name=Papirus-Dark
";
        assert_eq!(
            setting(text, "gtk-icon-theme-name").as_deref(),
            Some("Papirus-Dark")
        );
        assert_eq!(
            setting("[Other]\ngtk-icon-theme-name=x\n", "gtk-icon-theme-name"),
            None
        );
        assert_eq!(
            setting("[Settings]\ngtk-icon-theme-name=\n", "gtk-icon-theme-name"),
            None
        );
    }
}
