//! The `org.freedesktop.impl.portal.FileChooser` service, and the request object
//! that lets the frontend cancel it.
//!
//! The interface is the one installed by `xdg-desktop-portal` 1.22.1
//! (`$datadir/dbus-1/interfaces/org.freedesktop.impl.portal.FileChooser.xml`),
//! not a remembered one, and the details that would be guesses are pinned by the
//! private-bus test in `tests/private_bus.rs`:
//!
//! ```text
//! OpenFile  (o handle, s app_id, s parent_window, s title, a{sv} options)
//! SaveFile  (…same…)
//! SaveFiles (…same…)          — 1.22.1 keeps the file list in `options`, not an argument
//!   → (u response, a{sv} results)
//! org.freedesktop.impl.portal.Request at `handle`: Close ()
//! ```
//!
//! `response` 0 is a choice and 1 is a cancel; `results` carries `uris` as
//! `file://` URIs (percent-encoded from **bytes**, [`crate::file_uri`]),
//! `current_filter` as the application sent it, and `writable`. The last one is
//! sent for every method because GTK's backend does, and it says what the
//! *question* means: an open is read-only (this picker has no write toggle) and a
//! save is writable (the application has to write the file).
//!
//! ## Lifecycle
//!
//! A method call is an `async fn`, so the reply waits for the person while the
//! connection carries on: zbus spawns a task per method call, which is also what
//! lets `Request.Close()` run while the call is pending.
//!
//! 1. The options are decoded before anything is exported; a malformed request
//!    gets `InvalidArgs`, not a request object nobody will ever answer.
//! 2. The request object is exported at the frontend's `handle` *before* the
//!    host is told, because the frontend may call `Close()` the instant the
//!    method was sent, and an unseen close would be an unknown object.
//! 3. The host is handed the [`Call`] and the method future waits on the answer
//!    and the close signal, whichever comes first. A host that drops its
//!    `Call` without answering wakes it too, as a cancel: "never hang" is worth
//!    more than "blame the window".
//! 4. One answer exactly: the reply channel has one slot and `Reply::send`
//!    reports whether it won. The request object is unexported before the reply
//!    leaves, so `Close()` after the answer is `UnknownObject`, which is the
//!    truth — the interaction is over.

use std::collections::HashMap;
use std::future::Future;
use std::pin::pin;
use std::sync::{Arc, Mutex};
use std::task::Poll;

use zbus::fdo;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::host::{Answer, Call, Host, Reply, RequestId, WindowContext};
use crate::options::{self, Decoded, Kind};
use crate::{paths, uri};

/// The bus name a backend owns. The frontend's `dtr.portal` file names it, and
/// the activation `.service` file must own exactly this one.
pub const SERVICE_NAME: &str = "org.freedesktop.impl.portal.desktop.dtr";

/// Where `xdg-desktop-portal` looks for a backend interface.
pub const OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";

/// The backend half of the file chooser.
pub const INTERFACE: &str = "org.freedesktop.impl.portal.FileChooser";

/// The request object the frontend calls `Close()` on.
pub const REQUEST_INTERFACE: &str = "org.freedesktop.impl.portal.Request";

/// The service: a host, and the bus connection that carries its answers.
pub struct Portal {
    state: Arc<State>,
}

impl Portal {
    /// Build a portal that hands requests to `host`.
    pub fn new(host: impl Host) -> Self {
        Self {
            state: Arc::new(State {
                host: Mutex::new(Box::new(host)),
            }),
        }
    }

    /// Serve on the session bus under [`SERVICE_NAME`], and return the
    /// connection so the caller can keep it (the daemon does; the test keeps it
    /// alive on its thread).
    pub async fn serve(self) -> zbus::Result<zbus::Connection> {
        let builder = zbus::connection::Builder::session()?;
        self.build(builder).await
    }

    /// Serve on a bus at `address` instead of the session's.
    ///
    /// This exists for the test that runs its own `dbus-daemon`, the same seam
    /// `notify.rs` has: a test must not depend on, or disturb, the desktop's own
    /// bus.
    pub async fn serve_at(self, address: &str) -> zbus::Result<zbus::Connection> {
        let builder = zbus::connection::Builder::address(address)?;
        self.build(builder).await
    }

