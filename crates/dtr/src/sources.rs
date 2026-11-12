//! Where the palette's candidates come from: this machine's serial ports, the
//! user's ssh hosts, and the shells it has installed.
//!
//! Everything here is best-effort. A missing `~/.ssh/config` or a serial
//! subsystem that refuses to enumerate must not stop a terminal from starting;
//! the palette simply offers fewer things.
//!
//! The shells are *discovered*, not listed: there is no portable registry of
//! them, so each platform is asked the way it keeps the answer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use dtr_complete::{Engine, PortInfo};

/// Rebuild the engine from the current state of the machine.
pub fn engine() -> Engine {
    let mut engine = Engine::new();
    engine.set_ports(serial_ports());
    engine.set_hosts(ssh_hosts(&ssh_config_path()));
    engine.set_shells(shell_programs());
    engine.set_themes(crate::theme::names());
    engine
}

/// Serial ports with whatever detail the OS gives us.
///
/// USB VID/PID is the interesting part: it is what tells two identical
/// adapters apart, and what reconnect-by-device will eventually match on.
pub fn serial_ports() -> Vec<PortInfo> {
    let ports = match serialport::available_ports() {
        Ok(ports) => ports,
        Err(err) => {
            tracing::debug!(%err, "serial enumeration failed");
            return Vec::new();
        }
    };

    ports
        .into_iter()
        // The 8250 driver creates four COM ports whether or not the board has a
        // serial header, and libudev lists all of them; the ones with nothing
        // behind them are four rows of silence in the new-tab menu.
        .filter(|port| !is_a_phantom_com_port(&port.port_name))
        .map(|port| {
            let detail = match &port.port_type {
                serialport::SerialPortType::UsbPort(usb) => {
                    let mut parts = Vec::new();
                    if let Some(product) = &usb.product {
                        parts.push(product.clone());
                    }
                    if let Some(manufacturer) = &usb.manufacturer {
                        parts.push(manufacturer.clone());
                    }
                    parts.push(format!("{:04x}:{:04x}", usb.vid, usb.pid));
                    if let Some(serial) = &usb.serial_number {
                        parts.push(serial.clone());
                    }
                    parts.join(" · ")
                }
                serialport::SerialPortType::BluetoothPort => "bluetooth".to_owned(),
                serialport::SerialPortType::PciPort => "pci".to_owned(),
                serialport::SerialPortType::Unknown => String::new(),
            };
            PortInfo {
                path: port.port_name,
                detail,
            }
        })
        .collect()
}

/// Whether a legacy COM port is a slot with no UART in it.
///
/// `/dev/ttyS0` is COM1's I/O port. The kernel creates four of them on a PC
/// whether or not the board has a serial header, and `/sys/class/tty/ttySN/type`
/// reads `0` when the driver found nothing there — `PORT_UNKNOWN`, which is what
/// `setserial` prints as `UART: unknown`. Opening one fails, or waits forever
/// for bytes nothing can send.
///
/// Only that `type` file is consulted, and only for `ttyS*`: a USB or ACM adapter
/// has no such file, and neither has a host that is not Linux, so a port whose
/// file is missing stays in the list. The filter may only ever remove something
/// the kernel itself has said is empty.
fn is_a_phantom_com_port(path: &str) -> bool {
    let Some(name) = com_port_name(path) else {
        return false;
    };
    match std::fs::read_to_string(format!("/sys/class/tty/{name}/type")) {
        Ok(found) => uart_absent(&found),
        Err(_) => false,
    }
}

/// The sysfs name of a legacy COM port, if this path is one.
fn com_port_name(path: &str) -> Option<&str> {
    let name = path.strip_prefix("/dev/")?;
    name.starts_with("ttyS").then_some(name)
}

/// `0` is `PORT_UNKNOWN`: the driver found no UART, so the device is a placeholder.
///
/// An empty or unreadable value is not a `0`, and keeps the port: a file that does
/// not say "nothing" is not evidence of nothing.
fn uart_absent(type_file: &str) -> bool {
    type_file.trim() == "0"
}

