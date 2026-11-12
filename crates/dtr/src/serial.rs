//! Serial transport.
//!
//! Two things live here that are specific to serial rather than PTYs: the port
//! configuration, and the CR→CRLF mapping real devices need because they send
//! bare `\r` and a VT parser reads that as "column 0" rather than a newline.
//!
//! The port is **blocking on purpose**, and that is the whole story of this
//! module. When a read counts as finished is the driver's decision, not ours: on
//! Windows a `ReadFile` completes when the requested number of bytes has arrived
//! or the total timeout elapses, and a continuous stream never leaves a gap for
//! an inter-byte timeout to notice. So a runtime wrapper that asks for 8 KB and
//! installs a 1 ms inter-byte timeout gets its first 4 KB when the driver's
//! buffer fills — 355 ms of silence at 115200, then fifty lines at once. Asking
//! for a blocking read whose *timeout* is [`IDLE_FLUSH`] is the same code on both
//! platforms and gives what arrived every 25 ms; `pane::read_serial` is an
//! ordinary thread because of it.

use std::io::{Read, Write};
use std::time::Duration;

/// How long an idle read may hold a pending CR before flushing it as CRLF.
///
/// A `\r` can only be judged once the next byte is known: `\r\n` must pass
/// through untouched, a lone `\r` must become `\r\n`. The timeout bounds how
/// long the last line of a burst can be held back.
pub const IDLE_FLUSH: Duration = Duration::from_millis(25);

/// How the session log is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogFormat {
    /// Exactly the bytes the wire delivered: replayable, not readable.
    Raw,
    /// One line per read with a relative timestamp and escaped bytes: readable,
    /// not replayable.
    Timestamped,
}

/// The platform's own port object.
///
/// `serialport`'s trait object would do for reading and writing, but not for the
/// things that need the descriptor: `modem` duplicates it, and `try_clone_native`
/// is what gives the writing half a handle of its own. So the concrete type is
/// kept, which is per-platform anyway.
#[cfg(unix)]
type Native = serialport::TTYPort;
#[cfg(windows)]
type Native = serialport::COMPort;

/// An open port: one reader, one writer, and the control lines.
///
/// A `read` here blocks for at most the port's timeout and returns whatever has
/// arrived. A timeout comes back as [`std::io::ErrorKind::TimedOut`], which is
/// "nothing yet" rather than a failure — the shape `pane::read_serial` needs to
/// tell an idle line, a CR waiting to be flushed and a device that is gone apart.
pub struct Port(Native);

impl Port {
    fn from_builder(builder: &serialport::SerialPortBuilder) -> anyhow::Result<Self> {
        Ok(Self(Native::open(builder)?))
    }

    /// Another handle to the same device, for the half that writes.
    ///
    /// A duplicate, not a second open: opening a serial device asserts DTR,
    /// which resets the board you were about to talk to.
    pub fn try_clone(&self) -> anyhow::Result<Self> {
        Ok(Self(self.0.try_clone_native()?))
    }

    pub fn set_dtr(&mut self, level: bool) -> serialport::Result<()> {
        use serialport::SerialPort as _;
        self.0.write_data_terminal_ready(level)
    }

    pub fn set_rts(&mut self, level: bool) -> serialport::Result<()> {
        use serialport::SerialPort as _;
        self.0.write_request_to_send(level)
    }
}

impl Read for Port {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for Port {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

#[cfg(unix)]
impl std::os::unix::io::AsRawFd for Port {
    fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
        self.0.as_raw_fd()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub port: String,
    pub baud: u32,
    pub data_bits: dtr_store::DataBits,
    pub parity: dtr_store::Parity,
    pub stop_bits: dtr_store::StopBits,
    pub flow_control: dtr_store::FlowControl,
    /// Levels to apply as soon as the port is open. The open itself asserts
    /// DTR on every platform dtr runs on, so "off" means "lowered immediately
    /// afterwards", not "never raised".
    pub dtr: bool,
    pub rts: bool,
    pub cr_to_crlf: bool,
    /// Follow the device across a replug instead of giving up on the first
    /// read error. On by default: a serial console that dies when you jiggle
    /// the cable is not a console.
    pub auto_reconnect: bool,
}

impl Config {
    /// The transport settings a sheet's remembered record becomes.
    pub fn from_settings(port: impl Into<String>, settings: &dtr_store::SerialSettings) -> Self {
        Self {
            port: port.into(),
            baud: settings.baud,
            data_bits: settings.data_bits,
            parity: settings.parity,
            stop_bits: settings.stop_bits,
            flow_control: settings.flow_control,
            dtr: settings.dtr,
            rts: settings.rts,
            cr_to_crlf: settings.cr_to_crlf,
            auto_reconnect: settings.auto_reconnect,
        }
    }

    pub fn line_code(&self) -> String {
        let parity = match self.parity {
            dtr_store::Parity::None => 'N',
            dtr_store::Parity::Odd => 'O',
            dtr_store::Parity::Even => 'E',
        };
        format!(
            "{}{}{}",
            u8::from(self.data_bits),
            parity,
            u8::from(self.stop_bits)
        )
    }

    pub fn open(&self) -> anyhow::Result<Port> {
        self.open_at(&self.port)
    }

