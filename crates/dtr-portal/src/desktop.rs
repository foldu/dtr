//! What the manager window's chrome needs from the machine: places, mounts and
//! this session's ssh hosts.
//!
//! `docs/manager.md` §6 draws the line: the window supplies the *data*, the
//! chrome crate draws it. That is why these readers live here and not in
//! `dtr-files` — a crate that paints a window must not also decide what a mount
//! is, and a `SidebarEntry` is a row, not a filesystem.
//!
//! Three sources, each with its own honesty rule:
//!
//! - **Places** are the desktop's, read by `dtr_files::Places` from
//!   `user-dirs.dirs`. Home and the root are added by `Sidebar::places` itself,
//!   because every desktop puts those at the ends.
//! - **Drives** are `/proc/mounts`, filtered to *real* mounts: a pseudo
//!   filesystem is not a place a person can open, and a sidebar full of
//!   `cgroup2` and `nsfs` is worse than no drives section. Linux-only; elsewhere
//!   the section is empty and `docs/manager.md` §6 says so.
//! - **Hosts** are `~/.ssh/config`'s aliases, through `dtr-sshconfig`, so the
//!   same parser the terminal's host picker uses. A host row's home directory is
//!   `None`: `dtr-vfs::Ssh` has no `home()` and inventing `/root` for somebody's
//!   server would be a guess — the row opens at `.`, the one path every ssh
//!   server resolves against that account's home. That gap is in the report.
//!
//! Every reader has a `from text` half so the filter rules are unit tests
//! without a machine, and a `load` half that reads the real file.

use std::path::{Path, PathBuf};

use dtr_files::Places;
use dtr_files::manager::{Drive, Host};

/// Everything the sidebar shows, read once when a manager window opens.
///
/// Not re-read per frame: the places file is a file read, `/proc/mounts` is a
/// read and the ssh config may pull in includes. A mount appearing while the
/// window is open is not noticed until the next window — the same "polled, not
/// watched" bargain the terminal makes for the config file, at a much slower
/// cadence (`docs/compio.md` §5).
#[derive(Clone, Debug, Default)]
pub struct Desktop {
    /// The person's home, for `~` in paths and the places' order.
    pub home: PathBuf,
    /// The directories the desktop decorated.
    pub places: Places,
    /// Real mounts, in `/proc/mounts` order, one row per mount point.
    pub drives: Vec<Drive>,
    /// The ssh aliases this session can open.
    pub hosts: Vec<Host>,
}

impl Desktop {
    /// Read this machine's places, mounts and ssh hosts.
    pub fn load() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        Self {
            places: Places::load(),
            drives: read_mounts(&Path::new("/proc/mounts")),
            hosts: read_ssh_hosts(),
            home,
        }
    }

    /// The same, from given files: what a test uses, and what a machine with no
    /// `/proc` gets (empty drives).
    pub fn from_parts(
        home: impl Into<PathBuf>,
        places: Places,
        mount_text: Option<&str>,
        ssh_config: Option<&str>,
    ) -> Self {
        Self {
            home: home.into(),
            places,
            drives: mount_text.map(parse_mounts).unwrap_or_default(),
            hosts: ssh_config
                .map(|text| parse_ssh_hosts(text, Path::new("/tmp/.ssh")))
                .unwrap_or_default(),
        }
    }
}

/// Read real mounts from `/proc/mounts`; a machine without one has none.
fn read_mounts(path: &Path) -> Vec<Drive> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_mounts(&text),
        Err(err) => {
            tracing::debug!(%err, path = %path.display(), "no mount table; the drives section is empty");
            Vec::new()
        }
    }
}

/// The pseudo filesystems a sidebar must not offer.
///
/// A **deny list** rather than an allow list: an allow list would silently drop
/// the next real filesystem (bcachefs, a fuse mount a person uses for work), and
/// a mount the sidebar forgets is a directory they cannot reach from the window.
/// The cost of the other direction is one row of noise, which is visible.
///
/// `overlay` is here because on NixOS the root is one: the root is already a
/// place (`Sidebar::places` ends with it), so a drive row would be the same
/// directory twice. `squashfs` is the Nix store's read-only images, not a place.
const PSEUDO: &[&str] = &[
    "proc",
    "sysfs",
    "devtmpfs",
    "devpts",
    "tmpfs",
    "cgroup",
    "cgroup2",
    "pstore",
    "securityfs",
    "debugfs",
    "tracefs",
    "configfs",
    "fusectl",
    "mqueue",
    "hugetlbfs",
    "binfmt_misc",
    "autofs",
    "rpc_pipefs",
    "nsfs",
    "bpf",
    "ramfs",
    "rootfs",
    "efivarfs",
    "overlay",
    "squashfs",
    "fuse.gvfsd-fuse",
    "fuse.portal",
];