fn ssh_config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".ssh")
        .join("config")
}
/// One thing the new-tab menu can open.
///
/// Typed, rather than a `(String, String)` pair whose kind is guessed from the
/// shape of the first field. The guess was "starts with `/` means serial", and
/// although that is true of every device path it is also true of an ssh alias
/// someone spelled `prod/db` — while the app opened *every* entry as `ssh`, so
/// picking a port ran `ssh /dev/ttyUSB0`. The kind is what the menu groups by,
/// so it belongs in the type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Destination {
    /// A local shell, discovered on this machine.
    Shell { name: String, program: String },
    /// A serial device path, which is what a port is opened with.
    Serial { path: String, detail: String },
    /// An alias in `~/.ssh/config`.
    Ssh { alias: String, detail: String },
}

impl Destination {
    /// What the menu says. The kind is in the text as well as in the grouping:
    /// "shell bash", "serial /dev/ttyUSB0" and "ssh web" are different verbs,
    /// and a row that only said `web` would not tell you which one it was going
    /// to do.
    ///
    /// A shell is its name and nothing else. The program is not printed beside
    /// it for two reasons: one row per name means it could not tell two rows
    /// apart, and a store path is fifty characters of menu for a word that
    /// already says which shell it is. `dtr -v` logs `shell started program=…`
    /// when a tab opens, which is where "which bash was that" belongs.
    pub fn label(&self) -> String {
        match self {
            Self::Shell { name, .. } => format!("shell {name}"),
            Self::Serial { path, detail } if detail.is_empty() => format!("serial {path}"),
            Self::Serial { path, detail } => format!("serial {path}  {detail}"),
            Self::Ssh { alias, detail } if detail.is_empty() => format!("ssh {alias}"),
            Self::Ssh { alias, detail } => format!("ssh {alias}  {detail}"),
        }
    }

    pub fn is_shell(&self) -> bool {
        matches!(self, Self::Shell { .. })
    }

    pub fn is_serial(&self) -> bool {
        matches!(self, Self::Serial { .. })
    }
}

/// The shells the new-tab menu offers.
pub fn shell_destinations() -> Vec<Destination> {
    local_shells()
        .into_iter()
        .map(|shell| Destination::Shell {
            name: shell.name,
            program: shell.program,
        })
        .collect()
}

/// The ssh hosts the new-tab menu offers: `~/.ssh/config`'s aliases, with the
/// detail line each one shows, in the order the menu should show them.
///
/// The file decides *which* hosts; the remembered uses only decide in what order.
/// A host that was deleted from the config is not a row, however many times it
/// was opened, because the store is a lookup for the aliases the file names and
/// never a list of its own.
pub fn ssh_destinations(uses: &HashMap<String, dtr_store::SshUse>) -> Vec<Destination> {
    by_use(ssh_hosts(&ssh_config_path()), uses)
        .into_iter()
        .map(|(alias, detail)| Destination::Ssh { alias, detail })
        .collect()
}

/// Hosts by how much they are used: most opened first, the most recent breaking
/// a tie, and the config file's own order breaking the rest.
///
/// The sort is stable, so a machine that has never opened a host from the menu
/// looks exactly as it did before there was a counter: the order somebody wrote
/// `ssh_config` in is one they chose, and it is only recorded use that outranks
/// it.
///
/// It sorts and never adds: a count is only looked up for a host the caller
/// already has, so a host the config no longer names cannot return as a row.
fn by_use(
    mut hosts: Vec<(String, String)>,
    uses: &HashMap<String, dtr_store::SshUse>,
) -> Vec<(String, String)> {
    hosts.sort_by(|a, b| {
        let of = |alias: &str| {
            let used = uses.get(alias).copied().unwrap_or_default();
            (used.uses, used.last)
        };
        of(&b.0).cmp(&of(&a.0))
    });
    hosts
}

/// Serial ports the new-tab menu offers.
///
/// Polled rather than watched: `available_ports` is cross-platform, while udev
/// is Linux-only and would give Windows a second, different behaviour.
pub fn serial_destinations() -> Vec<Destination> {
    serial_ports()
        .into_iter()
        .map(|port| Destination::Serial {
            path: port.path,
            detail: port.detail,
        })
        .collect()
}

