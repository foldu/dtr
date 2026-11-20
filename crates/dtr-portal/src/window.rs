//! The daemon's windows: one warm picker, and a manager window per request.
//!
//! `docs/manager.md` §5 asks for a daemon, not a process per dialog: "D-Bus
//! activated, one window per request, kept hidden between requests, idle-exit
//! after a timeout when it owns no visible window. The first picker pays for a
//! process; every later one is a frame." F4 puts the manager window in the same
//! process: `dtr-files [PATH]` calls `org.dtr.Files.OpenWindow` and this module
//! opens a window on the same `dtr-frame` loop, beside any picker.
//!
//! # Why the window loop owns the process's main thread
//!
//! winit refuses to create its `EventLoop` off the main thread (0.30 panics with
//! "Initializing the event loop outside of the main thread…"; the X11/Wayland
//! `any_thread` extensions exist but are exactly the "nasty surprises when
//! porting" the panic is there to prevent). So the halves are the other way
//! round from the obvious shape: **the window loop is `main`'s work and the
//! D-Bus connection is the thread that moves.** `main` builds the loop, hands
//! the D-Bus side a [`WindowHost`] and a [`ManagerHost`], and runs the loop; the
//! process lives exactly as long as the loop does. That is also why the hosts
//! are `send_event`: the bus task posts into winit's queue and returns.
//!
//! # Per window, a screen
//!
//! [`Windows`](dtr_frame::Windows) is a `Vec<Frame>` over one loop, with one
//! `egui::Context` per window; this app keeps a `HashMap<WindowId, Screen>` of
//! what each *visible* window is:
//!
//! - **Picker(..)** — the F2 browser answering a portal request. There is at
//!   most one, because a portal request is a modal question and `Desk` queues the
//!   rest; its window is created once, hidden, and reused.
//! - **Manager(..)** — a `dtr-files` window: `Manager` in `src/manager.rs`, with
//!   its own browser, history and sidebar. Several may be open at once, each a
//!   `Frame` of its own.
//!
//! A hidden picker window is *not* in the map: the map is the set of windows a
//! person can see, which is what the idle policy asks about. The manager windows
//! are created visible and closed with their own close button; the picker is
//! created hidden at startup (`resumed`) and only mapped when a request arrives.
//!
//! # The state machine, without a window
//!
//! [`Desk`] is the picker's queue — what is waiting, what is showing, and when the
//! window last did anything — and its unit tests build `Call`s with a real
//! `Reply` and no event loop. The manager's model (`src/manager.rs`) is window-free
//! for the same reason: its history, its answer handling and its sidebar are
//! unit-tested without a display.
//!
//! # Verification hook
//!
//! `DTR_PORTAL_CAPTURE=/path.png` makes the **first window this process shows** —
//! picker or manager — photograph its own surface (`dtr_frame`'s `COPY_SRC`
//! capture, the same path `dtr --capture` uses) `DTR_PORTAL_CAPTURE_AFTER_MS`
//! after it appears, default 700 ms. That is the only picture of the *window*: the
//! offscreen path draws a frame from injected state, and what is being checked
//! here is that Xvfb's window, the surface, the skin, the sidebar and the listing
//! agree. A capture of a manager window is what F4's gate asks for; a picker run
//! captures a picker as before, because managers are not pickers.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use dtr_files::manager::{Band, ManagerLayout};
use dtr_files::{Browser, Key, Outcome, PaneOutcome, Select, SortChange};
use dtr_frame::{Deadlines, Frame, FrameLoop, RenderOptions, Scene, WindowOptions, Windows};
use dtr_pane_host::{Gestures, Scope, pane_key};
use dtr_widgets::Skin;
use winit::event::{ElementState, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::keyboard::{Key as WinitKey, NamedKey};
use winit::window::WindowId;

use crate::desktop::Desktop;
use crate::files::{self, CloseWindowAck, OpenWindowRequest};
use crate::host::{Answer, Call, Chosen, Host, RequestId};
use crate::manager::Manager;
use crate::worker::{Worker, WorkerEvent};
use crate::x11;

/// How often a visible window asks its directory whether it changed — the same
/// twice-a-second `stat` dtr's file panes do (`Browser::refresh_if_changed`).
const REFRESH: Duration = Duration::from_millis(500);

/// How long the process stays warm with no window on screen before it exits, so
/// that D-Bus activation starts it fresh for the next request.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// How long after a window appears the capture hook photographs it.
const CAPTURE_AFTER: Duration = Duration::from_millis(700);

/// The manager window's requested size, in points.
///
/// Above `manager::MIN_WIDTH`/`MIN_HEIGHT` so the toolbar, the 196-point sidebar
/// and a grid all fit without eliding; a person resizes from here.
const MANAGER_SIZE: (f32, f32) = (1000.0, 700.0);

/// What the windows are asked for at startup.
///
/// The defaults are the production ones; [`Options::from_env`] is the one place
/// an environment variable changes them, so a capture run and an idle-timeout
/// test are the same binary with different surroundings.
#[derive(Clone, Debug)]
pub struct Options {
    /// The window stays warm for this long after the last one closes. Zero keeps
    /// it warm for ever, which is what a long verification run wants.
    pub idle: Duration,
    /// Write a PNG of the first shown window here (`DTR_PORTAL_CAPTURE`).
    pub capture: Option<PathBuf>,
    /// How long after that window appears to take the picture.
    pub capture_after: Duration,
    /// Adwaita's dark scheme or its light one.
    pub dark: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            idle: IDLE_TIMEOUT,
            capture: None,
            capture_after: CAPTURE_AFTER,
            dark: true,
        }
    }
}

impl Options {
    /// The options this machine's environment asks for.
    ///
    /// These are hooks rather than preferences: a daemon has no settings file
    /// (`docs/portal.md`), and the two that exist are for seeing a window and for
    /// keeping a verification run's daemon alive.
    pub fn from_env() -> Self {
        let mut options = Self::default();
        if let Some(ms) = number("DTR_PORTAL_IDLE_TIMEOUT_MS") {
            options.idle = Duration::from_millis(ms);
        }
        if let Some(ms) = number("DTR_PORTAL_CAPTURE_AFTER_MS") {
            options.capture_after = Duration::from_millis(ms);
        }
        if let Some(path) = std::env::var_os("DTR_PORTAL_CAPTURE") {
            options.capture = Some(PathBuf::from(path));
        }
        if let Ok(scheme) = std::env::var("DTR_PORTAL_SCHEME") {
            match scheme.trim().to_ascii_lowercase().as_str() {
                "light" => options.dark = false,
                "dark" => options.dark = true,
                other => tracing::warn!(
                    scheme = other,
                    "DTR_PORTAL_SCHEME is `light` or `dark`; keeping the default"
                ),
            }
        }
        options
    }

    /// The chrome the windows are painted in.
    ///
    /// **A documented fallback, not the last word.** dtr's `native` asks the
    /// desktop for its own palette through `crates/dtr/src/system`, which is the
    /// terminal's crate and needs a running desktop; this process has neither the
    /// crate nor a promise of a portal that answers. Adwaita is the right guess
    /// for a Linux desktop — it is what GNOME's own backend draws — and the scheme
    /// is the one thing a machine might disagree about, so `DTR_PORTAL_SCHEME`
    /// settles it until the platform reader moves into a crate both frontends can
    /// use (the same answer the picker needs, `docs/portal.md` §8).
    pub fn skin(&self) -> Skin {
        dtr_widgets::Skin::adwaita(self.dark, dtr_widgets::ADWAITA_ACCENT)
    }
}

fn number(name: &str) -> Option<u64> {
    let value = std::env::var(name).ok()?;
    match value.trim().parse::<u64>() {
        Ok(number) => Some(number),
        Err(err) => {
            tracing::warn!(%name, %value, %err, "not a number; ignoring");
            None
        }
    }
}

/// A message from the D-Bus task (or the file worker) to the window loop.
///
/// Public because it is the loop's event type (`FrameLoop<Event>`); nothing
/// outside this module sends one, which is what [`WindowHost`] and
/// [`ManagerHost`] are for.
#[derive(Debug)]
pub enum Event {
    /// Show this picker request.
    Open(Call),
    /// The frontend called `Request.Close()`: take the picker down if it is up.
    Close(RequestId),
    /// `org.dtr.Files.OpenWindow`: open a manager window.
    OpenManager(OpenWindowRequest),
    /// `org.dtr.Files.CloseWindow`: close the manager window a handle names, and
    /// answer whether anything had that handle.
    CloseManager { handle: String, ack: CloseWindowAck },
    /// The file worker finished a job for a manager window.
    Worker(WorkerEvent),
}

