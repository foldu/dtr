//! The portal daemon: D-Bus activation runs this, and it answers the desktop's
//! file chooser requests.
//!
//! It is `dtr-portal` rather than a second binary in `dtr` because the two have
//! nothing to say to each other: the terminal owns a VT core, panes and a scroll
//! buffer, and a picker is a file browser. The process the desktop activates is
//! small on purpose (`docs/manager.md` §2).
//!
//! **The picker window is the follow-up.** When this milestone was split, the
//! window needed `crates/dtr-frame` (the multi-window event loop, surface and
//! DPI handling), which was being extracted in parallel and is not a dependency
//! here yet. So the binary serves the interface with the smallest possible host
//! — one that answers every request with a cancel and says why in the log — and
//! the window task replaces [`NoWindowPicker`] with the real `Host`. Everything
//! else (the interface, the options, the request lifecycle, the `uri` encoding,
//! the packaging) is finished and tested over a private bus.
//!
//! Failing loudly rather than hanging is the point: a person sees the
//! application's own "no file chosen" instead of a dialog that never appears
//! with no explanation anywhere.

use std::process::ExitCode;

use dtr_portal::{Answer, Call, Host, Portal};
use tracing_subscriber::EnvFilter;

/// The placeholder host.
///
/// `Host::open` is called on the D-Bus task, and the real implementation posts
/// to the window thread; this one has no thread to post to, so it answers at
/// once. `docs/portal.md` names this as the one thing the window follow-up
/// replaces.
struct NoWindowPicker;

impl Host for NoWindowPicker {
    fn open(&mut self, call: Call) {
        tracing::warn!(
            request = %call.id,
            title = %call.window.title,
            app_id = %call.window.app_id,
            select = ?call.request.select,
            "no picker window in this build; answering cancel"
        );
        let _ = call.reply.send(Answer::Cancelled);
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

    match pollster::block_on(run()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(%err, "could not serve the file chooser portal");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> zbus::Result<()> {
    // The connection is kept in scope: dropping it would take the service name
    // and every exported object with it.
    let _connection = Portal::new(NoWindowPicker).serve().await?;
    tracing::info!("dtr-portal is running");
    // zbus's `async-io` executor owns the sockets on its own thread; this future
    // has no work of its own, and finishing would end the process the desktop
    // activated.
    std::future::pending::<()>().await;
    Ok(())
}
