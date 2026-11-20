//! The manager **screen**: one `dtr-files` browser with window chrome around it.
//!
//! `dtr_files::manager` is the painter (toolbar, sidebar, status bar) and
//! `dtr_files::pane_ui` is the content; this module is the *window* half of
//! `docs/manager.md` §6 — it owns the `Browser`, the history stacks, the sidebar's
//! data, the location field's draft and the search box, and it turns the
//! chrome's [`ManagerAnswer`] and the pane's [`PaneOutcome`] into a browser call.
//!
//! Three things make it more than a copy of the terminal's file pane:
//!
//! - **It has history.** Back/forward/up are the toolbar's buttons, and each
//!   navigation records where it came from. A history entry is a [`Where`] — a
//!   path *and* the ssh alias, because `/srv` on this machine and `/srv` on
//!   jupyter are different directories.
//! - **It can change machine.** A sidebar host row replaces the browser with
//!   `Browser::remote_at`; a path row replaces a remote browser with a local one.
//!   `Browser` has no "switch backend" call, so a switch is a new browser — the
//!   history is kept, and back returns to the other machine.
//! - **It is writable.** The browser reports a [`WriteRequest`] and this module
//!   hands it to the worker (`src/worker.rs`); the answer comes back as
//!   `write_succeeded`/`write_failed` and the listing refreshes. Nothing here
//!   touches a file.
//!
//! The picker's window (`src/window.rs`) still owns the event loop; this module
//! has no winit, no wgpu and no event proxy, which is what makes its history and
//! answer handling unit-testable without a display.

use std::path::{Path, PathBuf};

use dtr_files::manager::{
    self, EntryCount, LocationMode, ManagerAnswer, ManagerLayout, ManagerState, SelectionSummary,
    Sidebar, SidebarChoice,
};
use dtr_files::{Browser, Key, MenuChoice, PaneOutcome, SortChange, View};
use dtr_pane_host::listing::{FinishedListing, waiter};
use dtr_vfs::{Backend as _, Ssh};
use dtr_widgets::Skin;

use crate::desktop::Desktop;

/// A directory on a particular machine: what a history entry remembers.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Where {
    /// The ssh alias, or `None` on this machine.
    alias: Option<String>,
    path: PathBuf,
}

impl Where {
    fn local(path: impl Into<PathBuf>) -> Self {
        Self {
            alias: None,
            path: path.into(),
        }
    }

    fn remote(alias: &str, path: impl Into<PathBuf>) -> Self {
        Self {
            alias: Some(alias.to_owned()),
            path: path.into(),
        }
    }
}

/// One manager window's model.
pub struct Manager {
    /// The handle `OpenWindow` answered with, for `CloseWindow`.
    pub handle: String,
    /// The window's routing id for the worker's answers (never reused).
    pub id: u64,
    /// The listing, selection and view.
    browser: Browser,
    /// The person's home, for `~` in paths and the places section.
    home: PathBuf,
    /// Places, mounts and hosts, read once when the window opened.
    desktop: Desktop,
    /// Where back goes, most recent last.
    back: Vec<Where>,
    /// Where forward goes, most recent last.
    forward: Vec<Where>,
    /// The toolbar's location: breadcrumb, or the text field and its draft.
    location_mode: LocationMode,
    /// The text field's text while it is being edited.
    ///
    /// The chrome draws the field from `ManagerState.location`, so an edit is a
    /// *draft location*: the window writes what was typed there and the field
    /// reads it back next frame. `None` means "the browser's directory".
    draft: Option<PathBuf>,
    /// The search box's text. The chrome reports it; `dtr-files` has no search
    /// API yet, so this is typed and shown but does not filter — the gap is in
    /// the report (`docs/manager.md` §6 says the window owns the meaning).
    search: String,
    /// The last count the worker reported, for the status bar.
    entries: EntryCount,
    /// A count is wanted for the directory now on screen.
    count_wanted: bool,
    /// A write request is out with the worker; `take_write`'s contract is one at
    /// a time and the answer must land before the next is asked for.
    writing: bool,
}

