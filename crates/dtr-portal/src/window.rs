//! The picker **window**: one warm `dtr-frame` window, and the F2 browser inside
//! it answering a portal request.
//!
//! `docs/manager.md` §5 asks for a daemon, not a process per dialog: "D-Bus
//! activated, one window per request, kept hidden between requests, idle-exit
//! after a timeout when it owns no visible window. The first picker pays for a
//! process; every later one is a frame." This module is that window. The bus
//! half is `src/portal.rs`; the browser (listing, selection, footer, `Outcome`)
//! is `dtr-files`; the window, surface, DPI and egui loop are `dtr-frame`.
//!
//! # Why the window loop owns the process's main thread
//!
//! winit refuses to create its `EventLoop` off the main thread (0.30 panics with
//! "Initializing the event loop outside of the main thread…"; the X11/Wayland
//! `any_thread` extensions exist but are exactly the "nasty surprises when
//! porting" the panic is there to prevent). So the halves are the other way
//! round from the obvious shape: **the window loop is `main`'s work and the
//! D-Bus connection is the thread that moves.** `main` builds the loop, hands
//! the D-Bus side a [`WindowHost`], and runs the loop; the process lives exactly
//! as long as the loop does. That is also why `Host::open` is a `send_event`:
//! the bus task posts into winit's queue and returns.
//!
//! # One window, one picker at a time
//!
//! The window is created once (`resumed`), hidden, and only mapped when a request
//! arrives. A second request that turns up while the first is on screen is
//! **queued** ([`Desk`]) and shown when the first is answered or closed — which
//! is what the memo's single warm window can promise, and the frontend's method
//! call simply waits, which is what an async portal method is for. Closing a
//! request before the window has got to it drops it: the portal answered cancel
//! when it saw the `Close`, so the window has nothing to say
//! (`docs/portal.md` §4).
//!
//! # The state machine, without a window
//!
//! [`Desk`] is the whole of it — what is waiting, what is showing, and when the
//! window last did anything — and its unit tests build `Call`s with a real
//! `Reply` and no event loop, so the queueing, the two kinds of close and the
//! idle timer are proved without a display.
//!
//! # Verification hook
//!
//! `DTR_PORTAL_CAPTURE=/path.png` makes the first shown picker photograph its
//! own surface (`dtr_frame`'s `COPY_SRC` capture, the same path `dtr --capture`
//! uses) `DTR_PORTAL_CAPTURE_AFTER_MS` after it appears, default 700 ms. That is
//! the only picture of the *window*: the offscreen path draws a frame from
//! injected state, and what is being checked here is that Xvfb's window, the
//! surface, the skin and the listing agree.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use dtr_files::{Browser, Key, Outcome, PaneOutcome, Select, SortChange};
use dtr_frame::{Deadlines, Frame, FrameLoop, RenderOptions, Scene, WindowOptions, Windows};
use dtr_widgets::Skin;
use winit::event::{ElementState, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};
use winit::window::WindowId;

use crate::host::{Answer, Call, Chosen, Host, RequestId};
use crate::x11;

/// How often a visible picker asks its directory whether it changed — the same
/// twice-a-second `stat` dtr's file panes do (`Browser::refresh_if_changed`).
const REFRESH: Duration = Duration::from_millis(500);

/// How long the process stays warm with no picker on screen before it exits, so
/// that `xdg-desktop-portal` activates it fresh for the next request.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// How long after a picker appears the capture hook photographs it.
const CAPTURE_AFTER: Duration = Duration::from_millis(700);

/// A click this soon after the last one, and this close to it, is the second
/// click of a double-click: the same numbers dtr's own panes use.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
const DOUBLE_CLICK_SLOP: f32 = 4.0;

/// What the picker window is asked to do at startup.
///
/// The defaults are the production ones; [`Options::from_env`] is the one place
/// an environment variable changes them, so a capture run and an idle-timeout
/// test are the same binary with different surroundings.
#[derive(Clone, Debug)]
pub struct Options {
    /// The window stays warm for this long after the last request. Zero keeps it
    /// warm for ever, which is what a long verification run wants.
    pub idle: Duration,
    /// Write a PNG of the first shown picker here (`DTR_PORTAL_CAPTURE`).
    pub capture: Option<PathBuf>,
    /// How long after that picker appears to take the picture.
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
    /// These are hooks rather than preferences: a picker has no settings file
    /// (`docs/portal.md`), and the two that exist are for seeing the window and
    /// for keeping a verification run's daemon alive.
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