/// Parse `/proc/mounts` into the sidebar's drives.
///
/// One line is `device mountpoint fstype options dump pass`; the mount point is
/// octal-escaped (`\040` for a space), which is the specification's spelling and
/// not a suggestion. The root is skipped (it is a place), pseudo filesystems are
/// skipped, and a mount point listed twice keeps its first row — bind mounts
/// share a mount point far more often than they add a directory.
pub fn parse_mounts(text: &str) -> Vec<Drive> {
    let mut drives: Vec<Drive> = Vec::new();
    for line in text.lines() {
        let mut fields = line.split_ascii_whitespace();
        let Some(_device) = fields.next() else {
            continue;
        };
        let Some(mount) = fields.next() else {
            continue;
        };
        let Some(fstype) = fields.next() else {
            continue;
        };
        if PSEUDO.contains(&fstype) {
            continue;
        }
        let path = PathBuf::from(unescape_mount(mount));
        if path == Path::new("/") || !path.is_absolute() {
            continue;
        }
        if drives.iter().any(|drive| drive.path == path) {
            continue;
        }
        let label = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| path.display().to_string());
        drives.push(Drive { label, path });
    }
    // A stable, readable order: the mount table's own order is by device, which
    // is not what a person scans. Mount points sort as paths, so `/data` is above
    // `/media/usb` and both are above `/srv`.
    drives.sort_by(|a, b| a.path.cmp(&b.path));
    drives
}