/// Host aliases from `~/.ssh/config`, with the detail line the picker shows.
///
/// Aliases only: a wildcard pattern is not something you can connect to, and
/// `HostEntry::aliases()` already filters those out.
pub fn ssh_hosts(config: &Path) -> Vec<(String, String)> {
    if !config.exists() {
        return Vec::new();
    }
    let parsed = match dtr_sshconfig::parse_file(config) {
        Ok(parsed) => parsed,
        Err(err) => {
            tracing::warn!(path = %config.display(), %err, "could not read the ssh config");
            return Vec::new();
        }
    };

    let mut hosts = Vec::new();
    for entry in parsed.hosts() {
        for alias in entry.aliases() {
            let resolved = parsed.resolve(alias);
            let mut detail = String::new();
            if let Some(user) = &resolved.user {
                detail.push_str(user);
                detail.push('@');
            }
            let hostname = resolved.effective_hostname();
            if hostname != alias {
                detail.push_str(hostname);
            }
            if resolved.effective_port() != 22 {
                if !detail.is_empty() {
                    detail.push(':');
                }
                detail.push_str(&resolved.effective_port().to_string());
            }
            hosts.push((alias.to_owned(), detail));
        }
    }
    hosts
}

/// One program this machine offers as a local shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalShell {
    /// What is executed: an absolute path, wherever one was found.
    pub program: String,
    /// What the menu calls it: the executable's name, without the `.exe` a
    /// Windows build carries.
    pub name: String,
}

/// Every shell this machine has, the one a bare `+` would start first.
///
/// There is no portable registry of shells, so each platform answers the way it
/// keeps the answer. Unix reads `/etc/shells`, the file whose whole purpose is
/// to say which programs may be a login shell. Windows has no such file: its
/// shells are programs like any other, so the places they are installed and
/// `PATH` are searched. Both fall back to where a shell is usually installed and
/// to the names a shell usually has, because a container or a fresh install has
/// no list at all and a terminal still has to offer something to type in.
///
/// Anything named here is checked before it is offered, and the list has one row
/// per name: `/etc/shells` on a NixOS machine names the profile symlink and the
/// store path behind it, and `nix develop` puts a second `bash` on `PATH`, but a
/// menu of shells offers a name once.
pub fn local_shells() -> Vec<LocalShell> {
    let login = login_shell();
    let mut candidates: Vec<String> = Vec::new();
    if let Some(login) = &login {
        candidates.push(login.clone());
    }
    // A machine with no `/etc/shells` is a container or a chroot, not an error:
    // the fallbacks below still find a shell. A line that does not name a real
    // file is a typo in the system config (this machine's says `baash`), and
    // `unique` is where it goes.
    #[cfg(not(windows))]
    candidates.extend(shells_in(
        &std::fs::read_to_string("/etc/shells").unwrap_or_default(),
    ));
    candidates.extend(installed_shells());
    candidates.extend(on_path(SHELL_NAMES));
    ordered(unique(candidates), login.as_deref())
}

/// The programs the palette's `exec` completion offers: the same shells, because
/// a completion that disagrees with the menu offers a shell the terminal cannot
/// find.
pub fn shell_programs() -> Vec<String> {
    local_shells()
        .into_iter()
        .map(|shell| shell.program)
        .collect()
}

/// The shell a plain new tab starts: the first candidate the pane's own spawner
/// would try that is really on the machine.
///
/// Taken from [`crate::pty`] rather than written out again, so the menu's first
/// row cannot promise a different shell from the one `$SHELL` and the fallbacks
/// give a tab that was opened without the menu.
fn login_shell() -> Option<String> {
    crate::pty::candidates()
        .into_iter()
        .filter(|program| !program.is_empty())
        .find_map(|program| resolved(&program))
        .map(|path| path.display().to_string())
}

/// The programs in an `/etc/shells`: one per line, `#` comments and blank lines
/// ignored, and anything that is not an absolute path dropped — the file is a
/// list of programs, so a relative one is a typo rather than a shell.
fn shells_in(contents: &str) -> Vec<String> {
    contents
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with('/'))
        .map(str::to_owned)
        .collect()
}