/// What the window loop tells the D-Bus side about the picker window it may show.
#[derive(Clone, Debug)]
enum Status {
    /// The picker window exists and is hidden: requests can be posted.
    Ready,
    /// No window could be created — no display, no adapter. Every request will be
    /// answered cancel with this reason in the log, and the process will exit at
    /// the idle timeout for activation to try again later.
    Unavailable(String),
}

/// The picker [`Host`]: a proxy to the window loop, and what that loop has said
/// about itself.
///
/// `Host::open` runs on the D-Bus task under the portal's mutex, so everything
/// here is a non-blocking `send_event`/`try_recv`; the answer travels back through
/// the [`Call`]'s own [`Reply`](crate::Reply) when the person has made up their
/// mind.
pub struct WindowHost {
    proxy: EventLoopProxy<Event>,
    status: async_channel::Receiver<Status>,
    unavailable: Option<String>,
}

impl WindowHost {
    fn new(proxy: EventLoopProxy<Event>, status: async_channel::Receiver<Status>) -> Self {
        Self {
            proxy,
            status,
            unavailable: None,
        }
    }

    /// Take whatever the window loop has said since the last request.
    fn drain(&mut self) {
        while let Ok(status) = self.status.try_recv() {
            match status {
                Status::Ready => tracing::debug!("the picker window is ready"),
                Status::Unavailable(reason) => {
                    tracing::warn!(%reason, "no picker window; requests will be answered cancel");
                    self.unavailable = Some(reason);
                }
            }
        }
    }
}

impl Host for WindowHost {
    fn open(&mut self, call: Call) {
        self.drain();
        if let Some(reason) = &self.unavailable {
            tracing::warn!(
                request = %call.id,
                title = %call.window.title,
                app_id = %call.window.app_id,
                %reason,
                "cannot show a picker; answering cancel"
            );
            let _ = call.cancel();
            return;
        }
        if let Err(closed) = self.proxy.send_event(Event::Open(call)) {
            // The loop has ended: the process is on its way out (the idle
            // timeout, or a fatal window error). Answer rather than drop the
            // request on the floor — "never hang" is worth more than blame.
            if let Event::Open(call) = closed.0 {
                tracing::warn!(
                    request = %call.id,
                    "the picker loop has stopped; answering cancel"
                );
                let _ = call.cancel();
            }
        }
    }

    fn close(&mut self, id: RequestId) {
        // Best effort by design: the request is already answered cancel by the
        // time the host hears about a `Close` (`Host::close`'s contract), so a
        // closed loop has nothing left to take down.
        if let Err(closed) = self.proxy.send_event(Event::Close(id))
            && let Event::Close(id) = closed.0
        {
            tracing::debug!(request = %id, "the picker loop has stopped; nothing to close");
        }
    }
}

/// The manager window [`WindowOpener`](files::WindowOpener): a proxy to the same
/// loop, and nothing else.
///
/// The reader is the same one the picker uses; the difference is that an
/// `OpenWindow` call waits for the loop's answer, because a launcher that asked
/// for a window should learn whether it got one. That answer travels on the
/// request's own ack channel, so this never blocks the bus.
pub struct ManagerHost {
    proxy: EventLoopProxy<Event>,
}

impl ManagerHost {
    fn new(proxy: EventLoopProxy<Event>) -> Self {
        Self { proxy }
    }
}

impl files::WindowOpener for ManagerHost {
    fn open(&mut self, request: OpenWindowRequest) {
        if let Err(closed) = self.proxy.send_event(Event::OpenManager(request)) {
            if let Event::OpenManager(request) = closed.0 {
                tracing::warn!(
                    handle = %request.handle,
                    "the window loop has stopped; refusing a manager window"
                );
                request.refuse("the window loop stopped");
            }
        }
    }

    fn close(&mut self, handle: String, ack: CloseWindowAck) {
        if let Err(closed) = self.proxy.send_event(Event::CloseManager { handle, ack })
            && let Event::CloseManager { handle, ack } = closed.0
        {
            tracing::debug!(%handle, "the window loop has stopped; nothing to close");
            let _ = ack.try_send(false);
        }
    }
}

/// Why a request left the picker queue without being shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Close {
    /// It was still waiting for the window.
    Queued,
    /// It was the picker on screen.
    Showing,
    /// Nothing by that id: a reordered close, remembered so a late open is not
    /// shown.
    Unknown,
}

/// The picker window's queue: at most one picker on screen, the rest waiting, and
/// the clock the idle timeout reads.
///
/// Deliberately window-free so the decisions are testable without a display: the
/// event loop only ever asks it "what is next", "did a close take the picker down"
/// and "may I exit yet".
struct Desk {
    waiting: std::collections::VecDeque<Call>,
    showing: Option<RequestId>,
    /// Requests a frontend hung up on before this window heard of them.
    ///
    /// The wire cannot produce one — `src/portal.rs` calls `Host::open` before it
    /// can see a `Close`, and winit's proxy keeps sends in order — but a queue
    /// that ever shifted must drop the dialog its person is no longer waiting for
    /// rather than paint it. Bounded and aged, because a request path can be
    /// reused after its interaction ends.
    hung_up: std::collections::VecDeque<(RequestId, Instant)>,
    last_active: Instant,
}

/// How long a remembered early close survives. Long enough to cover a reordered
/// queue, short enough that a reused handle path cannot be swallowed by it.
const EARLY_CLOSE: Duration = Duration::from_secs(5);
const EARLY_CLOSES: usize = 8;

impl Desk {
    fn new(now: Instant) -> Self {
        Self {
            waiting: std::collections::VecDeque::new(),
            showing: None,
            hung_up: std::collections::VecDeque::new(),
            last_active: now,
        }
    }

    /// Queue a request. `false` means a close for it arrived first, so it was
    /// dropped — the frontend's cancel already stands (`src/portal.rs` §4).
    fn open(&mut self, call: Call, now: Instant) -> bool {
        self.last_active = now;
        self.forget_stale(now);
        if let Some(at) = self.hung_up.iter().position(|(id, _)| *id == call.id) {
            self.hung_up.remove(at);
            return false;
        }
        self.waiting.push_back(call);
        true
    }

    /// The frontend closed a request. What the caller has to do is the answer.
    fn close(&mut self, id: &RequestId, now: Instant) -> Close {
        self.last_active = now;
        self.forget_stale(now);
        if self.showing.as_ref() == Some(id) {
            self.showing = None;
            return Close::Showing;
        }
        if let Some(at) = self.waiting.iter().position(|call| call.id == *id) {
            self.waiting.remove(at);
            return Close::Queued;
        }
        if self.hung_up.len() >= EARLY_CLOSES {
            self.hung_up.pop_front();
        }
        if !self.hung_up.iter().any(|(early, _)| early == id) {
            self.hung_up.push_back((id.clone(), now));
        }
        Close::Unknown
    }

    /// The request to put on screen, when nothing is there already.
    fn next(&mut self, now: Instant) -> Option<Call> {
        if self.showing.is_some() {
            return None;
        }
        let call = self.waiting.pop_front()?;
        self.showing = Some(call.id.clone());
        self.last_active = now;
        Some(call)
    }

    /// The picker was answered: the window may take the next request.
    fn answered(&mut self, id: &RequestId, now: Instant) {
        if self.showing.as_ref() == Some(id) {
            self.showing = None;
        }
        self.last_active = now;
    }

    /// Every request still waiting, for the no-window path to answer cancel.
    fn take_all(&mut self) -> Vec<Call> {
        self.showing = None;
        self.waiting.drain(..).collect()
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.showing.is_none() && self.waiting.is_empty()
    }

    /// When the idle timeout would fire. `last_active` is the last request and
    /// the last answer, so this is "how long has the window been unused".
    #[cfg(test)]
    fn idle_deadline(&self, idle: Duration) -> Instant {
        self.last_active + idle
    }

    fn forget_stale(&mut self, now: Instant) {
        self.hung_up
            .retain(|(_, at)| now.duration_since(*at) < EARLY_CLOSE);
    }
}

/// The picker on screen: its call, the browser drawn for it, and the title.
struct Picker {
    call: Call,
    browser: Browser,
}

/// What a visible window is, beyond the `Frame` that draws it.
///
/// Per-window input state lives here because the same event fields (a pointer, a
/// click count, modifiers) mean different rows in different windows — the picker
/// must not select in the next request with the last request's click.
struct Screen {
    kind: ScreenKind,
    /// The last pointer position, in points, from `CursorMoved`.
    pointer: Option<egui::Pos2>,
    /// The click run, from the shared 400 ms / four-pixel rule: a picker and a
    /// manager count a hand the same way dtr's file panes do.
    gestures: Gestures,
    modifiers: Modifiers,
    /// The manager's bands from the last frame, in points: the wheel and the
    /// presses ask this instead of recomputing the layout (`AGENTS.md`,
    /// `pane_area`). `None` until the first frame has been drawn.
    layout: Option<ManagerLayout>,
    next_refresh: Instant,
}