/// `/proc/mounts`' octal escapes: `\040` space, `\011` tab, `\012` newline,
/// `\134` backslash. Any other `\NNN` is left as written — the kernel would not
/// produce one, and inventing a byte is worse than a literal.
fn unescape_mount(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'\\'
            && at + 3 < bytes.len()
            && bytes[at + 1..at + 4]
                .iter()
                .all(|byte| (b'0'..=b'7').contains(byte))
        {
            let value = (bytes[at + 1] - b'0') as u32 * 64
                + (bytes[at + 2] - b'0') as u32 * 8
                + (bytes[at + 3] - b'0') as u32;
            if let Some(byte) = u8::try_from(value).ok() {
                out.push(byte);
                at += 4;
                continue;
            }
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Read `~/.ssh/config`'s aliases; a machine without one has no hosts.
fn read_ssh_hosts() -> Vec<Host> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let path = home.join(".ssh").join("config");
    if !path.is_file() {
        return Vec::new();
    }
    match dtr_sshconfig::parse_file(&path) {
        Ok(config) => hosts_of(&config),
        Err(err) => {
            // A config that does not parse is the person's file to fix, and a
            // window that refused to open over it would be worse than one with
            // no hosts section. The sentence goes to the log, once.
            tracing::warn!(%err, path = %path.display(), "could not read the ssh config; no hosts");
            Vec::new()
        }
    }
}

/// The aliases of a parsed config: `Host` patterns without wildcards, in file
/// order, each once.
///
/// A wildcard block (`Host *`) is a default, not a host a person can open, and
/// `aliases()` already drops the negations; the dedupe is for the config that
/// lists `web` in two blocks, which OpenSSH accepts and a sidebar must not draw
/// twice.
fn hosts_of(config: &dtr_sshconfig::Config) -> Vec<Host> {
    let mut hosts: Vec<Host> = Vec::new();
    for entry in config.hosts() {
        for alias in entry.aliases() {
            if hosts.iter().any(|host| host.alias == alias) {
                continue;
            }
            hosts.push(Host {
                alias: alias.to_owned(),
                // `dtr-vfs::Ssh` has no `home()`; the row opens at `.` — the one
                // path every SFTP server resolves against the far account's home.
                home: None,
            });
        }
    }
    hosts
}

/// Aliases from ssh-config **text**, with `base_dir` for its `Include`s.
///
/// The same parser the terminal's host picker uses, so a `Host web` hidden
/// behind an `Include config.d/*` is found here too.
pub fn parse_ssh_hosts(text: &str, base_dir: &Path) -> Vec<Host> {
    match dtr_sshconfig::parse_str(text, base_dir) {
        Ok(config) => hosts_of(&config),
        Err(err) => {
            tracing::warn!(%err, "could not parse the ssh config text; no hosts");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_mount_becomes_a_row_and_a_pseudo_one_does_not() {
        let mounts = "\
sysfs /sys sysfs rw,nosuid,nodev,noexec,relatime 0 0
proc /proc proc rw,nosuid,nodev,noexec,relatime 0 0
/dev/nvme0n1p2 / btrfs rw,relatime,ssd,space_cache=v2 0 0
/dev/nvme0n1p3 /home ext4 rw,relatime 0 0
/dev/sda1 /media/Backup\\040disk vfat rw,relatime 0 0
tmpfs /run tmpfs rw,nosuid,nodev,relatime 0 0
/dev/sdb1 /data xfs rw,relatime 0 0
";
        let drives = parse_mounts(mounts);
        assert_eq!(
            drives,
            vec![
                Drive {
                    label: "data".to_owned(),
                    path: PathBuf::from("/data"),
                },
                Drive {
                    label: "home".to_owned(),
                    path: PathBuf::from("/home"),
                },
                Drive {
                    label: "Backup disk".to_owned(),
                    path: PathBuf::from("/media/Backup disk"),
                },
            ],
            "root and pseudo filesystems are not rows, and the mount's octal space is a space"
        );
    }

    #[test]
    fn a_mount_point_listed_twice_keeps_its_first_row() {
        let mounts = "\
/dev/sda1 /mnt/thing ext4 rw 0 0
/dev/sdb1 /mnt/thing ext4 rw 0 0
";
        let drives = parse_mounts(mounts);
        assert_eq!(drives.len(), 1);
        assert_eq!(drives[0].path, PathBuf::from("/mnt/thing"));
    }

    #[test]
    fn a_bind_mount_whose_point_is_another_row_is_not_a_second_row() {
        // The kernel writes the same mount point for a bind mount; the sidebar
        // wants one row per directory, not one per mount.
        let mounts = "\
/dev/sda1 /srv ext4 rw 0 0
/dev/sda1 /srv/bind ext4 rw 0 0
/dev/sda1 /srv ext4 rw 0 0
";
        assert_eq!(parse_mounts(mounts).len(), 2);
    }

    #[test]
    fn a_relative_or_empty_mount_point_is_not_a_row() {
        assert!(parse_mounts("device relative ext4 rw 0 0\n").is_empty());
        assert!(parse_mounts("").is_empty());
        assert!(parse_mounts("device /data\n").is_empty());
    }

    #[test]
    fn octal_escapes_are_the_kernels_and_anything_else_is_literal() {
        assert_eq!(unescape_mount(r"/a\040b"), "/a b");
        assert_eq!(unescape_mount(r"/a\011b"), "/a\tb");
        assert_eq!(unescape_mount(r"/a\134b"), r"/a\b");
        // Not an escape the kernel writes: left as written.
        assert_eq!(unescape_mount(r"/a\999b"), r"/a\999b");
        assert_eq!(unescape_mount(r"trailing\"), r"trailing\");
    }

    #[test]
    fn ssh_aliases_are_real_hosts_in_file_order_once_each() {
        let text = "\
Host web
    HostName web.example.com
Host db web
    User alice
Host *
    ServerAliveInterval 20
Host !secret staging?
    User nobody
";
        let hosts = parse_ssh_hosts(text, Path::new("/tmp/.ssh"));
        let aliases: Vec<&str> = hosts.iter().map(|host| host.alias.as_str()).collect();
        // `!secret` is a negation and `staging?` a wildcard: neither names a
        // host a person can open, and `Host *` is a default, not a row.
        assert_eq!(aliases, ["web", "db"]);
        assert!(
            hosts.iter().all(|host| host.home.is_none()),
            "the vfs has no home(); the row opens at `.` instead of guessing"
        );
    }

    #[test]
    fn a_wildcard_or_negated_pattern_is_not_a_host() {
        let text = "Host *.internal\n    User root\nHost dev-?\n    User root\n";
        assert!(parse_ssh_hosts(text, Path::new("/tmp/.ssh")).is_empty());
    }

    #[test]
    fn a_config_that_does_not_parse_yields_no_hosts_instead_of_failing_the_window() {
        // An unterminated quote is a parse error (`dtr-sshconfig`'s own rules);
        // the window opens with an empty section and the log says why.
        let text = "Host web\n    ProxyCommand \"broken\n";
        assert!(parse_ssh_hosts(text, Path::new("/tmp/.ssh")).is_empty());
    }

    #[test]
    fn the_desktop_reads_a_machine_with_no_proc_and_no_ssh_config_as_empty() {
        let desktop =
            Desktop::from_parts(PathBuf::from("/home/ada"), Places::default(), None, None);
        assert_eq!(desktop.home, PathBuf::from("/home/ada"));
        assert!(desktop.drives.is_empty());
        assert!(desktop.hosts.is_empty());
    }
}
