//! A directory listing whose attributes arrive with the name.
//!
//! `std::fs::read_dir` is enough for a listing on Unix, where "is this hidden" is
//! the first character of the name and a `d_type` says what the entry is. Windows
//! is not that:
//!
//! - **Hidden is an attribute**, so the name says nothing and the answer is
//!   `FILE_ATTRIBUTE_HIDDEN` — which `std::fs::DirEntry` does not carry. Asking for
//!   it means a `stat` per file, and a second pass that can move a row between the
//!   visible and the hidden half *after* it was drawn.
//! - **`FindFirstFileW` already knows everything the row needs** — the attributes,
//!   the size and the write time — in the call that carries the name, so one walk
//!   costs one syscall per entry and no `stat` at all.
//!
//! So the walk is this: `FindFirstFileW` for the first entry, `FindNextFileW` until
//! it says there are no more, and a `Found` for each, with `.` and `..` skipped
//! because a listing's parent row is the view's business and not the filesystem's.
//!
//! The iterator is fallible per entry: one unreadable name does not end a directory,
//! and the caller shows the row it can and moves on.

use std::path::Path;
use std::time::SystemTime;

/// One name a walk found, with everything the same call said about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// The file name alone.
    pub name: String,
    pub is_dir: bool,
    /// The size, for a file. `None` for a directory, whose length is not a fact.
    pub len: Option<u64>,
    pub modified: Option<SystemTime>,
    /// Whether this machine calls it hidden: `FILE_ATTRIBUTE_HIDDEN` or
    /// `FILE_ATTRIBUTE_SYSTEM`. Both, because Explorer hides both by default and a
    /// `desktop.ini` carries both — its directories would otherwise be a pane of
    /// `$Recycle.Bin`, `System Volume Information` and a page file nobody wants to
    /// pick.
    pub hidden: bool,
}

/// `FILETIME`'s count of 100-nanosecond ticks since 1601 as a `SystemTime`.
///
/// Zero is "this file has no time", which is a real answer for a filesystem that
/// does not keep one, and a time before 1970 cannot be a `SystemTime` — both are
/// `None` rather than 1970, because 1970 sorts first and looks like a fact.
///
/// The offset is the number of those ticks between 1601 and 1970, and it is a
/// constant of the calendar, not a guess: 116 444 736 000 000 000.
#[cfg(any(windows, test))]
pub(crate) fn system_time(ticks: u64) -> Option<SystemTime> {
    const EPOCH: u64 = 116_444_736_000_000_000;
    let since = ticks.checked_sub(EPOCH)?;
    Some(std::time::UNIX_EPOCH + std::time::Duration::from_nanos(since.checked_mul(100)?))
}

/// A directory walk in progress.
///
/// The handle is closed on drop, and the iterator ends itself after the first
/// `ERROR_NO_MORE_FILES`: a walk that keeps calling `FindNextFileW` after the end is
/// a loop that never terminates on a machine that answers the same error forever.
pub struct Dir {
    inner: platform::Inner,
}

impl Dir {
    /// Start a walk of `path`. A directory that cannot be read is an error here, with
    /// the OS's own message, rather than an empty listing.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        Ok(Self {
            inner: platform::Inner::open(path)?,
        })
    }
}

impl Iterator for Dir {
    type Item = std::io::Result<Found>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next()
    }
}