impl Screen {
    fn new(kind: ScreenKind, now: Instant) -> Self {
        Self {
            kind,
            pointer: None,
            gestures: Gestures::new(),
            modifiers: Modifiers::default(),
            layout: None,
            next_refresh: now + REFRESH,
        }
    }

    fn picker(&mut self) -> Option<&mut Picker> {
        match &mut self.kind {
            ScreenKind::Picker(picker) => Some(picker),
            ScreenKind::Manager(_) => None,
        }
    }

    fn manager(&mut self) -> Option<&mut Manager> {
        match &mut self.kind {
            ScreenKind::Manager(manager) => Some(manager),
            ScreenKind::Picker(_) => None,
        }
    }
}

enum ScreenKind {
    Picker(Picker),
    Manager(Manager),
}

/// The visible windows, and what each is.
///
/// A thin `HashMap` wrapper rather than the map itself so the *policy* has a
/// name and a test: the idle timeout asks how many windows are visible, the
/// worker events route an answer to the window whose manager id matches, and a
/// closed window leaves both. Its keys are `WindowId`s in the running app and
/// plain numbers in the tests, which is why it is generic.
struct Screens<K, V> {
    open: HashMap<K, V>,
}

impl<K: std::hash::Hash + Eq, V> Screens<K, V> {
    fn new() -> Self {
        Self {
            open: HashMap::new(),
        }
    }

    /// How many windows a person can see: the idle policy's whole input.
    fn visible(&self) -> usize {
        self.open.len()
    }
}

impl<K: std::hash::Hash + Eq, V> Default for Screens<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: std::hash::Hash + Eq, V> std::ops::Deref for Screens<K, V> {
    type Target = HashMap<K, V>;

    fn deref(&self) -> &Self::Target {
        &self.open
    }
}

impl<K: std::hash::Hash + Eq, V> std::ops::DerefMut for Screens<K, V> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.open
    }
}

/// The capture hook: one photograph of the first window this process shows.
///
/// A window can be shown more than once in a run (the picker is reused), so the
/// hook is armed by the first show and disarmed by the photograph. A run whose
/// first window is a manager window proves the manager; a picker-only run is
/// unchanged, because a picker *is* the first window there.
struct Capture {
    path: Option<PathBuf>,
    after: Duration,
    due: Option<(WindowId, Instant)>,
    done: bool,
}

impl Capture {
    fn new(path: Option<PathBuf>, after: Duration) -> Self {
        Self {
            path,
            after,
            due: None,
            done: false,
        }
    }

    /// This window has just been shown: photograph it unless one already was.
    fn schedule(&mut self, window: WindowId, now: Instant) {
        if self.done || self.due.is_some() || self.path.is_none() {
            return;
        }
        self.due = Some((window, now + self.after));
    }

    /// The moment has come for this window: the path to write, once.
    fn take(&mut self, window: WindowId, now: Instant) -> Option<PathBuf> {
        let (due_window, due) = self.due?;
        if due_window != window || now < due {
            return None;
        }
        self.due = None;
        self.done = true;
        self.path.take()
    }

    /// A window went away before its photograph.
    fn cancel(&mut self, window: WindowId) {
        if self.due.is_some_and(|(due, _)| due == window) {
            self.due = None;
        }
    }

    /// When the pending photograph is due, for the loop's deadline list.
    fn deadline(&self) -> Option<Instant> {
        self.due.map(|(_, due)| due)
    }
}

/// The frames: one hidden picker window, plus a manager window per request.
struct PickerApp {
    options: Options,
    status: async_channel::Sender<Status>,
    skin: Skin,
    started: Instant,
    frames_drawn: u64,
    desk: Desk,
    /// The visible windows and what they are.
    screens: Screens<WindowId, Screen>,
    /// The warm picker window, hidden between requests. Not in `screens` while
    /// hidden: the map is what the idle policy asks about.
    picker_window: Option<WindowId>,
    /// A window could not be created: requests are answered cancel, and the
    /// process exits at the idle timeout.
    failed: Option<String>,
    /// The manager's routing ids, handed to each window and to the worker.
    next_manager_id: u64,
    /// The thread that lists remote directories, counts local ones and runs the
    /// write verbs.
    worker: Worker,
    capture: Capture,
    /// The last moment a visible window did something, for the idle timeout.
    last_activity: Instant,
    /// Manager windows whose `Frame` is to be dropped; a `window_event` has no
    /// `Windows` to close it with, so the removal happens in `about_to_wait`.
    pending_close: Vec<WindowId>,
}

impl PickerApp {
    fn new(options: Options, status: async_channel::Sender<Status>, worker: Worker) -> Self {
        let now = Instant::now();
        let capture = Capture::new(options.capture.clone(), options.capture_after);
        Self {
            skin: options.skin(),
            options,
            status,
            started: now,
            frames_drawn: 0,
            desk: Desk::new(now),
            screens: Screens::new(),
            picker_window: None,
            failed: None,
            next_manager_id: 1,
            worker,
            capture,
            last_activity: now,
            pending_close: Vec::new(),
        }
    }

    fn manager_mut(&mut self, window: WindowId) -> Option<&mut Manager> {
        self.screens.get_mut(&window)?.manager()
    }

    /// The window a manager routing id names.
    fn manager_window(&self, manager: u64) -> Option<WindowId> {
        self.screens.iter().find_map(|(window, screen)| {
            let ScreenKind::Manager(window_manager) = &screen.kind else {
                return None;
            };
            (window_manager.id == manager).then_some(*window)
        })
    }

    /// The window the picker on screen belongs to.
    fn picker_showing(&self, id: &RequestId) -> Option<WindowId> {
        self.screens.iter().find_map(|(window, screen)| {
            let ScreenKind::Picker(picker) = &screen.kind else {
                return None;
            };
            (&picker.call.id == id).then_some(*window)
        })
    }

    /// Hide the picker window and forget the picker, without answering.
    ///
    /// The caller is a frontend `Close`: the portal has already answered cancel,
    /// so the window has nothing left to say (`Host::close`'s contract).
    fn drop_picker(&mut self, window: WindowId, frame: &mut Frame) {
        let Some(screen) = self.screens.remove(&window) else {
            return;
        };
        if let ScreenKind::Picker(picker) = screen.kind {
            tracing::debug!(request = %picker.call.id, "the picker went away without an answer");
        }
        self.capture.cancel(window);
        frame.window().set_visible(false);
        self.last_activity = Instant::now();
    }

    /// Put the waiting request on screen: build the browser, title the window,
    /// map it.
    fn show_next(&mut self, windows: &mut Windows, now: Instant) {
        let Some(window) = self.picker_window else {
            return;
        };
        let Some(call) = self.desk.next(now) else {
            return;
        };
        let Some(frame) = windows.get_mut(window) else {
            return;
        };

        let request = call.request.clone();
        // Where to start: the folder the application asked for, else the person's
        // home. The daemon's working directory is whatever D-Bus activation left
        // it in, and nobody chose that.
        let start = request.folder.clone().or_else(dirs::home_dir);
        let mut browser = Browser::new(start);
        browser.set_pick(request.clone());
        if let Some(index) = call.filter {
            // `current_filter` is an index into the request's own list — the one
            // the footer offers (`docs/portal.md` §2) — and the picker keeps the
            // draft, so this only chooses a row.
            if let Some(pick) = browser.picking() {
                pick.set_filter(index);
            }
        }

        let title = title(&call);
        frame.set_title(&title);
        // The X11 hints are written while the window is unmapped, which is the
        // window EWMH defines for a window's initial state — and the reason the
        // window is hidden between requests instead of closed.
        if let Err(err) = x11::set_transient_for(
            frame.window(),
            x11::parent_window(&call.window.parent_window),
            call.window.modal,
        ) {
            // A Wayland handle lands here by design; so does an X11 handle that
            // is not a window any more. A picker that is not transient still picks
            // files.
            tracing::debug!(
                %err,
                parent = %call.window.parent_window,
                "the picker is not transient for the asking window"
            );
        }
        frame.window().set_visible(true);
        frame.window().focus_window();
        frame.request_redraw();

        self.capture.schedule(window, now);
        tracing::debug!(request = %call.id, %title, "showing the picker");
        self.last_activity = now;
        self.screens.insert(
            window,
            Screen::new(ScreenKind::Picker(Picker { call, browser }), now),
        );
    }

