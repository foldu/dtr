//! The "keep both" name rule, as pure functions.
//!
//! A copy whose destination exists has three answers (`docs/manager.md` §7),
//! and one of them must invent a name. The rule is the one every file manager
//! uses — `name (2).ext`, `name (3).ext`, … — because a person who has just
//! said "keep both" is looking for the second one to be obviously the second.
//!
//! **This is the only copy of the rule.** The browser's own keep-both answer
//! (`dtr_files::write`), the transfer queue's publish step and the app's
//! "is this destination the queue's numbered name?" pre-answer all come here:
//! three approximations that agree only by tests is one data bug waiting for a
//! name with a bracket in it. The number belongs to the stem, so a second
//! keep-both of `report (2).txt` is `report (3).txt`, never
//! `report (2) (2).txt` — and [`numbered_name`] is where that continuation is
//! decided.
//!
//! The name is a [`Path`], not a `String`: a Unix file name is bytes, and
//! round-tripping one through `to_string_lossy` to add a number would move the
//! file the user asked for. The one caller that starts from a `&str` (the
//! browser's `next_free_name`) is a `String` because the conflict card was
//! drawn from one; it converts at its own edge.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The first number a "keep both" name tries. `name` is taken, so the next one
/// is `name (2)`.
pub const FIRST_SUFFIX: u32 = 2;

/// The number of suffixes tried before giving up: a directory with two thousand
/// copies of one name is not a directory a transfer should keep probing.
pub const LAST_SUFFIX: u32 = 2_000;

/// The stem before a trailing ` (n)`, and `n`, when the stem already ends in one.
///
/// Not UTF-8 is not a trailing number: a name this cannot read keeps its bytes
/// and takes the plain ` (2)` suffix.
fn trailing_number(stem: &str) -> Option<(&str, u32)> {
    let rest = stem.strip_suffix(')')?;
    let (base, digits) = rest.rsplit_once(" (")?;
    let number = digits.parse::<u32>().ok()?;
    (!base.is_empty()).then_some((base, number))
}

/// The first number [`numbered_name`] will use for `name`: one past a number the
/// name already ends in, else [`FIRST_SUFFIX`].
pub fn first_suffix(name: &Path) -> u32 {
    let stem = name.file_stem().unwrap_or(name.as_os_str());
    match stem.to_str().and_then(trailing_number) {
        Some((_, number)) => number.saturating_add(1),
        None => FIRST_SUFFIX,
    }
}

/// The `n`th candidate for `name`: the stem, ` (number)`, then the extension.
///
/// `n == FIRST_SUFFIX` is the first number the rule may choose, which is one
/// past a number the name already ends in: `report (2).txt` gives
/// `report (3).txt`, so a repeated keep-both counts up instead of stacking
/// suffixes. The extension is whatever [`Path::extension`] says it is, so
/// `a.tar.gz` becomes `a.tar (2).gz` and a dotfile has no extension at all:
/// `.env` becomes `.env (2)`.
pub fn numbered_name(name: &Path, n: u32) -> PathBuf {
    let extension = name.extension();
    let stem = name.file_stem().unwrap_or(name.as_os_str());
    let (base, first) = match stem.to_str().and_then(trailing_number) {
        Some((base, number)) => (OsString::from(base), number.saturating_add(1)),
        None => (stem.to_owned(), FIRST_SUFFIX),
    };
    // `n` counts candidates from `FIRST_SUFFIX`; the number written is offset by
    // whatever the name already ended in.
    let number = first.saturating_add(n.saturating_sub(FIRST_SUFFIX));
    let mut numbered = base;
    numbered.push(format!(" ({number})"));
    if let Some(extension) = extension {
        numbered.push(".");
        numbered.push(extension);
    }
    PathBuf::from(numbered)
}

/// Whether `candidate` is one of the numbered names the rule makes for `name`.
///
/// This is the question a caller asks when it must decide "is this destination
/// the keep-both name, or a name the person typed?" without walking the
/// filesystem: the app's queue pre-answer does. Both arguments are file names,
/// not paths, because that is the unit the rule works on.
pub fn is_numbered_name(name: &Path, candidate: &Path) -> bool {
    (FIRST_SUFFIX..=LAST_SUFFIX).any(|n| numbered_name(name, n) == candidate)
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

    /// The rule counts up from a number the name already ends in, because the
    /// number belongs to the stem: a second keep-both of `report (2).txt` is
    /// `report (3).txt`. The old queue implementation appended instead
    /// (`report (2) (2).txt`) while the browser counted; one rule now.
    #[test]
    fn a_name_that_already_ends_in_a_number_counts_up() {
        assert_eq!(numbered("report (2).txt", FIRST_SUFFIX), "report (3).txt");
        assert_eq!(
            numbered("report (2).txt", FIRST_SUFFIX + 1),
            "report (4).txt"
        );
        assert_eq!(numbered(".env (7)", FIRST_SUFFIX), ".env (8)");
        // A bracket that is not a suffix is part of the name.
        assert_eq!(
            numbered("report (x).txt", FIRST_SUFFIX),
            "report (x) (2).txt"
        );
        assert_eq!(numbered("(2).txt", FIRST_SUFFIX), "(2) (2).txt");
        assert_eq!(first_suffix(Path::new("report (2).txt")), 3);
        assert_eq!(first_suffix(Path::new("report.txt")), FIRST_SUFFIX);
    }

    /// The question the app's queue pre-answer asks: is this destination one of
    /// the rule's numbered names for that source?
    #[test]
    fn a_numbered_name_is_recognised() {
        let source = Path::new("report.txt");
        assert!(is_numbered_name(source, Path::new("report (2).txt")));
        assert!(is_numbered_name(source, Path::new("report (7).txt")));
        assert!(!is_numbered_name(source, Path::new("report.txt")));
        assert!(!is_numbered_name(source, Path::new("other (2).txt")));
        assert!(!is_numbered_name(source, Path::new("report (2).md")));
        // And a source that already carries a number continues from it.
        let numbered_source = Path::new("report (2).txt");
        assert!(is_numbered_name(
            numbered_source,
            Path::new("report (3).txt")
        ));
        assert!(!is_numbered_name(
            numbered_source,
            Path::new("report (2) (2).txt")
        ));
    }
}