    async fn build<'a>(
        self,
        builder: zbus::connection::Builder<'a>,
    ) -> zbus::Result<zbus::Connection> {
        let connection = builder
            .name(SERVICE_NAME)?
            .serve_at(OBJECT_PATH, self)?
            .build()
            .await?;
        tracing::info!(
            service = SERVICE_NAME,
            path = OBJECT_PATH,
            interface = INTERFACE,
            "serving the file chooser portal"
        );
        Ok(connection)
    }

    /// The one method all three names call.
    #[allow(clippy::too_many_arguments)]
    async fn request(
        &self,
        server: &zbus::ObjectServer,
        kind: Kind,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        let decoded = options::decode(kind, options).map_err(|err| {
            tracing::warn!(method = %kind, %err, "refusing a malformed file chooser request");
            fdo::Error::InvalidArgs(err.to_string())
        })?;

        // `SaveFiles` with no names has nothing to append to a folder; GTK would
        // answer with an empty `uris` array, which no caller can use. A cancel is
        // the honest answer.
        if kind == Kind::SaveFiles && decoded.options.files.is_empty() {
            tracing::warn!(
                method = %kind,
                request = handle.as_str(),
                "SaveFiles named no files; answering cancel"
            );
            return Ok((1, HashMap::new()));
        }

        let id = RequestId::new(handle.as_str());
        let (answer_tx, answer_rx) = async_channel::bounded::<Answer>(1);
        let (close_tx, close_rx) = async_channel::bounded::<()>(1);

        let exported = server
            .at(handle.as_str(), RequestObject { close: close_tx })
            .await
            .map_err(fdo::Error::ZBus)?;
        if !exported {
            // The frontend generated a path that is already a request. That is a
            // bug on its side; answering with an error is better than silently
            // attaching a second picker to one Close.
            return Err(fdo::Error::InvalidArgs(format!(
                "{id} is already a request object"
            )));
        }

        let call = Call {
            id: id.clone(),
            request: decoded.request.clone(),
            filter: decoded.filter,
            window: WindowContext {
                app_id,
                parent_window,
                title,
                modal: decoded.options.modal,
            },
            reply: Reply::new(answer_tx),
        };
        tracing::debug!(
            method = %kind,
            request = %id,
            select = ?call.request.select,
            "asking the host for a picker"
        );
        self.state.open(call);

        let answer = match wait(answer_rx, close_rx).await {
            Wake::Answered(answer) => answer,
            Wake::Closed => {
                tracing::debug!(request = %id, "the frontend closed the request");
                self.state.close(id.clone());
                Answer::Cancelled
            }
            Wake::Dropped => {
                tracing::warn!(
                    request = %id,
                    "the host dropped a pending request; answering cancel rather than waiting"
                );
                self.state.close(id.clone());
                Answer::Cancelled
            }
        };

        // Before the reply: a `Close()` after this point must be told the
        // interaction is over, and an exported object that still answers would
        // suggest it is not.
        if let Err(err) = server.remove::<RequestObject, _>(handle.as_str()).await {
            tracing::debug!(request = %id, %err, "could not unexport the request object");
        }

        let (response, results) = completion(kind, &decoded, answer);
        tracing::debug!(
            method = %kind,
            request = %id,
            response,
            uris = results.contains_key("uris"),
            "answered the file chooser request"
        );
        Ok((response, results))
    }
}

/// The behaviour behind [`Portal::request`], as the three D-Bus names.
#[zbus::interface(name = "org.freedesktop.impl.portal.FileChooser")]
impl Portal {
    /// Open one or more files.
    #[zbus(out_args("response", "results"))]
    #[allow(clippy::too_many_arguments)]
    async fn open_file(
        &self,
        #[zbus(object_server)] server: &zbus::ObjectServer,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        self.request(
            server,
            Kind::OpenFile,
            handle,
            app_id,
            parent_window,
            title,
            options,
        )
        .await
    }

    /// Save one file.
    #[zbus(out_args("response", "results"))]
    #[allow(clippy::too_many_arguments)]
    async fn save_file(
        &self,
        #[zbus(object_server)] server: &zbus::ObjectServer,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        self.request(
            server,
            Kind::SaveFile,
            handle,
            app_id,
            parent_window,
            title,
            options,
        )
        .await
    }

    /// Save several files into one folder.
    #[zbus(out_args("response", "results"))]
    #[allow(clippy::too_many_arguments)]
    async fn save_files(
        &self,
        #[zbus(object_server)] server: &zbus::ObjectServer,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        self.request(
            server,
            Kind::SaveFiles,
            handle,
            app_id,
            parent_window,
            title,
            options,
        )
        .await
    }
}

