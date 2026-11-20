//! The manager window's **private** interface: `org.dtr.Files`.
//!
//! The file chooser on this connection is the desktop's, and its requests name a
//! *path* with no host and no way to open more than one dialog per call. The
//! manager window (`docs/manager.md` §6) is dtr's own window, so it gets its own
//! interface — served on the same bus connection and by the same process as
//! `org.freedesktop.impl.portal.FileChooser`, because the point of the daemon is
//! one warm wgpu device and one icon cache for both (`docs/portal.md` §9.8).
//!
//! ```text
//! org.dtr.Files                     at /org/dtr/Files
//!   OpenWindow  (s path, a{sv} options) -> (s handle)
//!   CloseWindow (s handle)             -> ()
//! ```
//!
//! **`path` is empty for "the person's home"**, so a launcher with no argument
//! says that rather than guessing in a different process. A `path` that names a
//! file opens its parent with the file selected, which is what the terminal's
//! `--files some/file` means. The interface is `s path` and not `ay` on purpose:
//! `dtr-files` is a *launcher*, its argument is the shell's `argv` and is UTF-8
//! by the time it arrives; the portal's `ay` current-folder carries bytes because
//! a *frontend* sends a path it read from the filesystem.
//!
//! ## The handle
//!
//! `OpenWindow` answers only once the window exists: the method posts the request
//! to the window loop and waits for the loop's answer, which is either the
//! `dtr-frame` window's creation or its failure (`no display`, `no adapter`). A
//! launcher therefore knows the window opened, and a future caller has something
//! to `CloseWindow` — a handle, not "the one window".
//!
//! A handle is an object path by shape (`/org/dtr/Files/window/3`) so that a
//! later `Close()` on the window can be an object method without changing the
//! wire; today it is only ever a string argument.
//!
//! ## Options
//!
//! `a{sv}` rather than positional arguments, because this is the one part of the
//! interface that will grow (`docs/manager.md` §6 names a second tab and a
//! splitter as later work):
//!
//! | key | type | meaning |
//! | --- | --- | --- |
//! | `title` | `s` | the window's caption; empty means `Files` |
//! | `parent_window` | `s` | an `x11:0x…` handle to be transient for |
//! | `modal` | `b` | whether that transiency is modal |
//!
//! Unknown keys are ignored — an interface may grow — and a known key with the
//! wrong type is `InvalidArgs`, because a caller that sends `title` as an integer
//! has a bug worth surfacing rather than a window with a default name.
//!
//! ## The seam, for the tests
//!
//! [`WindowOpener`] is the whole of what this module does with a window: the
//! daemon's implementation posts to the `dtr-frame` loop, and the private-bus
//! test's implementation records the request and answers through the same
//! [`OpenWindowAck`] the real loop does. So the interface's decode, its handle
//! bookkeeping, its activation/ownership and its error paths are all testable
//! without a display.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use zbus::zvariant::OwnedValue;

/// The name `dtr-files` calls and the activation service file owns.
pub const FILES_SERVICE_NAME: &str = "org.dtr.Files";

/// Where the private interface lives on that connection.
pub const FILES_OBJECT_PATH: &str = "/org/dtr/Files";

/// The interface the launcher calls.
pub const FILES_INTERFACE: &str = "org.dtr.Files";

/// The one-shot answer an `OpenWindow` method waits for.
///
/// `Ok` means the window exists (the loop created it); `Err` carries the reason
/// it does not (`no display`, `the window loop stopped`). One shot, like
/// [`crate::Reply`](crate::host::Reply): a second send is a bug, not a retry.
pub type OpenWindowAck = async_channel::Sender<Result<(), String>>;

/// Let a caller say which window went away, in a future that may outlive it.
pub type CloseWindowAck = async_channel::Sender<bool>;