impl Manager {
    /// A manager window on `start`, or on home when the caller named nothing.
    pub fn new(id: u64, handle: String, start: Option<PathBuf>, desktop: Desktop) -> Self {
        let home = desktop.home.clone();
        let path = start.unwrap_or_else(|| home.clone());
        let browser = Browser::new(Some(path));
        let manager = Self {
            handle,
            id,
            browser,
            home,
            desktop,
            back: Vec::new(),
            forward: Vec::new(),
            location_mode: LocationMode::Breadcrumb,
            draft: None,
            search: String::new(),
            entries: EntryCount::default(),
            count_wanted: true,
            writing: false,
        };
        manager
    }

    /// The caption: `Files — <path>`, with the host in front when remote.
    pub fn title(&self) -> String {
        let path = dtr_files::display_path(self.browser.directory(), self.state_home().as_path());
        match self.browser.alias() {
            Some(alias) => format!("Files — {alias}:{path}"),
            None => format!("Files — {path}"),
        }
    }

    /// The directory being shown.
    pub fn directory(&self) -> &Path {
        self.browser.directory()
    }

    /// The alias being browsed, if any.
    pub fn alias(&self) -> Option<&str> {
        self.browser.alias()
    }

    /// Whether the backend has the write verbs, for the keyboard's scope.
    ///
    /// The menu is already filtered by this (`Browser::caps`); the key table asks
    /// the same question so a read-only pane cannot take a chord that would only
    /// fail.
    pub fn can_write(&self) -> bool {
        self.browser.caps().write
    }

    /// Where this window is, as a history entry.
    fn current(&self) -> Where {
        Where {
            alias: self.browser.alias().map(str::to_owned),
            path: self.browser.directory().to_path_buf(),
        }
    }

    /// The home the *chrome* should use: the local one, or `.` while remote so
    /// `display_path` never abbreviates a remote path against a local home.
    fn state_home(&self) -> PathBuf {
        match self.alias() {
            Some(_) => PathBuf::from("."),
            None => self.home.clone(),
        }
    }

    /// The sidebar, rebuilt for the directory on screen.
    ///
    /// Cheap enough per frame — three small vectors over data read once — and
    /// doing it at draw time is what keeps a row's tick in step with a
    /// navigation that happened in the same frame.
    pub fn sidebar(&self) -> Sidebar {
        let remote = self.alias();
        // While remote, no *path* row can be the current one: a host's `/srv` is
        // not this machine's `/srv`. An empty path matches no place.
        let location = match remote {
            Some(_) => Path::new(""),
            None => self.browser.directory(),
        };
        Sidebar {
            places: Sidebar::places(&self.desktop.places, &self.home, location),
            drives: Sidebar::drives(&self.desktop.drives, location),
            hosts: Sidebar::hosts(&self.desktop.hosts, remote),
        }
    }

    /// What the chrome draws this frame.
    pub fn state(&self) -> ManagerState {
        let location = self
            .draft
            .clone()
            .unwrap_or_else(|| self.browser.directory().to_path_buf());
        let mut state = ManagerState::new(location, self.state_home(), self.browser.view());
        state.remote = self.alias().map(str::to_owned);
        state.location_mode = self.location_mode;
        state.search.clone_from(&self.search);
        state.entries = self.entries;
        state.entries.reading = self.browser.reading();
        let selected = self.browser.selected_paths();
        state.selection = SelectionSummary {
            count: selected.len(),
            // The browser does not expose a selected entry's size, and a total
            // summed with made-up zeroes is a smaller number than the truth:
            // `None` says "count only", which is what a remote pane does too.
            bytes: None,
        };
        state.can_go_back = !self.back.is_empty();
        state.can_go_forward = !self.forward.is_empty();
        state.can_go_up = match self.alias() {
            // `.` is the far account's home: it has no parent on this side of
            // the connection, and asking for one would list `/`.
            Some(_) => self.browser.directory() != Path::new("."),
            None => dtr_files::parent_of(self.browser.directory()).is_some(),
        };
        state
    }