/// The per-request object exported at `handle`.
///
/// One method, and it only signals: the request future owns the answer, so a
/// `Close` racing a window's own answer is resolved by whoever reached the slot
/// first, and this object never has to know which.
struct RequestObject {
    close: async_channel::Sender<()>,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Request")]
impl RequestObject {
    /// The frontend ends the interaction; the dialog should go away.
    fn close(&self) {
        // `try_send` rather than a blocking send: a second close is not an error,
        // and the bus task must never wait for the request task.
        let _ = self.close.try_send(());
    }
}

/// What woke the method future.
enum Wake {
    /// The host answered.
    Answered(Answer),
    /// The frontend called `Close()`.
    Closed,
    /// Every `Reply` was dropped without an answer: the host went away.
    Dropped,
}

/// Wait for an answer or a close, whichever comes first.
async fn wait(
    answers: async_channel::Receiver<Answer>,
    closes: async_channel::Receiver<()>,
) -> Wake {
    let mut answers = pin!(answers.recv());
    let mut closes = pin!(closes.recv());
    std::future::poll_fn(move |cx| {
        // The answer is polled first, so an answer and a close that are both
        // ready resolve in favour of the answer: the person decided before the
        // frontend gave up.
        if let Poll::Ready(result) = answers.as_mut().poll(cx) {
            return Poll::Ready(match result {
                Ok(answer) => Wake::Answered(answer),
                Err(_) => Wake::Dropped,
            });
        }
        if closes.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Wake::Closed);
        }
        Poll::Pending
    })
    .await
}

/// The `(response, results)` pair for an answer.
///
/// Cancel is response `1` and an empty vardict. (GTK's backend answers `2` for a
/// `Close`; the interface documents 0 and 1, the task is that a cancel answers
/// cancel, and the frontend treats 0 as a choice and everything else as "not
/// chosen" — 1 is the answer that cannot be mistaken for a bug.)
fn completion(kind: Kind, decoded: &Decoded, answer: Answer) -> (u32, HashMap<String, OwnedValue>) {
    let Answer::Chosen(chosen) = answer else {
        return (1, HashMap::new());
    };
    let paths = match kind {
        Kind::SaveFiles => save_files_paths(&decoded.options.files, &chosen.paths),
        _ => chosen.paths,
    };
    let uris: Vec<String> = paths
        .iter()
        .filter_map(|path| uri::file_uri(path))
        .collect();
    if uris.is_empty() {
        tracing::warn!(
            method = %kind,
            "the picker chose no path that can be a file:// URI; answering cancel"
        );
        return (1, HashMap::new());
    }
    let mut results = HashMap::new();
    results.insert("uris".to_owned(), owned(Value::from(uris)));
    // A picker that opens read-only and a save that the application has to
    // write: `writable` is the question's property, not the host's choice.
    let writable = matches!(kind, Kind::SaveFile | Kind::SaveFiles);
    results.insert("writable".to_owned(), owned(Value::from(writable)));
    if let Some(filter) = decoded.selected_filter(chosen.filter) {
        results.insert(
            "current_filter".to_owned(),
            owned(Value::from((filter.name.clone(), filter.patterns.clone()))),
        );
    }
    (0, results)
}

/// The folder a `SaveFiles` answer chose, with each application-supplied name
/// appended — and made unique first, because the application is about to write
/// these names and a portal that handed back an existing one would have it
/// overwritten without asking.
fn save_files_paths(
    names: &[std::path::PathBuf],
    chosen: &[std::path::PathBuf],
) -> Vec<std::path::PathBuf> {
    let Some(folder) = chosen.first() else {
        return Vec::new();
    };
    names
        .iter()
        .map(|name| paths::unique_path(&folder.join(name)))
        .collect()
}

/// A value for the results vardict.
///
/// `OwnedValue::try_from` fails only for a file descriptor when the process is
/// out of them; everything built here is text and booleans, so a failure would
/// be a programming error rather than a condition to answer.
fn owned(value: Value<'static>) -> OwnedValue {
    OwnedValue::try_from(value).expect("a results value that is not a file descriptor")
}

/// The host and the pending-request bookkeeping, shared with the request object.
struct State {
    host: Mutex<Box<dyn Host>>,
}

impl State {
    /// Hand a request to the host, on the D-Bus task.
    ///
    /// The lock is held while `open` runs, which is safe *because* `Host::open`
    /// is required not to block: it posts the request to whatever owns the
    /// window and returns. A host that waited here would stall every other
    /// method on the connection.
    fn open(&self, call: Call) {
        let mut host = match self.host.lock() {
            Ok(host) => host,
            // A host that panicked took a request with it; the lock is still
            // usable and the next request must not be lost because of it.
            Err(poisoned) => poisoned.into_inner(),
        };
        host.open(call);
    }

    /// Tell the host to take the window down.
    fn close(&self, id: RequestId) {
        let mut host = match self.host.lock() {
            Ok(host) => host,
            Err(poisoned) => poisoned.into_inner(),
        };
        host.close(id);
    }
}

// A host that never learns about a cancel would leave a window up for a request
// nobody is waiting for; `Call::request` carries the rest, but the pieces below
// are here so this module's tests can build a request without a bus.
#[cfg(test)]
mod tests {
    use super::*;
    use dtr_files::pick::{Filter, Request, Select};
    use std::path::PathBuf;

