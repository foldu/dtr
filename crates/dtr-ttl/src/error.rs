//! The errors a macro can hit at run time.
//!
//! §8 draws the line that matters: a *runtime* failure is a `result` value that
//! the next line is expected to read, and only an *interpreter* error stops the
//! run. This module is the second kind. Its messages are the ones §8 lists,
//! word for word, because a macro author who has seen them in Tera Term should
//! recognise them here — the variant carries the detail (which variable, which
//! index) for tests and for logging, while `Display` gives the documented text.
//!
//! Some of §8's messages belong to the evaluator that does not exist yet rather
//! than to this module: `Can't call sub.`, `Label required.`, `Stack overflow.`
//! and the rest arrive with control flow.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A value of the wrong type for what it was used for (§5.1: expressions
    /// take integers, `str*` commands take strings).
    TypeMismatch,

    /// §8 `Divide by zero.`
    DivideByZero,

    /// §8 `Variable not initialized.` — reading a name before its first
    /// assignment, which is an error rather than a zero.
    VariableNotInitialized(String),

    /// §8 `Index out of range.`
    IndexOutOfRange { name: String, index: i32 },

    /// §8 `Too many variables.` — §3.8's four budgets of 256.
    TooManyVariables(&'static str),

    /// A size `intdim`/`strdim`/`redim` cannot take: §4.3 allows 1 to 65536.
    InvalidArraySize(i32),

    /// `intdim` on a name that is already a scalar or an array of the other
    /// kind. Reported as a type mismatch, which is what it is.
    AlreadyAnotherKind(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TypeMismatch => f.write_str("Type mismatch."),
            Self::DivideByZero => f.write_str("Divide by zero."),
            Self::VariableNotInitialized(_) => f.write_str("Variable not initialized."),
            Self::IndexOutOfRange { .. } => f.write_str("Index out of range."),
            Self::TooManyVariables(_) => f.write_str("Too many variables."),
            Self::InvalidArraySize(_) => f.write_str("Invalid array size."),
            Self::AlreadyAnotherKind(_) => f.write_str("Type mismatch."),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_messages_are_the_manual_ones() {
        assert_eq!(Error::TypeMismatch.to_string(), "Type mismatch.");
        assert_eq!(Error::DivideByZero.to_string(), "Divide by zero.");
        assert_eq!(
            Error::VariableNotInitialized("A".into()).to_string(),
            "Variable not initialized."
        );
        assert_eq!(
            Error::IndexOutOfRange { name: "a".into(), index: 7 }.to_string(),
            "Index out of range."
        );
        assert_eq!(
            Error::TooManyVariables("integer").to_string(),
            "Too many variables."
        );
    }
}