    /// Draw the chrome and the browser into `ui`, and hand back what they asked
    /// for.
    ///
    /// The layout is computed once from the window's client rectangle and given
    /// to both the chrome and the browser, so the content cannot start at a
    /// different place than the chrome's toolbar ends (`AGENTS.md`, `pane_area`).
    pub fn ui(&self, ui: &mut egui::Ui, skin: &Skin) -> (ManagerAnswer, PaneOutcome) {
        let client = ui.max_rect();
        let layout = ManagerLayout::new(client, skin);
        let state = self.state();
        let sidebar = self.sidebar();
        let answer = manager::manager_ui(ui, &layout, skin, &state, &sidebar);
        let outcome = dtr_files::pane_ui(
            ui,
            egui::Id::unique(("dtr-portal-manager", self.id)),
            layout.content,
            &self.browser,
            skin,
            true,
        );
        (answer, outcome)
    }

    /// Take whatever a listing or a refresh delivered, and say whether the frame
    /// needs one.
    pub fn poll(&mut self, refresh_due: bool) -> bool {
        let mut repaint = self.browser.poll();
        if refresh_due {
            repaint |= self.browser.refresh_if_changed();
        }
        if repaint {
            // The listing changed under the count: ask the worker again.
            self.count_wanted = true;
        }
        repaint
    }

    /// The remote listing the browser is waiting for, if any.
    pub fn take_listing_request(&mut self) -> Option<(String, PathBuf)> {
        self.browser.take_request()
    }

    /// The local directory whose count the status bar needs, if one is due.
    ///
    /// A remote count arrives with its listing (`Browser::apply_listing` carries
    /// the entries); a local one is a second walk, and it exists only because
    /// `Browser` has no public entry count — see the gap in the report.
    pub fn take_count_request(&mut self) -> Option<PathBuf> {
        if !self.count_wanted {
            return None;
        }
        self.count_wanted = false;
        if self.alias().is_some() {
            return None;
        }
        Some(self.browser.directory().to_path_buf())
    }

    /// A remote listing arrived, whole.
    ///
    /// `dtr-pane-host`'s `waiter` is the routing and the stale-reply rule the
    /// terminal's pump uses too: host *and* directory *and* still waiting, so a
    /// reply that lost a race with a navigation is dropped rather than moving the
    /// view back. The rows are applied through the one handshake; a walk that
    /// ended with an error still shows the rows that arrived, and the footer says
    /// why (the same rule the terminal's panes follow).
    pub fn listed(&mut self, listing: FinishedListing) -> bool {
        if waiter(
            std::iter::once(&mut self.browser),
            &listing.host,
            &listing.path,
        )
        .is_none()
        {
            return false;
        }
        let total = listing.entries.len();
        let hidden = listing.entries.iter().filter(|entry| entry.hidden).count();
        self.entries = EntryCount {
            total,
            shown: total - hidden,
            reading: false,
        };
        listing.apply(&mut self.browser);
        true
    }

    /// A walk that never started: the backend could not be resolved, so there is
    /// no listing to adopt — only the footer's reason.
    pub fn listing_failed(&mut self, host: &str, path: &Path, message: &str) -> bool {
        if waiter(std::iter::once(&mut self.browser), host, path).is_none() {
            return false;
        }
        self.browser.apply_failure(message.to_owned());
        true
    }

    /// The worker counted a local directory.
    pub fn counted(&mut self, total: usize, shown: usize) {
        self.entries = EntryCount {
            total,
            shown,
            reading: self.browser.reading(),
        };
    }

    /// Take the next write request, if the worker is free and the browser has
    /// one.
    pub fn take_write(&mut self) -> Option<dtr_files::WriteRequest> {
        if self.writing {
            return None;
        }
        let request = self.browser.take_write()?;
        self.writing = true;
        Some(request)
    }

    /// A write request finished.
    pub fn write_succeeded(&mut self) {
        self.writing = false;
        if self.browser.write_succeeded() {
            self.after_write();
        }
    }