    /// Open a specific path with these settings. Reconnect uses this because
    /// the device may come back under a different name.
    pub fn open_at(&self, path: &str) -> anyhow::Result<Port> {
        let builder = serialport::new(path, self.baud)
            .data_bits(self.data_bits)
            .parity(self.parity)
            .stop_bits(self.stop_bits)
            .flow_control(self.flow_control)
            // The read timeout *is* the latency knob: a blocking read returns
            // when this elapses with whatever it has. See the module doc.
            .timeout(IDLE_FLUSH);
        let mut port = Port::from_builder(&builder)?;
        // The line levels are best-effort: a driver that refuses to lower DTR
        // is not a reason to refuse the session. Hardware flow control owns RTS
        // on some drivers, so a failure there is even less surprising.
        if let Err(err) = port.set_dtr(self.dtr) {
            tracing::warn!(%err, dtr = self.dtr, "could not set DTR; continuing");
        }
        if let Err(err) = port.set_rts(self.rts) {
            tracing::warn!(%err, rts = self.rts, "could not set RTS; continuing");
        }
        Ok(port)
    }
}

/// The USB serial number of a port, if it has one.
///
/// This is the identity that survives a replug: the path will change
/// (`ttyUSB0` becomes `ttyUSB1`) and so may the device node, but the serial
/// number belongs to the adapter.
pub fn identity(port: &str) -> Option<String> {
    let ports = serialport::available_ports().ok()?;
    let found = ports
        .into_iter()
        .find(|candidate| candidate.port_name == port)?;
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
            serialport::SerialPortType::UsbPort(usb) => {
                usb.serial_number.as_deref() == Some(serial)
            }
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

    #[test]
    fn a_remembered_record_becomes_the_open_settings() {
        let settings = dtr_store::SerialSettings {
            baud: 57_600,
            data_bits: dtr_store::DataBits::Seven,
            parity: dtr_store::Parity::Even,
            stop_bits: dtr_store::StopBits::Two,
            flow_control: dtr_store::FlowControl::Hardware,
            dtr: false,
            rts: true,
            cr_to_crlf: false,
            auto_reconnect: false,
            profile: Some("gps".to_owned()),
        };
        let config = super::Config::from_settings("/dev/ttyUSB0", &settings);
        assert_eq!(config.port, "/dev/ttyUSB0");
        assert_eq!(config.baud, 57_600);
        assert_eq!(config.data_bits, dtr_store::DataBits::Seven);
        assert_eq!(config.parity, dtr_store::Parity::Even);
        assert_eq!(config.stop_bits, dtr_store::StopBits::Two);
        assert_eq!(config.flow_control, dtr_store::FlowControl::Hardware);
        assert!(!config.dtr);
        assert!(config.rts);
        assert!(!config.cr_to_crlf);
        assert!(!config.auto_reconnect);
        assert_eq!(config.line_code(), "7E2");
    }
}

/// What a read is allowed to hand back, asserted against a real descriptor.
///
/// These are the rules that made a u-boot `#` flood crawl: a read that waits for
/// the bytes it asked for shows nothing until the driver's buffer fills, and a
/// read that cannot tell a timeout from a closure turns every idle line into a
/// disconnect. Both belong to the port, so both are tested here rather than
/// through the pane.
#[cfg(all(unix, test))]
mod port_tests {
    use super::{CrToCrlf, IDLE_FLUSH, Port};
    use serialport::SerialPort as _;
    use std::io::{Read, Write};

    /// A device on one end and the pane's port on the other, both raw.
    fn pair() -> (serialport::TTYPort, Port) {
        let (device, slave) = serialport::TTYPort::pair().expect("pty pair");
        let mut port = Port(slave);
        // The pair's own default is 100 ms; the port a pane opens is told
        // `IDLE_FLUSH`, and these tests assert on that.
        port.0.set_timeout(IDLE_FLUSH).expect("set timeout");
        (device, port)
    }

    /// A read of a big buffer returns the few bytes that arrived, not the count.
    ///
    /// 8192 bytes asked for, three written: a port that waits for its count is
    /// the bug this replaced.
    #[test]
    fn a_read_returns_what_arrived_not_what_was_asked_for() {
        let (mut device, mut port) = pair();
        device.write_all(b"###").expect("device write");

        let mut buf = [0u8; 8192];
        let n = port.read(&mut buf).expect("read");
        assert_eq!(&buf[..n], b"###", "a read of 8192 bytes came back with {n}");
    }

    /// An idle line is a timeout, not an error and not end of file.
    #[test]
    fn an_idle_read_times_out_rather_than_returning_zero() {
        let (_device, mut port) = pair();

        let mut buf = [0u8; 64];
        let started = std::time::Instant::now();
        let err = port.read(&mut buf).expect_err("nothing was sent");
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        // The timeout is the port's, and it is the pane's flush interval. Five
        // milliseconds of slack for a poll that rounds.
        assert!(
            started.elapsed() >= IDLE_FLUSH - std::time::Duration::from_millis(5),
            "a read gave up after {:?}, before its own timeout",
            started.elapsed()
        );
    }

    /// The CR that ends a burst is held until the line goes quiet, which is what
    /// the timeout is for: a `\r` can only be judged once the next byte is known.
    #[test]
    fn a_cr_at_the_end_of_a_burst_waits_for_the_timeout() {
        let (mut device, mut port) = pair();
        let mut mapper = CrToCrlf::default();
        let mut out = Vec::new();

        device.write_all(b"hi\r").expect("device write");
        let mut buf = [0u8; 64];
        let n = port.read(&mut buf).expect("read");
        mapper.feed(&buf[..n], &mut out);
        assert_eq!(out, b"hi", "the CR is held: CRLF may still follow");

        let err = port.read(&mut buf).expect_err("the line is quiet");
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        mapper.flush(&mut out);
        assert_eq!(out, b"hi\r\n", "the held CR becomes a newline");
    }
}
