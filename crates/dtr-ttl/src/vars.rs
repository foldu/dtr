//! Variables: the four kinds of §4 and the budgets of §3.8.
//!
//! A user variable has no declared type. It takes one at its first assignment
//! and keeps it: §4.2 is explicit that a later assignment of a different type
//! is an error, not a conversion. Reading before that first assignment is also
//! an error — `Variable not initialized.` — rather than an implicit zero, which
//! is the difference between a macro that fails loudly and one that sends an
//! empty line to a bootloader.
//!
//! Names are case-insensitive and at most 31 characters (§3.4). Neither is
//! enforced by the lexer, because both are properties of the *namespace*: this
//! module lowercases for lookup and truncates past the limit, which is what a
//! 32-byte buffer in a C implementation does. Truncating rather than refusing
//! means a macro that runs under Tera Term still runs here.
//!
//! The four budgets are separate (§3.8): 256 integers, 256 strings, 256 integer
//! arrays, 256 string arrays.

use std::collections::BTreeMap;

use crate::error::Error;
use crate::value::Value;

/// Scalars of one kind. `256 integer variables` and `256 string variables`.
pub const MAX_SCALARS: usize = 256;

/// Arrays of one kind: `256 integer arrays`, `256 string arrays`.
pub const MAX_ARRAYS: usize = 256;

/// §4.3: a size from 1 to 65536, indexed from 0.
pub const MAX_ARRAY_SIZE: usize = 65_536;

/// §3.4: identifiers are at most 31 characters.
pub const MAX_NAME_CHARS: usize = 31;

/// §3.7: a string is at most 511 characters.
pub const MAX_STRING_CHARS: usize = 511;

/// One namespace per kind, keyed by the normalised name.
#[derive(Debug, Default, Clone)]
pub struct Vars {
    ints: BTreeMap<String, i32>,
    strs: BTreeMap<String, String>,
    int_arrays: BTreeMap<String, Vec<i32>>,
    str_arrays: BTreeMap<String, Vec<String>>,
}

impl Vars {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read a scalar.
    pub fn get(&self, name: &str) -> Result<Value, Error> {
        let key = key(name);
        if let Some(value) = self.ints.get(&key) {
            return Ok(Value::Int(*value));
        }
        if let Some(value) = self.strs.get(&key) {
            return Ok(Value::Str(value.clone()));
        }
        if self.int_arrays.contains_key(&key) || self.str_arrays.contains_key(&key) {
            // §4.2: an array is a variable, but it is not a value — it cannot be
            // assigned, compared or passed, so using its name bare is a type
            // error rather than a reference to its contents.
            return Err(Error::AlreadyAnotherKind(name.to_owned()));
        }
        Err(Error::VariableNotInitialized(name.to_owned()))
    }

    /// Assign a scalar, fixing its type if this is the first assignment.
    pub fn set(&mut self, name: &str, value: Value) -> Result<(), Error> {
        let key = key(name);

        if self.int_arrays.contains_key(&key) || self.str_arrays.contains_key(&key) {
            return Err(Error::AlreadyAnotherKind(name.to_owned()));
        }

        match value {
            Value::Int(v) => {
                if self.ints.contains_key(&key) {
                    self.ints.insert(key, v);
                    return Ok(());
                }
                if self.strs.contains_key(&key) {
                    return Err(Error::TypeMismatch);
                }
                if self.ints.len() >= MAX_SCALARS {
                    return Err(Error::TooManyVariables("integer variables"));
                }
                self.ints.insert(key, v);
                Ok(())
            }
            Value::Str(s) => {
                if self.strs.contains_key(&key) {
                    self.strs.insert(key, s);
                    return Ok(());
                }
                if self.ints.contains_key(&key) {
                    return Err(Error::TypeMismatch);
                }
                if self.strs.len() >= MAX_SCALARS {
                    return Err(Error::TooManyVariables("string variables"));
                }
                self.strs.insert(key, s);
                Ok(())
            }
        }
    }

    /// `intdim <array> <size>` (§7.6): entries are integers, default 0.
    pub fn declare_int_array(&mut self, name: &str, size: i32) -> Result<(), Error> {
        let len = array_size(size)?;
        let key = key(name);
        self.refuse_if_another_kind(&key, name, Kind::IntArray)?;

        if !self.int_arrays.contains_key(&key) {
            if self.int_arrays.len() >= MAX_ARRAYS {
                return Err(Error::TooManyVariables("integer arrays"));
            }
        }
        self.int_arrays.insert(key, vec![0; len]);
        Ok(())
    }

