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

fn ssh_config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".ssh")
        .join("config")
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
