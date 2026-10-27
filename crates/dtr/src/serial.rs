//! Serial transport.
//!
//! Two things live here that are specific to serial rather than PTYs: the port
//! configuration, and the CR→CRLF mapping real devices need because they send
//! bare `\r` and a VT parser reads that as "column 0" rather than a newline.

use std::time::Duration;

/// How long an idle read may hold a pending CR before flushing it as CRLF.
///
/// A `\r` can only be judged once the next byte is known: `\r\n` must pass
/// through untouched, a lone `\r` must become `\r\n`. The timeout bounds how
/// long the last line of a burst can be held back.
pub const IDLE_FLUSH: Duration = Duration::from_millis(25);

#[derive(Clone, Debug)]
pub struct Config {
    pub port: String,
    pub baud: u32,
    pub cr_to_crlf: bool,
    /// Follow the device across a replug instead of giving up on the first
    /// read error. On by default: a serial console that dies when you jiggle
    /// the cable is not a console.
    pub auto_reconnect: bool,
}

impl Config {
    pub fn open(&self) -> anyhow::Result<tokio_serial::SerialStream> {
        self.open_at(&self.port)
    }

    /// Open a specific path with these settings. Reconnect uses this because
    /// the device may come back under a different name.
    pub fn open_at(&self, path: &str) -> anyhow::Result<tokio_serial::SerialStream> {
        let builder = tokio_serial::new(path, self.baud)
            .data_bits(tokio_serial::DataBits::Eight)
            .parity(tokio_serial::Parity::None)
            .stop_bits(tokio_serial::StopBits::One)
            .flow_control(tokio_serial::FlowControl::None);
        Ok(tokio_serial::SerialStream::open(&builder)?)
    }
}

/// The USB serial number of a port, if it has one.
///
/// This is the identity that survives a replug: the path will change
/// (`ttyUSB0` becomes `ttyUSB1`) and so may the device node, but the serial
/// number belongs to the adapter.
pub fn identity(port: &str) -> Option<String> {
    let ports = serialport::available_ports().ok()?;
    let found = ports.into_iter().find(|candidate| candidate.port_name == port)?;
    match found.port_type {
        serialport::SerialPortType::UsbPort(usb) => usb.serial_number,
        _ => None,
    }
}

/// Find the port that currently carries this USB serial number.
pub fn find_by_serial(serial: &str) -> Option<String> {
    let ports = serialport::available_ports().ok()?;
    ports
        .into_iter()
        .find(|candidate| match &candidate.port_type {
            serialport::SerialPortType::UsbPort(usb) => usb.serial_number.as_deref() == Some(serial),
            _ => false,
        })
        .map(|candidate| candidate.port_name)
}

/// Turns a lone CR into CRLF without doubling a real CRLF.
///
/// This has to be a state machine, not a replace: `\r` and `\n` routinely land
/// in different reads, and a blind substitution gives every device that *does*
/// send CRLF a blank line between every line.
#[derive(Debug, Default)]
pub struct CrToCrlf {
    pending: bool,
}

impl CrToCrlf {
    pub fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) {
        for &byte in input {
            if self.pending {
                self.pending = false;
                out.push(b'\r');
                out.push(b'\n');
                if byte == b'\n' {
                    // A real CRLF: already emitted, drop the LF.
                    continue;
                }
            }

            if byte == b'\r' {
                self.pending = true;
            } else {
                out.push(byte);
            }
        }
    }

    /// Emit a held CR as CRLF. Call when the stream goes idle.
    pub fn flush(&mut self, out: &mut Vec<u8>) {
        if self.pending {
            self.pending = false;
            out.push(b'\r');
            out.push(b'\n');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CrToCrlf;

    fn run(chunks: &[&[u8]]) -> Vec<u8> {
        let mut mapper = CrToCrlf::default();
        let mut out = Vec::new();
        for chunk in chunks {
            mapper.feed(chunk, &mut out);
        }
        mapper.flush(&mut out);
        out
    }

    #[test]
    fn lone_cr_becomes_crlf() {
        assert_eq!(run(&[b"a\rb"]), b"a\r\nb");
    }

    #[test]
    fn existing_crlf_is_not_doubled() {
        assert_eq!(run(&[b"a\r\nb"]), b"a\r\nb");
    }

    #[test]
    fn crlf_split_across_reads_is_not_doubled() {
        assert_eq!(run(&[b"a\r", b"\nb"]), b"a\r\nb");
    }

    #[test]
    fn trailing_cr_is_flushed() {
        assert_eq!(run(&[b"hi\r"]), b"hi\r\n");
    }

    #[test]
    fn back_to_back_crs_become_two_lines() {
        assert_eq!(run(&[b"\r\r"]), b"\r\n\r\n");
    }

    #[test]
    fn an_unknown_serial_number_matches_no_port() {
        // Enumeration itself may fail in a sandbox; either way there is no port
        // carrying this serial number.
        assert!(super::find_by_serial("dtr-no-such-serial-0000").is_none());
    }

    #[test]
    fn nothing_pending_means_no_output() {
        assert_eq!(run(&[b"plain\n"]), b"plain\n");
    }
}