    /// `strdim <array> <size>` (§7.6): entries are strings, default "".
    pub fn declare_str_array(&mut self, name: &str, size: i32) -> Result<(), Error> {
        let len = array_size(size)?;
        let key = key(name);
        self.refuse_if_another_kind(&key, name, Kind::StrArray)?;

        if !self.str_arrays.contains_key(&key) {
            if self.str_arrays.len() >= MAX_ARRAYS {
                return Err(Error::TooManyVariables("string arrays"));
            }
        }
        self.str_arrays.insert(key, vec![String::new(); len]);
        Ok(())
    }

    /// Read one element of an array.
    pub fn get_index(&self, name: &str, index: i32) -> Result<Value, Error> {
        let key = key(name);
        if let Some(array) = self.int_arrays.get(&key) {
            return Ok(Value::Int(array[index_of(name, index, array.len())?]));
        }
        if let Some(array) = self.str_arrays.get(&key) {
            return Ok(Value::Str(
                array[index_of(name, index, array.len())?].clone(),
            ));
        }
        if self.ints.contains_key(&key) || self.strs.contains_key(&key) {
            return Err(Error::AlreadyAnotherKind(name.to_owned()));
        }
        Err(Error::VariableNotInitialized(name.to_owned()))
    }

    /// Assign one element of an array.
    pub fn set_index(&mut self, name: &str, index: i32, value: Value) -> Result<(), Error> {
        let key = key(name);
        if let Some(array) = self.int_arrays.get_mut(&key) {
            let at = index_of(name, index, array.len())?;
            let v = value.as_int().ok_or(Error::TypeMismatch)?;
            array[at] = v;
            return Ok(());
        }
        if let Some(array) = self.str_arrays.get_mut(&key) {
            let at = index_of(name, index, array.len())?;
            let s = value.as_str().ok_or(Error::TypeMismatch)?.to_owned();
            array[at] = s;
            return Ok(());
        }
        if self.ints.contains_key(&key) || self.strs.contains_key(&key) {
            return Err(Error::AlreadyAnotherKind(name.to_owned()));
        }
        Err(Error::VariableNotInitialized(name.to_owned()))
    }

    /// How many elements an array has: what `listbox` walks and what
    /// `ifdefined`'s code 5 or 6 implies. `None` for a scalar or a name that
    /// was never declared.
    pub fn array_len(&self, name: &str) -> Option<i32> {
        let key = key(name);
        self.int_arrays
            .get(&key)
            .map(|array| array.len() as i32)
            .or_else(|| self.str_arrays.get(&key).map(|array| array.len() as i32))
    }

    /// The `ifdefined` code for a name (§7.6).
    ///
    /// 0 undefined, 1 integer, 3 string, 5 integer array, 6 string array. Code 4
    /// is a label, which lives in the program rather than in this namespace and
    /// is answered by the interpreter.
    pub fn defined(&self, name: &str) -> i32 {
        match self.kind(&key(name)) {
            Some(Kind::Int) => 1,
            Some(Kind::Str) => 3,
            Some(Kind::IntArray) => 5,
            Some(Kind::StrArray) => 6,
            None => 0,
        }
    }

    /// `delvar <var> [<var>...]` (5.8.0): return names to the unused state so a
    /// different type can be assigned.
    pub fn delvar(&mut self, name: &str) -> Result<(), Error> {
        let key = key(name);
        let removed = self.ints.remove(&key).is_some()
            || self.strs.remove(&key).is_some()
            || self.int_arrays.remove(&key).is_some()
            || self.str_arrays.remove(&key).is_some();
        if removed {
            Ok(())
        } else {
            Err(Error::VariableNotInitialized(name.to_owned()))
        }
    }

    /// `redim <array> <size> ['Preserve']` (5.8.0).
    ///
    /// Without `Preserve` the contents are reset; with it they are kept, and a
    /// shrink drops the tail. A grow fills with the element's default, which is
    /// what a new array of that size would have.
    pub fn redim(&mut self, name: &str, size: i32, preserve: bool) -> Result<(), Error> {
        let len = array_size(size)?;
        let key = key(name);

        if let Some(array) = self.int_arrays.get_mut(&key) {
            if preserve {
                array.resize(len, 0);
            } else {
                *array = vec![0; len];
            }
            return Ok(());
        }
        if let Some(array) = self.str_arrays.get_mut(&key) {
            if preserve {
                array.resize(len, String::new());
            } else {
                *array = vec![String::new(); len];
            }
            return Ok(());
        }
        if self.ints.contains_key(&key) || self.strs.contains_key(&key) {
            return Err(Error::AlreadyAnotherKind(name.to_owned()));
        }
        Err(Error::VariableNotInitialized(name.to_owned()))
    }

