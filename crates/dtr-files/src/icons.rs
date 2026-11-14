//! What an entry's picture is called, and where the desktop already said so.
//!
//! The grid asks three questions about a tile's icon, in this order
//! (`docs/manager.md` §3):
//!
//! 1. **What the entry is.** A directory is `folder`; a file's extension decides
//!    a theme name — `image-x-generic`, `application-pdf`, `text-x-script` — and
//!    an unknown file is `text-x-generic`, which is what Nautilus draws too.
//! 2. **What the packaging says.** A `.desktop` file's own `Icon=` is a better
//!    answer than "this is a text file", and so is the icon of an executable's
//!    desktop entry.
//! 3. **Whether the desktop decorated it.** A directory named in
//!    `user-dirs.dirs` is `folder-download` or `folder-pictures`, which is why
//!    `~/Downloads` carries an arrow in Nautilus.
//!
//! This module is the *naming* half and knows nothing about textures: it answers
//! with freedesktop icon names, which [`crate::theme`] then looks up. That split
//! is what makes the table testable without a machine that has icons installed.
//!
//! **A name is a request, not a promise.** Every name here is one the icon theme
//! may not have — a machine with only `hicolor` has no `folder-music` — so the
//! caller falls back to a drawn glyph, and a grid with a missing picture is a
//! grid of pages, not a grid of holes.

use std::path::{Path, PathBuf};

/// The icon-theme name for a file, from its name alone.
///
/// Directories are [`FOLDER`] and are not this function's business: the caller
/// has an `is_dir` and asking a name whether it is a directory would make the
/// table lie for a file called `Documents`.
pub fn type_icon(name: &str) -> &'static str {
    let extension = extension_of(name);
    match extension.as_str() {
        // Pictures, which a grid would rather show a thumbnail of; this is what
        // is drawn until the decode answers.
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "svg" | "avif"
        | "heic" | "ico" => "image-x-generic",
        "mp4" | "mkv" | "webm" | "avi" | "mov" | "m4v" | "mpg" | "mpeg" | "ogv" => {
            "video-x-generic"
        }
        "mp3" | "flac" | "ogg" | "opus" | "wav" | "m4a" | "aac" | "wma" => "audio-x-generic",
        "pdf" => "application-pdf",
        "zip" | "tar" | "gz" | "xz" | "zst" | "bz2" | "7z" | "rar" | "tgz" | "tbz" | "lz4"
        | "lz" => "package-x-generic",
        "doc" | "docx" | "odt" | "rtf" | "tex" => "x-office-document",
        "xls" | "xlsx" | "ods" | "csv" | "tsv" => "x-office-spreadsheet",
        "ppt" | "pptx" | "odp" | "key" => "x-office-presentation",
        "iso" | "img" | "vmdk" | "qcow2" => "application-x-cd-image",
        "exe" | "msi" | "deb" | "rpm" | "appimage" | "flatpak" | "snap" | "apk" => {
            "application-x-executable"
        }
        "ttf" | "otf" | "woff" | "woff2" => "font-x-generic",
        "desktop" => "application-x-desktop",
        "toml" | "yaml" | "yml" | "json" | "ini" | "conf" | "cfg" => "text-x-generic",
        // Scripts are the ones a person runs, and the theme has a picture that
        // says so; source that is not a script is a text file with a syntax.
        "sh" | "bash" | "zsh" | "fish" | "py" | "pl" | "rb" | "lua" | "ps1" | "bat" => {
            "text-x-script"
        }
        "rs" | "c" | "h" | "cc" | "cpp" | "hpp" | "go" | "zig" | "java" | "ts" | "js" | "jsx"
        | "tsx" | "html" | "htm" | "css" | "scss" | "xml" | "sql" | "md" | "rst" | "txt"
        | "log" | "diff" | "patch" | "el" | "vim" | "nix" | "elv" => "text-x-generic",
        _ => "text-x-generic",
    }
}

/// The icon-theme name of a directory that is not a place.
pub const FOLDER: &str = "folder";

/// The lowercased extension of a name, or `""`.
///
/// The same rule the sort uses ([`crate::browser`]'s `extension_of`): after the
/// *last* dot, so `archive.tar.gz` is a `gz`. A name with no dot, and a dotfile
/// like `.bashrc`, have no extension.
pub(crate) fn extension_of(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() && !extension.is_empty() => {
            extension.to_ascii_lowercase()
        }
        _ => String::new(),
    }
}