/// What a caller asked for, as the window loop receives it.
///
/// This is a *post*, like [`Call`](crate::Call): the D-Bus task hands it to the
/// loop and returns; the method waits on [`OpenWindowAck`] until the loop says
/// whether the window exists.
pub struct OpenWindowRequest {
    /// The handle the method answers with once the window is up.
    pub handle: String,
    /// Where to start; `None` is the person's home.
    pub path: Option<PathBuf>,
    /// The caption the caller asked for, if any.
    pub title: Option<String>,
    /// An `x11:0x…` handle to be transient for, or empty.
    pub parent_window: String,
    /// Whether that transiency is modal.
    pub modal: bool,
    /// The loop's answer, once it has one.
    pub ack: OpenWindowAck,
}

impl std::fmt::Debug for OpenWindowRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenWindowRequest")
            .field("handle", &self.handle)
            .field("path", &self.path)
            .field("title", &self.title)
            .field("parent_window", &self.parent_window)
            .field("modal", &self.modal)
            .finish_non_exhaustive()
    }
}

impl OpenWindowRequest {
    /// The window exists: answer the waiting method.
    pub fn accept(&self) -> bool {
        self.ack.try_send(Ok(())).is_ok()
    }

    /// No window: say why. Returns whether the answer won the slot.
    pub fn refuse(&self, reason: impl Into<String>) -> bool {
        self.ack.try_send(Err(reason.into())).is_ok()
    }
}

/// What the window loop does with a request: nothing, at this seam.
///
/// The daemon's implementation posts to `dtr_frame`'s event loop; the tests'
/// answers at once. The trait is `Send` because it is held behind a mutex and
/// called on the D-Bus task, which is not the thread that owns a window.
pub trait WindowOpener: Send + 'static {
    /// Open a manager window, and eventually answer through the request's ack.
    fn open(&mut self, request: OpenWindowRequest);

    /// Close the window a handle names. `false` means nothing had that handle.
    fn close(&mut self, handle: String, ack: CloseWindowAck);
}

/// The interface, holding the opener behind the same mutex a `Host` lives behind.
///
/// A method call runs on zbus's task; `open` and `close` therefore only post and
/// the answer travels back through the ack channels from wherever the window
/// loop is. Neither method blocks the bus.
pub struct FilesService {
    opener: Mutex<Box<dyn WindowOpener>>,
}

impl FilesService {
    /// Build the interface over `opener`.
    pub fn new(opener: impl WindowOpener) -> Self {
        Self {
            opener: Mutex::new(Box::new(opener)),
        }
    }
}

/// Handles are unique numbers in an object-path shape: one counter for the
/// process, so two manager windows can never answer to one `CloseWindow`.
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

fn next_handle() -> String {
    let number = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    format!("{FILES_OBJECT_PATH}/window/{number}")
}

/// A `a{sv}` as the window options, strictly typed and leniently extended.
fn decode_options(options: HashMap<String, OwnedValue>) -> Result<WindowOptions, zbus::fdo::Error> {
    let mut decoded = WindowOptions::default();
    for (key, value) in options {
        match key.as_str() {
            "title" => {
                decoded.title =
                    Some(String::try_from(value).map_err(|_| invalid("title", "a string"))?);
            }
            "parent_window" => {
                decoded.parent_window =
                    String::try_from(value).map_err(|_| invalid("parent_window", "a string"))?;
            }
            "modal" => {
                decoded.modal = bool::try_from(value).map_err(|_| invalid("modal", "a boolean"))?;
            }
            // An unknown key is ignored: the interface may grow and a caller
            // built against a later one must still open a window.
            _ => tracing::debug!(option = %key, "ignoring an unknown Files.OpenWindow option"),
        }
    }
    Ok(decoded)
}

/// The decoded options, with "nothing was asked for" as the values.
#[derive(Debug, Default, PartialEq, Eq)]
struct WindowOptions {
    title: Option<String>,
    parent_window: String,
    modal: bool,
}

fn invalid(option: &str, wanted: &str) -> zbus::fdo::Error {
    zbus::fdo::Error::InvalidArgs(format!("the window option `{option}` is not {wanted}",))
}

