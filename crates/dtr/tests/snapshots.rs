//! The kittest baselines are inputs, and an input that is not in the repository
//! is not one.
//!
//! `egui_kittest` compares the rendered frame against `tests/snapshots/<name>.png`
//! and fails with "Missing snapshot" when it is not there. That made the
//! baselines' absence a *silent* problem in one direction and a confusing one in
//! the other: `.gitignore` had `*.png` with no exception for this directory, so
//! the five baselines existed only on the machine that generated them — CI
//! compared against nothing, and a checkout whose git-lfs had not smudged them
//! failed with a decode error that reads like a rendering bug.
//!
//! This test is the guard, from both sides: every `snapshot("…")` the crate's
//! tests take must have a real PNG beside it, and every PNG there must belong to
//! a test that takes it, so a renamed snapshot cannot leave a stale baseline
//! behind.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every name passed to `harness.snapshot("…")` under `src/`.
fn snapshots_taken() -> BTreeSet<String> {
    fn walk(dir: &Path, names: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(dir)
            .expect("src is part of the crate")
            .flatten()
        {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, names);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let text = std::fs::read_to_string(&path).expect("a source file is readable");
                let mut rest = text.as_str();
                while let Some(at) = rest.find("snapshot(\"") {
                    let after = &rest[at + "snapshot(\"".len()..];
                    let Some(end) = after.find('"') else {
                        break;
                    };
                    names.insert(after[..end].to_string());
                    rest = after;
                }
            }
        }
    }

    let mut names = BTreeSet::new();
    walk(&manifest_dir().join("src"), &mut names);
    names
}

/// The baselines on disk, ignoring the pictures a failure or an update writes
/// next to them (`.diff.png`, `.new.png`, `.old.png`).
fn baselines_on_disk() -> BTreeSet<String> {
    let dir = manifest_dir().join("tests").join("snapshots");
    let mut names = BTreeSet::new();
    for entry in std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
        .flatten()
    {
        let path = entry.path();
        let Some(file) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !file.ends_with(".png") {
            continue;
        }
        if [".diff.png", ".new.png", ".old.png"]
            .iter()
            .any(|suffix| file.ends_with(suffix))
        {
            continue;
        }
        names.insert(file.trim_end_matches(".png").to_string());
    }
    names
}

#[test]
fn every_snapshot_has_a_baseline_in_the_repository() {
    let taken = snapshots_taken();
    let disk = baselines_on_disk();

    assert!(
        !taken.is_empty(),
        "no `snapshot(\"…\")` calls were found: the scan broke, not the baselines"
    );

    let missing: Vec<&String> = taken.difference(&disk).collect();
    assert!(
        missing.is_empty(),
        "these snapshots are taken but have no baseline: {missing:?}\n\
         A baseline that is not in the repository is not a baseline — egui_kittest fails with \
         \"Missing snapshot\" and CI compares against nothing. Check that `.gitignore` does not \
         exclude it and that `git lfs` has it (`git lfs ls-files`)."
    );

    let stale: Vec<&String> = disk.difference(&taken).collect();
    assert!(
        stale.is_empty(),
        "these baselines belong to no test any more: {stale:?}\n\
         A renamed snapshot leaves its old picture behind, which compares against nothing and \
         hides a rendering change. Delete them."
    );

    for name in &taken {
        let path = manifest_dir()
            .join("tests")
            .join("snapshots")
            .join(format!("{name}.png"));
        let bytes =
            std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        assert!(
            bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
            "{} is not a PNG: a 130-byte file starting with \"version https://git-lfs\" is a \
             pointer, so this checkout's git-lfs did not smudge it (`git lfs pull`).",
            path.display()
        );
        assert!(
            bytes.len() > 1024,
            "{} is only {} bytes, which is too small to be a 1280x720 frame",
            path.display(),
            bytes.len()
        );
    }
}
