//! The file system the §7.4 commands work on.
//!
//! The interpreter does not touch `std::fs`. A macro is written against
//! whatever machine it runs on, and a test that reads and writes the real disk
//! is a test that leaves files behind and cannot say what was in them. So the
//! file commands talk to [`Files`], [`MemoryFiles`] is the one a test uses, and
//! a real implementation for the host is a small wrapper this crate will grow
//! with the application.
//!
//! Two conventions of §7.4 live here rather than in the commands, because they
//! are properties of files: **`result` is 0 for success** in this family (the
//! opposite of the connection commands), and `filereadln`'s `result` is 1 when
//! the read hit the end of the file. Each command in [`crate::commands`] maps
//! this module's `bool`/`Option` onto those numbers, and says so.

use std::collections::BTreeMap;

/// A position in an open file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Open {
    path: String,
    position: usize,
    read_only: bool,
    append: bool,
}

/// What a macro's `fileopen` and its relatives need.
pub trait Files {
    /// `fileopen`: `append` and `read_only` are its two flags. `Err` is the
    /// failure `result` reports as non-zero.
    fn open(&mut self, path: &str, append: bool, read_only: bool) -> Result<i32, ()>;

    /// `filecreate`: create or truncate, then open for writing.
    fn create(&mut self, path: &str) -> Result<i32, ()>;

    fn close(&mut self, handle: i32) -> bool;

    /// One line, without its terminator. `None` at end of file — which the
    /// command turns into `result` 1.
    fn read_line(&mut self, handle: i32) -> Option<String>;

    fn read(&mut self, handle: i32, bytes: usize) -> Option<Vec<u8>>;

    fn write(&mut self, handle: i32, data: &[u8]) -> bool;

    /// `fileseek`: `origin` 0 = start, 1 = current, 2 = end.
    fn seek(&mut self, handle: i32, offset: i64, origin: i32) -> bool;

    /// Where the marker is, for `filemarkptr`.
    fn tell(&mut self, handle: i32) -> Option<i64>;

    /// `fileseekback`: back to a position the macro remembered.
    fn seek_to(&mut self, handle: i32, position: i64) -> bool;

    /// `filetruncate`, by name: truncate or extend, filling with NUL.
    fn truncate(&mut self, path: &str, size: u64) -> bool;

    fn delete(&mut self, path: &str) -> bool;

    fn exists(&mut self, path: &str) -> bool;

    fn create_dir(&mut self, path: &str) -> bool;

    /// `filestat`'s size.
    fn size(&mut self, path: &str) -> Option<u64>;

    /// `filemarkptr`: remember where the marker is, per handle.
    fn mark(&mut self, handle: i32) -> bool;

    /// `fileseekback`: go back to the mark.
    fn seek_mark(&mut self, handle: i32) -> bool;

    /// `filecopy`, `filerename`, `fileconcat`: 0 for success, as ever here.
    fn copy(&mut self, from: &str, to: &str) -> bool;
    fn rename(&mut self, from: &str, to: &str) -> bool;
    fn concat(&mut self, to: &str, from: &str) -> bool;

    /// `findfirst`: a handle over the names matching a pattern, or `None` when
    /// nothing matches.
    fn find_start(&mut self, pattern: &str) -> Option<i32>;
    fn find_next(&mut self, handle: i32) -> Option<String>;
    fn find_close(&mut self, handle: i32) -> bool;
}

/// Files in memory, which is what a test and a dry run want.
#[derive(Debug, Default, Clone)]
pub struct MemoryFiles {
    files: BTreeMap<String, Vec<u8>>,
    open: BTreeMap<i32, Open>,
    next_handle: i32,
    /// `filemarkptr` positions, per handle.
    marks: BTreeMap<i32, i64>,
    /// `findfirst`'s remaining names, per handle.
    finds: BTreeMap<i32, (Vec<String>, usize)>,
}

impl MemoryFiles {
    pub fn new() -> Self {
        Self::default()
    }

    /// Put a file there before the macro runs.
    pub fn with_file(mut self, path: &str, contents: &[u8]) -> Self {
        self.files.insert(path.to_string(), contents.to_vec());
        self
    }

    pub fn add(&mut self, path: &str, contents: &[u8]) {
        self.files.insert(path.to_string(), contents.to_vec());
    }

    /// What a file holds now: the assertion a file-handling macro's test makes.
    pub fn contents(&self, path: &str) -> Option<&[u8]> {
        self.files.get(path).map(Vec::as_slice)
    }

    pub fn paths(&self) -> Vec<&str> {
        self.files.keys().map(String::as_str).collect()
    }
}