    /// A write request failed with the worker's message.
    pub fn write_failed(&mut self, message: &str) {
        self.writing = false;
        let err = anyhow::anyhow!("{message}");
        self.fold_write_failure(&err);
    }

    /// A write request was refused because the destination exists.
    ///
    /// The error is rebuilt with its own type — `dtr_files`'s overwrite card is
    /// chosen by downcasting to [`dtr_vfs::Conflict`], and a message string is not
    /// that type. The worker carried the destination across the thread for exactly
    /// this.
    pub fn write_conflicted(&mut self, destination: PathBuf) {
        self.writing = false;
        let err = anyhow::Error::new(dtr_vfs::Conflict { path: destination });
        self.fold_write_failure(&err);
    }

    fn fold_write_failure(&mut self, err: &anyhow::Error) {
        match self.browser.write_failed(err) {
            // The batch stopped (or was already idle): what succeeded before the
            // failure is on disk, so the listing is stale either way.
            dtr_files::Failure::Stopped | dtr_files::Failure::Idle => self.after_write(),
            // A conflict card is up; the answer is the pane's own, and the next
            // request comes from the browser once it is given.
            dtr_files::Failure::Asking | dtr_files::Failure::Answered => {}
        }
    }

    /// Every verb changes the listing: read it again, and ask the count again.
    fn after_write(&mut self) {
        self.browser.refresh();
        self.count_wanted = true;
    }

    /// The toolbar's back button, and `Alt+Left`.
    pub fn back(&mut self) {
        self.leave(Direction::Back);
    }

    /// The toolbar's forward button, and `Alt+Right`.
    pub fn forward(&mut self) {
        self.leave(Direction::Forward);
    }

    /// The toolbar's up button.
    pub fn up(&mut self) {
        let Some(parent) = dtr_files::parent_of(self.browser.directory()) else {
            return;
        };
        if self.alias().is_some() && self.browser.directory() == Path::new(".") {
            return;
        }
        let where_to = match self.alias() {
            Some(alias) => Where::remote(alias, parent),
            None => Where::local(parent),
        };
        self.go(where_to);
    }

    /// The grid/list toggle, from the toolbar or `Ctrl+1`/`Ctrl+2`.
    pub fn set_layout(&mut self, layout: dtr_files::Layout) {
        self.browser.set_layout(layout);
    }

    /// Show or hide hidden entries, the menu's `show hidden` row and `Ctrl+H`.
    pub fn toggle_hidden(&mut self) {
        let hidden = !self.browser.view().show_hidden;
        self.browser.set_hidden(hidden);
        self.count_wanted = true;
    }

    /// Whether the pane's right-press menu is up — it owns the keyboard while it
    /// is.
    pub fn menu_open(&self) -> bool {
        self.browser.menu_open()
    }

    /// Whether a field, an overwrite card or a confirmation wants the keyboard.
    pub fn write_modal(&self) -> bool {
        self.browser.write_modal()
    }

    /// The window's burger menu row: a request, not state.
    ///
    /// The pane's own right-press menu is a different list with a different
    /// answer ([`Self::menu_chosen`]) — this one is the *window's* view controls.
    fn window_menu(&mut self, choice: manager::MenuChoice) {
        match choice {
            manager::MenuChoice::Grid => self.browser.set_layout(dtr_files::Layout::Grid),
            manager::MenuChoice::List => self.browser.set_layout(dtr_files::Layout::List),
            manager::MenuChoice::EditLocation => self.set_location_mode(LocationMode::Edit),
            manager::MenuChoice::BreadcrumbLocation => {
                self.set_location_mode(LocationMode::Breadcrumb);
            }
            manager::MenuChoice::ToggleHidden => {
                let hidden = !self.browser.view().show_hidden;
                self.browser.set_hidden(hidden);
                self.count_wanted = true;
            }
        }
    }

    /// Dismiss the pane's right-press menu.
    pub fn close_menu(&mut self) {
        self.browser.close_menu();
    }

