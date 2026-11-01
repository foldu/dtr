//! Every vendored macro, run to completion.
//!
//! The corpus in `tests/corpus/` is upstream Tera Term's own regression set:
//! 53 macros, most of which end in a `messagebox` so that a person can read the
//! answer. That is exactly the shape a recording host can check — the macro
//! asks, the host answers, and the transcript is the evidence — so this test
//! runs all of them and requires each to finish.
//!
//! It is not a claim that the macros *pass*: several of them are reproductions
//! of bugs in a specific Tera Term build and say so in their dialogs. What it
//! proves is that the interpreter gets through them: every command they use is
//! one this crate knows, every loop ends, every `goto` lands, and the dialogs
//! they reach are reached in the order the macro wrote them. That is a stronger
//! statement than "the file parses", and it is the one that would catch a
//! command family going missing.

use std::path::{Path, PathBuf};

use dtr_ttl::host::RecordingHost;
use dtr_ttl::interp::Interpreter;
use dtr_ttl::program::Program;
use dtr_ttl::source;

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

/// The one macro that is a test of *errors*: it opens with `if 0` and nothing
/// after it, which is a syntax error, and its own dialogs say so. It cannot
/// run, so it is skipped here and asserted in `corpus.rs` instead.
const DOES_NOT_COMPILE: &[&str] = &["oneline-if.ttl"];

/// Macros that are supposed to *stop*, and where.
///
/// Each of these is a reproduction of a Tera Term bug whose expected outcome is
/// an error — the file says so in a comment or in the dialog that never
/// appears. A macro that stops here is this interpreter agreeing with the
/// report, so the line is pinned rather than tolerated.
const EXPECTED_FAILURES: &[(&str, usize, &str)] = &[
    (
        "#39452.ttl",
        1,
        "filetruncate with no size: the report is that it crashes Tera Term",
    ),
    (
        "array.ttl",
        14,
        "a missing ]: the macro is the reproduction of Tera Term not noticing",
    ),
    (
        "gui_commands_test.ttl",
        33,
        "a command that does not exist, deliberately",
    ),
    (
        "strspecial.ttl",
        17,
        "three parameters to strspecial, which the file marks `must be error`",
    ),
];

/// Macros too heavy for a unit test, and why.
const TOO_HEAVY: &[(&str, &str)] = &[(
    "#35822-random.ttl",
    "110,000 samples for each of 100 buckets: eleven million iterations",
)];

#[derive(Default)]
struct Ran {
    macros: usize,
    steps: u64,
    dialogs: usize,
    expected: usize,
}

/// How a recorder answers a macro that expects a person.
///
/// Half the corpus drives a dialog and loops until somebody answers — `test_
/// file.ttl` will not leave its folder prompt otherwise — so the recorder says
/// yes, picks a folder and chooses the first item. That is not "passing the
/// test": it is running the macro far enough for its later dialogs to be
/// reached.
fn answering_host() -> RecordingHost {
    let mut host = RecordingHost::new();
    host.yes = true;
    host.path = Some("C:/tmp".into());
    host.choice = Some(0);
    host.input = Some("typed".into());
    host.uptime_ms = 1_000;
    host
}

/// Run every macro. Returns the statistics and one line per macro that did not
/// behave as expected, so a failure lists all of them instead of the first.
fn run_all() -> (Ran, Vec<String>) {
    let mut ran = Ran::default();
    let mut failures = Vec::new();

    for path in corpus_files() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if DOES_NOT_COMPILE.contains(&name.as_str()) {
            continue;
        }
        if let Some((_, why)) = TOO_HEAVY.iter().find(|(file, _)| *file == name) {
            eprintln!("skipping {name}: {why}");
            continue;
        }
        let bytes = std::fs::read(&path).expect("the file is in the repository");
        let (text, _) = source::decode(&bytes).expect("decoding is tested in corpus.rs");
        let program = match Program::parse(&text) {
            Ok(program) => program,
            Err(error) => {
                failures.push(format!("{name}: does not compile: {error}"));
                continue;
            }
        };
        let steps = program.len() as u64;
        let mut host = answering_host();
        let expected = EXPECTED_FAILURES
            .iter()
            .find(|(file, _, _)| *file == name)
            .map(|(_, line, _)| *line);
        let mut interp = Interpreter::with_program(&mut host, program);
        let outcome = interp.run();
        let dialogs = host.messages().len();

        match (outcome, expected) {
            (Ok(_), None) => {
                ran.macros += 1;
                ran.steps += steps;
                ran.dialogs += dialogs;
            }
            (Err(failure), Some(line)) if failure.line == line => {
                ran.expected += 1;
            }
            (Ok(_), Some(line)) => failures.push(format!(
                "{name}: expected a failure at line {line}, but it ran to completion"
            )),
            (Err(failure), Some(line)) => failures.push(format!(
                "{name}: expected a failure at line {line}, got {:?} at {}",
                failure.error, failure.line
            )),
            (Err(failure), None) => {
                // The line the failure names, so a report says what the macro
                // was doing and not only which error it hit.
                let at = text
                    .lines()
                    .nth(failure.line.saturating_sub(1))
                    .unwrap_or("")
                    .trim();
                failures.push(format!(
                    "{name}:{}: {:?} `{at}`",
                    failure.line, failure.error
                ));
            }
        }
    }

    (ran, failures)
}

