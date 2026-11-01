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
//! Nothing evaluates yet: there is no expression parser, no command set, and no
//! host interface for `send`, `wait` or the dialog boxes. The families of the
//! manual's §7 are the intended module boundaries for that work, because a
//! parameter's meaning is the command's business and not the scanner's.
//!
//! # The corpus
//!
//! `tests/corpus/` holds upstream Tera Term's own macro tests: 53 files, mostly
//! named after the issue that produced them, vendored verbatim under the
//! BSD-3-Clause notice in `tests/corpus/NOTICE`. They are regression recipes
//! rather than an automated suite — most report by showing a dialog box — but
//! they are real TTL, which is what makes them worth having: `tests/corpus.rs`
//! requires every one of them to decode, lex and nest correctly, so a change
//! that would reject a real macro fails there.

pub mod lex;
pub mod line;
pub mod source;
