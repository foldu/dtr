//! Where the palette's candidates come from: this machine's serial ports, the
//! user's ssh hosts, and the shells we know how to launch.
//!
//! Everything here is best-effort. A missing `~/.ssh/config` or a serial
//! subsystem that refuses to enumerate must not stop a terminal from starting;
//! the palette simply offers fewer things.

use std::path::{Path, PathBuf};

use dtr_complete::{Engine, PortInfo};

/// Rebuild the engine from the current state of the machine.
pub fn engine() -> Engine {
    let mut engine = Engine::new();
    engine.set_ports(serial_ports());
    engine.set_hosts(ssh_hosts(&ssh_config_path()));
    engine.set_shells(shells());
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
    /// A serial device path, which is what a port is opened with.
    Serial { path: String, detail: String },
    /// An alias in `~/.ssh/config`.
    Ssh { alias: String, detail: String },
}

impl Destination {
    /// What the menu says. The kind is in the text as well as in the grouping:
    /// "serial /dev/ttyUSB0" and "ssh web" are different verbs, and a row that
    /// only said `web` would not tell you which one it was going to do.
    pub fn label(&self) -> String {
        match self {
            Self::Serial { path, detail } if detail.is_empty() => format!("serial {path}"),
            Self::Serial { path, detail } => format!("serial {path}  {detail}"),
            Self::Ssh { alias, detail } if detail.is_empty() => format!("ssh {alias}"),
            Self::Ssh { alias, detail } => format!("ssh {alias}  {detail}"),
        }
    }

    pub fn is_serial(&self) -> bool {
        matches!(self, Self::Serial { .. })
    }
}

/// The ssh hosts the new-tab menu offers: `~/.ssh/config`'s aliases, with the
/// detail line each one shows.
pub fn ssh_destinations() -> Vec<Destination> {
    ssh_hosts(&ssh_config_path())
        .into_iter()
        .map(|(alias, detail)| Destination::Ssh { alias, detail })
        .collect()
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

/// Shells worth offering. We cannot enumerate every program on PATH and do not
/// want to; this is the short list a terminal actually launches, filtered to
/// what exists on this machine.
fn shells() -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();
    for variable in ["SHELL", "COMSPEC"] {
        if let Ok(value) = std::env::var(variable) {
            if !value.is_empty() {
                candidates.push(value);
            }
        }
    }
    #[cfg(windows)]
    candidates.extend(["pwsh.exe", "powershell.exe", "cmd.exe"].map(str::to_owned));
    #[cfg(not(windows))]
    candidates.extend(["/bin/bash", "/bin/sh", "/usr/bin/fish"].map(str::to_owned));

    candidates.retain(|path| {
        // A bare name is resolved by the spawner; only check paths.
        if path.contains('/') || path.contains('\\') {
            Path::new(path).exists()
        } else {
            true
        }
    });
    candidates.sort();
    candidates.dedup();
    candidates
}

#[cfg(test)]
mod tests {
    use super::{com_port_name, uart_absent};

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
}