#[cfg(windows)]
mod platform {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_SYSTEM, FindClose,
        FindFirstFileW, FindNextFileW, WIN32_FIND_DATAW,
    };

    use super::{Found, system_time};

    pub struct Inner {
        handle: windows_sys::Win32::Foundation::HANDLE,
        data: WIN32_FIND_DATAW,
        finished: bool,
    }

    impl Inner {
        pub fn open(path: &Path) -> std::io::Result<Self> {
            let pattern = pattern(path);
            let mut data: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
            let handle = unsafe { FindFirstFileW(pattern.as_ptr(), &mut data) };
            if handle == INVALID_HANDLE_VALUE || handle.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            Ok(Self {
                handle,
                data,
                finished: false,
            })
        }
    }

    impl Iterator for Inner {
        type Item = std::io::Result<Found>;

        fn next(&mut self) -> Option<Self::Item> {
            loop {
                if self.finished {
                    return None;
                }
                let found = found(&self.data);
                // Advance before answering, so the last entry of a directory is
                // returned by the call that also learns there is nothing after it.
                let mut data: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
                if unsafe { FindNextFileW(self.handle, &mut data) } == 0 {
                    self.finished = true;
                } else {
                    self.data = data;
                }
                // `.` and `..` are the walk's own bookkeeping: the parent row is the
                // view's, and it is not a file in this directory.
                if let Some(found) = found {
                    return Some(Ok(found));
                }
            }
        }
    }

    impl Drop for Inner {
        fn drop(&mut self) {
            unsafe { FindClose(self.handle) };
        }
    }

    /// `dir\*`, which is what a find pattern is: a name, not a path.
    fn pattern(path: &Path) -> Vec<u16> {
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        let separator = b'\\' as u16;
        let slash = b'/' as u16;
        if !matches!(wide.last(), Some(&unit) if unit == separator || unit == slash) {
            wide.push(separator);
        }
        wide.push(b'*' as u16);
        wide.push(0);
        wide
    }

    fn found(data: &WIN32_FIND_DATAW) -> Option<Found> {
        let length = data
            .cFileName
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(0);
        let name = String::from_utf16_lossy(&data.cFileName[..length]);
        if name == "." || name == ".." {
            return None;
        }
        let directory = data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
        let ticks = ((data.ftLastWriteTime.dwHighDateTime as u64) << 32)
            | data.ftLastWriteTime.dwLowDateTime as u64;
        Some(Found {
            name,
            is_dir: directory,
            len: (!directory)
                .then_some(((data.nFileSizeHigh as u64) << 32) | data.nFileSizeLow as u64),
            modified: system_time(ticks),
            hidden: data.dwFileAttributes & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0,
        })
    }
}

#[cfg(not(windows))]
mod platform {
    use std::path::Path;

    use super::Found;

    /// There is no shell to walk: `dtr-vfs`'s own listing is `read_dir`, and this is
    /// the answer that says so out loud rather than a silent empty directory.
    pub struct Inner;

    impl Inner {
        pub fn open(_path: &Path) -> std::io::Result<Self> {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "dtr-shell walks the Windows shell, and this is not Windows",
            ))
        }
    }

    impl Iterator for Inner {
        type Item = std::io::Result<Found>;

        fn next(&mut self) -> Option<Self::Item> {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The offset is the calendar's, and a file written a second after 1970 is a
    /// second after 1970 — the test that catches a decimal typo in a constant nobody
    /// can read.
    #[test]
    fn a_file_time_is_measured_from_the_windows_epoch() {
        let unix_epoch = 116_444_736_000_000_000u64;
        assert_eq!(system_time(unix_epoch), Some(std::time::UNIX_EPOCH));
        assert_eq!(
            system_time(unix_epoch + 10_000_000),
            Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))
        );
        // 2026-11-15T00:00:00Z, in those ticks.
        let known = system_time(116_444_736_000_000_000 + 1_764_000_000 * 10_000_000);
        assert_eq!(
            known,
            Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_764_000_000))
        );
    }

    /// A file with no time, and a file from before 1970, are both "no date" rather
    /// than 1970 — which would sort first and look like a fact.
    #[test]
    fn a_missing_or_ancient_time_is_none() {
        assert_eq!(system_time(0), None);
        assert_eq!(system_time(116_444_736_000_000_000 - 1), None);
        // And a value that would overflow the nanosecond multiply is refused rather
        // than wrapped.
        assert_eq!(system_time(u64::MAX), None);
    }

    /// On a machine with no shell the walk is an error that says why, so a caller
    /// that reaches for it by mistake is told rather than shown an empty directory.
    #[cfg(not(windows))]
    #[test]
    fn there_is_no_walk_without_a_shell() {
        // `err()` rather than `expect_err`: a `Dir` holds a find handle and is not
        // `Debug`, and making it so to satisfy a test would be the tail wagging the
        // dog.
        let err = Dir::open(Path::new("/tmp")).err().expect("no shell");
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    }
}
