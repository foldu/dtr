//! The freedesktop.org Trash specification, for [`Local`](crate::Local)'s
//! `Removal::Trash`.
//!
//! The spec (v1.0, 2014-01-02) defines a *storage* format two desktops share, and
//! this module implements it so a file dtr deletes can be restored by Nautilus or
//! `trash-cli`, and a file those deleted shows up in the trash. `docs/trash.md`
//! is the account: the decisions, the mount/fallback rules, and the failure
//! policy.
//!
//! Three rules from the specification shape the code:
//!
//! - **A trash directory is chosen per file, by device.** The home trash
//!   (`$XDG_DATA_HOME/Trash`) is for files on `$XDG_DATA_HOME`'s filesystem;
//!   anything else uses the top-directory trash of its own mount (§5).
//! - **An entry is a pair.** `$trash/files/<name>` holds the moved file and
//!   `$trash/info/<name>.trashinfo` holds where it came from and when (§6).
//!   The info file is created **first**, atomically, with `O_EXCL` — that is what
//!   makes a collision pick a new name instead of overwriting a previous delete.
//! - **The name is never reused.** `files/<name>` must be unique, and the info
//!   file must have exactly that name plus `.trashinfo`.

use std::ffi::{OsStr, OsString};
use std::io::Write as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};

/// Move `path` into the trash the specification picks for it.
///
/// `root` pins the trash directory — the test/sandbox seam, and a caller that
/// wants a private trash — and skips the per-mount choice. `None` means the
/// home trash when `path` is on `$XDG_DATA_HOME`'s filesystem, otherwise the
/// top-directory trash of `path`'s mount.
pub fn trash(path: &Path, root: Option<&Path>) -> Result<()> {
    // §6's `Path=` is an absolute location or one relative to the trash's base;
    // a relative *input* would be resolved against the process's working
    // directory and stored as a lie. The UI always has the absolute path.
    if !path.is_absolute() {
        anyhow::bail!("cannot trash {}: the path is not absolute", path.display());
    }
    let meta =
        std::fs::symlink_metadata(path).with_context(|| format!("trash {}", path.display()))?;
    let store = match root {
        Some(root) => Store::prepare(root.to_path_buf(), None)?,
        None => choose(path, meta.dev())?,
    };
    store.put(path)
}

/// One trash directory, ready to take an entry.
struct Store {
    /// `$trash` itself; `files/` and `info/` live under it.
    root: PathBuf,
    /// The directory a `Path=` value is written relative to, for a
    /// top-directory trash. `None` means an absolute path — the rule for the
    /// home trash (§6: relative paths are for top-directory trashes).
    base: Option<PathBuf>,
}

impl Store {
    /// Create `files/` and `info/` if needed, as the spec says to do without
    /// warnings or delays.
    fn prepare(root: PathBuf, base: Option<PathBuf>) -> Result<Self> {
        std::fs::create_dir_all(root.join("files"))
            .with_context(|| format!("create {}", root.join("files").display()))?;
        std::fs::create_dir_all(root.join("info"))
            .with_context(|| format!("create {}", root.join("info").display()))?;
        Ok(Self { root, base })
    }

    /// The `Path=` value: the original location, percent-encoded as §6 spells it.
    fn original(&self, path: &Path) -> String {
        match &self.base {
            Some(base) => match path.strip_prefix(base) {
                Ok(relative) => percent_encode(relative),
                // Every path on the mount is under its top directory, so this is
                // a bind-mount oddity. An absolute value is more useful to a
                // person restoring the file than a refusal; the spec prefers
                // absolute values only in the home trash, so this is the one
                // recorded deviation.
                Err(_) => percent_encode(path),
            },
            None => percent_encode(path),
        }
    }

    /// Move one path in, creating its info file first (atomically) and never
    /// overwriting an existing entry.
    fn put(&self, path: &Path) -> Result<()> {
        let file_name = path.file_name().with_context(|| {
            format!(
                "cannot trash {}: it has no file name of its own",
                path.display()
            )
        })?;
        let mut attempt: u32 = 0;
        loop {
            let name = candidate_name(file_name, attempt);
            let stored = self.root.join("files").join(&name);
            let info = self.root.join("info").join(info_name(&name));

            // An info file with no stored file (an interrupted delete) and a
            // stored file with no info file (a broken one) both take the name.
            // Only a free pair is ours.
            if std::fs::symlink_metadata(&stored).is_ok() {
                attempt += 1;
                continue;
            }
            match open_new(&info) {
                Ok(mut file) => {
                    let body = format!(
                        "[Trash Info]\nPath={}\nDeletionDate={}\n",
                        self.original(path),
                        deletion_date()
                    );
                    if let Err(err) = file.write_all(body.as_bytes()) {
                        drop(file);
                        let _ = std::fs::remove_file(&info);
                        return Err(err).with_context(|| format!("write {}", info.display()));
                    }
                    drop(file);
                    // The move is last, and a failure takes the info file with
                    // it: an info entry pointing at nothing would show as an
                    // "emergency case" in a desktop's trash view.
                    return match std::fs::rename(path, &stored) {
                        Ok(()) => Ok(()),
                        Err(err) => {
                            let _ = std::fs::remove_file(&info);
                            Err(err)
                                .with_context(|| format!("move {} into the trash", path.display()))
                        }
                    };
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    attempt += 1;
                    if attempt > 10_000 {
                        anyhow::bail!("could not find a free trash name for {}", path.display());
                    }
                    continue;
                }
                Err(err) => return Err(err).with_context(|| format!("create {}", info.display())),
            }
        }
    }
}