    /// Fold the chrome's answer into the browser.
    pub fn apply(&mut self, answer: ManagerAnswer) {
        if answer.back {
            self.back();
        }
        if answer.forward {
            self.forward();
        }
        if answer.up {
            self.up();
        }
        if let Some(path) = answer.navigate {
            let where_to = match self.alias() {
                Some(alias) => Where::remote(alias, path),
                None => Where::local(path),
            };
            self.go(where_to);
        }
        if let Some(choice) = answer.sidebar {
            self.sidebar_chosen(choice);
        }
        if let Some(search) = answer.search {
            self.search = search;
        }
        if let Some(layout) = answer.layout {
            self.browser.set_layout(layout);
        }
        if let Some(mode) = answer.location_mode {
            self.set_location_mode(mode);
        }
        if let Some(text) = answer.typed_location {
            // The field is drawn from the draft location, so a keystroke is the
            // new draft: writing it here is what makes the next frame's field
            // show what was typed instead of snapping back to the directory.
            self.draft = Some(PathBuf::from(text));
        }
        if answer.location_submitted {
            self.submit_location();
        }
        if let Some(choice) = answer.menu {
            self.window_menu(choice);
        }
    }

    /// Fold the pane's own answer into the browser.
    ///
    /// Returns whether the window should close: the pane's close cross is a
    /// manager window's close button, because with no window manager (the Xvfb
    /// capture) it is the only one there is.
    pub fn apply_pane(&mut self, outcome: PaneOutcome, ctx: &egui::Context) -> bool {
        if outcome.close {
            return true;
        }
        if let Some(change) = outcome.sort {
            let mut view: View = self.browser.view();
            match change {
                SortChange::Key(key) => view.sort = key,
                SortChange::Reverse => view.reverse = !view.reverse,
                SortChange::FoldersFirst => view.folders_first = !view.folders_first,
            }
            self.browser.set_view(view);
            self.count_wanted = true;
        }
        if let Some(path) = outcome.navigate {
            self.go(Where::local(path));
        }
        if let Some(choice) = outcome.menu {
            self.menu_chosen(choice, ctx);
        }
        false
    }

    /// One listing key, as the window maps it.
    ///
    /// `Browser::handle` may navigate (a folder's `Enter`, `Backspace`, a list's
    /// `Left`), so the history is recorded around it rather than only for the
    /// toolbar's own moves.
    pub fn handle(&mut self, key: Key) {
        let before = self.current();
        let opened = self.browser.handle(key);
        let after = self.current();
        if after != before {
            self.record(before);
        }
        if let Some(path) = opened {
            open_externally(&path);
        }
    }

    /// A left press in the content: a breadcrumb, a row, or nothing.
    ///
    /// The toolbar and sidebar are egui widgets and their presses never get
    /// here; the pane's header is painted, so its crumbs are asked back through
    /// the browser (`crumb_at`), the same handshake the terminal's pane uses.
    pub fn press(&mut self, at: egui::Pos2, clicks: u8, additive: bool, extend: bool) -> bool {
        if self.browser.menu_open() {
            if self.browser.menu_contains(at) {
                return false;
            }
            self.browser.close_menu();
            return true;
        }
        if let Some(path) = self.browser.crumb_at(at) {
            let where_to = match self.alias() {
                Some(alias) => Where::remote(alias, path),
                None => Where::local(path),
            };
            self.go(where_to);
            return true;
        }
        if let Some(path) = self.browser.clicked(at, clicks, additive, extend) {
            open_externally(&path);
        }
        true
    }

    /// A right press in the content: the pane's own menu, if a row is under it.
    pub fn right_press(&mut self, at: egui::Pos2) -> bool {
        self.browser.menu_at(at)
    }

    /// The wheel in the content.
    pub fn scroll(&mut self, lines: f64) {
        self.browser.scroll(lines);
    }

    // ---- navigation ---------------------------------------------------------