    fn decoded(kind: Kind, filter: Option<usize>) -> Decoded {
        let mut options = options::Options {
            filters: vec![options::PortalFilter::globs("Images", ["*.png".to_owned()])],
            ..options::Options::default()
        };
        if filter.is_some() {
            options.current_filter = options.filters.first().cloned();
        }
        options.into_request(kind)
    }

    fn uris(results: &mut HashMap<String, OwnedValue>) -> Vec<String> {
        let value = results.remove("uris").expect("the uris result");
        Vec::<String>::try_from(value).expect("an array of strings")
    }

    #[test]
    fn a_chosen_answer_is_response_zero_with_percent_encoded_uris() {
        let decoded = decoded(Kind::OpenFile, None);
        let chosen = crate::host::Chosen {
            paths: vec![PathBuf::from("/tmp/a b+c#d.png")],
            filter: Some(0),
        };
        let (response, mut results) = completion(Kind::OpenFile, &decoded, Answer::Chosen(chosen));
        assert_eq!(response, 0);
        assert_eq!(
            uris(&mut results),
            vec!["file:///tmp/a%20b%2Bc%23d.png".to_owned()]
        );
        assert_eq!(
            bool::try_from(results.remove("writable").unwrap()).unwrap(),
            false
        );
        let filter = results.remove("current_filter").expect("the filter");
        let (name, patterns) =
            <(String, Vec<(u32, String)>)>::try_from(filter).expect("a serialized filter");
        assert_eq!(name, "Images");
        assert_eq!(patterns, vec![(0u32, "*.png".to_owned())]);
    }

    #[test]
    fn a_cancel_is_response_one_and_no_results() {
        let decoded = decoded(Kind::OpenFile, None);
        let (response, results) = completion(Kind::OpenFile, &decoded, Answer::Cancelled);
        assert_eq!(response, 1);
        assert!(results.is_empty());
    }

    /// A picker that reports a filter it does not have is answered without a
    /// `current_filter` rather than with a wrong one.
    #[test]
    fn an_unknown_filter_index_is_omitted_from_the_results() {
        let decoded = decoded(Kind::OpenFile, Some(0));
        let chosen = crate::host::Chosen {
            paths: vec![PathBuf::from("/tmp/photo.png")],
            filter: Some(7),
        };
        let (response, mut results) = completion(Kind::OpenFile, &decoded, Answer::Chosen(chosen));
        assert_eq!(response, 0);
        assert!(!results.contains_key("current_filter"));
        assert_eq!(uris(&mut results), vec!["file:///tmp/photo.png".to_owned()]);
    }

    #[test]
    fn a_relative_path_cannot_be_a_file_uri_and_so_the_answer_is_a_cancel() {
        let decoded = decoded(Kind::OpenFile, None);
        let chosen = crate::host::Chosen {
            paths: vec![PathBuf::from("photo.png")],
            filter: None,
        };
        let (response, results) = completion(Kind::OpenFile, &decoded, Answer::Chosen(chosen));
        assert_eq!(response, 1);
        assert!(results.is_empty());
    }

    /// `SaveFiles` appends the application's names to the chosen folder, and a
    /// name that is already there is made unique rather than overwritten.
    #[test]
    fn save_files_appends_the_names_to_the_chosen_folder() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        std::fs::write(dir.path().join("taken.txt"), b"here first").expect("write");
        let options = options::Options {
            files: vec![PathBuf::from("taken.txt"), PathBuf::from("new.txt")],
            ..options::Options::default()
        };
        let decoded = options.into_request(Kind::SaveFiles);
        let chosen = crate::host::Chosen {
            paths: vec![dir.path().to_path_buf()],
            filter: None,
        };
        let (response, mut results) = completion(Kind::SaveFiles, &decoded, Answer::Chosen(chosen));
        assert_eq!(response, 0);
        let uris = uris(&mut results);
        assert!(
            uris[0].ends_with("/taken%281%29.txt"),
            "the existing name is not overwritten (and the parentheses are encoded): {uris:?}"
        );
        assert!(uris[1].ends_with("/new.txt"), "{uris:?}");
        assert_eq!(
            bool::try_from(results.remove("writable").unwrap()).unwrap(),
            true
        );
    }

    /// The browser's request is untouched by the answer path: a folder picker
    /// still asks for a folder.
    #[test]
    fn the_three_kinds_ask_three_different_questions() {
        let open = decoded(Kind::OpenFile, None);
        assert_eq!(open.request.select, Select::File);
        let save = options::Options::default().into_request(Kind::SaveFile);
        assert_eq!(save.request.select, Select::Save);
        let many = options::Options::default().into_request(Kind::SaveFiles);
        assert_eq!(many.request.select, Select::Directory);
        // And the filter list is the picker's, not the portal's.
        assert_eq!(
            open.request.filters,
            vec![Filter::new("Images", ["*.png".to_owned()])]
        );
        let _: Request = open.request;
    }
}