/// Pick the trash directory for a path on `device`.
fn choose(path: &Path, device: u64) -> Result<Store> {
    let home = data_home()?;
    // The home trash is for the filesystem `$XDG_DATA_HOME` is on (the spec's
    // own footnote to "same partition/device"); the base may not exist yet, so
    // the device is the nearest existing ancestor's.
    if device_of(&home)? == device {
        return Store::prepare(home.join("Trash"), None);
    }

    let topdir = topdir_of(path, device);
    // (1) An administrator's `$topdir/.Trash`, if it passes the spec's checks:
    // a real directory (not a symlink) with the sticky bit set. Then the user's
    // own `$uid` subdirectory is the trash, created if missing.
    if let Some(admin) = admin_trash(&topdir) {
        let user = admin.join(uid().to_string());
        if create_user_trash(&user).is_ok() {
            return Store::prepare(user, Some(topdir));
        }
        // "If the system refuses to create an $uid directory in it, fall back
        // to method (2)" — immediately, without a warning.
    }
    // (2) `$topdir/.Trash-$uid`, which we create.
    let user = topdir.join(format!(".Trash-{}", uid()));
    let mounted_at = topdir.clone();
    Store::prepare(user, Some(topdir)).with_context(|| {
        format!(
            "no usable trash on the filesystem mounted at {}",
            mounted_at.display()
        )
    })
}

/// The mount point `path` lives on: the highest ancestor with the same device.
fn topdir_of(path: &Path, device: u64) -> PathBuf {
    let mut current = path.to_path_buf();
    while let Some(parent) = current.parent() {
        match std::fs::metadata(parent) {
            Ok(meta) if meta.dev() == device => current = parent.to_path_buf(),
            _ => break,
        }
    }
    current
}

/// `$topdir/.Trash`, if it passes the spec's checks.
fn admin_trash(topdir: &Path) -> Option<PathBuf> {
    let candidate = topdir.join(".Trash");
    let meta = std::fs::symlink_metadata(&candidate).ok()?;
    // A symlink could point anywhere; the spec says do not use it for trashing
    // even if a `$uid` directory already sits inside.
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return None;
    }
    // The sticky bit: without it one user could delete or replace another's
    // files in a shared trash.
    if meta.permissions().mode() & 0o1000 == 0 {
        return None;
    }
    Some(candidate)
}

/// Create `$topdir/.Trash/$uid` if needed; "already there" is success.
fn create_user_trash(user: &Path) -> Result<()> {
    match std::fs::create_dir(user) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists && user.is_dir() => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            // The name is taken by something that is not a directory; the caller
            // falls back to method (2), which picks a different path.
            Err(err).with_context(|| format!("{} is not a directory", user.display()))
        }
        Err(err) => Err(err).with_context(|| format!("create {}", user.display())),
    }
}

/// `$XDG_DATA_HOME`, or `~/.local/share` when it is unset or relative.
///
/// The base-directory specification says a relative `$XDG_DATA_HOME` is invalid
/// and must be ignored, so it falls back rather than resolving against the
/// process's working directory.
fn data_home() -> Result<PathBuf> {
    if let Some(value) = std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            return Ok(path);
        }
    }
    Ok(dirs::home_dir()
        .context("neither $XDG_DATA_HOME nor a home directory is set, so there is no trash")?
        .join(".local")
        .join("share"))
}

/// The device of `path`, or of its nearest existing ancestor when it is not
/// there yet (the home trash may never have been created).
fn device_of(path: &Path) -> Result<u64> {
    let mut current = path;
    loop {
        match std::fs::metadata(current) {
            Ok(meta) => return Ok(meta.dev()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                current = current.parent().with_context(|| {
                    format!(
                        "{} does not exist and has no ancestor that does",
                        path.display()
                    )
                })?;
            }
            Err(err) => {
                return Err(err).with_context(|| format!("stat {}", current.display()));
            }
        }
    }
}

