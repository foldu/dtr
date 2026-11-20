//! The portal daemon: D-Bus activation runs this, and it answers the desktop's
//! file chooser requests with a real picker window.
//!
//! It is `dtr-portal` rather than a second binary in `dtr` because the two have
//! nothing to say to each other: the terminal owns a VT core, panes and a scroll
//! buffer, and a picker is a file browser. The process the desktop activates is
//! small on purpose (`docs/manager.md` §2).
//!
//! **The window loop owns the main thread.** winit panics if its `EventLoop` is
//! created anywhere else, so it is the *bus* that moves: `main` builds the
//! picker loop on this thread, hands the D-Bus side a [`WindowHost`], waits for
//! the bus handshake, and then runs the loop until the idle timeout. The process
//! therefore lives exactly as long as the picker loop does, which is what makes
//! "idle-exit after a timeout when it owns no visible window" one line —
//! `event_loop.exit()` — instead of a second way to shut a process down
//! (`src/window.rs`).
//!
//! A machine with no display keeps the interface honest: the loop cannot be
//! built, so `main` serves the portal with a host that answers every request with
//! a cancel and says why in the log. A person sees the application's own "no
//! file chosen" instead of a dialog that never appears with no explanation
//! anywhere.

use std::process::ExitCode;
use std::sync::mpsc;
use std::time::Duration;

use dtr_portal::window::{self, Options, WindowHost};
use dtr_portal::{Call, Host, Portal};
use tracing_subscriber::EnvFilter;

/// The host for a machine that cannot open a window.
///
/// `Host::open` is called on the D-Bus task and the real host posts to the
/// window loop; this one has no loop to post to, so it answers at once, with the
/// reason the loop could not be built in the log. A portal that hangs is worse
/// than one that says no.
struct NoWindowPicker {
    reason: String,
}

impl Host for NoWindowPicker {
    fn open(&mut self, call: Call) {
        tracing::warn!(
            request = %call.id,
            title = %call.window.title,
            app_id = %call.window.app_id,
            reason = %self.reason,
            "no picker window on this machine; answering cancel"
        );
        let _ = call.cancel();
    }
}

fn main() -> ExitCode {
    // `RUST_LOG` for the detail, `info` otherwise: an activated service has no
    // stdout to look at, so its log is the journal's.
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    match window::start(Options::from_env()) {
        Ok((run, host)) => match serve_beside_the_window(host) {
            Ok(()) => match run.run() {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    tracing::error!(%err, "the picker window failed");
                    ExitCode::FAILURE
                }
            },
            Err(()) => ExitCode::FAILURE,
        },
        Err(reason) => {
            // No display (or no adapter): keep the bus and answer each request
            // with a cancel, loudly.
            tracing::warn!(%reason, "serving the portal without a picker window");
            match pollster::block_on(serve(NoWindowPicker { reason }, None)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    tracing::error!(%err, "could not serve the file chooser portal");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

/// Start the D-Bus half on a thread and wait for it to own the name.
///
/// The window loop cannot start until this returns, so a handshake failure is an
/// exit status rather than a window nobody can reach. The connection lives on
/// that thread for the life of the process; `main` ends when the picker loop
/// does, and the thread — and the service name — go with it.
fn serve_beside_the_window(host: WindowHost) -> Result<(), ()> {
    let (ready, handshake) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("dtr-portal-bus".to_owned())
        .spawn(move || {
            if let Err(err) = pollster::block_on(serve(host, Some(ready))) {
                tracing::error!(%err, "the file chooser portal stopped");
            }
        });
    if let Err(err) = spawned {
        tracing::error!(%err, "could not start the D-Bus thread");
        return Err(());
    }
    match handshake.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => {
            tracing::error!(%err, "could not serve the file chooser portal");
            Err(())
        }
        Err(err) => {
            tracing::error!(%err, "the D-Bus thread never finished its handshake");
            Err(())
        }
    }
}

/// Serve the interface, and then hold the connection open.
///
/// zbus's `async-io` executor owns the sockets on its own thread; this future
/// only has to not finish. `ready` is the handshake's answer for `main` (absent
/// on the no-display path, where `main` *is* the server).
async fn serve(
    host: impl Host,
    ready: Option<mpsc::Sender<Result<(), String>>>,
) -> Result<(), String> {
    let connection = Portal::new(host)
        .serve()
        .await
        .map_err(|err| format!("{err}"))?;
    if let Some(ready) = ready {
        let _ = ready.send(Ok(()));
    }
    // Dropping the connection would take the service name and every exported
    // object with it, so the future never finishes: the process's lifetime is
    // the window loop's, and that is what ends it.
    std::future::pending::<()>().await;
    drop(connection);
    Ok(())
}
