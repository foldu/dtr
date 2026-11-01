//! Upstream Tera Term's own macros, read by this crate.
//!
//! The corpus in `tests/corpus/` is vendored from TeraTermProject/teraterm at
//! the commit recorded in the NOTICE next to it: 53 real TTL files, most of them
//! the reproduction attached to a bug report. They are not an automated suite —
//! two thirds of them end in `messagebox` so that a person can read the answer —
//! but they are exactly what a lexer needs to be tested against, because every
//! one was written for Tera Term and every quirk of the language shows up
//! somewhere in them: Shift-JIS comments, `#$0D` escapes inside strings, labels
//! that are numbers, `if` in both forms.
//!
//! What this test asserts is that the reading half of the interpreter accepts all
//! of it: every file decodes, every line lexes, every line is one of the four
//! kinds, every block is closed, and the whole file compiles to a program with
//! every jump resolved. That is a weaker claim than "the macros run", and
//! deliberately so — it is the claim this crate can currently keep.

use std::path::{Path, PathBuf};

use dtr_ttl::line::{self, LineKind};
use dtr_ttl::program::{BuildError, LoadError, Program};
use dtr_ttl::source::{self, Encoding};

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("corpus")
}

fn corpus_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(corpus_dir())
        .expect("tests/corpus is part of the crate")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "ttl"))
        .collect();
    files.sort();
    files
}

#[derive(Default, Debug)]
struct Stats {
    files: usize,
    lines: usize,
    labels: usize,
    assignments: usize,
    commands: usize,
    blank: usize,
    utf8: usize,
    ansi: usize,
    utf16_be: usize,
    utf16_le: usize,
}

/// Read the whole corpus. Returns what was found and, separately, the files
/// that could not be read — so a failure is reported with all of them at once
/// rather than at the first.
fn measure() -> (Stats, Vec<String>) {
    let files = corpus_files();
    let mut stats = Stats::default();
    let mut failures: Vec<String> = Vec::new();

    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let bytes = std::fs::read(path).expect("the file is in the repository");

        let (text, encoding) = match source::decode(&bytes) {
            Ok(decoded) => decoded,
            Err(error) => {
                failures.push(format!("{name}: does not decode: {error}"));
                continue;
            }
        };
        stats.files += 1;
        match encoding {
            Encoding::Utf8 | Encoding::Utf8Bom => stats.utf8 += 1,
            Encoding::Ansi => stats.ansi += 1,
            Encoding::Utf16Be => stats.utf16_be += 1,
            Encoding::Utf16Le => stats.utf16_le += 1,
        }

        let lines = match line::scan(&text) {
            Ok(lines) => lines,
            Err(error) => {
                failures.push(format!("{name} ({encoding:?}): {error}"));
                continue;
            }
        };

        for parsed in &lines {
            stats.lines += 1;
            match &parsed.kind {
                LineKind::Blank => stats.blank += 1,
                LineKind::Label(_) => stats.labels += 1,
                LineKind::Assign { .. } => stats.assignments += 1,
                LineKind::Command { .. } => stats.commands += 1,
            }
        }

        if let Err(error) = line::check_blocks(&lines) {
            failures.push(format!("{name}: {error}"));
        }
    }

    (stats, failures)
}