    /// Go somewhere, recording the history on the way.
    fn go(&mut self, to: Where) {
        if to == self.current() {
            // A press on the crumb of the directory already on screen is not a
            // navigation; recording one would make Back do nothing.
            return;
        }
        let before = self.current();
        self.open(to);
        self.record(before);
    }

    /// Open a where without recording: a new browser when the machine changed.
    fn open(&mut self, to: Where) {
        match (&to.alias, self.alias()) {
            (None, Some(_)) => {
                self.browser = Browser::new(Some(to.path));
            }
            (Some(alias), Some(open)) if alias == open => {
                self.browser.navigate(to.path);
            }
            (Some(alias), _) => {
                let mut browser = Browser::remote_at(alias, to.path);
                // `remote_at` is read-only by default; the ssh backend can write
                // and has no trash. Saying so is what makes the pane's menu offer
                // the verbs it can actually run (`docs/manager.md` §7).
                browser.set_caps(Ssh::new(alias.clone()).caps());
                self.browser = browser;
            }
            (None, None) => {
                self.browser.navigate(to.path);
            }
        }
        self.draft = None;
        self.count_wanted = true;
    }

    fn record(&mut self, from: Where) {
        self.back.push(from);
        self.forward.clear();
    }

    fn leave(&mut self, direction: Direction) {
        let from = match direction {
            Direction::Back => self.back.pop(),
            Direction::Forward => self.forward.pop(),
        };
        let Some(from) = from else {
            return;
        };
        let here = self.current();
        match direction {
            Direction::Back => self.forward.push(here),
            Direction::Forward => self.back.push(here),
        }
        self.open(from);
    }

    fn sidebar_chosen(&mut self, choice: SidebarChoice) {
        match choice {
            // A local place pressed while remote changes the machine, so it is
            // a new browser; `go` records the old one in the history first.
            SidebarChoice::Path(path) => self.go(Where::local(path)),
            SidebarChoice::Host { alias, home } => {
                let path = home.unwrap_or_else(|| PathBuf::from("."));
                self.go(Where::remote(&alias, path));
            }
        }
    }

    fn set_location_mode(&mut self, mode: LocationMode) {
        if mode == self.location_mode {
            return;
        }
        self.location_mode = mode;
        self.draft = match mode {
            LocationMode::Edit => Some(self.browser.directory().to_path_buf()),
            LocationMode::Breadcrumb => None,
        };
    }

    /// The editable location's Enter.
    ///
    /// `~` is the local home only; a remote location is written relative to the
    /// far account's home (`.`), because only the server knows where that is. A
    /// `host:` prefix names an ssh alias from the sidebar and switches machine —
    /// the one way to reach a host without a row in the sidebar.
    fn submit_location(&mut self) {
        let Some(draft) = self.draft.take() else {
            return;
        };
        let text = draft.to_string_lossy().trim().to_owned();
        self.location_mode = LocationMode::Breadcrumb;
        if text.is_empty() {
            return;
        }
        if let Some((host, rest)) = text.split_once(':')
            && !host.is_empty()
            && self.desktop.hosts.iter().any(|known| known.alias == host)
        {
            let path = remote_path(rest);
            self.go(Where::remote(host, path));
            return;
        }
        match self.alias() {
            // A typed absolute path while remote is another machine's path:
            // switching browsers is the only honest reading.
            Some(_) if Path::new(&text).is_absolute() => {
                let path = local_path(&text, &self.home);
                self.go(Where::local(path));
            }
            Some(alias) => {
                // A relative remote path is relative to the directory on
                // screen, not to the process's working directory: `.` on an
                // sftp connection is the far account's home, which is a
                // different directory entirely.
                let alias = alias.to_owned();
                let path = if text == "~" || text.starts_with("~/") {
                    remote_path(&text)
                } else {
                    self.browser.directory().join(&text)
                };
                self.go(Where::remote(&alias, path));
            }
            None => {
                let path = local_path(&text, &self.home);
                // A typed relative path is relative to the directory on screen,
                // the way a shell is: the daemon's own working directory is not a
                // place the person has ever been.
                let path = if path.is_absolute() {
                    path
                } else {
                    self.browser.directory().join(path)
                };
                self.go(Where::local(path));
            }
        }
    }