    /// Answer the picker on screen and take the window down.
    fn answer_picker(&mut self, window: WindowId, answer: Answer, frame: &mut Frame) {
        let Some(screen) = self.screens.remove(&window) else {
            return;
        };
        let ScreenKind::Picker(picker) = screen.kind else {
            return;
        };
        let id = picker.call.id.clone();
        if !picker.call.reply.send(answer) {
            // The frontend closed it first: `Reply` has one slot and the portal
            // answered cancel when the close won (`docs/portal.md` §4).
            tracing::debug!(request = %id, "answered a request that was already closed");
        }
        self.capture.cancel(window);
        self.desk.answered(&id, Instant::now());
        frame.window().set_visible(false);
        self.last_activity = Instant::now();
    }

    /// The footer's accept button, or `Enter`.
    fn accept(&mut self, window: WindowId, frame: &mut Frame) {
        let outcome = {
            let Some(picker) = self.screens.get_mut(&window).and_then(Screen::picker) else {
                return;
            };
            picker.browser.accept_pick()
        };
        if let Some(outcome) = outcome {
            self.apply_picker(window, outcome, frame);
        }
    }

    /// `Escape`, or the footer's cancel.
    ///
    /// One button, one word, two depths: a save's `replace` question closes
    /// first, and the picker's next Escape is the cancel (`docs/manager.md` §4).
    fn cancel(&mut self, window: WindowId, frame: &mut Frame) {
        let still_picking = {
            let Some(picker) = self.screens.get_mut(&window).and_then(Screen::picker) else {
                return;
            };
            picker.browser.cancel_pick();
            picker.browser.is_picking()
        };
        if still_picking {
            frame.request_redraw();
            return;
        }
        self.answer_picker(window, Answer::Cancelled, frame);
    }

    /// The window's close button, the header's cross, `Escape` on a picker with
    /// no question inside it: the person dismissed the dialog.
    fn dismiss(&mut self, window: WindowId, frame: &mut Frame) {
        self.answer_picker(window, Answer::Cancelled, frame);
    }

    /// Fold a picker `Outcome` into what the portal permits.
    ///
    /// Only `Chosen` and `Cancelled` reach the bus: `Refused` and `Replace` are
    /// the footer saying something and asking again, and `Navigate` is the browser
    /// going into a folder with the question still open (`docs/portal.md` §9).
    fn apply_picker(&mut self, window: WindowId, outcome: Outcome, frame: &mut Frame) {
        let answer = match outcome {
            Outcome::Chosen(paths) => {
                let pick = self
                    .screens
                    .get(&window)
                    .and_then(|screen| match &screen.kind {
                        ScreenKind::Picker(picker) => picker.browser.picking(),
                        ScreenKind::Manager(_) => None,
                    });
                portal_answer(paths, pick)
            }
            // The footer is saying why it cannot answer, or asking whether to
            // replace; both are the picker's own business.
            Outcome::Refused(_) | Outcome::Replace(_) => {
                frame.request_redraw();
                None
            }
            // A folder pressed in a file picker is a way in, with the question
            // still open.
            Outcome::Navigate(folder) => {
                if let Some(picker) = self.screens.get_mut(&window).and_then(Screen::picker) {
                    picker.browser.navigate(folder);
                }
                frame.request_redraw();
                None
            }
        };
        if let Some(answer) = answer {
            self.answer_picker(window, answer, frame);
        }
    }

    /// Open a manager window: create the `Frame`, put it on screen and answer the
    /// waiting D-Bus method.
    fn open_manager(
        &mut self,
        event_loop: &ActiveEventLoop,
        windows: &mut Windows,
        request: OpenWindowRequest,
    ) {
        if let Some(reason) = self.failed.clone() {
            request.refuse(reason);
            return;
        }
        let id = self.next_manager_id;
        self.next_manager_id += 1;
        let desktop = Desktop::load();
        let manager = Manager::new(id, request.handle.clone(), request.path.clone(), desktop);
        let title = request.title.clone().unwrap_or_else(|| manager.title());
        let opened = windows.open(
            event_loop,
            WindowOptions {
                title,
                inner_size: MANAGER_SIZE,
                // A file manager is an application of its own in the dock, not a
                // dialog: the launcher's name is what a window list shows.
                app_id: Some(("org.dtr.Files".to_owned(), "dtr-files".to_owned())),
                ..WindowOptions::default()
            },
        );
        let frame = match opened {
            Ok(frame) => frame,
            Err(err) => {
                let reason = format!("{err:#}");
                tracing::error!(%reason, "could not create the manager window");
                request.refuse(reason);
                return;
            }
        };
        let window = frame.id();
        if let Err(err) = x11::set_transient_for(
            frame.window(),
            x11::parent_window(&request.parent_window),
            request.modal,
        ) {
            tracing::debug!(
                %err,
                parent = %request.parent_window,
                "the manager is not transient for the asking window"
            );
        }
        self.skin.apply(frame.context());
        frame.window().focus_window();
        frame.request_redraw();
        // Answer after the `Frame` exists: a launcher learns what it asked for.
        request.accept();
        let now = Instant::now();
        self.capture.schedule(window, now);
        self.last_activity = now;
        tracing::debug!(window = ?window, manager = id, "showing a manager window");
        self.screens
            .insert(window, Screen::new(ScreenKind::Manager(manager), now));
    }

    /// A manager window asked to go away: hide it now, drop the `Frame` after the
    /// event batch (`Windows::close` needs the collection, which `window_event`
    /// does not have).
    fn close_manager(&mut self, window: WindowId, frame: Option<&mut Frame>) {
        if let Some(frame) = frame {
            frame.window().set_visible(false);
        }
        if let Some(screen) = self.screens.remove(&window)
            && let ScreenKind::Manager(manager) = screen.kind
        {
            tracing::debug!(handle = %manager.handle, "a manager window closed");
        }
        self.capture.cancel(window);
        if !self.pending_close.contains(&window) {
            self.pending_close.push(window);
        }
        self.last_activity = Instant::now();
    }

    /// The worker finished a job for a manager window.
    fn worker_event(&mut self, windows: &mut Windows, event: WorkerEvent) {
        match event {
            WorkerEvent::Listed { manager, listing } => {
                let Some(window) = self.manager_window(manager) else {
                    return;
                };
                let changed = self
                    .manager_mut(window)
                    .is_some_and(|manager| manager.listed(listing));
                if changed {
                    self.repaint(windows, window);
                }
            }
            WorkerEvent::Failed {
                manager,
                host,
                path,
                message,
            } => {
                let Some(window) = self.manager_window(manager) else {
                    return;
                };
                if self
                    .manager_mut(window)
                    .is_some_and(|manager| manager.listing_failed(&host, &path, &message))
                {
                    self.repaint(windows, window);
                }
            }
            WorkerEvent::Counted {
                manager,
                total,
                shown,
            } => {
                let Some(window) = self.manager_window(manager) else {
                    return;
                };
                if let Some(manager) = self.manager_mut(window) {
                    manager.counted(total, shown);
                }
                self.repaint(windows, window);
            }
            WorkerEvent::Wrote { manager, result } => {
                let Some(window) = self.manager_window(manager) else {
                    return;
                };
                let Some(manager) = self.manager_mut(window) else {
                    return;
                };
                match result {
                    crate::worker::WriteResult::Ok => manager.write_succeeded(),
                    crate::worker::WriteResult::Conflict(destination) => {
                        manager.write_conflicted(destination);
                    }
                    crate::worker::WriteResult::Failed(message) => {
                        manager.write_failed(&message);
                    }
                }
                self.repaint(windows, window);
            }
        }
        self.last_activity = Instant::now();
    }

    fn repaint(&self, windows: &mut Windows, window: WindowId) {
        if let Some(frame) = windows.get_mut(window) {
            frame.request_redraw();
        }
    }

