//! The crate's baselines are inputs, and an input that is not in the repository
//! is not one.
//!
//! `egui_kittest` compares a frame against `tests/snapshots/<name>.png` and fails
//! with "Missing snapshot" when it is absent — a silent problem in one direction
//! and a confusing one in the other, because `.gitignore` ignores `*.png` and a
//! checkout whose `git-lfs` did not smudge a baseline holds a 130-byte pointer.
//! This is the guard from both sides: every `snapshot(…)` call this crate makes has
//! a real PNG beside it, and every PNG belongs to a test that takes it.

use std::collections::BTreeSet;
use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every name passed to `harness.snapshot(…)` under `tests/`.
fn snapshots_taken() -> BTreeSet<String> {
    let dir = manifest_dir().join("tests");
    let mut names = BTreeSet::new();
    for entry in std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
        .flatten()
    {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("a source file is readable");
        // The needle is written with the escape so this file's own text does not
        // match itself when it is scanned.
        let needle = concat!("snapshot", "(\"");
        let mut rest = text.as_str();
        while let Some(at) = rest.find(needle) {
            let after = &rest[at + needle.len()..];
            let Some(end) = after.find('"') else {
                break;
            };
            names.insert(after[..end].to_string());
            rest = after;
        }
    }
    names
}

/// The baselines on disk, ignoring the pictures a failure or an update writes
/// next to them (`.diff.png`, `.new.png`, `.old.png`).
fn baselines_on_disk() -> BTreeSet<String> {
    let dir = manifest_dir().join("tests").join("snapshots");
    let mut names = BTreeSet::new();
    // A missing directory is "no baselines", not a panic: the assertion below
    // then names the snapshots that were taken, which is the useful message.
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return names;
    };
    for entry in entries.flatten() {
        let Some(file) = entry
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
        else {
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
        names.insert(file.trim_end_matches(".png").to_owned());
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
         A baseline that is not in the repository is not a baseline. Check that `.gitignore` \
         does not exclude it and that `git lfs` has it (`git lfs ls-files`)."
    );

    let stale: Vec<&String> = disk.difference(&taken).collect();
    assert!(
        stale.is_empty(),
        "these baselines belong to no test any more: {stale:?}\n\
         A renamed snapshot leaves its old picture behind, which compares against nothing. \
         Delete them."
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
    }
}
