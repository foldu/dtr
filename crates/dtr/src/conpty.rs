//! The Windows exe carries its own ConPTY.
//!
//! A shell pane on Windows is ConPTY, and inbox ConPTY is a terminal emulator in
//! the middle of it: it renders the child's output into a console buffer and
//! re-emits VT to dtr, so the unknown APC sequences the kitty graphics protocol
//! rides in are consumed before the VT core can see them. Microsoft ships a newer
//! ConPTY as an ordinary MIT-licensed package (Windows Terminal's), and
//! `portable-pty` already prefers a `conpty.dll` it finds on the DLL search path
//! — so the two files ride inside the one-file exe and are unpacked into a
//! private directory at startup, without touching the pty layer.
//!
//! `PSEUDOCONSOLE_PASSTHROUGH_MODE` (0x8) was measured first and changes nothing
//! on inbox conhost; the A/B and the bundled host's measurements are in
//! `docs/windows.md` §12, and `assets/conpty/README.md` says where the binaries
//! come from and how to update them.
//!
//! A failure here is not fatal: a read-only profile, a locked file or an
//! antivirus quarantine leaves shell panes on the inbox ConPTY and costs images,
//! not the terminal.

#[cfg(any(windows, test))]
use std::path::Path;

/// The bundled package's version. It names the directory the files are unpacked
/// into, so an upgrade cannot reuse a ConPTY it did not carry.
pub const VERSION: &str = "1.25.261109001";

/// `conpty.dll` and the `OpenConsole.exe` it starts from its own directory, from
/// `Microsoft.Windows.Console.ConPTY`.
#[cfg(windows)]
const FILES: [(&str, &[u8]); 2] = [
    ("conpty.dll", include_bytes!("../assets/conpty/conpty.dll")),
    (
        "OpenConsole.exe",
        include_bytes!("../assets/conpty/OpenConsole.exe"),
    ),
];

/// Unpack the bundled ConPTY and put it on the DLL search path. Call once,
/// before the first terminal exists: `portable-pty` loads its ConPTY lazily, on
/// the first pane, and a pane may open before the window has finished its first
/// frame.
#[cfg(windows)]
pub fn install() {
    if let Err(err) = install_bundled() {
        tracing::warn!(%err, "could not unpack the bundled ConPTY; shell panes use the inbox one");
    }
}

/// Nothing to install off Windows: the pty is the kernel's.
#[cfg(not(windows))]
pub fn install() {}

#[cfg(windows)]
fn install_bundled() -> std::io::Result<()> {
    let base = dirs::data_local_dir().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "%LOCALAPPDATA% is not set")
    })?;
    let dir = base.join("dtr").join("conpty").join(VERSION);
    unpack(&dir, &FILES)?;
    set_dll_directory(&dir)?;
    tracing::info!(version = VERSION, path = %dir.display(), "bundled ConPTY");
    Ok(())
}

/// Write each `(name, bytes)` into `dir` unless a file of that name and length
/// is already there.
///
/// Staging and rename rather than `fs::write` over the destination: another dtr
/// starting at the same time must never load a half-written DLL, and an
/// interrupted run must leave the old file or nothing, never a truncated one.
#[cfg(any(windows, test))]
fn unpack(dir: &Path, files: &[(&str, &[u8])]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for (name, bytes) in files {
        let path = dir.join(name);
        let present = std::fs::metadata(&path)
            .map(|meta| meta.len() == bytes.len() as u64)
            .unwrap_or(false);
        if present {
            continue;
        }
        let staging = dir.join(format!("{name}.new"));
        std::fs::write(&staging, bytes)?;
        std::fs::rename(&staging, &path)?;
    }
    Ok(())
}