    /// One frame of the picker, and whatever the person did to it.
    fn redraw_picker(&mut self, window: WindowId, frame: &mut Frame) {
        let Some(screen) = self.screens.remove(&window) else {
            return;
        };
        let Screen {
            kind: ScreenKind::Picker(mut picker),
            pointer,
            gestures,
            modifiers,
            layout,
            next_refresh,
        } = screen
        else {
            // The screen changed kind between the event and the frame, which
            // cannot happen — but putting it back is cheaper than a panic in a
            // render loop.
            return;
        };
        let now = Instant::now();
        let mut next_refresh = next_refresh;
        let mut repaint = picker.browser.poll();
        if now >= next_refresh {
            next_refresh = now + REFRESH;
            // A picker notices a file that appeared under it the way a pane does:
            // a `stat` of the directory it is showing, twice a second.
            repaint |= picker.browser.refresh_if_changed();
        }
        let capture = self.capture.take(window, now);

        let skin = &self.skin;
        let id = egui::Id::unique(("dtr-portal-picker", picker.call.id.path()));
        let mut outcome: Option<PaneOutcome> = None;
        let mut scene = Empty {
            background: clear_color(skin.palette.card),
        };
        let rendered = frame.render(
            &mut scene,
            RenderOptions {
                frames_drawn: self.frames_drawn,
                started: self.started,
                capture: capture.as_deref(),
                measure_present: false,
            },
            |ui| {
                // The client area is the pane: `pane_ui` divides nothing and
                // measures nothing, it is handed points and paints.
                let rect = ui.max_rect();
                outcome = Some(dtr_files::pane_ui(
                    ui,
                    id,
                    rect,
                    &picker.browser,
                    skin,
                    true,
                ));
            },
        );
        match rendered {
            Ok(Some(_)) => self.frames_drawn += 1,
            // A lost or busy surface: nothing was drawn, so ask again.
            Ok(None) => repaint = true,
            Err(err) => tracing::warn!(%err, "a picker frame failed"),
        }
        if frame.context().has_requested_repaint() {
            repaint = true;
        }

        // The browser is put back before the outcome is applied, because
        // answering takes the picker out from under it.
        self.screens.insert(
            window,
            Screen {
                kind: ScreenKind::Picker(picker),
                pointer,
                gestures,
                modifiers,
                layout,
                next_refresh,
            },
        );
        if let Some(outcome) = outcome {
            if outcome.close {
                // The pane's own close cross in a window is the window's close
                // button: dismiss the dialog.
                self.dismiss(window, frame);
                return;
            }
            if let Some(change) = outcome.sort {
                self.sort_picker(window, change);
            }
            if let Some(path) = outcome.navigate {
                if let Some(picker) = self.screens.get_mut(&window).and_then(Screen::picker) {
                    picker.browser.navigate(path);
                }
                repaint = true;
            }
            if let Some(outcome) = outcome.pick {
                self.apply_picker(window, outcome, frame);
            } else if outcome.pick_cancel {
                self.cancel(window, frame);
            }
        }
        if repaint && self.screens.contains_key(&window) {
            frame.request_redraw();
        }
    }

    fn sort_picker(&mut self, window: WindowId, change: SortChange) {
        let Some(picker) = self.screens.get_mut(&window).and_then(Screen::picker) else {
            return;
        };
        let mut view = picker.browser.view();
        match change {
            SortChange::Key(key) => view.sort = key,
            SortChange::Reverse => view.reverse = !view.reverse,
            SortChange::FoldersFirst => view.folders_first = !view.folders_first,
        }
        picker.browser.set_view(view);
    }

    /// One frame of a manager window.
    fn redraw_manager(&mut self, window: WindowId, frame: &mut Frame) {
        let Some(screen) = self.screens.remove(&window) else {
            return;
        };
        let Screen {
            kind: ScreenKind::Manager(mut manager),
            pointer,
            gestures,
            modifiers,
            layout: _,
            next_refresh,
        } = screen
        else {
            return;
        };
        let now = Instant::now();
        let mut next_refresh = next_refresh;
        let mut repaint = manager.poll(now >= next_refresh);
        if now >= next_refresh {
            next_refresh = now + REFRESH;
        }
        if let Some(path) = manager.take_count_request() {
            self.worker.count(manager.id, path);
        }
        if let Some((alias, path)) = manager.take_listing_request() {
            self.worker.list(manager.id, alias, path);
        }
        while let Some(request) = manager.take_write() {
            if !self.worker.write(manager.id, request) {
                manager.write_failed("the file worker has stopped");
            }
        }
        // The caption follows the directory: a manager window that kept its
        // first title would be one you cannot find in a task list after a
        // navigation.
        frame.set_title(&manager.title());
        let capture = self.capture.take(window, now);

        let skin = &self.skin;
        let mut answer = dtr_files::manager::ManagerAnswer::default();
        let mut outcome: Option<PaneOutcome> = None;
        let mut layout = None;
        let mut scene = Empty {
            background: clear_color(skin.palette.window),
        };
        let rendered = frame.render(
            &mut scene,
            RenderOptions {
                frames_drawn: self.frames_drawn,
                started: self.started,
                capture: capture.as_deref(),
                measure_present: false,
            },
            |ui| {
                let client = ui.max_rect();
                layout = Some(ManagerLayout::new(client, skin));
                let (chrome, pane) = manager.ui(ui, skin);
                answer = chrome;
                outcome = Some(pane);
            },
        );
        match rendered {
            Ok(Some(_)) => self.frames_drawn += 1,
            Ok(None) => repaint = true,
            Err(err) => tracing::warn!(%err, "a manager frame failed"),
        }
        if frame.context().has_requested_repaint() {
            repaint = true;
        }

        let ctx = frame.context().clone();
        manager.apply(answer);
        let close = outcome.is_some_and(|outcome| manager.apply_pane(outcome, &ctx));
        if close {
            self.close_manager(window, None);
            frame.window().set_visible(false);
            return;
        }
        self.screens.insert(
            window,
            Screen {
                kind: ScreenKind::Manager(manager),
                pointer,
                gestures,
                modifiers,
                layout,
                next_refresh,
            },
        );
        if repaint {
            frame.request_redraw();
        }
    }

    /// A left press in a manager window: the chrome's widgets get their own
    /// presses through egui; the content is the pane's.
    fn manager_press(&mut self, window: WindowId, frame: &mut Frame) {
        let Some(at) = self.screens.get(&window).and_then(|screen| screen.pointer) else {
            return;
        };
        // The click run is counted by the shared rule, in the same place the
        // pointer lives: the last press and the window's modifiers are the
        // window's state, not the model's.
        let (modifiers, layout, clicks) = {
            let Some(screen) = self.screens.get_mut(&window) else {
                return;
            };
            let clicks = screen
                .gestures
                .press((at.x as f64, at.y as f64), Instant::now());
            (screen.modifiers.state(), screen.layout, clicks)
        };
        // The content band is the pane's; the toolbar, sidebar and status bar are
        // egui widgets and their presses are already the chrome's. A card or a
        // menu in a layer above the root owns the press too.
        if layout.and_then(|layout| layout.band_at(at)) != Some(Band::Content) {
            return;
        }
        if over_egui(frame.context(), at) {
            return;
        }
        let Some(manager) = self.screens.get_mut(&window).and_then(Screen::manager) else {
            return;
        };
        let changed = manager.press(at, clicks, modifiers.control_key(), modifiers.shift_key());
        if changed {
            frame.request_redraw();
        }
    }

    /// One key, while a picker is up.
    fn picker_key(&mut self, window: WindowId, frame: &mut Frame, event: winit::event::KeyEvent) {
        // A release is not a press: the release of the keystroke that opened the
        // window arrives after it is open, and acting on both answers twice
        // (`AGENTS.md`, "a modal must ignore key releases").
        if event.state != ElementState::Pressed {
            return;
        }
        // A save's name field owns what is typed into it. egui's own text field
        // has the keyboard focus, and a listing that moved behind it would be a
        // picker typing into two places at once.
        let typing = frame.context().memory(|memory| memory.focused().is_some());
        let modifiers = self
            .screens
            .get(&window)
            .map(|screen| screen.modifiers.state())
            .unwrap_or_default();
        match &event.logical_key {
            WinitKey::Named(NamedKey::Escape) => self.cancel(window, frame),
            WinitKey::Named(NamedKey::Enter) => self.accept(window, frame),
            _ if !typing => {
                // A picker is a read-only question: the shared table gives it
                // movement and refuses the write chords, and `Enter`/`Escape`
                // above are its footer's.
                if let Some(key) = pane_key(&event.logical_key, modifiers, Scope::picker()) {
                    if let Some(picker) = self.screens.get_mut(&window).and_then(Screen::picker) {
                        let _ = picker.browser.handle(key);
                    }
                    frame.request_redraw();
                }
            }
            _ => {}
        }
    }