    /// The chrome the picker is painted in.
    ///
    /// **A documented fallback, not the last word.** dtr's `native` asks the
    /// desktop for its own palette through `crates/dtr/src/system`, which is the
    /// terminal's crate and needs a running desktop; this process has neither
    /// the crate nor a promise of a portal that answers. Adwaita is the right
    /// guess for a Linux desktop — it is what GNOME's own backend draws — and
    /// the scheme is the one thing a machine might disagree about, so
    /// `DTR_PORTAL_SCHEME` settles it until the platform reader moves into a
    /// crate both frontends can use (F4 needs the same answer).
    fn skin(&self) -> Skin {
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

/// A message from the D-Bus task to the window loop.
///
/// Public because it is the loop's event type (`FrameLoop<Event>`); nothing
/// outside this module sends one, which is what [`WindowHost`] is for.
#[derive(Debug)]
pub enum Event {
    /// Show this request.
    Open(Call),
    /// The frontend called `Request.Close()`: take the window down if it is up.
    Close(RequestId),
}

/// What the window loop tells the D-Bus side about the window it may show.
#[derive(Clone, Debug)]
enum Status {
    /// The window exists and is hidden: requests can be posted.
    Ready,
    /// No window could be created — no display, no adapter. Every request will
    /// be answered cancel with this reason in the log, and the process will exit
    /// at the idle timeout for activation to try again later.
    Unavailable(String),
}

/// The [`Host`] the portal calls: a proxy to the window loop, and what that loop
/// has said about itself.
///
/// `Host::open` runs on the D-Bus task under the portal's mutex, so everything
/// here is a non-blocking `send_event`/`try_recv`; the answer travels back
/// through the [`Call`]'s own [`Reply`](crate::Reply) when the person has made up
/// their mind.
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

/// Why a request left the queue without being shown.
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

/// The window's queue: at most one picker on screen, the rest waiting, and the
/// clock the idle timeout reads.
///
/// Deliberately window-free so the decisions are testable without a display: the
/// event loop only ever asks it "what is next", "did a close take the picker
/// down" and "may I exit yet".
struct Desk {
    waiting: VecDeque<Call>,
    showing: Option<RequestId>,
    /// Requests a frontend hung up on before this window heard of them.
    ///
    /// The wire cannot produce one — `src/portal.rs` calls `Host::open` before
    /// it can see a `Close`, and winit's proxy keeps sends in order — but a queue
    /// that ever shifted must drop the dialog its person is no longer waiting
    /// for rather than paint it. Bounded and aged, because a request path can be
    /// reused after its interaction ends.
    hung_up: VecDeque<(RequestId, Instant)>,
    last_active: Instant,
}

/// How long a remembered early close survives. Long enough to cover a reordered
/// queue, short enough that a reused handle path cannot be swallowed by it.
const EARLY_CLOSE: Duration = Duration::from_secs(5);
const EARLY_CLOSES: usize = 8;

impl Desk {
    fn new(now: Instant) -> Self {
        Self {
            waiting: VecDeque::new(),
            showing: None,
            hung_up: VecDeque::new(),
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
    fn idle_deadline(&self, idle: Duration) -> Instant {
        self.last_active + idle
    }

    fn forget_stale(&mut self, now: Instant) {
        self.hung_up
            .retain(|(_, at)| now.duration_since(*at) < EARLY_CLOSE);
    }
}

/// The request on screen: its call, the browser drawn for it, and the title.
struct Picker {
    call: Call,
    browser: Browser,
}

/// The frames: one hidden window, one picker at a time.
struct PickerApp {
    options: Options,
    status: async_channel::Sender<Status>,
    skin: Skin,
    started: Instant,
    frames_drawn: u64,
    desk: Desk,
    picker: Option<Picker>,
    window: Option<WindowId>,
    /// A window could not be created: requests are answered cancel, and the
    /// process exits at the idle timeout.
    failed: Option<String>,
    /// The last frame size, for the pointer, since egui's own input is only
    /// taken at frame time.
    modifiers: Modifiers,
    pointer: Option<egui::Pos2>,
    last_click: Option<(Instant, egui::Pos2)>,
    next_refresh: Instant,
    /// A capture hook waiting for its moment, and whether it has fired.
    capture_due: Option<(PathBuf, Instant)>,
    captured: bool,
}

impl PickerApp {
    fn new(options: Options, status: async_channel::Sender<Status>) -> Self {
        Self {
            skin: options.skin(),
            options,
            status,
            started: Instant::now(),
            frames_drawn: 0,
            desk: Desk::new(Instant::now()),
            picker: None,
            window: None,
            failed: None,
            modifiers: Modifiers::default(),
            pointer: None,
            last_click: None,
            next_refresh: Instant::now(),
            capture_due: None,
            captured: false,
        }
    }

    fn frame<'a>(&self, windows: &'a mut Windows) -> Option<&'a mut Frame> {
        windows.get_mut(self.window?)
    }

    /// Hide the window and forget the picker, without answering.
    ///
    /// The caller is a frontend `Close`: the portal has already answered cancel,
    /// so the window has nothing left to say (`Host::close`'s contract).
    fn drop_picker(&mut self, frame: &mut Frame) {
        let Some(picker) = self.picker.take() else {
            return;
        };
        tracing::debug!(request = %picker.call.id, "the picker went away without an answer");
        self.capture_due = None;
        self.forget_pointer();
        frame.window().set_visible(false);
    }

    /// Put the request on screen: build the browser, title the window, map it.
    fn show_next(&mut self, windows: &mut Windows, now: Instant) {
        let Some(frame) = self.frame(windows) else {
            return;
        };
        let Some(call) = self.desk.next(now) else {
            return;
        };

        let request = call.request.clone();
        // Where to start: the folder the application asked for, else the
        // person's home. The daemon's working directory is whatever D-Bus
        // activation left it in, and nobody chose that.
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
            // is not a window any more. A picker that is not transient still
            // picks files.
            tracing::debug!(
                %err,
                parent = %call.window.parent_window,
                "the picker is not transient for the asking window"
            );
        }
        frame.window().set_visible(true);
        frame.window().focus_window();
        frame.request_redraw();

        if !self.captured
            && let Some(path) = &self.options.capture
        {
            self.capture_due = Some((path.clone(), now + self.options.capture_after));
        }
        self.next_refresh = now + REFRESH;
        self.forget_pointer();
        tracing::debug!(request = %call.id, %title, "showing the picker");
        self.picker = Some(Picker { call, browser });
    }

    /// Answer the request on screen and take the window down.
    fn answer(&mut self, answer: Answer, frame: &mut Frame) {
        let Some(picker) = self.picker.take() else {
            return;
        };
        let id = picker.call.id.clone();
        if !picker.call.reply.send(answer) {
            // The frontend closed it first: `Reply` has one slot and the portal
            // answered cancel when the close won (`docs/portal.md` §4).
            tracing::debug!(request = %id, "answered a request that was already closed");
        }
        self.capture_due = None;
        self.forget_pointer();
        self.desk.answered(&id, Instant::now());
        frame.window().set_visible(false);
    }

    /// A press belongs to the picker it was made in: a position and a click
    /// count from the last request must not select a row in the next one.
    fn forget_pointer(&mut self) {
        self.pointer = None;
        self.last_click = None;
    }

    /// The footer's accept button, or `Enter`.
    fn accept(&mut self, frame: &mut Frame) {
        let Some(picker) = &self.picker else {
            return;
        };
        let Some(outcome) = picker.browser.accept_pick() else {
            return;
        };
        self.apply(outcome, frame);
    }

    /// `Escape`, or the footer's cancel.
    ///
    /// One button, one word, two depths: a save's `replace` question closes
    /// first, and the picker's next Escape is the cancel (`docs/manager.md` §4).
    fn cancel(&mut self, frame: &mut Frame) {
        let Some(picker) = &mut self.picker else {
            return;
        };
        picker.browser.cancel_pick();
        if picker.browser.is_picking() {
            frame.request_redraw();
            return;
        }
        self.answer(Answer::Cancelled, frame);
    }

    /// The window's close button, the header's cross, `Escape` on a picker with
    /// no question inside it: the person dismissed the dialog.
    fn dismiss(&mut self, frame: &mut Frame) {
        self.answer(Answer::Cancelled, frame);
    }

    /// Fold a picker `Outcome` into what the portal permits.
    ///
    /// Only `Chosen` and `Cancelled` reach the bus: `Refused` and `Replace` are
    /// the footer saying something and asking again, and `Navigate` is the
    /// browser going into a folder with the question still open
    /// (`docs/portal.md` §9).
    fn apply(&mut self, outcome: Outcome, frame: &mut Frame) {
        let answer = match outcome {
            Outcome::Chosen(paths) => {
                let pick = self
                    .picker
                    .as_ref()
                    .and_then(|picker| picker.browser.picking());
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
                if let Some(picker) = &mut self.picker {
                    picker.browser.navigate(folder);
                    frame.request_redraw();
                }
                None
            }
        };
        if let Some(answer) = answer {
            self.answer(answer, frame);
        }
    }

    /// One frame of the picker, and whatever the person did to it.
    fn redraw(&mut self, frame: &mut Frame) {
        let Some(mut picker) = self.picker.take() else {
            return;
        };
        let now = Instant::now();
        let mut repaint = picker.browser.poll();
        if now >= self.next_refresh {
            self.next_refresh = now + REFRESH;
            // A picker notices a file that appeared under it the way a pane
            // does: a `stat` of the directory it is showing, twice a second.
            repaint |= picker.browser.refresh_if_changed();
        }
        let capture = match &self.capture_due {
            Some((path, due)) if now >= *due => {
                let path = path.clone();
                self.capture_due = None;
                self.captured = true;
                tracing::info!(path = %path.display(), "photographing the picker window");
                Some(path)
            }
            _ => None,
        };

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
        self.picker = Some(picker);
        if let Some(outcome) = outcome {
            if outcome.close {
                // The pane's own close cross in a window is the window's close
                // button: dismiss the dialog.
                self.dismiss(frame);
                return;
            }
            if let Some(change) = outcome.sort {
                self.sort(change);
            }
            if let Some(path) = outcome.navigate {
                if let Some(picker) = &mut self.picker {
                    picker.browser.navigate(path);
                }
                repaint = true;
            }
            if let Some(outcome) = outcome.pick {
                self.apply(outcome, frame);
            } else if outcome.pick_cancel {
                self.cancel(frame);
            }
        }
        if repaint && self.picker.is_some() {
            frame.request_redraw();
        }
    }

    fn sort(&mut self, change: SortChange) {
        let Some(picker) = &mut self.picker else {
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

    /// A left press: the listing's selection, its double-click, and nothing
    /// else. A picker's answer is the footer's button, never a row.
    fn press(&mut self, frame: &mut Frame) {
        let Some(at) = self.pointer else {
            return;
        };
        let now = Instant::now();
        let clicks = match self.last_click {
            Some((when, last))
                if now.duration_since(when) < DOUBLE_CLICK
                    && last.distance(at) < DOUBLE_CLICK_SLOP =>
            {
                2
            }
            _ => 1,
        };
        self.last_click = Some((now, at));
        let modifiers = self.modifiers.state();
        let Some(picker) = &mut self.picker else {
            return;
        };
        // `clicked` opens a folder on its second click and reports the file a
        // single one would open in a manager; in a picker neither is an answer,
        // and a double-click on a file must not insert anything anywhere.
        let _ = picker
            .browser
            .clicked(at, clicks, modifiers.control_key(), modifiers.shift_key());
        frame.request_redraw();
    }

    /// One key, while a picker is up.
    fn key(&mut self, frame: &mut Frame, event: winit::event::KeyEvent) {
        // A release is not a press: the release of the keystroke that opened the
        // window arrives after it is open, and acting on both answers twice
        // (`AGENTS.md`, "a modal must ignore key releases").
        if event.state != ElementState::Pressed || self.picker.is_none() {
            return;
        }
        // A save's name field owns what is typed into it. egui's own text field
        // has the keyboard focus, and a listing that moved behind it would be a
        // picker typing into two places at once.
        let typing = frame.context().memory(|memory| memory.focused().is_some());
        match &event.logical_key {
            WinitKey::Named(NamedKey::Escape) => self.cancel(frame),
            WinitKey::Named(NamedKey::Enter) => self.accept(frame),
            _ if !typing => {
                if let Some(key) = browser_key(&event.logical_key, self.modifiers.state()) {
                    if let Some(picker) = &mut self.picker {
                        let _ = picker.browser.handle(key);
                    }
                    frame.request_redraw();
                }
            }
            _ => {}
        }
    }
}

impl dtr_frame::App for PickerApp {
    type Event = Event;

    fn resumed(&mut self, event_loop: &ActiveEventLoop, windows: &mut Windows) {
        // `resumed` runs again after a suspend; a window that is already there
        // is the whole answer.
        if self.window.is_some() || self.failed.is_some() {
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
        self.window = Some(frame.id());
        // The memo's warm window: created now so the adapter, device, context
        // and fonts are paid for before the first dialog needs them, and hidden
        // until there is a request to put in it.
        frame.window().set_visible(false);
        self.skin.apply(frame.context());
        let _ = self.status.try_send(Status::Ready);
        tracing::debug!(?self.window, "the picker window is warm and hidden");
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, windows: &mut Windows, event: Event) {
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
                    if let Some(frame) = self.frame(windows) {
                        self.drop_picker(frame);
                    }
                }
                Close::Queued => tracing::debug!(request = %id, "dropped a queued picker"),
                Close::Unknown => {
                    tracing::debug!(request = %id, "close for a request this window does not know")
                }
            },
        }
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        frame: &mut Frame,
        event: WindowEvent,
    ) {
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
            // The window manager's close button. A picker has no unsaved state,
            // so this is the person saying no.
            WindowEvent::CloseRequested => self.dismiss(frame),
            WindowEvent::Resized(size) => {
                // Recorded, not applied: the frame configures the swapchain once,
                // on the frame that draws the new size (`docs/frame.md`).
                frame.resize(size);
                frame.request_redraw();
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers,
            WindowEvent::CursorMoved { position, .. } => {
                // egui's own pointer is only readable at frame time, and the
                // picker has to know where a press landed when it arrives.
                let scale = frame.scale();
                self.pointer = Some(egui::pos2(
                    position.x as f32 / scale,
                    position.y as f32 / scale,
                ));
            }
            WindowEvent::CursorLeft { .. } => self.pointer = None,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.press(frame),
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(y),
                    MouseScrollDelta::PixelDelta(position) => position.y / 20.0,
                };
                if let Some(picker) = &mut self.picker {
                    picker.browser.scroll(lines);
                    frame.request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => self.key(frame, event),
            WindowEvent::RedrawRequested => self.redraw(frame),
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
        if self.picker.is_none() {
            self.show_next(windows, now);
        }
        if self.picker.is_some() {
            // Two clocks while a picker is up: the directory's `stat`, and the
            // capture hook's moment. Whichever is sooner is the wake; a deadline
            // already past must not be handed to `set_control_flow`, or every
            // poll times out and the loop spins (`AGENTS.md`).
            let capture = self.capture_due.as_ref().map(|(_, due)| *due);
            let wake = capture.map_or(self.next_refresh, |due| due.min(self.next_refresh));
            if wake > now {
                deadlines.soon(wake);
            } else if let Some(frame) = self.frame(windows) {
                frame.request_redraw();
            }
            return;
        }
        if self.options.idle.is_zero() {
            // Kept warm for ever: a verification run, or a person who would
            // rather the daemon never went away.
            return;
        }
        let deadline = self.desk.idle_deadline(self.options.idle);
        if now >= deadline {
            tracing::info!(
                idle_s = self.options.idle.as_secs(),
                "no picker for the idle timeout; exiting so D-Bus activation can start afresh"
            );
            event_loop.exit();
            return;
        }
        deadlines.soon(deadline);
    }
}

/// The underlay: the browser paints its own surface, so the pass under egui only
/// has to clear to the skin's card colour. It is covered a moment later, and if
/// a frame ever shows through, it is the colour the pane would have been.
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
/// because egui prefers to do its own conversion), so these are the byte values
/// as fractions — not a linearisation, which would darken what egui paints over
/// it in the same frame.
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
/// Split out from [`PickerApp::apply`] because it is the decideable half, and a
/// test can ask it without a window.
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

/// One key of a listing, from one key of the keyboard.
///
/// The picker's own keys are `Enter` and `Escape` (`PickerApp::key`); everything
/// else here is movement, and it is the same mapping dtr's file panes use so a
/// person does not have to learn a second one. `Alt+Up` is the parent in a grid,
/// where `Left` is a movement.
fn browser_key(key: &WinitKey, modifiers: ModifiersState) -> Option<Key> {
    if modifiers.alt_key() || modifiers.control_key() || modifiers.super_key() {
        return match key {
            WinitKey::Named(NamedKey::ArrowUp)
                if modifiers.alt_key() && !modifiers.control_key() =>
            {
                Some(Key::Parent)
            }
            _ => None,
        };
    }
    match key {
        WinitKey::Named(NamedKey::ArrowUp) => Some(Key::Up),
        WinitKey::Named(NamedKey::ArrowDown) => Some(Key::Down),
        WinitKey::Named(NamedKey::ArrowLeft) => Some(Key::Left),
        WinitKey::Named(NamedKey::ArrowRight) => Some(Key::Right),
        WinitKey::Named(NamedKey::PageUp) => Some(Key::PageUp),
        WinitKey::Named(NamedKey::PageDown) => Some(Key::PageDown),
        WinitKey::Named(NamedKey::Home) => Some(Key::Top),
        WinitKey::Named(NamedKey::End) => Some(Key::Bottom),
        // `Enter` is the accept button in a picker, so it is not a listing key.
        WinitKey::Named(NamedKey::Backspace) => Some(Key::Parent),
        WinitKey::Character(text) => match text.as_str() {
            "h" => Some(Key::Left),
            "j" => Some(Key::Down),
            "k" => Some(Key::Up),
            "l" => Some(Key::Right),
            "r" => Some(Key::Reload),
            "g" => Some(Key::Top),
            "G" => Some(Key::Bottom),
            _ => None,
        },
        _ => None,
    }
}

/// A running picker loop, and the host the D-Bus side was given.
///
/// Splitting "build the loop" from "run the loop" is what makes the two halves
/// possible: `main` starts the bus on the host, waits for its handshake, and only
/// then enters the loop.
pub struct Run {
    frames: FrameLoop<Event>,
    app: PickerApp,
}

impl Run {
    /// Run the window loop until the idle timeout ends it.
    ///
    /// Nothing here returns while a picker is up; the loop ends only when the
    /// window has been unused for [`Options::idle`], which is the signal for the
    /// process — and therefore the bus — to go away until activation needs it.
    pub fn run(self) -> Result<(), String> {
        self.frames
            .run(self.app)
            .map_err(|err| format!("the picker's event loop: {err}"))
    }
}

/// Build the picker loop on this thread, and the host to serve it with.
///
/// **Must be called on the main thread**: winit's `EventLoop` panics anywhere
/// else, which is why the D-Bus half is the thread that moves. `Err` is "this
/// machine has no display to open a window on" — the caller serves the portal
/// with a host that answers cancel (`main.rs`), rather than exiting and leaving
/// every application with a broken dialog.
pub fn start(options: Options) -> Result<(Run, WindowHost), String> {
    let frames = FrameLoop::<Event>::new().map_err(|err| format!("no window loop: {err}"))?;
    let proxy = frames.proxy();
    let (status, status_rx) = async_channel::unbounded();
    let app = PickerApp::new(options, status);
    Ok((Run { frames, app }, WindowHost::new(proxy, status_rx)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{Reply, WindowContext};
    use crate::{Request, Select};
    use std::path::PathBuf;

    /// A request the way `src/portal.rs` builds one, with a receiver so a test
    /// can tell "not answered" from "answered".
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
}