/// Where a shell is installed when the machine has no list of them.
#[cfg(not(windows))]
fn installed_shells() -> Vec<String> {
    [
        "/bin/sh",
        "/bin/bash",
        "/bin/dash",
        "/bin/ksh",
        "/bin/zsh",
        "/usr/bin/bash",
        "/usr/bin/zsh",
        "/usr/bin/fish",
        "/usr/bin/nu",
        "/usr/local/bin/bash",
        "/usr/local/bin/zsh",
        "/usr/local/bin/fish",
        "/opt/homebrew/bin/bash",
        "/opt/homebrew/bin/fish",
    ]
    .map(str::to_owned)
    .to_vec()
}

/// The same, on the platform whose shells are all installed packages.
///
/// Windows has no `/etc/shells` and no single directory for shells: `cmd` is
/// part of the system, Windows PowerShell is a component beside it,
/// PowerShell 7 is a package of its own, and a POSIX shell arrives with Git for
/// Windows, MSYS2 or Cygwin. These are the places each of those installs into —
/// `%COMSPEC%` first because Windows itself names its command interpreter there.
#[cfg(windows)]
fn installed_shells() -> Vec<String> {
    let mut found: Vec<std::path::PathBuf> = Vec::new();
    if let Some(comspec) = std::env::var_os("COMSPEC") {
        found.push(comspec.into());
    }
    if let Some(root) = std::env::var_os("SystemRoot") {
        let root = std::path::PathBuf::from(root);
        found.push(root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"));
        found.push(root.join(r"System32\cmd.exe"));
    }
    for variable in ["ProgramFiles", "ProgramFiles(x86)"] {
        let Some(dir) = std::env::var_os(variable) else {
            continue;
        };
        let dir = std::path::PathBuf::from(dir);
        found.push(dir.join(r"PowerShell\7\pwsh.exe"));
        found.push(dir.join(r"Git\bin\bash.exe"));
    }
    // A per-user Git for Windows installs under `Programs`, and that is where
    // its `bash` is — the one shell here that is not a Microsoft program.
    if let Some(dir) = std::env::var_os("LocalAppData") {
        found.push(std::path::PathBuf::from(dir).join(r"Programs\Git\bin\bash.exe"));
    }
    found.push(r"C:\msys64\usr\bin\bash.exe".into());
    found.push(r"C:\cygwin64\bin\bash.exe".into());
    found
        .into_iter()
        .map(|path| path.display().to_string())
        .collect()
}

/// The names a shell has, for the `PATH` search.
///
/// The list is the shells a person installs, not every program that ends in
/// `sh`: a `PATH` walk is a search, and a search that matches anything is how a
/// `bashbug` or a `fish_indent` ends up in a menu of shells.
#[cfg(not(windows))]
const SHELL_NAMES: &[&str] = &[
    "bash", "zsh", "fish", "dash", "ksh", "tcsh", "mksh", "yash", "ash", "csh", "nu", "elvish",
    "xonsh", "sh",
];

#[cfg(windows)]
const SHELL_NAMES: &[&str] = &[
    "pwsh.exe",
    "powershell.exe",
    "cmd.exe",
    "bash.exe",
    "zsh.exe",
    "fish.exe",
    "nu.exe",
    "elvish.exe",
    "xonsh.exe",
];

/// Every program with one of these names that `PATH` can reach, in `PATH` order.
fn on_path(names: &[&str]) -> Vec<String> {
    names
        .iter()
        .filter_map(|name| resolved(name))
        .map(|path| path.display().to_string())
        .collect()
}

/// Where a program is, when it is anywhere.
///
/// An explicit path is itself if it exists; a bare name is looked for along
/// `PATH` — the search the spawner performs for a `$SHELL` or a `DTR_SHELL`
/// spelled `fish`, and the only way to turn a name into something the menu can
/// show or dedupe.
fn resolved(program: &str) -> Option<PathBuf> {
    if program.contains(['/', '\\']) {
        let path = Path::new(program);
        return path.is_file().then(|| path.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// The candidates that are really on this machine, in the order given — the
/// caller decides what the order means.
fn unique(candidates: impl IntoIterator<Item = String>) -> Vec<LocalShell> {
    let mut shells: Vec<LocalShell> = Vec::new();
    for program in candidates {
        let Some(found) = resolved(&program) else {
            continue;
        };
        shells.push(LocalShell {
            name: shell_name(&found),
            program: found.display().to_string(),
        });
    }
    one_per_name(shells)
}

/// One row per name, the first candidate of that name winning.
///
/// The candidates are ordered by how much the machine means them: the login
/// shell, then `/etc/shells`, then where a shell is installed, then `PATH`. So
/// the two bashes a `nix develop` puts in front of you are one row, and it is
/// the login one — a menu that says `bash` twice is not a choice, and the row a
/// person picks has to be the one their shell actually is.
fn one_per_name(shells: impl IntoIterator<Item = LocalShell>) -> Vec<LocalShell> {
    let mut kept: Vec<LocalShell> = Vec::new();
    for shell in shells {
        if kept.iter().any(|kept| kept.name == shell.name) {
            continue;
        }
        kept.push(shell);
    }
    kept
}

/// Alphabetical by name, with the login shell lifted to the front.
///
/// A menu is read by name, so the order a scan happened to find shells in is not
/// worth keeping — but the first row is the shell a bare `+` starts, and that is
/// a fact about the machine rather than a sort.
fn ordered(mut shells: Vec<LocalShell>, login: Option<&str>) -> Vec<LocalShell> {
    shells.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.program.cmp(&b.program))
    });
    // Found after the sort: an index taken before it names a different shell.
    if let Some(login) = login {
        if let Some(at) = shells.iter().position(|shell| shell.program == login) {
            let login = shells.remove(at);
            shells.insert(0, login);
        }
    }
    shells
}

/// What the menu calls a shell: its executable's name without `.exe`.
///
/// The name is cut out of the string rather than asked of [`Path`], because the
/// paths in `/etc/shells` are this platform's and a Windows one in a test is
/// still a name with a separator in it. The extension is stripped by hand rather
/// than with `file_stem`, which would also eat a number from a versioned name.
fn shell_name(program: &Path) -> String {
    let program = program.to_string_lossy();
    let name = match program.rsplit(['/', '\\']).next() {
        Some(name) => name,
        None => &program,
    };
    match name.len().checked_sub(4) {
        Some(cut) if name.as_bytes()[cut..].eq_ignore_ascii_case(b".exe") => name[..cut].to_owned(),
        _ => name.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LocalShell, by_use, com_port_name, local_shells, one_per_name, ordered, resolved,
        shell_name, shells_in, ssh_hosts, uart_absent, unique,
    };

    /// `/etc/shells` is programs, one per line, with `#` comments and blank
    /// lines: a line that is not an absolute path is not a shell to offer.
    #[test]
    fn the_shell_list_is_read_line_by_line() {
        let listed =
            shells_in("# /etc/shells: valid login shells\n\n/bin/sh\n  /bin/bash  \nbaash\n");
        assert_eq!(listed, ["/bin/sh", "/bin/bash"]);
    }

    /// The name in the menu is the executable's, and only `.exe` is an
    /// extension: `file_stem` would turn a versioned `nu-0.99.1` into `nu-0.99`.
    #[test]
    fn a_shell_is_named_by_its_program() {
        assert_eq!(shell_name(std::path::Path::new("/bin/bash")), "bash");
        assert_eq!(
            shell_name(std::path::Path::new("/usr/local/bin/fish")),
            "fish"
        );
        assert_eq!(
            shell_name(std::path::Path::new(
                r"C:\Program Files\PowerShell\7\pwsh.exe"
            )),
            "pwsh"
        );
        assert_eq!(
            shell_name(std::path::Path::new(r"C:\Windows\System32\cmd.EXE")),
            "cmd"
        );
        assert_eq!(
            shell_name(std::path::Path::new("/usr/bin/nu-0.99.1")),
            "nu-0.99.1"
        );
    }

    /// A program that is not on this machine is not a shell to offer, and two
    /// spellings of one name are one row — the NixOS case, where `/etc/shells`
    /// names the profile symlink and the store path behind it.
    #[test]
    fn only_programs_that_exist_are_offered_once() {
        assert!(unique(["/nonexistent/dtr-test/bash".to_owned()]).is_empty());
        #[cfg(unix)]
        {
            let shells = unique(["/bin/sh".to_owned(), "/bin/sh".to_owned()]);
            assert_eq!(shells.len(), 1, "one name is one row");
            assert_eq!(shells[0].name, "sh");
        }
    }

    /// Two bashes are one row, and the first candidate is the one kept: the list
    /// is ordered by how much the machine means it, so the login shell wins over
    /// a toolchain's.
    #[test]
    fn two_shells_with_one_name_are_one_row() {
        let shell = |name: &str, program: &str| LocalShell {
            name: name.to_owned(),
            program: program.to_owned(),
        };
        let kept = one_per_name([
            shell("bash", "/run/current-system/sw/bin/bash"),
            shell("sh", "/run/current-system/sw/bin/sh"),
            shell("bash", "/nix/store/xyz-bash/bin/bash"),
        ]);
        assert_eq!(
            kept.iter()
                .map(|shell| (shell.name.as_str(), shell.program.as_str()))
                .collect::<Vec<_>>(),
            [
                ("bash", "/run/current-system/sw/bin/bash"),
                ("sh", "/run/current-system/sw/bin/sh"),
            ]
        );
    }

    /// The list is deduplicated by that rule, on this machine, however many
    /// spellings `/etc/shells` and `PATH` between them have.
    #[cfg(unix)]
    #[test]
    fn no_shell_is_listed_twice_under_one_name() {
        let mut names: Vec<String> = local_shells().into_iter().map(|shell| shell.name).collect();
        let count = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), count, "one row per name");
    }

    /// A bare name is looked for on `PATH` — what the spawner does with a
    /// `$SHELL` or a `DTR_SHELL` spelled `fish` — and a name nothing answers to
    /// is not a shell.
    #[test]
    fn a_bare_name_is_looked_for_on_the_path() {
        assert!(resolved("dtr-no-such-shell-6f1c").is_none());
        assert!(resolved("/nonexistent/dtr-test/bash").is_none());
        #[cfg(unix)]
        assert!(resolved("/bin/sh").is_some_and(|path| path.is_file()));
    }

    /// The first row is the shell a bare `+` starts and the rest are
    /// alphabetical: a menu is read by name, not in the order a scan found
    /// things in.
    #[test]
    fn the_login_shell_is_first_and_the_rest_are_alphabetical() {
        let shell = |name: &str, program: &str| LocalShell {
            name: name.to_owned(),
            program: program.to_owned(),
        };
        let names = |shells: &[LocalShell]| -> Vec<String> {
            shells.iter().map(|shell| shell.name.clone()).collect()
        };
        let shells = vec![
            shell("zsh", "/usr/bin/zsh"),
            shell("bash", "/usr/bin/bash"),
            shell("fish", "/usr/bin/fish"),
        ];

        assert_eq!(
            names(&ordered(shells.clone(), Some("/usr/bin/fish"))),
            ["fish", "bash", "zsh"]
        );
        assert_eq!(
            names(&ordered(shells.clone(), None)),
            ["bash", "fish", "zsh"]
        );
        // A login shell that was not discovered is not invented for the menu.
        assert_eq!(
            names(&ordered(shells, Some("/bin/ion"))),
            ["bash", "fish", "zsh"]
        );
    }

    /// The machine half of the discovery: every row names a real program. This
    /// is the test that fails if `/etc/shells` is read without checking, or a
    /// symlink is offered instead of what it points at.
    #[cfg(unix)]
    #[test]
    fn this_machine_offers_shells_that_exist() {
        let shells = local_shells();
        assert!(!shells.is_empty(), "a terminal always has something to run");
        for shell in &shells {
            assert!(
                std::path::Path::new(&shell.program).is_file(),
                "{} is not a program",
                shell.program
            );
            assert!(!shell.name.is_empty());
        }
    }

    /// Only a legacy COM port has its driver's verdict in a `type` file. A USB or
    /// ACM adapter is a different device with real hardware behind it, and a
    /// Windows name is not a path at all.
    #[test]
    fn only_legacy_com_ports_are_probed() {
        assert_eq!(com_port_name("/dev/ttyS0"), Some("ttyS0"));
        assert_eq!(com_port_name("/dev/ttyS12"), Some("ttyS12"));
        assert_eq!(com_port_name("/dev/ttyUSB0"), None);
        assert_eq!(com_port_name("/dev/ttyACM0"), None);
        assert_eq!(com_port_name("/dev/pts/3"), None);
        assert_eq!(com_port_name("COM3"), None);
    }

    /// `0` is the kernel's "no UART here", and nothing else is: a port whose file
    /// does not say that stays in the menu.
    #[test]
    fn only_a_zero_type_is_an_absent_uart() {
        assert!(uart_absent("0"));
        assert!(uart_absent("0\n"));
        assert!(!uart_absent("4"), "a 16550A is a port worth offering");
        assert!(!uart_absent(""));
        assert!(!uart_absent("unknown"));
    }
    /// The menu is a list of what a person reaches for, so the hosts they open
    /// come first; a machine that has opened none keeps the order the config file
    /// was written in, which is an order somebody chose.
    #[test]
    fn the_hosts_a_person_uses_come_first() {
        use dtr_store::SshUse;
        use std::collections::HashMap;

        let hosts = || {
            vec![
                ("alpha".to_owned(), String::new()),
                ("beta".to_owned(), String::new()),
                ("gamma".to_owned(), String::new()),
            ]
        };
        let names = |hosts: Vec<(String, String)>| -> Vec<String> {
            hosts.into_iter().map(|(alias, _)| alias).collect()
        };

        assert_eq!(
            names(by_use(hosts(), &HashMap::new())),
            vec!["alpha", "beta", "gamma"],
            "nothing remembered leaves the file's order alone"
        );

        let mut uses = HashMap::new();
        uses.insert("gamma".to_owned(), SshUse { uses: 3, last: 10 });
        uses.insert("beta".to_owned(), SshUse { uses: 1, last: 20 });
        assert_eq!(
            names(by_use(hosts(), &uses)),
            vec!["gamma", "beta", "alpha"],
            "the most used first, then the one used once"
        );

        let mut uses = HashMap::new();
        uses.insert("alpha".to_owned(), SshUse { uses: 1, last: 30 });
        uses.insert("gamma".to_owned(), SshUse { uses: 1, last: 10 });
        assert_eq!(
            names(by_use(hosts(), &uses)),
            vec!["alpha", "gamma", "beta"],
            "the same count, the more recent first"
        );
    }

    /// The config file is the list and the store is only its order: a host that
    /// was deleted from `~/.ssh/config` is not offered, however many times it was
    /// opened, and a config that is gone leaves nothing behind either.
    #[test]
    fn a_host_the_config_no_longer_names_is_not_offered() {
        use dtr_store::SshUse;
        use std::collections::HashMap;

        let dir = tempfile::tempdir().expect("a temporary directory");
        let config = dir.path().join("config");
        std::fs::write(
            &config,
            "Host alpha\n  HostName 127.0.0.1\n\nHost gamma\n  HostName 127.0.0.1\n",
        )
        .expect("write the config");

        // `beta` is the one that was used most and it is no longer in the file.
        let mut uses = HashMap::new();
        uses.insert("beta".to_owned(), SshUse { uses: 9, last: 30 });

        let offered = |uses: &HashMap<String, SshUse>| -> Vec<String> {
            by_use(ssh_hosts(&config), uses)
                .into_iter()
                .map(|(alias, _)| alias)
                .collect()
        };
        assert_eq!(
            offered(&uses),
            ["alpha", "gamma"],
            "a count for a host the file does not name is a lookup, not a row"
        );
        assert_eq!(
            offered(&uses),
            offered(&HashMap::new()),
            "a stale count changes nothing at all about the rows that are left"
        );

        std::fs::remove_file(&config).expect("remove the config");
        assert!(
            offered(&uses).is_empty(),
            "no config file is no hosts, whatever the store remembers"
        );
    }
}