    /// One key, while a manager window has it.
    fn manager_key(&mut self, window: WindowId, frame: &mut Frame, event: winit::event::KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        let modifiers = self
            .screens
            .get(&window)
            .map(|screen| screen.modifiers.state())
            .unwrap_or_default();
        // The pane's right-press menu owns the keyboard briefly: any key closes
        // it and then goes where it was going, except Escape, which was aimed at
        // the menu.
        let menu_open = self
            .screens
            .get(&window)
            .and_then(|screen| match &screen.kind {
                ScreenKind::Manager(manager) => Some(manager),
                ScreenKind::Picker(_) => None,
            })
            .is_some_and(|manager| manager.menu_open());
        if menu_open {
            let escape = matches!(&event.logical_key, WinitKey::Named(NamedKey::Escape));
            if let Some(screen) = self.screens.get_mut(&window)
                && let ScreenKind::Manager(manager) = &mut screen.kind
            {
                manager.close_menu();
            }
            frame.request_redraw();
            if escape {
                return;
            }
        }
        // A field, a card or a confirmation owns the keyboard: the chrome's
        // widgets read egui's input, and a listing that moved behind them would
        // be typing into two places at once.
        let typing = frame.context().memory(|memory| memory.focused().is_some());
        if typing {
            return;
        }
        let Some(screen) = self.screens.get_mut(&window) else {
            return;
        };
        let Some(manager) = screen.manager() else {
            return;
        };
        if manager.write_modal() {
            // Escape and Enter belong to the open card or field, which reads them
            // from egui's input; anything the window did here would race it.
            return;
        }
        let mut repaint = false;
        match &event.logical_key {
            // `Alt+Left`/`Alt+Right` are the desktop's back/forward, and
            // `Alt+Up` the parent: the toolbar's buttons have keys.
            WinitKey::Named(NamedKey::ArrowLeft) if modifiers.alt_key() => {
                manager.back();
                repaint = true;
            }
            WinitKey::Named(NamedKey::ArrowRight) if modifiers.alt_key() => {
                manager.forward();
                repaint = true;
            }
            WinitKey::Named(NamedKey::ArrowUp) if modifiers.alt_key() => {
                manager.up();
                repaint = true;
            }
            WinitKey::Named(NamedKey::Escape) => {}
            WinitKey::Named(NamedKey::Enter) => {
                manager.handle(Key::Enter);
                repaint = true;
            }
            WinitKey::Character(text) if modifiers.control_key() => match text.as_str() {
                "h" | "H" => {
                    manager.toggle_hidden();
                    repaint = true;
                }
                "1" => {
                    manager.set_layout(dtr_files::Layout::Grid);
                    repaint = true;
                }
                "2" => {
                    manager.set_layout(dtr_files::Layout::List);
                    repaint = true;
                }
                _ => {}
            },
            _ => {
                // The manager's content, through the shared table: movement
                // always, the write chords only when the backend has the verbs.
                // Before F6 this window had a copy of the table that never
                // learned `F2`, `Delete`, `Ctrl+C`, `Ctrl+V` or `Ctrl+Shift+N`,
                // so a manager could not rename from the keyboard at all.
                if let Some(key) = pane_key(
                    &event.logical_key,
                    modifiers,
                    Scope::manager(manager.can_write()),
                ) {
                    manager.handle(key);
                    repaint = true;
                }
            }
        }
        if repaint {
            frame.request_redraw();
        }
    }
}

impl dtr_frame::App for PickerApp {
    type Event = Event;