/// The numeric user id the spec's `$topdir/.Trash/$uid` and `.Trash-$uid` use.
fn uid() -> u32 {
    // `getuid` is the real uid, which is the user the spec means; effective and
    // real differ only under setuid, and dtr never runs setuid.
    unsafe { libc::getuid() }
}

/// `DeletionDate`: local time, `YYYY-MM-DDThh:mm:ss` (§6, RFC 3339's shape).
///
/// Local rather than UTC because the spec says so, and because a trash view
/// shows the time to the person who deleted it. `libc` is the only way to ask
/// the C library for the offset without a whole date crate.
fn deletion_date() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as libc::time_t)
        .unwrap_or(0);
    let mut broken_down: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `broken_down` is a valid `tm`, and `localtime_r` only writes it.
    let filled = unsafe { libc::localtime_r(&seconds, &mut broken_down) };
    if filled.is_null() {
        // The C library's local-time database is broken; a fixed, valid date is
        // better than a garbage one, and the file is still readable.
        return "1970-01-01T00:00:00".to_owned();
    }
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        broken_down.tm_year + 1900,
        broken_down.tm_mon + 1,
        broken_down.tm_mday,
        broken_down.tm_hour,
        broken_down.tm_min,
        broken_down.tm_sec
    )
}

/// The `Path=` encoding: an URL-style escape of the path's raw bytes.
///
/// The spec asks for RFC 2396 escaping "as in URLs"; only alphanumerics and the
/// URL path characters are left alone, and every other byte — spaces, `%`,
/// newlines, and the bytes of a non-UTF-8 name — becomes `%XX` with uppercase
/// hex. `/` stays literal, because a path is what the value is.
fn percent_encode(path: &Path) -> String {
    let mut out = String::with_capacity(path.as_os_str().len() + 8);
    for &byte in path.as_os_str().as_bytes() {
        let bare = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/');
        if bare {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

/// The unique name the spec expects when `name` is already taken: `name.1`,
/// `name.2`, … The suffix goes on the whole name, not before the extension,
/// because a reader takes the name from the info file, not from the extension.
fn candidate_name(name: &OsStr, attempt: u32) -> OsString {
    let mut candidate = name.to_os_string();
    if attempt > 0 {
        candidate.push(format!(".{attempt}"));
    }
    candidate
}

fn info_name(name: &OsStr) -> OsString {
    let mut info = name.to_os_string();
    info.push(".trashinfo");
    info
}

/// `O_CREAT | O_EXCL`: the atomic gate that makes two deletes of one name two
/// entries rather than one overwritten entry.
fn open_new(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The encoding rule is the spec's, not a URL encoder's: `/` stays, a space
    /// and `%` do not, and a non-ASCII byte is escaped byte by byte.
    #[test]
    fn a_path_is_percent_encoded_for_the_info_file() {
        assert_eq!(
            percent_encode(Path::new("/home/lab/notes.txt")),
            "/home/lab/notes.txt"
        );
        assert_eq!(
            percent_encode(Path::new("/home/lab/a file 100%.txt")),
            "/home/lab/a%20file%20100%25.txt"
        );
        assert_eq!(
            percent_encode(Path::new("/home/lab/\u{e9}t\u{e9}.txt")),
            "/home/lab/%C3%A9t%C3%A9.txt"
        );
        assert_eq!(percent_encode(Path::new("/x\n/y")), "/x%0A/y");
    }

    #[test]
    fn a_collision_gets_the_next_suffix_on_the_whole_name() {
        assert_eq!(candidate_name(OsStr::new("notes.txt"), 0), "notes.txt");
        assert_eq!(candidate_name(OsStr::new("notes.txt"), 1), "notes.txt.1");
        assert_eq!(candidate_name(OsStr::new("notes.txt"), 12), "notes.txt.12");
        assert_eq!(
            info_name(OsStr::new("notes.txt.1")),
            "notes.txt.1.trashinfo"
        );
    }

    /// A date in the spec's exact shape: 19 characters, digits where RFC 3339
    /// puts them, a `T` between the date and the time.
    #[test]
    fn the_deletion_date_has_the_spec_shape() {
        let date = deletion_date();
        assert_eq!(date.len(), 19, "{date}");
        let expected_separators = [(4, '-'), (7, '-'), (10, 'T'), (13, ':'), (16, ':')];
        for (index, separator) in expected_separators {
            assert_eq!(
                date.as_bytes()[index] as char,
                separator,
                "{date} is not YYYY-MM-DDThh:mm:ss"
            );
        }
        for index in [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18] {
            assert!(
                date.as_bytes()[index].is_ascii_digit(),
                "{date} is not YYYY-MM-DDThh:mm:ss"
            );
        }
    }
}