#[test]
fn every_upstream_macro_decodes_lexes_and_nests() {
    let (stats, failures) = measure();

    assert!(
        failures.is_empty(),
        "{} macro(s) were rejected:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert_eq!(stats.files, corpus_files().len(), "every file must decode");
    assert_eq!(
        stats.blank + stats.labels + stats.assignments + stats.commands,
        stats.lines,
        "every line is one of the four kinds"
    );
}

/// The corpus is vendored, so its shape is a constant: if these numbers move,
/// either the corpus was re-vendored or the scanner started reading something
/// differently. Both are worth knowing about.
///
/// `lines` is 2485, which is what `wc -l` counts over the same files — every
/// vendored file ends with a newline, so the splitter's "a trailing newline
/// does not add an empty line" rule shows up here as agreement.
#[test]
fn the_corpus_is_the_one_that_was_vendored() {
    let (stats, failures) = measure();
    assert!(failures.is_empty(), "{failures:?}");

    assert_eq!(stats.files, 53, "the vendored corpus has 53 macros");
    assert_eq!(stats.lines, 2485);
    assert_eq!(
        (stats.blank, stats.labels, stats.assignments, stats.commands),
        (648, 48, 168, 1621),
        "blank / label / assignment / command lines"
    );

    // Not every file is ASCII, which is the point: §3.1 has to be implemented
    // for the corpus to read at all.
    assert_eq!(
        (stats.utf8, stats.ansi, stats.utf16_be, stats.utf16_le),
        (36, 15, 1, 1),
        "36 UTF-8, 15 CP932, one each of UTF-16BE and UTF-16LE"
    );
}

/// One vendored macro is a test *of* errors rather than a working macro:
/// `oneline-if.ttl` is a list of single-line `if`s whose dialogs say which of
/// them Tera Term rejects, and it opens with `if 0` and nothing after it — a
/// branch with no statement — which is a syntax error. So it is the one file
/// expected not to compile, and expected to fail at that line.
const EXPECTED_BUILD_FAILURES: &[(&str, usize)] = &[("oneline-if.ttl", 2)];

/// Every macro compiles: statements become ops, every block is resolved, and
/// every `goto`/`call` finds its label. This is where a construct the language
/// has and this crate does not shows up as a file and a line rather than as a
/// surprise later.
#[test]
fn every_upstream_macro_builds_a_program() {
    let files = corpus_files();
    let mut failures: Vec<String> = Vec::new();
    let mut ops = 0usize;
    let mut labels = 0usize;
    let mut expected = 0usize;

    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let bytes = std::fs::read(path).expect("the file is in the repository");
        let (text, _) = source::decode(&bytes).expect("decoding is tested above");
        let wanted = EXPECTED_BUILD_FAILURES
            .iter()
            .find(|(file, _)| *file == name)
            .map(|(_, line)| *line);

        match (Program::parse(&text), wanted) {
            (Ok(program), None) => {
                ops += program.len();
                labels += program.labels.len();
            }
            (Ok(_), Some(line)) => failures.push(format!(
                "{name}: expected a syntax error at line {line}, but it compiled"
            )),
            (Err(LoadError::Build(BuildError::Syntax { line, .. })), Some(want))
                if line == want =>
            {
                expected += 1;
            }
            (Err(error), Some(_)) => {
                failures.push(format!("{name}: expected a syntax error, got {error}"))
            }
            (Err(error), None) => failures.push(format!("{name}: {error}")),
        }
    }

    assert!(
        failures.is_empty(),
        "{} macro(s) did not compile:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert_eq!(
        expected,
        EXPECTED_BUILD_FAILURES.len(),
        "every known-bad macro still fails for the documented reason"
    );
    assert!(ops > 1000, "the corpus compiles to {ops} ops");
    assert!(labels >= 40, "and carries {labels} labels");
}

/// A macro that a person would write, in the idioms the corpus uses, to show
/// that the pieces fit together rather than only in isolation.
#[test]
fn the_idioms_of_a_real_macro_scan() {
    let text = "\
; connect and look at the answer
intdim ar 4
:again
A = 33
sendln 'cat readme.txt'#13#10
waitln 'OK' 'ERROR'
if result = 2 goto again
strreplace msg 1 '([' 'x'
if 1 then
    messagebox msg 'title'
else
    mpause 100
endif
for i 0 3
    ar[i] = i * 2
next
";

    let lines = line::scan(text).expect("a well-formed macro");
    line::check_blocks(&lines).expect("balanced blocks");

    let kinds: Vec<&LineKind> = lines.iter().map(|l| &l.kind).collect();
    assert!(matches!(kinds[0], LineKind::Blank), "a comment-only line");
    assert!(matches!(kinds[1], LineKind::Command { name, .. } if name == "intdim"));
    assert!(matches!(kinds[2], LineKind::Label(name) if name == "again"));
    assert!(matches!(kinds[3], LineKind::Assign { target, .. } if target == "A"));
}