    fn resumed(&mut self, event_loop: &ActiveEventLoop, windows: &mut Windows) {
        // `resumed` runs again after a suspend; a window that is already there is
        // the whole answer.
        if self.picker_window.is_some() || self.failed.is_some() {
            return;
        }
        let frame = match windows.open(
            event_loop,
            WindowOptions {
                title: "Files".to_owned(),
                inner_size: (900.0, 620.0),
                app_id: Some(("dtr-portal".to_owned(), "dtr-portal".to_owned())),
                ..WindowOptions::default()
            },
        ) {
            Ok(frame) => frame,
            Err(err) => {
                let reason = format!("{err:#}");
                tracing::error!(%reason, "could not create the picker window");
                self.failed = Some(reason.clone());
                let _ = self.status.try_send(Status::Unavailable(reason));
                return;
            }
        };
        self.picker_window = Some(frame.id());
        // The memo's warm window: created now so the adapter, device, context and
        // fonts are paid for before the first dialog needs them, and hidden until
        // there is a request to put in it. It is not in `screens`: the idle policy
        // and the capture hook both ask what a person can see.
        frame.window().set_visible(false);
        self.skin.apply(frame.context());
        let _ = self.status.try_send(Status::Ready);
        tracing::debug!(
            window = ?self.picker_window,
            "the picker window is warm and hidden"
        );
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, windows: &mut Windows, event: Event) {
        self.last_activity = Instant::now();
        match event {
            Event::Open(call) => {
                if let Some(reason) = &self.failed {
                    tracing::warn!(
                        request = %call.id,
                        %reason,
                        "no window to show this request; answering cancel"
                    );
                    let _ = call.cancel();
                    return;
                }
                let now = Instant::now();
                if !self.desk.open(call, now) {
                    tracing::debug!("a request closed before the window reached it");
                }
            }
            Event::Close(id) => match self.desk.close(&id, Instant::now()) {
                Close::Showing => {
                    if let Some(window) = self.picker_showing(&id)
                        && let Some(frame) = windows.get_mut(window)
                    {
                        self.drop_picker(window, frame);
                    }
                }
                Close::Queued => tracing::debug!(request = %id, "dropped a queued picker"),
                Close::Unknown => {
                    tracing::debug!(request = %id, "close for a request this window does not know")
                }
            },
            Event::OpenManager(request) => self.open_manager(event_loop, windows, request),
            Event::CloseManager { handle, ack } => {
                let window = self.screens.iter().find_map(|(window, screen)| {
                    let ScreenKind::Manager(manager) = &screen.kind else {
                        return None;
                    };
                    (manager.handle == handle).then_some(*window)
                });
                match window {
                    Some(window) => {
                        self.close_manager(window, windows.get_mut(window));
                        let _ = ack.try_send(true);
                    }
                    None => {
                        tracing::debug!(%handle, "close for a manager window that is not open");
                        let _ = ack.try_send(false);
                    }
                }
            }
            Event::Worker(event) => self.worker_event(windows, event),
        }
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        frame: &mut Frame,
        event: WindowEvent,
    ) {
        let window = frame.id();
        let is_picker = self
            .screens
            .get(&window)
            .is_none_or(|screen| matches!(screen.kind, ScreenKind::Picker(_)));
        let egui_takes = frame.feed_event(&event);
        let pointer = matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
        );
        if egui_takes || pointer {
            frame.request_redraw();
        }
        match event {
            // The window manager's close button.
            WindowEvent::CloseRequested => {
                if is_picker {
                    // A picker has no unsaved state, so this is the person saying
                    // no.
                    if self.screens.contains_key(&window) {
                        self.dismiss(window, frame);
                    }
                } else {
                    self.close_manager(window, Some(frame));
                }
            }
            WindowEvent::Resized(size) => {
                // Recorded, not applied: the frame configures the swapchain once,
                // on the frame that draws the new size (`docs/frame.md`).
                frame.resize(size);
                frame.request_redraw();
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                if let Some(screen) = self.screens.get_mut(&window) {
                    screen.modifiers = modifiers;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                // egui's own pointer is only readable at frame time, and a press
                // has to know where it landed when it arrives.
                let scale = frame.scale();
                let at = egui::pos2(position.x as f32 / scale, position.y as f32 / scale);
                if let Some(screen) = self.screens.get_mut(&window) {
                    screen.pointer = Some(at);
                }
            }
            WindowEvent::CursorLeft { .. } => {
                if let Some(screen) = self.screens.get_mut(&window) {
                    screen.pointer = None;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if is_picker {
                    self.picker_press(window, frame);
                } else {
                    self.manager_press(window, frame);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                if !is_picker {
                    self.manager_right_press(window, frame);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(y),
                    MouseScrollDelta::PixelDelta(position) => position.y / 20.0,
                };
                if is_picker {
                    if let Some(picker) = self.screens.get_mut(&window).and_then(Screen::picker) {
                        picker.browser.scroll(lines);
                        frame.request_redraw();
                    }
                } else {
                    let over_content = {
                        let Some(screen) = self.screens.get(&window) else {
                            return;
                        };
                        match (screen.pointer, screen.layout) {
                            (Some(at), Some(layout)) => layout.band_at(at) == Some(Band::Content),
                            _ => false,
                        }
                    };
                    if over_content {
                        if let Some(manager) =
                            self.screens.get_mut(&window).and_then(Screen::manager)
                        {
                            manager.scroll(lines);
                        }
                        frame.request_redraw();
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if is_picker {
                    self.picker_key(window, frame, event);
                } else {
                    self.manager_key(window, frame, event);
                }
            }
            WindowEvent::RedrawRequested => {
                if is_picker {
                    self.redraw_picker(window, frame);
                } else {
                    self.redraw_manager(window, frame);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(
        &mut self,
        event_loop: &ActiveEventLoop,
        windows: &mut Windows,
        deadlines: &mut Deadlines,
    ) {
        let now = Instant::now();
        // With no window nothing can ever be shown; answer whatever is queued so
        // nobody waits on a machine with no display, then idle out.
        if self.failed.is_some() {
            for call in self.desk.take_all() {
                tracing::warn!(
                    request = %call.id,
                    "no window to show this request; answering cancel"
                );
                let _ = call.cancel();
            }
        }
        // A picker waiting for the warm window goes up as soon as the loop is
        // idle — never inside `user_event`, so an Open and the Close that raced it
        // cannot paint a window (`docs/portal.md` §9.2).
        if self.screens.is_empty() && self.desk.showing.is_none() {
            self.show_next(windows, now);
        }
        for window in self.pending_close.drain(..) {
            windows.close(window);
        }
        let mut wake: Option<Instant> = self.capture.deadline();
        for (window, screen) in self.screens.iter_mut() {
            if now >= screen.next_refresh {
                screen.next_refresh = now + REFRESH;
                if let Some(frame) = windows.get_mut(*window) {
                    frame.request_redraw();
                }
                wake = Some(now);
            } else {
                wake = Some(wake.map_or(screen.next_refresh, |at| at.min(screen.next_refresh)));
            }
        }
        match idle_exit(
            self.screens.visible(),
            self.last_activity,
            self.options.idle,
        ) {
            None => {
                if let Some(wake) = wake.filter(|wake| *wake > now) {
                    deadlines.soon(wake);
                }
            }
            Some(deadline) if now >= deadline => {
                tracing::info!(
                    idle_s = self.options.idle.as_secs(),
                    "nothing on screen for the idle timeout; exiting so D-Bus activation can start afresh"
                );
                event_loop.exit();
            }
            Some(deadline) => deadlines.soon(deadline),
        }
    }
}

impl PickerApp {
    fn picker_press(&mut self, window: WindowId, frame: &mut Frame) {
        let Some(at) = self.screens.get(&window).and_then(|screen| screen.pointer) else {
            return;
        };
        let (modifiers, clicks) = {
            let Some(screen) = self.screens.get_mut(&window) else {
                return;
            };
            let clicks = screen
                .gestures
                .press((at.x as f64, at.y as f64), Instant::now());
            (screen.modifiers.state(), clicks)
        };
        if let Some(picker) = self.screens.get_mut(&window).and_then(Screen::picker) {
            // `clicked` opens a folder on its second click and reports the file a
            // single one would open in a manager; in a picker neither is an answer,
            // and a double-click on a file must not insert anything anywhere.
            let _ =
                picker
                    .browser
                    .clicked(at, clicks, modifiers.control_key(), modifiers.shift_key());
        }
        frame.request_redraw();
    }

    fn manager_right_press(&mut self, window: WindowId, frame: &mut Frame) {
        let Some(at) = self.screens.get(&window).and_then(|screen| screen.pointer) else {
            return;
        };
        if over_egui(frame.context(), at) {
            return;
        }
        let over_content = {
            let Some(screen) = self.screens.get(&window) else {
                return;
            };
            screen.layout.and_then(|layout| layout.band_at(at)) == Some(Band::Content)
        };
        if !over_content {
            return;
        }
        if let Some(manager) = self.screens.get_mut(&window).and_then(Screen::manager)
            && manager.right_press(at)
        {
            frame.request_redraw();
        }
    }
}

/// When the process should exit for having no window on screen, if ever.
///
/// The memo's "idle-exit after a timeout when it owns no visible window": a
/// manager window is a window (a person is looking at a file manager, not at a
/// daemon), so any visible screen keeps the process alive; with none, the last
/// thing that happened plus the timeout is the exit, and a zero timeout is the
/// verification run's "never".
fn idle_exit(visible: usize, last_activity: Instant, idle: Duration) -> Option<Instant> {
    if visible > 0 || idle.is_zero() {
        return None;
    }
    Some(last_activity + idle)
}

/// A point over an egui surface above the root window — a card, a menu, the
/// overlay a file pane's own widgets live in.
///
/// The pane's body and its chrome are painted into background areas and are asked
/// back explicitly; anything the layers above the root have is egui's business,
/// and a press that fell through it would move the selection behind it
/// (`dtr-widgets::above_the_root`, the same rule dtr's panes use).
fn over_egui(ctx: &egui::Context, point: egui::Pos2) -> bool {
    ctx.layer_id_at(point)
        .is_some_and(dtr_widgets::above_the_root)
}

/// The underlay: the browser paints its own surface, so the pass under egui only
/// has to clear to the skin's card colour. It is covered a moment later, and if a
/// frame ever shows through, it is the colour the pane would have been.
struct Empty {
    background: wgpu::Color,
}

impl Scene for Empty {
    fn clear_color(&self) -> wgpu::Color {
        self.background
    }

    fn prepare(&mut self, _device: &wgpu::Device, _queue: &wgpu::Queue, _size: (u32, u32)) {}

    fn draw(&mut self, _pass: &mut wgpu::RenderPass<'_>) {}

    fn draw_above(&mut self, _pass: &mut wgpu::RenderPass<'_>) {}
}

/// A skin colour as a surface clear colour.
///
/// The surface is configured `Bgra8Unorm`/`Rgba8Unorm` (not an `*Srgb` format,
/// because egui prefers to do its own conversion), so these are the byte values as
/// fractions — not a linearisation, which would darken what egui paints over it in
/// the same frame.
fn clear_color(colour: egui::Color32) -> wgpu::Color {
    wgpu::Color {
        r: f64::from(colour.r()) / 255.0,
        g: f64::from(colour.g()) / 255.0,
        b: f64::from(colour.b()) / 255.0,
        a: 1.0,
    }
}

/// The portal's answer for a set of chosen paths, if the picker has one.
///
/// `filter` is echoed **only when the request offered filters**: it exists as
/// `current_filter` because the application asked for a filter list, so an index
/// into an empty list is not a filter the caller could use (`docs/portal.md` §2).
/// Split out from [`PickerApp::apply_picker`] because it is the decideable half,
/// and a test can ask it without a window.
fn portal_answer(paths: Vec<PathBuf>, pick: Option<&dtr_files::Pick>) -> Option<Answer> {
    let filter = pick.and_then(|pick| (!pick.filters().is_empty()).then(|| pick.filter()));
    Some(Answer::Chosen(Chosen { paths, filter }))
}

/// The window's caption: the application's title, or the question by name.
fn title(call: &Call) -> String {
    if !call.window.title.is_empty() {
        return call.window.title.clone();
    }
    match call.request.select {
        Select::File | Select::Files => "Open a file",
        Select::Directory => "Choose a folder",
        Select::Save => "Save a file",
    }
    .to_owned()
}

/// A running window loop, and the hosts the D-Bus side was given.
///
/// Splitting "build the loop" from "run the loop" is what makes the two halves
/// possible: `main` starts the bus on the hosts, waits for its handshake, and only
/// then enters the loop.
pub struct Run {
    frames: FrameLoop<Event>,
    app: PickerApp,
}

impl Run {
    /// Run the window loop until the idle timeout ends it.
    ///
    /// Nothing here returns while a window is on screen; the loop ends only when
    /// every window has been gone for [`Options::idle`], which is the signal for
    /// the process — and therefore the bus — to go away until activation needs it.
    pub fn run(self) -> Result<(), String> {
        self.frames
            .run(self.app)
            .map_err(|err| format!("the windows' event loop: {err}"))
    }
}

/// Build the window loop on this thread, and the hosts to serve it with.
///
/// **Must be called on the main thread**: winit's `EventLoop` panics anywhere
/// else, which is why the D-Bus half is the thread that moves. `Err` is "this
/// machine has no display to open a window on" — the caller serves the portal with
/// hosts that answer cancel (`main.rs`), rather than exiting and leaving every
/// application with a broken dialog.
pub fn start(options: Options) -> Result<(Run, WindowHost, ManagerHost), String> {
    let frames = FrameLoop::<Event>::new().map_err(|err| format!("no window loop: {err}"))?;
    let proxy = frames.proxy();
    let worker = Worker::start({
        let proxy = proxy.clone();
        move |event| {
            let _ = proxy.send_event(Event::Worker(event));
        }
    });
    let (status, status_rx) = async_channel::unbounded();
    let app = PickerApp::new(options, status, worker);
    Ok((
        Run { frames, app },
        WindowHost::new(proxy.clone(), status_rx),
        ManagerHost::new(proxy),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{Reply, WindowContext};
    use crate::{Request, Select};
    use std::path::PathBuf;

    /// A request the way `src/portal.rs` builds one, with a receiver so a test can
    /// tell "not answered" from "answered".
    fn call(id: &str) -> (Call, async_channel::Receiver<Answer>) {
        let (tx, rx) = async_channel::bounded(1);
        let handle = format!("/org/freedesktop/portal/desktop/request/1_1/{id}");
        (
            Call {
                id: RequestId::new(handle),
                request: Request {
                    select: Select::File,
                    accept: "open".to_owned(),
                    folder: Some(PathBuf::from("/tmp")),
                    filters: Vec::new(),
                    name: String::new(),
                },
                filter: None,
                window: WindowContext {
                    app_id: "org.example.App".to_owned(),
                    parent_window: "x11:0x1234".to_owned(),
                    title: "Open a file".to_owned(),
                    modal: true,
                },
                reply: Reply::new(tx),
            },
            rx,
        )
    }

    fn id(call: &Call) -> RequestId {
        call.id.clone()
    }

    #[test]
    fn one_picker_at_a_time_and_the_rest_wait() {
        let now = Instant::now();
        let mut desk = Desk::new(now);
        let (first, _first_rx) = call("one");
        let (second, _second_rx) = call("two");
        assert!(desk.open(first, now));
        assert!(desk.open(second, now));
        assert!(!desk.is_empty());

        let shown = desk.next(now).expect("the first request is shown");
        assert_eq!(
            shown.id.path(),
            "/org/freedesktop/portal/desktop/request/1_1/one"
        );
        assert!(desk.next(now).is_none(), "the window is busy");
        assert!(!desk.is_empty(), "the second request is still queued");

        desk.answered(&shown.id, now);
        let next = desk.next(now).expect("the second request follows");
        assert_eq!(
            next.id.path(),
            "/org/freedesktop/portal/desktop/request/1_1/two"
        );
        desk.answered(&next.id, now);
        assert!(desk.is_empty());
    }

    #[test]
    fn a_close_before_the_window_showed_it_drops_it_without_an_answer() {
        let now = Instant::now();
        let mut desk = Desk::new(now);
        let (waiting, waiting_rx) = call("waiting");
        let (showing, showing_rx) = call("showing");
        assert!(desk.open(waiting, now));
        assert!(desk.open(showing, now));
        let shown = desk.next(now).expect("a picker");

        // The frontend gives up on the one still queued.
        let queued_id = id(&desk.waiting[0]);
        assert_eq!(desk.close(&queued_id, now), Close::Queued);
        // ...and on the one on screen.
        assert_eq!(desk.close(&shown.id, now), Close::Showing);
        desk.answered(&shown.id, now);
        assert!(desk.is_empty());
        assert!(
            waiting_rx.try_recv().is_err() && showing_rx.try_recv().is_err(),
            "a closed request is answered by the portal, never by the window"
        );
    }

    #[test]
    fn a_close_that_arrives_before_its_open_is_remembered() {
        let now = Instant::now();
        let mut desk = Desk::new(now);
        let (early, early_rx) = call("early");
        let early_id = early.id.clone();
        assert_eq!(desk.close(&early_id, now), Close::Unknown);
        // The open turns up (a shifted queue): it must not paint a dialog the
        // person has already cancelled.
        assert!(!desk.open(early, now));
        assert!(desk.is_empty());
        assert!(desk.next(now).is_none());
        assert!(early_rx.try_recv().is_err());
        // And the memory is one-shot: the same path asked again is a request.
        let (again, _again_rx) = call("early");
        assert!(desk.open(again, now));
    }

    #[test]
    fn the_idle_clock_runs_while_nothing_is_showing() {
        let start = Instant::now();
        let mut desk = Desk::new(start);
        let idle = Duration::from_secs(30);
        assert_eq!(desk.idle_deadline(idle), start + idle);

        // A request waiting for the window is not idle.
        let (first, _first_rx) = call("one");
        assert!(desk.open(first, start + Duration::from_secs(5)));
        assert_eq!(
            desk.idle_deadline(idle),
            start + Duration::from_secs(35),
            "a request resets the clock"
        );
        let shown = desk.next(start + Duration::from_secs(5)).expect("shown");
        desk.answered(&shown.id, start + Duration::from_secs(10));
        assert_eq!(
            desk.idle_deadline(idle),
            start + Duration::from_secs(40),
            "the answer is the last activity, never the request"
        );
        assert!(
            desk.is_empty(),
            "the answered request is gone, not just hidden"
        );
    }

    /// Only a choice is an answer, and the filter comes back only when one was
    /// offered: the portal's `current_filter` is a row of a list it sent.
    #[test]
    fn only_a_choice_becomes_an_answer() {
        let offered =
            dtr_files::Pick::new(Request::open().with([dtr_files::Filter::of("Images", "*.png")]));
        offered.set_filter(0);
        let answer = portal_answer(vec![PathBuf::from("/tmp/a.png")], Some(&offered));
        assert_eq!(
            answer,
            Some(Answer::Chosen(Chosen {
                paths: vec![PathBuf::from("/tmp/a.png")],
                filter: Some(0),
            }))
        );
        let unfiltered = dtr_files::Pick::new(Request::open());
        assert_eq!(
            portal_answer(vec![PathBuf::from("/tmp/a.txt")], Some(&unfiltered)),
            Some(Answer::Chosen(Chosen {
                paths: vec![PathBuf::from("/tmp/a.txt")],
                filter: None,
            })),
            "no filters were offered, so no filter is echoed"
        );
        // `Pick` never answers a choice with nothing (it refuses instead); if it
        // ever did, the backend drops an empty `uris` and answers cancel.
        assert_eq!(
            portal_answer(vec![], None),
            Some(Answer::Chosen(Chosen {
                paths: vec![],
                filter: None,
            }))
        );
    }

    #[test]
    fn the_title_falls_back_to_the_question() {
        let (mut call, _rx) = call("titled");
        assert_eq!(title(&call), "Open a file");
        call.window.title.clear();
        assert_eq!(title(&call), "Open a file");
        call.request.select = Select::Directory;
        assert_eq!(title(&call), "Choose a folder");
        call.request.select = Select::Save;
        assert_eq!(title(&call), "Save a file");
        call.window.title = "Export as PDF".to_owned();
        assert_eq!(title(&call), "Export as PDF");
    }

    /// The screen registry: a window is visible while it is in the map, routing
    /// finds the one an answer names, and closing it leaves the rest.
    #[test]
    fn the_screen_registry_counts_and_routes_the_visible_windows() {
        let mut screens: Screens<u64, (&'static str, u64)> = Screens::new();
        assert_eq!(screens.visible(), 0, "nothing is visible before a window");
        screens.insert(10, ("picker", 1));
        screens.insert(20, ("manager", 7));
        assert_eq!(screens.visible(), 2);

        // The worker routes by the manager's routing id, not by window order.
        let routed = screens
            .iter()
            .find_map(|(window, (kind, id))| (*id == 7).then_some((*window, *kind)));
        assert_eq!(routed, Some((20, "manager")));
        // ...and a close for one window leaves the other visible.
        assert!(screens.remove(&20).is_some());
        assert_eq!(screens.visible(), 1);
        assert_eq!(
            screens.get(&10).map(|(kind, _)| *kind),
            Some("picker"),
            "a hidden picker screen is still the window it belongs to"
        );
        assert_eq!(screens.remove(&99), None);
        assert_eq!(screens.visible(), 1);
    }

    /// The idle policy: a visible window keeps the process alive; none means the
    /// idle timer is the exit, and `idle == 0` means forever.
    #[test]
    fn a_visible_window_suppresses_the_idle_exit() {
        let idle = Duration::from_secs(60);
        let last = Instant::now();
        assert_eq!(idle_exit(0, last, idle), Some(last + idle));
        assert_eq!(
            idle_exit(1, last, idle),
            None,
            "a manager window is a window: the daemon stays while it is on screen"
        );
        assert_eq!(
            idle_exit(3, last, idle),
            None,
            "several manager windows change nothing"
        );
        assert_eq!(
            idle_exit(0, last, Duration::ZERO),
            None,
            "zero keeps the process warm for ever"
        );
    }

    /// The capture hook only ever photographs a window it has a path for, and
    /// only once. Its `WindowId` half needs a real window, so what is testable
    /// here is the arming rule.
    #[test]
    fn a_capture_run_with_no_path_never_arms() {
        let capture = Capture::new(None, Duration::from_millis(50));
        assert_eq!(capture.deadline(), None);
        let capture = Capture::new(Some(PathBuf::from("/tmp/x.png")), Duration::from_millis(50));
        assert_eq!(
            capture.deadline(),
            None,
            "nothing is due before a window shows"
        );
    }
}