    /// Whether a name holds anything, of any kind. Used by the interpreter
    /// before a command that requires an existing variable.
    pub fn is_defined(&self, name: &str) -> bool {
        self.kind(&key(name)).is_some()
    }

    /// How many names of each kind are in use: integers, strings, integer
    /// arrays, string arrays. For tests and for `-v`-style reporting.
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        (
            self.ints.len(),
            self.strs.len(),
            self.int_arrays.len(),
            self.str_arrays.len(),
        )
    }

    fn kind(&self, key: &str) -> Option<Kind> {
        if self.ints.contains_key(key) {
            Some(Kind::Int)
        } else if self.strs.contains_key(key) {
            Some(Kind::Str)
        } else if self.int_arrays.contains_key(key) {
            Some(Kind::IntArray)
        } else if self.str_arrays.contains_key(key) {
            Some(Kind::StrArray)
        } else {
            None
        }
    }

    /// A declaration may reuse its own kind — `intdim a 5` twice is a reset —
    /// but may not turn a scalar or the other kind of array into this one.
    fn refuse_if_another_kind(&self, key: &str, name: &str, wanted: Kind) -> Result<(), Error> {
        match self.kind(key) {
            None => Ok(()),
            Some(kind) if kind == wanted => Ok(()),
            Some(_) => Err(Error::AlreadyAnotherKind(name.to_owned())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Int,
    Str,
    IntArray,
    StrArray,
}

/// §3.4: case-insensitive, and a name past 31 characters is cut, not refused.
fn key(name: &str) -> String {
    name.chars()
        .take(MAX_NAME_CHARS)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn array_size(size: i32) -> Result<usize, Error> {
    if (1..=MAX_ARRAY_SIZE as i32).contains(&size) {
        Ok(size as usize)
    } else {
        Err(Error::InvalidArraySize(size))
    }
}

/// A negative index cannot be a `usize`, and one past the end is §8's error;
/// both are reported as `Index out of range.`
fn index_of(name: &str, index: i32, len: usize) -> Result<usize, Error> {
    usize::try_from(index)
        .ok()
        .filter(|i| *i < len)
        .ok_or_else(|| Error::IndexOutOfRange {
            name: name.to_owned(),
            index,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_variable_takes_its_type_at_the_first_assignment() {
        let mut vars = Vars::new();
        vars.set("A", Value::Int(33)).unwrap();
        assert_eq!(vars.get("A").unwrap(), Value::Int(33));

        assert_eq!(
            vars.set("A", Value::from("thirty")),
            Err(Error::TypeMismatch)
        );
        assert_eq!(vars.get("A").unwrap(), Value::Int(33));
    }

    #[test]
    fn reading_before_assigning_is_an_error_not_a_zero() {
        let vars = Vars::new();
        assert_eq!(
            vars.get("nobody"),
            Err(Error::VariableNotInitialized("nobody".into()))
        );
    }

    #[test]
    fn names_are_case_insensitive() {
        let mut vars = Vars::new();
        vars.set("Count", Value::Int(1)).unwrap();
        assert_eq!(vars.get("count").unwrap(), Value::Int(1));
        assert_eq!(vars.defined("COUNT"), 1);
        assert_eq!(vars.counts().0, 1, "one variable, not three");
    }

    #[test]
    fn a_name_past_31_characters_is_the_same_variable() {
        let mut vars = Vars::new();
        let long = "a".repeat(40);
        let cut = "a".repeat(31);
        vars.set(&long, Value::Int(7)).unwrap();
        assert_eq!(vars.get(&cut).unwrap(), Value::Int(7));
        assert_eq!(vars.counts().0, 1);
    }

    #[test]
    fn the_four_budgets_are_separate() {
        let mut vars = Vars::new();
        for i in 0..MAX_SCALARS {
            vars.set(&format!("i{i}"), Value::Int(i as i32)).unwrap();
        }
        assert_eq!(
            vars.set("one_too_many", Value::Int(0)),
            Err(Error::TooManyVariables("integer variables"))
        );
        // The string budget is untouched by an exhausted integer budget.
        vars.set("s", Value::from("x")).unwrap();
        assert_eq!(vars.get("s").unwrap(), Value::from("x"));
    }

    #[test]
    fn arrays_are_declared_with_their_size_and_defaults() {
        let mut vars = Vars::new();
        vars.declare_int_array("n", 3).unwrap();
        assert_eq!(vars.get_index("n", 2).unwrap(), Value::Int(0));
        vars.set_index("n", 2, Value::Int(9)).unwrap();
        assert_eq!(vars.get_index("n", 2).unwrap(), Value::Int(9));

        vars.declare_str_array("s", 2).unwrap();
        assert_eq!(vars.get_index("s", 0).unwrap(), Value::from(""));
        // The declared size is exactly the size: 3 entries are 0, 1, 2.
        assert!(matches!(
            vars.get_index("n", 3),
            Err(Error::IndexOutOfRange { .. })
        ));
        assert!(matches!(
            vars.get_index("n", -1),
            Err(Error::IndexOutOfRange { .. })
        ));
    }

    #[test]
    fn an_array_is_not_a_scalar() {
        let mut vars = Vars::new();
        vars.declare_int_array("n", 3).unwrap();
        assert!(vars.get("n").is_err());
        assert!(vars.set("n", Value::Int(1)).is_err());
        // And an element will not take the other type.
        assert_eq!(
            vars.set_index("n", 0, Value::from("x")),
            Err(Error::TypeMismatch)
        );
    }

    #[test]
    fn a_size_outside_one_to_65536_is_refused() {
        let mut vars = Vars::new();
        assert_eq!(
            vars.declare_int_array("z", 0),
            Err(Error::InvalidArraySize(0))
        );
        assert_eq!(
            vars.declare_int_array("big", 65_537),
            Err(Error::InvalidArraySize(65_537))
        );
        vars.declare_int_array("big", 65_536).unwrap();
    }

    #[test]
    fn the_array_budgets_are_separate_too() {
        let mut vars = Vars::new();
        for i in 0..MAX_ARRAYS {
            vars.declare_int_array(&format!("a{i}"), 1).unwrap();
        }
        assert_eq!(
            vars.declare_int_array("one_more", 1),
            Err(Error::TooManyVariables("integer arrays"))
        );
        vars.declare_str_array("still_fine", 1).unwrap();
    }

    #[test]
    fn ifdefined_reports_the_four_kinds() {
        let mut vars = Vars::new();
        assert_eq!(vars.defined("x"), 0);
        vars.set("x", Value::Int(0)).unwrap();
        assert_eq!(vars.defined("x"), 1);
        vars.set("y", Value::from("")).unwrap();
        assert_eq!(vars.defined("y"), 3);
        vars.declare_int_array("ia", 1).unwrap();
        assert_eq!(vars.defined("ia"), 5);
        vars.declare_str_array("sa", 1).unwrap();
        assert_eq!(vars.defined("sa"), 6);
    }

    #[test]
    fn delvar_frees_the_name_and_its_budget() {
        let mut vars = Vars::new();
        vars.set("A", Value::Int(1)).unwrap();
        vars.delvar("A").unwrap();
        assert_eq!(vars.defined("A"), 0);
        // The type is no longer fixed, which is the point of delvar (§4.2).
        vars.set("A", Value::from("now a string")).unwrap();
        assert_eq!(vars.defined("A"), 3);
        assert_eq!(vars.counts(), (0, 1, 0, 0));
        // Deleting what is not there is an error, not a no-op.
        assert!(vars.delvar("A").is_ok());
        assert_eq!(
            vars.delvar("A"),
            Err(Error::VariableNotInitialized("A".into()))
        );
    }

    #[test]
    fn redim_keeps_or_drops_the_contents() {
        let mut vars = Vars::new();
        vars.declare_int_array("n", 3).unwrap();
        vars.set_index("n", 0, Value::Int(7)).unwrap();
        vars.set_index("n", 1, Value::Int(8)).unwrap();

        vars.redim("n", 4, true).unwrap();
        assert_eq!(vars.get_index("n", 0).unwrap(), Value::Int(7));
        assert_eq!(
            vars.get_index("n", 3).unwrap(),
            Value::Int(0),
            "new tail is default"
        );

        vars.redim("n", 2, true).unwrap();
        assert_eq!(vars.get_index("n", 1).unwrap(), Value::Int(8));
        assert!(vars.get_index("n", 2).is_err(), "the tail is gone");

        vars.redim("n", 4, false).unwrap();
        assert_eq!(
            vars.get_index("n", 0).unwrap(),
            Value::Int(0),
            "not preserved"
        );
    }

    #[test]
    fn redim_needs_an_array() {
        let mut vars = Vars::new();
        vars.set("A", Value::Int(1)).unwrap();
        assert!(vars.redim("A", 2, false).is_err());
        assert!(vars.redim("nothing", 2, false).is_err());
    }
}