/// `SetDllDirectoryW` adds a directory to the plain `LoadLibrary` search order,
/// which is what `portable-pty`'s `libloading` uses for the bare name
/// `conpty.dll` (`LoadLibraryExW(filename, 0, 0)`). `AddDllDirectory` would not
/// do: it is consulted only for `LOAD_LIBRARY_SEARCH_USER_DIRS` loads.
#[cfg(windows)]
fn set_dll_directory(dir: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::LibraryLoader::SetDllDirectoryW;

    let wide: Vec<u16> = dir
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `wide` is NUL-terminated and outlives the call, and the API copies
    // the string it is handed.
    if unsafe { SetDllDirectoryW(wide.as_ptr()) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{VERSION, unpack};
    use std::path::Path;

    const FAKE: [(&str, &[u8]); 2] = [
        ("conpty.dll", b"MZ a fake conpty"),
        ("OpenConsole.exe", b"MZ a fake host"),
    ];

    #[test]
    fn unpack_writes_both_files_and_leaves_a_complete_copy_alone() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("dtr").join("conpty").join(VERSION);
        unpack(&dir, &FAKE).unwrap();
        assert_eq!(std::fs::read(dir.join("conpty.dll")).unwrap(), FAKE[0].1);
        assert_eq!(
            std::fs::read(dir.join("OpenConsole.exe")).unwrap(),
            FAKE[1].1
        );
        assert!(!dir.join("conpty.dll.new").exists());

        let before = std::fs::metadata(dir.join("conpty.dll"))
            .unwrap()
            .modified()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        unpack(&dir, &FAKE).unwrap();
        let after = std::fs::metadata(dir.join("conpty.dll"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(before, after, "an unchanged file was rewritten");
    }

    #[test]
    fn unpack_replaces_a_file_of_the_wrong_length() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("conpty");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("conpty.dll"), b"short").unwrap();
        unpack(&dir, &FAKE).unwrap();
        assert_eq!(std::fs::read(dir.join("conpty.dll")).unwrap(), FAKE[0].1);
    }

    /// The assets are committed binaries, so the test that they are PE images
    /// rather than an LFS pointer or a truncated download runs off Windows,
    /// where the suite actually runs.
    #[test]
    fn the_committed_conpty_binaries_are_pe_images() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/conpty");
        for name in ["conpty.dll", "OpenConsole.exe"] {
            let bytes = std::fs::read(root.join(name)).unwrap();
            assert!(bytes.len() > 50_000, "{name} is only {} bytes", bytes.len());
            assert_eq!(&bytes[..2], b"MZ", "{name} has no DOS header");
            let header: [u8; 4] = bytes[0x3c..0x40].try_into().unwrap();
            let pe = u32::from_le_bytes(header) as usize;
            assert!(pe + 4 <= bytes.len(), "{name}'s e_lfanew is out of range");
            assert_eq!(&bytes[pe..pe + 4], b"PE\0\0", "{name} has no PE header");
        }
    }

    /// `UPSTREAM` is what `just conpty` writes and the only record of where
    /// the two binaries came from: its version is the one the unpack directory
    /// is named after, and its hashes have to still describe the committed
    /// bytes — a vendored binary swapped without the recipe is exactly the
    /// failure this is for.
    #[test]
    fn the_asset_upstream_describes_the_committed_binaries() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/conpty");
        let upstream = std::fs::read_to_string(root.join("UPSTREAM")).unwrap();
        let field = |key: &str| -> String {
            upstream
                .lines()
                .find_map(|line| line.strip_prefix(&format!("{key}=")))
                .unwrap_or_else(|| panic!("UPSTREAM has no {key}="))
                .to_owned()
        };
        assert_eq!(field("version"), VERSION);
        for (key, file) in [
            ("conpty-dll-sha256", "conpty.dll"),
            ("openconsole-sha256", "OpenConsole.exe"),
        ] {
            let recorded = field(key);
            assert!(
                recorded.len() == 64 && recorded.chars().all(|c| c.is_ascii_hexdigit()),
                "UPSTREAM's {key} is not a sha256: {recorded}"
            );
            // A hash is only provenance if it still describes the bytes. The nix
            // sandbox and CI have coreutils; a machine without sha256sum skips
            // the comparison rather than the whole suite.
            let output = match std::process::Command::new("sha256sum")
                .arg(root.join(file))
                .output()
            {
                Ok(output) if output.status.success() => output,
                _ => continue,
            };
            let actual = String::from_utf8_lossy(&output.stdout)
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_owned();
            assert_eq!(actual, recorded, "UPSTREAM's {key} does not match {file}");
        }
    }
}
