//! Tera Term Language (TTL) macros.
//!
//! TTL is the scripting language of Tera Term: a line-oriented language that
//! drives one terminal session from a file. It is the reason Tera Term is the
//! terminal of choice around serial hardware — a script can connect to a port,
//! wait for a prompt, send a command, pull the answer apart, write it to a log,
//! and set an exit status — so a terminal that means to replace Tera Term has
//! to speak it eventually.
//!
//! **Nothing is implemented yet.** `MANUAL.md`, in this directory, is the
//! specification this crate is to be built to: a distilled reference to the
//! language as Tera Term 5 documents it, complete enough to write an
//! interpreter from without reading the upstream HTML pages. It is deliberately
//! independent of dtr — a TTL program is meant to run unchanged — but the
//! places where a host terminal must answer for itself (how a link is
//! established, what `send` does with a byte, what a wait can see) are called
//! out there rather than left to the reader.
//!
//! The interpreter is expected to look like the rest of the workspace: a
//! lexer and parser producing a statement tree, an evaluator that owns the
//! 256-integer / 256-string variable budget, and a host interface that the
//! terminal implements for communication (`send`, `wait`, `connect`) and for
//! the desktop-facing commands (files, dialogs, clipboard). The manual's
//! command reference is grouped the same way, so the family names there are
//! the intended module boundaries here.
