//! The user's folders and the machine's drives: what a navigation pane lists.
//!
//! A freedesktop machine answers this with `user-dirs.dirs`, which `dtr-files`
//! reads, and the roots are one `/`. Windows has neither:
//!
//! - **A known folder is not a path.** `Documents` need not be
//!   `%USERPROFILE%\Documents` — OneDrive is free to move it, and a person may have
//!   redirected it to another drive — so it is asked for by `KNOWNFOLDERID` and not
//!   composed. A composed path would be a menu row that is right on a stock install
//!   and wrong on the machine that moved it, which is the machine whose owner cares.
//! - **The roots are drives.** There is no `/`, so "the filesystem" is a list, and
//!   each drive has a volume label, a kind, and possibly no media at all.
//!
//! The labels are Explorer's own — `win (C:)` for a labelled volume,
//! `Local Disk (C:)` for an unlabelled one, `DVD Drive (D:)` for a drive with no
//! media to ask. That rule is the only part with a decision in it, so it is a
//! function here with tests beside it, and the Win32 half below it is allowed to be
//! boring.

use std::path::PathBuf;

/// One row of a navigation pane that belongs to the platform rather than to the
/// config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    /// What the row says: a word of ours in lower case (`downloads`), or the
    /// volume's own spelling where it is a drive's.
    pub label: String,
    pub path: PathBuf,
}

/// What a drive is, which decides the word Explorer puts in front of the letter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveKind {
    Fixed,
    Removable,
    Remote,
    Optical,
    Ram,
    Unknown,
}

impl DriveKind {
    /// `GetDriveTypeW`'s answer as a kind. The numbers are the API's own
    /// `DRIVE_*` values; `DRIVE_UNKNOWN` and `DRIVE_NO_ROOT_DIR` both land in
    /// [`Self::Unknown`], which still lists the drive rather than hiding it.
    pub fn of(drive_type: u32) -> Self {
        match drive_type {
            2 => Self::Removable,
            3 => Self::Fixed,
            4 => Self::Remote,
            5 => Self::Optical,
            6 => Self::Ram,
            _ => Self::Unknown,
        }
    }

    /// The noun Explorer uses for a drive with no label of its own.
    fn noun(self) -> &'static str {
        match self {
            Self::Fixed => "Local Disk",
            Self::Removable => "Removable Disk",
            Self::Remote => "Network Drive",
            Self::Optical => "DVD Drive",
            Self::Ram => "Ram Disk",
            Self::Unknown => "Disk",
        }
    }
}

/// What to call a drive: its own label when it has one, the kind's noun when it
/// does not.
///
/// A volume label is padded to the filesystem's limit, and an empty or unreachable
/// volume answers nothing at all — which is not an error, because Explorer lists the
/// drive and lets the press ask for a disc.
pub fn drive_label(volume: &str, letter: char, kind: DriveKind) -> String {
    let volume = volume.trim();
    if volume.is_empty() {
        format!("{} ({letter}:)", kind.noun())
    } else {
        format!("{volume} ({letter}:)")
    }
}

/// The user's known folders, then the drives.
pub fn places() -> Vec<Place> {
    platform::places()
}