#[test]
fn every_upstream_macro_runs_to_completion() {
    let (ran, failures) = run_all();
    assert!(
        failures.is_empty(),
        "{} of {} macro(s) did not behave as expected:\n{}",
        failures.len(),
        corpus_files().len(),
        failures.join("\n")
    );
    assert_eq!(
        ran.macros + ran.expected,
        corpus_files().len() - DOES_NOT_COMPILE.len() - TOO_HEAVY.len()
    );
    assert_eq!(ran.expected, EXPECTED_FAILURES.len());
    // A macro that runs but reaches none of its dialogs has not really run:
    // these are bug reports, and the dialog is the report.
    assert!(
        ran.dialogs >= 40,
        "only {} dialogs were reached over {} macros",
        ran.dialogs,
        ran.macros
    );
    assert!(ran.steps > 0);
}

/// What a handful of the corpus macros say, recorded.
///
/// These are the ones that test a specific piece of arithmetic or formatting
/// inside TTL rather than a terminal setting, so their dialogs are the expected
/// values — which makes them a regression test for this crate as well as for
/// Tera Term. The `sprintf2` file is the interesting one: it is a printf
/// conformance table, and the strings below are what C's `printf` produces.
/// Every `messagebox` the whole corpus shows, in one file, one line per box:
/// `title :: message`, with a newline written `\n`.
///
/// This is the assertion the corpus was vendored for. A bug-report macro's
/// *only* output is the dialog at the end of it, so a change that prints a
/// different number, a different order or a dialog that no longer appears has
/// to show up here. The file is generated (`UPDATE_CORPUS_DIALOGS=1`) and then
/// reviewed as data, in the same spirit as a snapshot PNG: regenerating it
/// without reading the diff is how a regression becomes an expectation.
const DIALOGS: &str = "corpus-dialogs.txt";

fn transcript() -> String {
    let mut out = String::from(
        "# What each vendored macro says. Generated by tests/run_corpus.rs.\n\
         # Update with: UPDATE_CORPUS_DIALOGS=1 cargo test -p dtr-ttl --test run_corpus\n\
         # A line is `title :: message`, one per messagebox, with \\n for a line break.\n",
    );
    for path in corpus_files() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if DOES_NOT_COMPILE.contains(&name.as_str())
            || TOO_HEAVY.iter().any(|(file, _)| *file == name)
        {
            continue;
        }
        let bytes = std::fs::read(&path).expect("the file is in the repository");
        let (text, _) = source::decode(&bytes).expect("decoding is tested in corpus.rs");
        let Ok(program) = Program::parse(&text) else {
            continue;
        };
        let mut host = answering_host();
        let mut interp = Interpreter::with_program(&mut host, program);
        let _ = interp.run();
        out.push_str(&format!("\n== {name}\n"));
        for event in host.events() {
            if let dtr_ttl::host::Event::Message { message, title } = event {
                let escape = |text: &str| {
                    text.replace('\\', "\\\\")
                        .replace('\n', "\\n")
                        .replace('\r', "\\r")
                        .replace('\t', "\\t")
                };
                out.push_str(&format!("{} :: {}\n", escape(title), escape(message)));
            }
        }
    }
    out
}

#[test]
fn the_corpus_transcript_is_unchanged() {
    let path = corpus_dir().join(DIALOGS);
    let current = transcript();
    if std::env::var_os("UPDATE_CORPUS_DIALOGS").is_some() {
        std::fs::write(&path, &current).expect("the corpus directory is writable");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    assert_eq!(
        current, expected,
        "the corpus's dialogs changed; if that was intended, re-run with \
         UPDATE_CORPUS_DIALOGS=1 and review the diff"
    );
    assert!(
        current.contains("== test_file.ttl"),
        "the transcript has content"
    );
}

#[test]
fn the_macros_that_test_the_language_say_what_they_should() {
    let cases: &[(&str, &[&str])] = &[
        (
            // A `printf` conformance table: the strings are what C produces,
            // including the `#` prefixes and `%e`'s two-digit exponent.
            "sprintf2.ttl",
            &[
                "+00000000001",
                "       1",
                "0x1",
                "0X1",
                "a",
                "1.0000000000",
                "1.000000e+00",
            ],
        ),
        (
            // §7.6's `ifdefined` codes: a bare word is a name, a literal is not,
            // and `c[0]` asks about the array.
            "ifdefined.ttl",
            &["0", "0", "0", "1", "3", "5", "6", "5", "6"],
        ),
    ];

    for (name, expected) in cases {
        let path = corpus_dir().join(name);
        let bytes = std::fs::read(&path).unwrap_or_else(|_| panic!("{name} is vendored"));
        let (text, _) = source::decode(&bytes).expect("the file decodes");
        let program = Program::parse(&text).unwrap_or_else(|error| panic!("{name}: {error}"));
        let mut host = RecordingHost::new();
        let mut interp = Interpreter::with_program(&mut host, program);
        interp
            .run()
            .unwrap_or_else(|failure| panic!("{name}: {failure}"));
        assert_eq!(host.messages(), *expected, "{name}");
    }
}
