//! Tera Term Language (TTL) macros.
//!
//! TTL is the scripting language of Tera Term: a line-oriented language that
//! drives one terminal session from a file. It is why Tera Term is the terminal
//! of choice around serial hardware — a script connects, waits for a prompt,
//! sends a command, pulls the answer apart, writes it to a log and sets an exit
//! status — so a terminal that means to replace Tera Term has to speak it.
//!
//! `MANUAL.md`, in this directory, is the specification: a distilled reference
//! to the language as Tera Term 5 documents it, complete enough to build an
//! interpreter from without the upstream HTML. Section numbers in this crate's
//! documentation and tests refer to it.
//!
//! # What is implemented
//!
//! The part of the interpreter that can be checked against real macros without
//! executing anything:
//!
//! * [`source`] decodes a file — BOM, UTF-8, or the ANSI code page as a
//!   fallback (§3.1) — and cuts it into lines, truncating each at 1023
//!   characters (§3.2).
//! * [`lex`] turns one line into tokens: constants, strings with their adjacent
//!   pieces joined, labels, brackets and the operator table of §5.2, with `;`
//!   and `/* */` comments (§3.3–§3.7).
//! * [`line`] classifies each line as one of the four kinds of §3.2 — nothing, a
//!   label, an assignment, a command — and checks that every block of §6 is
//!   closed by its own keyword.
//!
//! The start of the evaluator:
//!
//! * [`value`] is what a value can be: a 32-bit integer or a string, nothing
//!   else (§4.1).
//! * [`vars`] is the variable namespace of §4 — four kinds in four separate
//!   budgets of 256, a type fixed at the first assignment, names that are
//!   case-insensitive — and [`error`] is the run-time error set of §8.
//! * [`expr`] parses and evaluates the operator table of §5.2, and writes down
//!   the behaviour §5.5 leaves undefined: wrapping arithmetic, masked shift
//!   counts, short-circuiting `&&`/`||`, and division that truncates toward
//!   zero.
//! * [`program`] turns lines into executable ops and resolves every jump §6
//!   only names: the branch table of an `if`, the end of each loop, the target
//!   of every `break`/`continue`, and the label of every `goto`/`call` — so a
//!   missing `endif` or a `goto` to nowhere is refused before the macro starts,
//!   as Tera Term refuses it.
//!
//! Nothing executes yet: there is no command set and no host interface for
//! `send`, `wait` or the dialog boxes. The families of the manual's §7 are the
//! intended module boundaries for that work, because a parameter's meaning is
//! the command's business and not the scanner's.
//!
//! # The corpus
//!
//! `tests/corpus/` holds upstream Tera Term's own macro tests: 53 files, mostly
//! named after the issue that produced them, vendored verbatim under the
//! BSD-3-Clause notice in `tests/corpus/NOTICE`. They are regression recipes
//! rather than an automated suite — most report by showing a dialog box — but
//! they are real TTL, which is what makes them worth having: `tests/corpus.rs`
//! requires 52 of them to decode, lex, nest and compile to a program, and
//! records the one that is a test of errors (`oneline-if.ttl`), so a change that
//! would reject a real macro fails there.

pub mod error;
pub mod expr;
pub mod lex;
pub mod line;
pub mod program;
pub mod source;
pub mod value;
pub mod vars;
