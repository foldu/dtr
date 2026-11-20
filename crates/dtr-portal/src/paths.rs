//! One question about names that `SaveFiles` asks: is this one taken?
//!
//! `SaveFiles` asks for a folder, and the names the application passed are
//! appended to it. If a name is already there, the portal *may* prompt or
//! choose a name of its own; GTK's backend chooses, with the scheme a desktop
//! user recognises: `report.pdf` becomes `report(1).pdf`. This is that scheme,
//! as a pure-ish function: it asks the filesystem `exists`, and it is the only
//! place in this crate that touches a path that is not the answer itself.
//!
//! It never invents a name for one that is not UTF-8. A name is bytes on Unix,
//! and lossily converting one to find a unique spelling would return a path
//! naming a *different* file; the application's name is left exactly as it came
//! instead, which is also what a caller that never checked would have got.

use std::path::{Path, PathBuf};

/// `path`, or the first `stem(N)ext` beside it that does not exist.
///
/// The suffix is split at the **first** dot, as GTK does: `a.tar.gz` becomes
/// `a(1).tar.gz`, not `a.tar(1).gz`. The search is bounded so a directory with a
/// pathological number of collisions cannot stall the D-Bus task; after a
/// thousand names the original path is returned and the caller has an honest
/// answer rather than no answer. (This is the one filesystem question the answer
/// path asks — one `exists` per name, no directory read.)
pub fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        // A name that is not UTF-8: no sane spelling to append to, and a lossy
        // one would name a different file.
        return path.to_path_buf();
    };
    let (stem, suffix) = match name.find('.') {
        Some(dot) => (&name[..dot], &name[dot..]),
        None => (name, ""),
    };
    let parent = path.parent().unwrap_or(Path::new(""));
    for n in 1..=1_000u32 {
        let candidate = parent.join(format!("{stem}({n}){suffix}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_free_name_is_kept_as_it_is() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("report.pdf");
        assert_eq!(unique_path(&path), path);
    }

    #[test]
    fn a_taken_name_gains_a_number_before_the_first_dot() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        std::fs::write(dir.path().join("report.pdf"), b"here first").expect("write");
        assert_eq!(
            unique_path(&dir.path().join("report.pdf")),
            dir.path().join("report(1).pdf")
        );
        // A name with several dots keeps all of them after the number.
        std::fs::write(dir.path().join("archive.tar.gz"), b"here first").expect("write");
        assert_eq!(
            unique_path(&dir.path().join("archive.tar.gz")),
            dir.path().join("archive(1).tar.gz")
        );
        // No dot at all: the number goes at the end.
        std::fs::write(dir.path().join("README"), b"here first").expect("write");
        assert_eq!(
            unique_path(&dir.path().join("README")),
            dir.path().join("README(1)")
        );
    }

    #[test]
    fn the_second_collision_takes_the_next_number() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        for name in ["report.pdf", "report(1).pdf"] {
            std::fs::write(dir.path().join(name), b"here first").expect("write");
        }
        assert_eq!(
            unique_path(&dir.path().join("report.pdf")),
            dir.path().join("report(2).pdf")
        );
    }
}