/// The places the desktop has decorated, and the icon each one asks for.
///
/// `user-dirs.dirs` is the desktop's own list of "this directory is the
/// downloads one"; the icons are freedesktop's `folder-*` names. Loaded once per
/// listing rather than per tile: a grid asks about forty entries a frame and this
/// is a file read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Places {
    /// The expanded directory and the icon name that decorates it.
    entries: Vec<(PathBuf, &'static str)>,
}

impl Places {
    /// Read `$XDG_CONFIG_HOME/user-dirs.dirs`, or `~/.config/user-dirs.dirs`.
    ///
    /// A machine without the file — most Windows and macOS installs, a minimal
    /// Linux one — has no decorations, which is `Places::default()` and not an
    /// error: every directory is then simply [`FOLDER`].
    pub fn load() -> Self {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|home| home.join(".config")));
        let home = dirs::home_dir().unwrap_or_default();
        match config {
            Some(config) => Self::load_from(&config.join("user-dirs.dirs"), &home),
            None => Self::default(),
        }
    }

    /// The same, from an exact file and home: what a test does.
    pub fn load_from(path: &Path, home: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        Self::parse(&text, home)
    }

    /// Parse the file's contents, `$HOME` expanded and quotes stripped.
    ///
    /// The format is `XDG_DOWNLOAD_DIR="$HOME/Downloads"` — one shell-ish
    /// assignment per line, comments with `#`. A value that expands to `$HOME`
    /// itself is the desktop saying "this one is disabled": a whole home
    /// directory is not the downloads directory, so it is dropped rather than
    /// decorated with an arrow.
    pub fn parse(text: &str, home: &Path) -> Self {
        let mut entries = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let Some(icon) = place_icon(key.trim()) else {
                continue;
            };
            let value = value.trim().trim_matches('"');
            let Some(path) = expand(value, home) else {
                continue;
            };
            if path == home || path.as_os_str().is_empty() {
                continue;
            }
            if entries.iter().any(|(known, _)| *known == path) {
                continue;
            }
            entries.push((path, icon));
        }
        Self { entries }
    }

    /// The icon for a directory, if the desktop decorated it.
    pub fn of(&self, path: &Path) -> Option<&'static str> {
        self.entries
            .iter()
            .find(|(known, _)| known == path)
            .map(|(_, icon)| *icon)
    }

    /// How many places were read, for a test that wants to know the file parsed.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The icon name for a `user-dirs.dirs` key.
fn place_icon(key: &str) -> Option<&'static str> {
    Some(match key {
        "XDG_DESKTOP_DIR" => "folder-desktop",
        "XDG_DOCUMENTS_DIR" => "folder-documents",
        "XDG_DOWNLOAD_DIR" => "folder-download",
        "XDG_MUSIC_DIR" => "folder-music",
        "XDG_PICTURES_DIR" => "folder-pictures",
        "XDG_PUBLICSHARE_DIR" => "folder-publicshare",
        "XDG_TEMPLATES_DIR" => "folder-templates",
        "XDG_VIDEOS_DIR" => "folder-videos",
        _ => return None,
    })
}

/// `$HOME/Downloads` against a home directory.
///
/// Only `$HOME` and `${HOME}` are understood, which is all the spec writes: a
/// value with `$USER` in it, or a `~`, is somebody's own invention and expanding
/// it wrongly would put an arrow on an unrelated directory.
fn expand(value: &str, home: &Path) -> Option<PathBuf> {
    let rest = if let Some(rest) = value.strip_prefix("${HOME}") {
        Some(rest)
    } else {
        value.strip_prefix("$HOME")
    };
    match rest {
        Some(rest) => {
            let rest = rest.strip_prefix('/').unwrap_or(rest);
            if rest.is_empty() {
                Some(home.to_path_buf())
            } else {
                Some(home.join(rest))
            }
        }
        None if value.starts_with('/') => Some(PathBuf::from(value)),
        None if value.is_empty() => None,
        // A relative value is legal in the file and means "under home".
        None => Some(home.join(value)),
    }
}

/// The `Icon=` of a `.desktop` file, which is its own picture and not its type's.
///
/// Only the `[Desktop Entry]` group counts: a `[Desktop Action …]` group carries
/// an icon too, and answering with an action's would put "new window" on the
/// application. Capped at 16 KiB, because a file called `x.desktop` need not be
/// one and the pane draws what the listing said without opening it first.
pub fn desktop_icon(path: &Path) -> Option<String> {
    if extension_of(&path.file_name()?.to_string_lossy()) != "desktop" {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(16 * 1024)]);
    entry_icon(&text)
}

