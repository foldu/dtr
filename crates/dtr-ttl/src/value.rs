//! The two types a value can have.
//!
//! §4.1 is short: a signed 32-bit integer, or a string of up to 511 characters.
//! There is no floating point anywhere in the language, no boolean (a
//! comparison yields `1` or `0`), and no null: an unassigned variable is not a
//! value at all, it is an error to read one.
//!
//! Arrays are not values. `intdim a 5` creates a variable whose elements are
//! integers; the array itself cannot be passed, compared or assigned, so it
//! never appears here. That is also why `Vars` keeps them in their own maps.

use std::fmt;

/// A TTL value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Int(i32),
    Str(String),
}

impl Value {
    /// The integer, if this is one.
    pub fn as_int(&self) -> Option<i32> {
        match self {
            Self::Int(v) => Some(*v),
            Self::Str(_) => None,
        }
    }

    /// The string, if this is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            Self::Int(_) => None,
        }
    }

    /// The code `ifdefined` reports for this value (§7.6): 1 for an integer, 3
    /// for a string. Arrays report 5 and 6, and are not values.
    pub fn type_code(&self) -> i32 {
        match self {
            Self::Int(_) => 1,
            Self::Str(_) => 3,
        }
    }

    /// The manual's word for the type, for error messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Int(_) => "integer",
            Self::Str(_) => "string",
        }
    }

    /// Whether this is a string longer than §3.7 allows.
    ///
    /// 511 characters. Nothing enforces it yet; this exists so the places that
    /// could produce one (concatenation, `sprintf`, reading a line) can be
    /// found when the rule is decided.
    pub fn is_over_long(&self) -> bool {
        self.as_str()
            .is_some_and(|s| s.chars().count() > crate::vars::MAX_STRING_CHARS)
    }
}

impl From<i32> for Value {
    fn from(v: i32) -> Self {
        Self::Int(v)
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Self::Str(s)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Self::Str(s.to_owned())
    }
}

/// How a value renders as a string.
///
/// §7.6: `int2str` "is equal to the formatted output (`%d`)" of `sprintf`, so
/// an integer is its decimal digits and a string is itself.
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Int(v) => write!(f, "{v}"),
            Self::Str(s) => f.write_str(s),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_type_codes_are_the_ones_ifdefined_reports() {
        assert_eq!(Value::Int(0).type_code(), 1);
        assert_eq!(Value::from("x").type_code(), 3);
    }

    #[test]
    fn an_integer_renders_as_decimal() {
        assert_eq!(Value::Int(-11).to_string(), "-11");
        assert_eq!(Value::from("11").to_string(), "11");
    }

    #[test]
    fn long_strings_can_be_spotted() {
        let long = "x".repeat(crate::vars::MAX_STRING_CHARS + 1);
        assert!(Value::from(long).is_over_long());
        assert!(!Value::Int(1).is_over_long());
    }
}
