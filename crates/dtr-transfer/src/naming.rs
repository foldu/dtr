//! The "keep both" name rule, as pure functions.
//!
//! A copy whose destination exists has three answers (`docs/manager.md` §7),
//! and one of them must invent a name. The rule is the one every file manager
//! uses — `name (2).ext`, `name (3).ext`, … — because a person who has just
//! said "keep both" is looking for the second one to be obviously the second.
//!
//! The name is a [`Path`], not a `String`: a Unix file name is bytes, and
//! round-tripping one through `to_string_lossy` to add a number would move the
//! file the user asked for.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The first number a "keep both" name tries. `name` is taken, so the next one
/// is `name (2)`.
pub const FIRST_SUFFIX: u32 = 2;

/// The number of suffixes tried before giving up: a directory with two thousand
/// copies of one name is not a directory a transfer should keep probing.
pub const LAST_SUFFIX: u32 = 2_000;

/// The `n`th candidate for `name`: the stem, ` (n)`, then the extension.
///
/// The extension is whatever [`Path::extension`] says it is, so `a.tar.gz`
/// becomes `a.tar (2).gz` and a dotfile has no extension at all:
/// `.env` becomes `.env (2)`.
pub fn numbered_name(name: &Path, n: u32) -> PathBuf {
    let extension = name.extension();
    let stem = name.file_stem().unwrap_or(name.as_os_str());
    let mut numbered = OsString::from(stem);
    numbered.push(format!(" ({n})"));
    if let Some(extension) = extension {
        numbered.push(".");
        numbered.push(extension);
    }
    PathBuf::from(numbered)
}

/// Walk the candidates in order, asking `taken` until it answers `false`.
///
/// The caller's `taken` is the filesystem *and* anything else that must not be
/// chosen; this function owns the naming rule only.
pub fn free_name(name: &Path, mut taken: impl FnMut(&Path) -> bool) -> Option<PathBuf> {
    (FIRST_SUFFIX..=LAST_SUFFIX)
        .map(|n| numbered_name(name, n))
        .find(|candidate| !taken(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(name: &str, n: u32) -> String {
        numbered_name(Path::new(name), n)
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn keep_both_names_keep_the_extension() {
        assert_eq!(numbered("report.txt", 2), "report (2).txt");
        assert_eq!(numbered("report.txt", 10), "report (10).txt");
    }

    #[test]
    fn a_multi_part_extension_keeps_its_last_part() {
        // `a.tar.gz` is a gzip of a tar: the number belongs on `a.tar`.
        assert_eq!(numbered("a.tar.gz", 3), "a.tar (3).gz");
    }

    #[test]
    fn a_dotfile_is_numbered_as_a_whole_name() {
        // `.env` has no extension — the dotfile is the name.
        assert_eq!(numbered(".env", 2), ".env (2)");
        assert_eq!(numbered(".config.toml", 2), ".config (2).toml");
    }

    #[test]
    fn a_name_without_an_extension_just_gets_a_number() {
        assert_eq!(numbered("README", 2), "README (2)");
        assert_eq!(numbered("archive", 7), "archive (7)");
    }

    #[test]
    fn a_trailing_dot_survives_the_round_trip() {
        assert_eq!(numbered("odd.", 2), "odd (2).");
    }

    #[test]
    fn the_walk_stops_at_the_first_free_name() {
        let mut asked = Vec::new();
        let chosen = free_name(Path::new("x.txt"), |candidate| {
            asked.push(candidate.to_path_buf());
            // `x (2).txt` is taken, `x (3).txt` is not.
            asked.len() < 2
        });
        assert_eq!(chosen.as_deref(), Some(Path::new("x (3).txt")));
        assert_eq!(asked.len(), 2);
    }

    #[test]
    fn the_walk_gives_up_rather_than_looping() {
        assert_eq!(free_name(Path::new("x"), |_| true), None);
    }

    #[test]
    fn a_non_utf8_name_is_preserved() {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            let raw = std::ffi::OsStr::from_bytes(b"caf\xe9.txt");
            let numbered = numbered_name(Path::new(raw), 2);
            assert_eq!(numbered.as_os_str().as_bytes(), b"caf\xe9 (2).txt");
        }
    }
}