impl Files for MemoryFiles {
    fn open(&mut self, path: &str, append: bool, read_only: bool) -> Result<i32, ()> {
        if !self.files.contains_key(path) {
            if read_only {
                return Err(());
            }
            // `fileopen` with the append flag creates; so does writing.
            self.files.insert(path.to_string(), Vec::new());
        }
        let handle = self.next_handle;
        self.next_handle += 1;
        let position = if append {
            self.files.get(path).map_or(0, Vec::len)
        } else {
            0
        };
        self.open.insert(
            handle,
            Open {
                path: path.to_string(),
                position,
                read_only,
                append,
            },
        );
        Ok(handle)
    }

    fn create(&mut self, path: &str) -> Result<i32, ()> {
        self.files.insert(path.to_string(), Vec::new());
        self.open(path, false, false)
    }

    fn close(&mut self, handle: i32) -> bool {
        self.open.remove(&handle).is_some()
    }

    fn read_line(&mut self, handle: i32) -> Option<String> {
        let open = self.open.get_mut(&handle)?;
        let bytes = self.files.get(&open.path)?;
        if open.position >= bytes.len() {
            return None;
        }
        let start = open.position;
        let mut end = start;
        while end < bytes.len() && bytes[end] != b'\n' && bytes[end] != b'\r' {
            end += 1;
        }
        let line = String::from_utf8_lossy(&bytes[start..end]).into_owned();
        // CR LF, LF and CR all end a line; step over whichever it was.
        let mut next = end;
        if next < bytes.len() && bytes[next] == b'\r' {
            next += 1;
        }
        if next < bytes.len() && bytes[next] == b'\n' {
            next += 1;
        }
        open.position = next;
        Some(line)
    }

    fn read(&mut self, handle: i32, bytes: usize) -> Option<Vec<u8>> {
        let open = self.open.get_mut(&handle)?;
        let data = self.files.get(&open.path)?;
        if open.position >= data.len() {
            return None;
        }
        let end = (open.position + bytes).min(data.len());
        let slice = data[open.position..end].to_vec();
        open.position = end;
        Some(slice)
    }

    fn write(&mut self, handle: i32, data: &[u8]) -> bool {
        let Some(open) = self.open.get_mut(&handle) else {
            return false;
        };
        if open.read_only {
            return false;
        }
        let (path, position, append) = (open.path.clone(), open.position, open.append);
        let Some(bytes) = self.files.get_mut(&path) else {
            return false;
        };
        let at = if append { bytes.len() } else { position };
        if at > bytes.len() {
            bytes.resize(at, 0);
        }
        let end = at + data.len();
        if end > bytes.len() {
            bytes.resize(end, 0);
        }
        bytes[at..end].copy_from_slice(data);
        if let Some(open) = self.open.get_mut(&handle) {
            open.position = end;
        }
        true
    }

    fn seek(&mut self, handle: i32, offset: i64, origin: i32) -> bool {
        let Some(open) = self.open.get(&handle) else {
            return false;
        };
        let length = self.files.get(&open.path).map_or(0, Vec::len);
        let base = match origin {
            0 => 0,
            1 => open.position as i64,
            2 => length as i64,
            _ => return false,
        };
        let target = base + offset;
        if target < 0 {
            return false;
        }
        if let Some(open) = self.open.get_mut(&handle) {
            open.position = target as usize;
        }
        true
    }

    fn tell(&mut self, handle: i32) -> Option<i64> {
        self.open.get(&handle).map(|open| open.position as i64)
    }

    fn seek_to(&mut self, handle: i32, position: i64) -> bool {
        self.seek(handle, position, 0)
    }

    fn truncate(&mut self, path: &str, size: u64) -> bool {
        // Truncating a name that is not there creates it, which is what
        // `filetruncate` is used for on a fresh file (`test_file.ttl` does
        // exactly that and then asks for the size).
        let bytes = self.files.entry(path.to_string()).or_default();
        bytes.resize(size as usize, 0);
        true
    }

    fn delete(&mut self, path: &str) -> bool {
        self.files.remove(path).is_some()
    }

    fn exists(&mut self, path: &str) -> bool {
        self.files.contains_key(path)
    }

    fn create_dir(&mut self, _path: &str) -> bool {
        // A memory file system has no directories, and `makepath` creates
        // nothing anyway (§7.4).
        true
    }

    fn size(&mut self, path: &str) -> Option<u64> {
        self.files.get(path).map(|bytes| bytes.len() as u64)
    }

    fn mark(&mut self, handle: i32) -> bool {
        let Some(open) = self.open.get(&handle) else {
            return false;
        };
        let position = open.position as i64;
        self.marks.insert(handle, position);
        true
    }

    fn seek_mark(&mut self, handle: i32) -> bool {
        let Some(position) = self.marks.get(&handle).copied() else {
            return false;
        };
        self.seek_to(handle, position)
    }

    fn copy(&mut self, from: &str, to: &str) -> bool {
        let Some(bytes) = self.files.get(from).cloned() else {
            return false;
        };
        self.files.insert(to.to_string(), bytes);
        true
    }

