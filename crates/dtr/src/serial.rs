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
}

impl Config {
    pub fn open(&self) -> anyhow::Result<tokio_serial::SerialStream> {
        let builder = tokio_serial::new(&self.port, self.baud)
            .data_bits(tokio_serial::DataBits::Eight)
            .parity(tokio_serial::Parity::None)
            .stop_bits(tokio_serial::StopBits::One)
            .flow_control(tokio_serial::FlowControl::None);
        Ok(tokio_serial::SerialStream::open(&builder)?)
    }
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
    fn nothing_pending_means_no_output() {
        assert_eq!(run(&[b"plain\n"]), b"plain\n");
    }
}