/// The `[Desktop Entry]` group's `Icon=`, from text.
pub fn entry_icon(text: &str) -> Option<String> {
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(group) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            in_entry = group == "Desktop Entry";
            continue;
        }
        if !in_entry {
            continue;
        }
        if let Some(icon) = line.strip_prefix("Icon=") {
            let icon = icon.trim();
            if !icon.is_empty() {
                return Some(icon.to_owned());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_is_a_picture_and_a_script_is_a_script() {
        assert_eq!(type_icon("holiday.PNG"), "image-x-generic");
        assert_eq!(type_icon("scan.pdf"), "application-pdf");
        assert_eq!(type_icon("deploy.sh"), "text-x-script");
        assert_eq!(type_icon("main.rs"), "text-x-generic");
        assert_eq!(type_icon("archive.tar.gz"), "package-x-generic");
        assert_eq!(type_icon("song.flac"), "audio-x-generic");
        assert_eq!(type_icon("clip.mkv"), "video-x-generic");
        assert_eq!(type_icon("report.xlsx"), "x-office-spreadsheet");
        assert_eq!(type_icon("dtr.exe"), "application-x-executable");
    }

    /// A name that says nothing is a text file, which is what Nautilus draws and
    /// what a person expects of an unknown extension: a question mark icon would
    /// make every build artefact look broken.
    #[test]
    fn an_unknown_name_is_a_plain_file() {
        assert_eq!(type_icon("Makefile"), "text-x-generic");
        assert_eq!(type_icon(".bashrc"), "text-x-generic");
        assert_eq!(type_icon("weird.qqq"), "text-x-generic");
        assert_eq!(type_icon(""), "text-x-generic");
    }

    #[test]
    fn the_places_are_the_desktops_own_and_its_arrows_follow() {
        let home = Path::new("/home/ada");
        let text = "\
# written by xdg-user-dirs-update
XDG_DOWNLOAD_DIR=\"$HOME/Downloads\"
XDG_DOCUMENTS_DIR=\"${HOME}/Documents\"
XDG_MUSIC_DIR=\"/srv/music\"
XDG_DESKTOP_DIR=\"$HOME\"
XDG_CONFIG_DIR=\"$HOME/.config\"
";
        let places = Places::parse(text, home);
        assert_eq!(
            places.of(Path::new("/home/ada/Downloads")),
            Some("folder-download")
        );
        assert_eq!(
            places.of(Path::new("/home/ada/Documents")),
            Some("folder-documents")
        );
        assert_eq!(places.of(Path::new("/srv/music")), Some("folder-music"));
        // `$HOME` itself is "disabled", and a key that is not a place is not one.
        assert_eq!(places.of(home), None);
        assert_eq!(places.of(Path::new("/home/ada/.config")), None);
        // And a directory nobody named is a plain folder.
        assert_eq!(places.of(Path::new("/home/ada/src")), None);
        assert_eq!(places.len(), 3);
    }

    #[test]
    fn a_missing_file_is_no_places_rather_than_an_error() {
        let places = Places::load_from(
            Path::new("/nonexistent/user-dirs.dirs"),
            Path::new("/home/ada"),
        );
        assert!(places.is_empty());
        assert_eq!(places.of(Path::new("/home/ada/Downloads")), None);
    }

    /// The picture on a `.desktop` file is the application's, not an *action's*:
    /// both are `Icon=` lines in the same file, and answering with the second
    /// puts "new window" on the app in the grid.
    #[test]
    fn a_desktop_entry_answers_with_its_own_icon() {
        let text = "\
[Desktop Entry]
Type=Application
Exec=dtr
Icon=dtr

[Desktop Action new-window]
Exec=dtr --new
Icon=window-new
";
        assert_eq!(entry_icon(text).as_deref(), Some("dtr"));
        // No `[Desktop Entry]` group at all: nothing to answer with.
        assert_eq!(entry_icon("[Desktop Action x]\nIcon=y\n"), None);
        assert_eq!(entry_icon("[Desktop Entry]\nIcon=\n"), None);
    }

    #[test]
    fn a_desktop_icon_is_only_read_from_a_desktop_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = dir.path().join("thing.desktop");
        std::fs::write(&entry, "[Desktop Entry]\nIcon=thing\n").expect("write");
        assert_eq!(desktop_icon(&entry).as_deref(), Some("thing"));

        // The same bytes in a file with another name are not an entry, and a
        // missing one is not an error.
        let other = dir.path().join("thing.txt");
        std::fs::write(&other, "[Desktop Entry]\nIcon=thing\n").expect("write");
        assert_eq!(desktop_icon(&other), None);
        assert_eq!(desktop_icon(&dir.path().join("absent.desktop")), None);
    }
}