#[zbus::interface(name = "org.dtr.Files")]
impl FilesService {
    /// Open a manager window, and answer with its handle once it exists.
    ///
    /// The answer is the loop's, not this method's: a window that could not be
    /// created turns into an `Err` here rather than a handle for nothing.
    async fn open_window(
        &self,
        path: String,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<String> {
        let options = decode_options(options)?;
        let handle = next_handle();
        let start = (!path.is_empty()).then(|| PathBuf::from(path));
        let (ack, answer) = async_channel::bounded(1);
        let request = OpenWindowRequest {
            handle: handle.clone(),
            path: start,
            title: options.title.filter(|title| !title.is_empty()),
            parent_window: options.parent_window,
            modal: options.modal,
            ack,
        };
        self.opener.lock().expect("the window opener").open(request);
        match answer.recv().await {
            Ok(Ok(())) => {
                tracing::debug!(%handle, "opened a manager window");
                Ok(handle)
            }
            Ok(Err(reason)) => {
                tracing::warn!(%handle, %reason, "no manager window could be opened");
                Err(zbus::fdo::Error::Failed(reason))
            }
            // The ack's sender dropped without answering: the window loop is
            // gone. A request with nowhere to go is an error, never a hang.
            Err(_) => Err(zbus::fdo::Error::Failed(
                "the window loop stopped before the window was opened".to_owned(),
            )),
        }
    }

    /// Close the manager window a handle names.
    ///
    /// Idempotent from the caller's side is not the same as silent: a handle
    /// nothing knows is `InvalidArgs`, because a launcher that closed the wrong
    /// thing should hear about it.
    async fn close_window(&self, handle: String) -> zbus::fdo::Result<()> {
        let (ack, answer) = async_channel::bounded(1);
        self.opener
            .lock()
            .expect("the window opener")
            .close(handle.clone(), ack);
        match answer.recv().await {
            Ok(true) => Ok(()),
            Ok(false) => Err(zbus::fdo::Error::InvalidArgs(format!(
                "no manager window has the handle `{handle}`",
            ))),
            Err(_) => Err(zbus::fdo::Error::Failed(
                "the window loop stopped before the window was closed".to_owned(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Value;

    fn a_string(value: &str) -> OwnedValue {
        OwnedValue::try_from(Value::from(value.to_owned())).expect("a string")
    }

    fn a_bool(value: bool) -> OwnedValue {
        OwnedValue::try_from(Value::from(value)).expect("a boolean")
    }

    #[test]
    fn the_options_are_typed_and_unknown_ones_are_ignored() {
        let mut options = HashMap::new();
        options.insert("title".to_owned(), a_string("Files — /tmp"));
        options.insert("parent_window".to_owned(), a_string("x11:0x1234"));
        options.insert("modal".to_owned(), a_bool(true));
        options.insert("a_later_option".to_owned(), a_string("ignored"));
        assert_eq!(
            decode_options(options).expect("decoded"),
            WindowOptions {
                title: Some("Files — /tmp".to_owned()),
                parent_window: "x11:0x1234".to_owned(),
                modal: true,
            }
        );
        assert_eq!(
            decode_options(HashMap::new()).expect("decoded"),
            WindowOptions::default()
        );
    }

    #[test]
    fn a_known_option_with_the_wrong_type_is_refused() {
        let mut options = HashMap::new();
        options.insert("title".to_owned(), a_bool(true));
        assert!(decode_options(options).is_err());
        let mut options = HashMap::new();
        options.insert("modal".to_owned(), a_string("yes"));
        assert!(decode_options(options).is_err());
        let mut options = HashMap::new();
        options.insert("parent_window".to_owned(), a_bool(false));
        assert!(decode_options(options).is_err());
    }

    #[test]
    fn handles_are_unique_and_shaped_like_object_paths() {
        let first = next_handle();
        let second = next_handle();
        assert_ne!(first, second);
        assert!(first.starts_with(FILES_OBJECT_PATH));
        assert!(
            first
                .rsplit('/')
                .next()
                .is_some_and(|n| n.parse::<u64>().is_ok())
        );
    }
}
