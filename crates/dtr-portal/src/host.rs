//! Who shows the picker, and how the person's answer gets back.
//!
//! The portal's job ends at "somebody must ask a person"; everything after that
//! is the [`Host`]. [`crate::window::WindowHost`] is the real one, the
//! private-bus tests have a headless double, and the binary's no-display path
//! has a stub that answers cancel; the trait is shaped so that a host is a small
//! thing:
//!
//! - **The request is the browser's own.** [`Call::request`] is a
//!   [`Request`](dtr_files::pick::Request) — the same value F2's picker draws —
//!   so the window builds a `Mode::Pick(Pick::new(call.request))` and nothing has
//!   to be translated a second time.
//! - **The initial filter rides beside it**, because [`Request`] has no field
//!   for "the filter to start on": `current_filter` is a portal addition, and it
//!   is an index into `request.filters`, the one list the window shows.
//! - **The answer is a [`Reply`]**, not a callback into the connection: the host
//!   may answer from another thread, and answering twice is impossible — the
//!   reply has one slot and the second send returns `false`.
//! - **`Close` is a message of its own.** The frontend ends the interaction by
//!   calling `Request.Close()`; the window has to be taken down with it, so
//!   [`Host::close`] is told, and the request future answers cancel without the
//!   host having to know the bus.
//!
//! A host is held behind a mutex and called on the D-Bus task, which is why
//! [`Host::open`] must not block: it posts to whatever owns the window and
//! returns. The answer comes back through the [`Reply`] whenever the person has
//! made up their mind, which may be minutes later and on another thread.

use std::fmt;
use std::path::PathBuf;

use dtr_files::pick::Request;

/// The request object's path, as the frontend and the host both know it.
///
/// The path is what `Request.Close()` is called on, and the host needs it to
/// match a cancel to the picker it belongs to — more than one request can be
/// pending (two applications can ask at once), so an id is not optional.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(String);

impl RequestId {
    pub(crate) fn new(path: impl Into<String>) -> Self {
        Self(path.into())
    }

    /// The D-Bus object path the request lives at.
    pub fn path(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Everything about the asking application that the window needs.
///
/// This is not decoration: `title` is the dialog's caption, `modal` and
/// `parent_window` are what make it transient for the application's own window
/// (an `x11:0x…` handle can be used as such; a Wayland one is opaque and winit
/// exposes no xdg-foreign, so there the window is not transient — a limitation
/// worth writing down rather than pretending a flag fixed, `docs/manager.md`
/// §5).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowContext {
    /// The application's id (`org.example.App`), for the window's own title bar
    /// or an "opened by" note.
    pub app_id: String,
    /// The frontend's window handle (`x11:0x…`).
    pub parent_window: String,
    /// The title the application asked the dialog to carry.
    pub title: String,
    /// Whether the application asked for a modal dialog. Default true.
    pub modal: bool,
}

/// A request, handed to a [`Host`] together with the way to answer it.
pub struct Call {
    /// The request object's path: `Close` is called on it.
    pub id: RequestId,
    /// The question, as the browser's picker understands it.
    pub request: Request,
    /// The `current_filter` option, as an index into `request.filters`.
    ///
    /// `None` means the application named no default and index 0 is as good as
    /// any. The window is expected to start its filter control here.
    pub filter: Option<usize>,
    /// What the window needs to know about the asking application.
    pub window: WindowContext,
    /// The one place an answer goes.
    pub reply: Reply,
}

impl Call {
    /// Answer with a choice. Consumes nothing, so a host that keeps the `Call`
    /// for a retry can still use it, and the second answer is refused.
    pub fn chosen(&self, chosen: Chosen) -> bool {
        self.reply.send(Answer::Chosen(chosen))
    }

    /// Answer with a cancel, which is also what a window that the person closed
    /// without choosing should do.
    pub fn cancel(&self) -> bool {
        self.reply.send(Answer::Cancelled)
    }
}

impl fmt::Debug for Call {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Call")
            .field("id", &self.id)
            .field("request", &self.request)
            .field("filter", &self.filter)
            .field("window", &self.window)
            .finish_non_exhaustive()
    }
}

/// The paths a person chose, and the rest of what the answer carries.
///
/// `writable` is deliberately not here: whether the application gets write
/// access is a property of the *question*, not of the person's answer — an open
/// is read-only because this picker has no write toggle, and a save is writable
/// because the application has to write the file. Letting a host set it would
/// make two hosts able to disagree about the same method.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chosen {
    /// The answer, in listing order. For a save that is one path; for a folder
    /// picker it is the folder on screen; for `SaveFiles` it is the folder the
    /// names are appended to.
    pub paths: Vec<PathBuf>,
    /// The filter the person left the picker on, as an index into
    /// `request.filters`; echoed back as `current_filter`.
    pub filter: Option<usize>,
}

/// What the picker decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// Response `0`.
    Chosen(Chosen),
    /// Response `1` — the person cancelled, or there was no window to ask.
    Cancelled,
}

/// The one-slot channel an answer travels on.
///
/// A `Reply` can be cloned and kept by a window thread, and dropping the last
/// clone without answering tells the portal the picker went away: it answers
/// cancel rather than leaving the application waiting forever.
#[derive(Clone)]
pub struct Reply {
    tx: async_channel::Sender<Answer>,
}

impl Reply {
    pub(crate) fn new(tx: async_channel::Sender<Answer>) -> Self {
        Self { tx }
    }

    /// Put the answer in the slot. `false` means it was already answered — or
    /// that the request was closed first — and is worth logging, not retrying.
    pub fn send(&self, answer: Answer) -> bool {
        self.tx.try_send(answer).is_ok()
    }
}

impl fmt::Debug for Reply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Reply { .. }")
    }
}

/// The thing that shows a request to a person.
///
/// Implementations are held behind a mutex and called from the D-Bus task:
/// [`open`](Host::open) and [`close`](Host::close) must not block, and the
/// answer travels through [`Call::reply`] from wherever the person is.
pub trait Host: Send + 'static {
    /// A new request. Post it to the thread that owns the window and return.
    fn open(&mut self, call: Call);

    /// The frontend ended the interaction (`Request.Close()`), or the portal is
    /// shutting down: take the window down if it is up. The request's `Reply`
    /// has already been answered with a cancel by the time this is called, so a
    /// host does not have to answer again — and a host that does will find the
    /// slot gone.
    fn close(&mut self, id: RequestId) {
        let _ = id;
    }
}