    fn menu_chosen(&mut self, choice: MenuChoice, ctx: &egui::Context) {
        match choice {
            MenuChoice::Open => {
                self.handle(Key::Enter);
            }
            MenuChoice::CopyPath => {
                let paths = self.browser.selected_paths();
                if paths.is_empty() {
                    return;
                }
                let text = paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                // egui's clipboard is in-app without a platform backend; the
                // window process has no arboard handle yet, and a path copied
                // inside this window is still a copy (`docs/manager.md` §6 lists
                // the clipboard as the window's work, not F4's gate).
                ctx.copy_text(text);
            }
            // The write rows open the pane's own field, card or confirmation;
            // the worker pump takes the requests they produce.
            choice => match choice {
                MenuChoice::NewFolder => self.browser.begin_new_folder(),
                MenuChoice::Rename => self.browser.begin_rename(),
                MenuChoice::Copy => self.browser.copy_selection(),
                MenuChoice::Paste => self.browser.paste(),
                MenuChoice::MakeCopy => self.browser.make_copy(),
                // The row's word is the machine's answer: a backend with no
                // trash says "delete permanently", so the request is the
                // permanent one; `delete(false)` asks only where a card must.
                MenuChoice::Delete => {
                    let permanent = !self.browser.caps().trash;
                    self.browser.delete(permanent);
                }
                _ => unreachable!("the read-only rows are handled above"),
            },
        }
    }
}

/// Which stack the toolbar's back/forward buttons move along.
#[derive(Clone, Copy)]
enum Direction {
    Back,
    Forward,
}

/// Open a file with the desktop's handler, best effort.
///
/// The path is this machine's (a remote `Enter` sets the browser's download
/// request instead), and the failure mode is a missing `xdg-open` on a machine
/// that has no desktop — logged, never a panic in an input handler.
fn open_externally(path: &Path) {
    if let Err(err) = dtr_vfs::open_externally(path) {
        tracing::warn!(%err, path = %path.display(), "could not open a file");
    }
}

/// A typed local location: `~`, `~/x`, an absolute path, or nothing useful.
fn local_path(text: &str, home: &Path) -> PathBuf {
    if text == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = text.strip_prefix("~/") {
        return home.join(rest);
    }
    PathBuf::from(text)
}