#[cfg(windows)]
mod platform {
    use std::ffi::{OsString, c_void};
    use std::os::windows::ffi::OsStringExt;
    use std::path::PathBuf;

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        GetDriveTypeW, GetLogicalDriveStringsW, GetVolumeInformationW,
    };
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{
        FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music,
        FOLDERID_Pictures, FOLDERID_Videos, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
    };

    use super::{DriveKind, Place, drive_label};

    /// The user's folders, in the order Explorer's navigation pane shows them.
    const KNOWN: &[(&windows_sys::core::GUID, &str)] = &[
        (&FOLDERID_Desktop, "desktop"),
        (&FOLDERID_Downloads, "downloads"),
        (&FOLDERID_Documents, "documents"),
        (&FOLDERID_Pictures, "pictures"),
        (&FOLDERID_Music, "music"),
        (&FOLDERID_Videos, "videos"),
    ];

    pub fn places() -> Vec<Place> {
        let mut places = known_folders();
        places.extend(drives());
        places
    }

    fn known_folders() -> Vec<Place> {
        let mut places: Vec<Place> = Vec::new();
        for (id, label) in KNOWN {
            let Some(path) = known_folder(id) else {
                continue;
            };
            // Two ids can name one directory on a machine somebody has tidied, and
            // a menu of two identical rows is a menu with a bug.
            if places.iter().any(|place| place.path == path) {
                continue;
            }
            places.push(Place {
                label: (*label).to_owned(),
                path,
            });
        }
        places
    }

    fn known_folder(id: &windows_sys::core::GUID) -> Option<PathBuf> {
        let mut wide: *mut u16 = std::ptr::null_mut();
        // `KF_FLAG_DEFAULT` and not `KF_FLAG_CREATE`: a folder that does not exist is
        // not created by looking for it.
        let answer = unsafe {
            SHGetKnownFolderPath(
                id,
                KF_FLAG_DEFAULT as u32,
                std::ptr::null_mut::<c_void>() as HANDLE,
                &mut wide,
            )
        };
        if answer < 0 || wide.is_null() {
            return None;
        }
        let length = unsafe { (0..).take_while(|offset| *wide.add(*offset) != 0).count() };
        let path = OsString::from_wide(unsafe { std::slice::from_raw_parts(wide, length) });
        // The string is the shell's allocation, not ours.
        unsafe { CoTaskMemFree(wide as *const c_void) };
        Some(PathBuf::from(path))
    }

    /// Every drive with a root, as `C:\`, with the label its volume reports.
    fn drives() -> Vec<Place> {
        // `A:\0B:\0…\0\0`: asking with no buffer is how the length is got.
        let needed = unsafe { GetLogicalDriveStringsW(0, std::ptr::null_mut()) };
        if needed == 0 {
            return Vec::new();
        }
        let mut buffer = vec![0u16; needed as usize + 1];
        let written = unsafe { GetLogicalDriveStringsW(buffer.len() as u32, buffer.as_mut_ptr()) };
        if written == 0 {
            return Vec::new();
        }
        let mut places = Vec::new();
        for root in buffer[..written as usize].split(|unit| *unit == 0) {
            let Some(letter) = root.first().and_then(|unit| char::from_u32(*unit as u32)) else {
                continue;
            };
            // The root keeps its separator — `C:\` is a root and `C:` is a
            // drive-relative name in Windows, which is not what a place means — so
            // the terminator goes back on for the API calls and off again for the
            // path.
            let mut wide = root.to_vec();
            wide.push(0);
            let kind = DriveKind::of(unsafe { GetDriveTypeW(wide.as_ptr()) });
            let volume = volume_name(&wide);
            places.push(Place {
                label: drive_label(&volume, letter, kind),
                path: PathBuf::from(OsString::from_wide(root)),
            });
        }
        places
    }

    /// The volume's label, or `""` — an empty drive, a network path that is asleep
    /// and a CD-ROM with no disc all answer nothing, which is not an error.
    fn volume_name(root: &[u16]) -> String {
        let mut buffer = [0u16; 261];
        let answered: windows_sys::core::BOOL = unsafe {
            GetVolumeInformationW(
                root.as_ptr(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
            )
        };
        if answered == 0 {
            return String::new();
        }
        let length = buffer.iter().position(|unit| *unit == 0).unwrap_or(0);
        String::from_utf16_lossy(&buffer[..length])
    }
}

#[cfg(not(windows))]
mod platform {
    use super::Place;

    /// No shell, no platform places: a freedesktop machine's are the desktop's own
    /// decorations (`user-dirs.dirs`), which `dtr-files` already reads, and the root
    /// is `/`.
    pub fn places() -> Vec<Place> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Explorer's three spellings, which is the whole rule: a label wins, a kind
    /// stands in when there is none, and the letter is always bracketed.
    #[test]
    fn a_drive_is_labelled_by_its_volume_and_otherwise_by_its_kind() {
        assert_eq!(drive_label("win", 'C', DriveKind::Fixed), "win (C:)");
        assert_eq!(drive_label("", 'C', DriveKind::Fixed), "Local Disk (C:)");
        assert_eq!(
            drive_label("", 'E', DriveKind::Removable),
            "Removable Disk (E:)"
        );
        assert_eq!(drive_label("", 'D', DriveKind::Optical), "DVD Drive (D:)");
        assert_eq!(
            drive_label("", 'Z', DriveKind::Remote),
            "Network Drive (Z:)"
        );
        assert_eq!(drive_label("", 'R', DriveKind::Ram), "Ram Disk (R:)");
        assert_eq!(drive_label("", 'Q', DriveKind::Unknown), "Disk (Q:)");
    }

    /// A filesystem pads the label, and a padded label in a menu is a row with a gap
    /// in the middle of it.
    #[test]
    fn a_padded_or_spacey_label_is_trimmed() {
        assert_eq!(drive_label("  win  ", 'C', DriveKind::Fixed), "win (C:)");
        assert_eq!(drive_label("   ", 'C', DriveKind::Fixed), "Local Disk (C:)");
    }

    /// The `DRIVE_*` constants, which are the API's own numbers and not ours to
    /// rename. `DRIVE_UNKNOWN` is 0 and `DRIVE_NO_ROOT_DIR` is 1, and neither is a
    /// reason to drop a drive from the list.
    #[test]
    fn the_drive_types_are_the_apis() {
        assert_eq!(DriveKind::of(2), DriveKind::Removable);
        assert_eq!(DriveKind::of(3), DriveKind::Fixed);
        assert_eq!(DriveKind::of(4), DriveKind::Remote);
        assert_eq!(DriveKind::of(5), DriveKind::Optical);
        assert_eq!(DriveKind::of(6), DriveKind::Ram);
        assert_eq!(DriveKind::of(0), DriveKind::Unknown);
        assert_eq!(DriveKind::of(1), DriveKind::Unknown);
    }

    /// On a machine with no shell there are no platform places, and the pane's menu
    /// is the home row and the decorations beside it.
    #[cfg(not(windows))]
    #[test]
    fn a_machine_without_a_shell_has_no_places_of_its_own() {
        assert!(places().is_empty());
    }
}
