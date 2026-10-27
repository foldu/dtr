//! Modem control lines: DTR, RTS and break.
//!
//! These are ioctls on the tty rather than data, and they need the file
//! descriptor — which `tokio_serial::SerialStream` will not hand out twice
//! (`try_clone` refuses). So the descriptor is duplicated here: a `dup` refers
//! to the same tty *without reopening it*, and that distinction is the whole
//! point. Opening a serial device asserts DTR, which resets the board you were
//! about to talk to.

use std::time::Duration;

/// How long a break is held. Long enough for any bootloader to notice, short
/// enough not to wedge the session.
pub const BREAK: Duration = Duration::from_millis(250);

#[cfg(unix)]
pub struct Modem {
    fd: std::os::unix::io::RawFd,
}

#[cfg(unix)]
impl Modem {
    pub fn new(stream: &tokio_serial::SerialStream) -> Option<Self> {
        use std::os::unix::io::AsRawFd;
        // SAFETY: `dup` on a descriptor the caller still owns; the duplicate is
        // owned by this struct and closed on drop.
        let fd = unsafe { libc::dup(stream.as_raw_fd()) };
        if fd < 0 {
            tracing::warn!(error = %std::io::Error::last_os_error(), "cannot duplicate the port descriptor");
            return None;
        }
        Some(Self { fd })
    }

    pub fn set_dtr(&mut self, level: bool) -> std::io::Result<()> {
        self.set_line(libc::TIOCM_DTR as i32, level)
    }

    pub fn set_rts(&mut self, level: bool) -> std::io::Result<()> {
        self.set_line(libc::TIOCM_RTS as i32, level)
    }

    fn set_line(&mut self, bit: i32, level: bool) -> std::io::Result<()> {
        let request = if level { libc::TIOCMBIS } else { libc::TIOCMBIC };
        // SAFETY: ioctl with a pointer to a live `c_int` for the call's duration.
        let result = unsafe { libc::ioctl(self.fd, request, &bit as *const i32) };
        if result == -1 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Send a break. `tcsendbreak` with a zero duration emits the POSIX minimum;
    /// the thread sleeps for the rest so the lines are actually held low.
    pub fn pulse_break(&mut self) -> std::io::Result<()> {
        // SAFETY: the descriptor is live for as long as this struct.
        let result = unsafe { libc::tcsendbreak(self.fd, 0) };
        if result == -1 {
            return Err(std::io::Error::last_os_error());
        }
        std::thread::sleep(BREAK);
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for Modem {
    fn drop(&mut self) {
        // SAFETY: closing a descriptor this struct owns, once.
        unsafe {
            libc::close(self.fd);
        }
    }
}

// Windows needs `EscapeCommFunction`/`SetCommBreak` through a duplicated
// HANDLE. Deliberately not written yet: it cannot be tested here, and blind
// unsafe FFI on the platform we cannot run is worse than an honest refusal.
#[cfg(not(unix))]
pub struct Modem;

#[cfg(not(unix))]
impl Modem {
    pub fn new(_stream: &tokio_serial::SerialStream) -> Option<Self> {
        tracing::warn!("modem control is not implemented on this platform yet");
        None
    }

    pub fn set_dtr(&mut self, _level: bool) -> std::io::Result<()> {
        Ok(())
    }

    pub fn set_rts(&mut self, _level: bool) -> std::io::Result<()> {
        Ok(())
    }

    pub fn pulse_break(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
