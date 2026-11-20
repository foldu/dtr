//! `dtr-files [PATH]` — ask the dtr daemon for a manager window.
//!
//! This is a **launcher, not an application**: it has no window, no listing and no
//! state. It connects to the session bus, calls
//! `org.dtr.Files.OpenWindow(s path, a{sv} options)` on `dtr-portal`, prints the
//! handle the daemon answered with, and exits. If the daemon is not running the
//! bus starts it (the `org.dtr.Files.service` file installed beside the binary),
//! so the first `dtr-files` of a session pays for a process and every later one —
//! a picker's window too — is a frame in that same process (`docs/manager.md` §6).
//!
//! **One process, two binaries.** Both live in `crates/dtr-portal` because they
//! are one program's two entry points: the daemon owns the bus names and the
//! window loop, the launcher is the client. `dtr-files` doing its own window
//! would mean a second wgpu device, a second icon cache and a second idle policy
//! to keep in step.
//!
//! ```text
//! dtr-files                 # the person's home
//! dtr-files /tmp/scratch    # there (a file opens its parent, selected)
//! dtr-files --help
//! ```
//!
//! A path is resolved against the current directory and canonicalised when it
//! exists: the daemon's working directory is whatever D-Bus activation left it in,
//! and nobody chose that. The interface takes a **string** path (the shell's
//! `argv` is text by the time it arrives); a path that is not valid UTF-8 cannot
//! travel on this wire and is refused with a sentence rather than replaced — a
//! lossy path is a different file.
//!
//! Failure is loud and non-zero: no session bus, no daemon and no activation file,
//! or a daemon that has no display are all `dtr-files: …` on stderr and an exit
//! status. A launcher that silently did nothing would read as a broken desktop.

use std::collections::HashMap;
use std::ffi::OsString;
use std::process::ExitCode;
use std::time::Duration;

use dtr_portal::{FILES_INTERFACE, FILES_OBJECT_PATH, FILES_SERVICE_NAME};
use zbus::zvariant::OwnedValue;

/// How long to wait for the daemon before giving up.
///
/// Generous: an activation that has to start the process, bring up a wgpu device
/// and load fonts is a second or two on this machine and longer on a cold one. A
/// daemon that hung is a bug, and a launcher that hangs with it is two.
const TIMEOUT: Duration = Duration::from_secs(30);

fn main() -> ExitCode {
    // Only this program's own sentences; the daemon's tracing is its journal's.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let mut argument: Option<OsString> = None;
    for arg in std::env::args_os().skip(1) {
        match arg.to_str() {
            Some("--help" | "-h") => {
                println!("usage: dtr-files [PATH]");
                println!();
                println!("Open a file manager window. PATH defaults to the home directory;");
                println!("a file opens its parent with the file selected.");
                return ExitCode::SUCCESS;
            }
            Some(other) if other.starts_with('-') && other != "-" => {
                eprintln!("dtr-files: unknown argument `{other}`; try --help");
                return ExitCode::FAILURE;
            }
            // A non-UTF-8 argument is refused by `start_argument`: the interface
            // carries a string, and a lossy replacement is a different file.
            None | Some(_) => argument = Some(arg),
        }
    }

    let path = match start_argument(argument) {
        Ok(path) => path,
        Err(reason) => {
            eprintln!("dtr-files: {reason}");
            return ExitCode::FAILURE;
        }
    };

    // The call runs on a thread so a hung daemon cannot hang the launcher; the
    // main thread waits with a timeout and the process's exit ends any straggler.
    let (answer, received) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("dtr-files-call".to_owned())
        .spawn(move || {
            let result = pollster::block_on(open_window(path));
            let _ = answer.send(result);
        });
    if let Err(err) = spawned {
        eprintln!("dtr-files: could not start the call: {err}");
        return ExitCode::FAILURE;
    }
    match received.recv_timeout(TIMEOUT) {
        Ok(Ok(handle)) => {
            // The handle is the whole answer: a future caller can `CloseWindow`
            // it, and a script can see the window was opened.
            println!("{handle}");
            ExitCode::SUCCESS
        }
        Ok(Err(reason)) => {
            eprintln!("dtr-files: {reason}");
            ExitCode::FAILURE
        }
        Err(_) => {
            eprintln!(
                "dtr-files: no answer from {FILES_SERVICE_NAME} within {}s",
                TIMEOUT.as_secs()
            );
            ExitCode::FAILURE
        }
    }
}

/// The path to ask for, or why the argument cannot be sent.
///
/// Empty means "the person's home", which is the interface's own rule — saying it
/// here as well would be a second guess at the same question.
fn start_argument(argument: Option<OsString>) -> Result<String, String> {
    let Some(argument) = argument else {
        return Ok(String::new());
    };
    if argument.is_empty() {
        return Ok(String::new());
    }
    let path = std::path::PathBuf::from(&argument);
    if path.to_str().is_none() {
        return Err(format!(
            "{} is not valid UTF-8, and the interface carries a string path",
            path.to_string_lossy()
        ));
    }
    // `canonicalize` both makes it absolute against *this* process's directory
    // (the one the shell was in) and resolves a symlink, which is what the person
    // meant; a path that does not exist is sent as written and the window says so
    // with the browser's own error.
    let resolved = std::fs::canonicalize(&path).unwrap_or(path);
    Ok(resolved.to_string_lossy().into_owned())
}

/// Call `org.dtr.Files.OpenWindow` and return the handle.
async fn open_window(path: String) -> Result<String, String> {
    let connection = zbus::Connection::session().await.map_err(|err| {
        format!(
            "no session bus ({err}); dtr-files needs a running desktop session. \
             On a machine with one, is the dtr-portal package installed?"
        )
    })?;
    let proxy = zbus::Proxy::new(
        &connection,
        FILES_SERVICE_NAME,
        FILES_OBJECT_PATH,
        FILES_INTERFACE,
    )
    .await
    .map_err(|err| format!("could not build the {FILES_INTERFACE} proxy: {err}"))?;
    let options: HashMap<String, OwnedValue> = HashMap::new();
    let reply = proxy
        .call_method("OpenWindow", &(path, options))
        .await
        .map_err(|err| {
            format!(
                "could not open a manager window: {err}. \
                 Is dtr-portal installed, and does its org.dtr.Files.service point at it?"
            )
        })?;
    let (handle,): (String,) = reply
        .body()
        .deserialize()
        .map_err(|err| format!("the daemon answered a handle this launcher cannot read: {err}"))?;
    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_argument_asks_for_the_home() {
        assert_eq!(start_argument(None).expect("no path"), "");
        assert_eq!(start_argument(Some(OsString::new())).expect("empty"), "");
    }

    #[test]
    fn a_relative_path_is_made_absolute_before_it_crosses_the_bus() {
        let path = start_argument(Some(OsString::from("."))).expect("a path");
        assert!(
            std::path::Path::new(&path).is_absolute(),
            "the daemon's working directory is not this shell's: {path}"
        );
    }

    #[test]
    fn a_path_that_is_not_utf8_is_refused_rather_than_replaced() {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let bad = OsString::from_vec(b"/tmp/\xff".to_vec());
            let reason = start_argument(Some(bad)).expect_err("not UTF-8");
            assert!(reason.contains("UTF-8"), "{reason}");
        }
    }
}