/// A typed remote path: `~` and `~/x` are the far home (`.`), because only the
/// server knows where that is; anything else is as written.
fn remote_path(text: &str) -> PathBuf {
    if text.is_empty() || text == "~" {
        return PathBuf::from(".");
    }
    if let Some(rest) = text.strip_prefix("~/") {
        return PathBuf::from(rest);
    }
    PathBuf::from(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dtr_files::Places;

    fn desktop(home: &Path) -> Desktop {
        Desktop::from_parts(home.to_path_buf(), Places::default(), None, None)
    }

    fn manager(start: &Path) -> Manager {
        Manager::new(
            1,
            "/org/dtr/Files/window/1".to_owned(),
            Some(start.to_path_buf()),
            desktop(start),
        )
    }

    /// The browser's first listing is streamed; a test that asks the model
    /// questions has to drain it first.
    fn settle(manager: &mut Manager) {
        for _ in 0..100 {
            if !manager.browser.reading() {
                break;
            }
            manager.browser.poll();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!manager.browser.reading(), "the listing never arrived");
    }

    #[test]
    fn a_fresh_window_opens_where_it_was_told_and_starts_with_no_history() {
        let scratch = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(scratch.path().join("pictures")).expect("dir");
        let manager = manager(scratch.path());
        let state = manager.state();
        assert_eq!(state.location, scratch.path());
        assert!(!state.can_go_back);
        assert!(!state.can_go_forward);
        assert!(state.can_go_up, "a directory under / can go up");
        let title = manager.title();
        assert!(title.starts_with("Files — "), "{title}");
    }

    #[test]
    fn back_and_forward_walk_the_history_across_machines() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let inner = scratch.path().join("pictures");
        std::fs::create_dir(&inner).expect("dir");
        let mut manager = manager(scratch.path());
        manager.go(Where::local(&inner));
        assert!(manager.state().can_go_back);
        assert!(!manager.state().can_go_forward);
        assert!(manager.state().location.ends_with("pictures"));

        // A host row is another machine; the local history survives.
        manager.go(Where::remote("jupyter", PathBuf::from(".")));
        assert_eq!(manager.alias(), Some("jupyter"));
        assert_eq!(
            manager.browser.caps(),
            Ssh::new("jupyter".to_owned()).caps()
        );
        assert!(manager.state().can_go_back);

        manager.back();
        assert_eq!(manager.alias(), None);
        assert!(manager.state().location.ends_with("pictures"));
        assert!(manager.state().can_go_forward);
    }

    #[test]
    fn pressing_the_crumb_of_the_directory_on_screen_is_not_a_navigation() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let mut manager = manager(scratch.path());
        let before = manager.back.len();
        manager.go(Where::local(scratch.path()));
        assert_eq!(manager.back.len(), before, "nothing was recorded");
    }

    #[test]
    fn the_editable_location_takes_a_tilde_and_a_host_prefix() {
        let home = PathBuf::from("/home/ada");
        assert_eq!(local_path("~", &home), home);
        assert_eq!(local_path("~/src", &home), PathBuf::from("/home/ada/src"));
        assert_eq!(local_path("/srv", &home), PathBuf::from("/srv"));
        assert_eq!(remote_path(""), PathBuf::from("."));
        assert_eq!(remote_path("~"), PathBuf::from("."));
        assert_eq!(remote_path("~/src"), PathBuf::from("src"));
        assert_eq!(remote_path("/var/log"), PathBuf::from("/var/log"));
    }

    #[test]
    fn a_typed_relative_location_lands_under_the_directory_on_screen() {
        let scratch = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(scratch.path().join("pictures")).expect("dir");
        let mut manager = manager(scratch.path());
        settle(&mut manager);
        manager.set_location_mode(LocationMode::Edit);
        manager.draft = Some(PathBuf::from("pictures"));
        manager.submit_location();
        assert_eq!(manager.browser.directory(), scratch.path().join("pictures"));
        assert_eq!(manager.location_mode, LocationMode::Breadcrumb);
        assert!(manager.state().can_go_back);
    }

    #[test]
    fn a_local_count_is_reported_when_the_ui_is_not_reading() {
        let scratch = tempfile::tempdir().expect("tempdir");
        std::fs::write(scratch.path().join("a.txt"), b"a").expect("file");
        std::fs::write(scratch.path().join(".hidden"), b"h").expect("file");
        let mut manager = manager(scratch.path());
        settle(&mut manager);
        manager.counted(2, 1);
        let state = manager.state();
        assert_eq!(state.entries.total, 2);
        assert_eq!(state.entries.shown, 1);
        assert!(!state.entries.reading);
    }

    #[test]
    fn the_sidebar_does_not_tick_a_place_that_only_looks_like_a_remote_path() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let mut manager = manager(scratch.path());
        // A host's `/` is not this machine's `/`: no path row may be selected.
        manager.go(Where::remote("jupyter", PathBuf::from("/")));
        let sidebar = manager.sidebar();
        assert!(
            sidebar
                .places
                .iter()
                .chain(sidebar.drives.iter())
                .all(|entry| !entry.selected),
            "a remote path selected a local place"
        );
        let hosts = Sidebar::hosts(
            &[dtr_files::manager::Host {
                alias: "jupyter".to_owned(),
                home: None,
            }],
            Some("jupyter"),
        );
        assert!(hosts[0].selected);
    }

    #[test]
    fn a_write_answer_refreshes_only_when_the_batch_is_done() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let mut manager = manager(scratch.path());
        assert!(!manager.writing);
        manager.write_succeeded();
        assert!(!manager.writing);
    }
}