    fn rename(&mut self, from: &str, to: &str) -> bool {
        let Some(bytes) = self.files.remove(from) else {
            return false;
        };
        self.files.insert(to.to_string(), bytes);
        true
    }

    fn concat(&mut self, to: &str, from: &str) -> bool {
        if to == from {
            return false;
        }
        let Some(more) = self.files.get(from).cloned() else {
            return false;
        };
        let Some(target) = self.files.get_mut(to) else {
            return false;
        };
        target.extend_from_slice(&more);
        true
    }

    fn find_start(&mut self, pattern: &str) -> Option<i32> {
        let names: Vec<String> = self
            .files
            .keys()
            .filter(|path| {
                let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
                matches_pattern(name, pattern)
            })
            .cloned()
            .collect();
        if names.is_empty() {
            return None;
        }
        let handle = self.next_handle;
        self.next_handle += 1;
        self.finds.insert(handle, (names, 0));
        Some(handle)
    }

    fn find_next(&mut self, handle: i32) -> Option<String> {
        let (names, at) = self.finds.get_mut(&handle)?;
        let name = names.get(*at)?.clone();
        *at += 1;
        Some(name)
    }

    fn find_close(&mut self, handle: i32) -> bool {
        self.finds.remove(&handle).is_some()
    }
}

/// `*` and `?`, the two wildcards `findfirst` has always understood.
fn matches_pattern(name: &str, pattern: &str) -> bool {
    let name: Vec<char> = name.chars().collect();
    let pattern: Vec<char> = pattern.chars().collect();
    let (mut n, mut p) = (0usize, 0usize);
    let (mut star, mut resume) = (None::<usize>, 0usize);
    while n < name.len() {
        match pattern.get(p) {
            Some('?') => {
                n += 1;
                p += 1;
            }
            Some('*') => {
                star = Some(p);
                resume = n;
                p += 1;
            }
            Some(&literal) if literal == name[n] => {
                n += 1;
                p += 1;
            }
            _ => match star {
                Some(at) => {
                    p = at + 1;
                    resume += 1;
                    n = resume;
                }
                None => return false,
            },
        }
    }
    while pattern.get(p) == Some(&'*') {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_ends_at_any_of_the_three_terminators() {
        let mut files = MemoryFiles::new().with_file("f", b"one\r\ntwo\nthree\r");
        let handle = files.open("f", false, true).unwrap();
        assert_eq!(files.read_line(handle).as_deref(), Some("one"));
        assert_eq!(files.read_line(handle).as_deref(), Some("two"));
        assert_eq!(files.read_line(handle).as_deref(), Some("three"));
        assert_eq!(files.read_line(handle), None);
    }

    #[test]
    fn an_empty_line_is_not_the_end() {
        let mut files = MemoryFiles::new().with_file("f", b"\n");
        let handle = files.open("f", false, true).unwrap();
        assert_eq!(files.read_line(handle).as_deref(), Some(""));
        assert_eq!(files.read_line(handle), None);
    }

    #[test]
    fn append_starts_at_the_end_and_write_extends() {
        let mut files = MemoryFiles::new().with_file("f", b"abc");
        let handle = files.open("f", true, false).unwrap();
        assert!(files.write(handle, b"de"));
        assert_eq!(files.contents("f").unwrap(), b"abcde");
    }

    #[test]
    fn opening_a_missing_file_for_reading_fails() {
        let mut files = MemoryFiles::new();
        assert!(files.open("missing", false, true).is_err());
        // …and for writing it is created.
        assert!(files.open("new", false, false).is_ok());
        assert_eq!(files.contents("new").unwrap(), b"");
    }

    #[test]
    fn seek_origin_and_marker() {
        let mut files = MemoryFiles::new().with_file("f", b"0123456789");
        let handle = files.open("f", false, true).unwrap();
        assert!(files.seek(handle, 4, 0));
        assert_eq!(files.tell(handle), Some(4));
        assert_eq!(files.read(handle, 2).unwrap(), b"45");
        assert!(files.seek(handle, -1, 1));
        assert_eq!(files.tell(handle), Some(5));
        assert!(files.seek(handle, -2, 2));
        assert_eq!(files.tell(handle), Some(8));
        assert!(!files.seek(handle, -100, 1));
    }

    #[test]
    fn truncate_extends_with_nul() {
        let mut files = MemoryFiles::new().with_file("f", b"abc");
        assert!(files.truncate("f", 5));
        assert_eq!(files.contents("f").unwrap(), b"abc\0\0");
        assert!(files.truncate("f", 2));
        assert_eq!(files.contents("f").unwrap(), b"ab");
    }

    #[test]
    fn a_read_only_handle_refuses_to_write() {
        let mut files = MemoryFiles::new().with_file("f", b"abc");
        let handle = files.open("f", false, true).unwrap();
        assert!(!files.write(handle, b"x"));
    }
}
