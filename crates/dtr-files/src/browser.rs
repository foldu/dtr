//! The browser itself: a directory listing, a selection, and the rows that paint
//! them.
//!
//! It is deliberately small still: a listing, a selection, and the one thing a
//! terminal wants from a file manager — "put this path on the command line". It
//! is not an editor, and it does not shell out to `ls`. The grid, the places
//! sidebar and the writes are `docs/manager.md`'s F1b and later; this is what
//! they are built on.
//!
//! The filesystem-facing part is a thin layer over [`std::fs::read_dir`]. The
//! decisions that are easy to get wrong (ordering, selection clamping, what
//! `..` means, how a path under `$HOME` is shown) live in plain functions that
//! take values, so their tests never touch a disk.
//!
//! Nothing here knows what it is drawn inside: a pane in a terminal tab, a
//! manager window or the desktop's picker all call [`pane_ui`] with a skin and a
//! rectangle in points.

use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use egui::{Align2, Stroke};

use dtr_vfs::{Backend as _, Local};
use dtr_widgets::{Glyph, Icon, IconTile, Skin, icon_tile, tile_height};

use crate::grid::{cell_rect, columns, first_row, index_at, row_of, rows, visible_rows};
use crate::theme::IconSet;
use crate::thumb::{Thumbs, looks_like_image};

const ROW_HEIGHT: f32 = 18.0;
/// The gap between two grid cells, and the inset of the grid inside the body:
/// two numbers because a gap between tiles and a margin around them are
/// different things that happened to be equal once.
const TILE_GAP: f32 = 6.0;
const TILE_PAD: f32 = 4.0;
const HEADER_HEIGHT: f32 = 26.0;
const FOOTER_HEIGHT: f32 = 18.0;
/// The header's close button: a square that fits inside the header, so a press
/// on its top edge is still a press on the header.
const CLOSE_SIDE: f32 = 20.0;
/// The sort control's width, fixed rather than measured.
///
/// A header is a row of things that must not move when a word gets longer:
/// `name` and `modified` share one slot, and the label elides inside it. The
/// count sits to its left and grows leftwards, so the control never moves.
const SORT_WIDTH: f32 = 72.0;
/// The places button's slot at the header's left: a glyph, not a word.
const PLACES_WIDTH: f32 = 22.0;

/// One line in the listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    /// File size in bytes; `None` for directories and for entries the OS would
    /// not report one for.
    pub size: Option<u64>,
    /// When it last changed, if anything would say. `None` is a real answer — a
    /// permission denied, a backend that does not carry a time — and it sorts
    /// last in both directions rather than pretending to be the newest.
    pub modified: Option<SystemTime>,
}

impl From<dtr_vfs::Entry> for Entry {
    fn from(entry: dtr_vfs::Entry) -> Self {
        Self {
            name: entry.name,
            is_dir: entry.kind.is_dir(),
            size: entry.len,
            modified: entry.modified,
        }
    }
}

impl Entry {
    fn parent() -> Self {
        Self {
            name: "..".to_owned(),
            is_dir: true,
            size: None,
            modified: None,
        }
    }

    /// The synthetic `..` row, as opposed to a directory genuinely named that.
    pub fn is_parent(&self) -> bool {
        self.name == ".." && self.size.is_none()
    }
}

/// What the focused file pane does with a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    /// Left is a movement in a grid and "the parent directory" in a list, which
    /// is why the parent has a key of its own ([`Key::Parent`]) as well.
    Left,
    Right,
    Enter,
    Reload,
    Top,
    Bottom,
    PageUp,
    PageDown,
    /// The parent directory, whatever the view: `Backspace` and `Alt+Up`.
    Parent,
}

/// What a listing is ordered by.
///
/// Four keys, and deliberately not a fifth: **arrival order** is what a streamed
/// view shows while a walk is still running, and a directory whose `readdir`
/// order carries meaning is what a shell is for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sort {
    #[default]
    Name,
    Modified,
    Size,
    /// The extension, which is what a file manager means by "type".
    Kind,
}

impl Sort {
    /// Every key, in the order a selector offers them.
    pub const ALL: [Sort; 4] = [Sort::Name, Sort::Modified, Sort::Size, Sort::Kind];

    /// The word a selector shows.
    pub fn label(self) -> &'static str {
        match self {
            Sort::Name => "name",
            Sort::Modified => "modified",
            Sort::Size => "size",
            Sort::Kind => "type",
        }
    }

    /// Whether ordering by this key needs a `stat` per entry.
    ///
    /// The load-bearing question of the whole listing: a **name** sort needs no
    /// metadata at all, so the common case is a `readdir` and a sort, with the
    /// size and date columns filled in for the rows on screen.
    pub fn needs_facts(self) -> bool {
        matches!(self, Sort::Modified | Sort::Size)
    }
}

/// How a listing is laid out.
///
/// **The grid is the default**, as it is in Nautilus, and the list is kept in the
/// same milestone rather than rewritten later: a pane in a terminal is often 450
/// points wide, where large tiles give four columns of short names and ten rows
/// fit in the same height. Grid-first is a preference, not an excuse to lose the
/// dense view (`docs/manager.md` §3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Grid,
    List,
}

impl Mode {
    pub const ALL: [Mode; 2] = [Mode::Grid, Mode::List];

    pub fn label(self) -> &'static str {
        match self {
            Mode::Grid => "grid",
            Mode::List => "list",
        }
    }
}

/// How a listing is ordered and filtered.
///
/// One value rather than four arguments, because the order is remembered (the
/// `[files]` table of the config file) and because a filter and a sort applied in
/// two places is two answers to "what is on screen".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct View {
    pub mode: Mode,
    pub sort: Sort,
    /// Reverses the **key**, not the grouping: under `modified ↓` folders stay
    /// on top and only the order inside each group turns around.
    pub reverse: bool,
    /// Folders above files, which is every file manager's default.
    pub folders_first: bool,
    /// Hidden entries, which `Ctrl+H` toggles. Not part of the sort, but it is
    /// the same pass over the same list, and a filter and a sort are one answer.
    pub show_hidden: bool,
}

impl View {
    /// The order in words, for a footer or a selector.
    ///
    /// Words and not arrows: egui's faces have none, and a `↓` in a label panics
    /// the harness and draws an empty box in the window (`AGENTS.md`, glyphs).
    pub fn sort_label(&self) -> String {
        match self.reverse {
            true => format!("{} reversed", self.sort.label()),
            false => self.sort.label().to_owned(),
        }
    }
}

impl Default for View {
    fn default() -> Self {
        Self {
            mode: Mode::Grid,
            sort: Sort::Name,
            reverse: false,
            folders_first: true,
            show_hidden: false,
        }
    }
}

/// One frame's layout, recorded for the press that comes after it.
#[derive(Clone, Copy, Debug)]
struct Frame {
    mode: Mode,
    /// The listing's own rectangle, in the pane's points.
    body: egui::Rect,
    /// Where the grid's first cell starts, unscrolled, and how wide a cell is.
    content: egui::Rect,
    cell: egui::Vec2,
    gap: f32,
    /// Cells across, the first row drawn, and the rows or cells it showed.
    across: usize,
    first: usize,
    visible: usize,
}

/// A file pane: a directory, its entries, and where the selection is.
pub struct Browser {
    cwd: PathBuf,
    home: PathBuf,
    entries: Vec<Entry>,
    /// The entries the view hides, sorted by the same key. Kept rather than
    /// dropped so `Ctrl+H` is a redraw: see [`arrange_split`].
    hidden: Vec<Entry>,
    selected: usize,
    /// First row the user scrolled to. The drawing code raises it if the
    /// selection would otherwise be off-screen, so this is a hint rather than
    /// an invariant.
    scroll: Cell<usize>,
    /// How the last frame laid the listing out, in cells across and per page.
    ///
    /// Both are the draw's business — only it knows the pane's width — and both
    /// are needed between frames by a key: `Down` in a grid moves a row, and a
    /// row is the column count.
    columns: Cell<usize>,
    page: Cell<usize>,
    /// What it is sorted by and what it hides. The selector and the config file
    /// are its two writers, and both go through `set_view`.
    view: View,
    /// A filesystem problem to show in the pane itself. The pane stays usable
    /// and its header stays visible; it never goes blank.
    message: Option<String>,
    /// Set when this pane browses another machine over SFTP. `None` is the
    /// local filesystem.
    remote: Option<Remote>,
    /// Where a local listing comes from. One per browser: the backend is
    /// stateless, and `list` starts its own thread, so no runtime is needed to
    /// ask for a directory.
    local: Local,
    /// The walk in flight, if any.
    incoming: Option<Incoming>,
    /// The metadata pass in flight, if any.
    refining: Option<Refining>,
    /// The directory whose sizes and times are all known: a re-sort re-uses them
    /// rather than asking again.
    facts_for: Option<PathBuf>,
    /// The rows picked *beside* the focus, by name.
    ///
    /// By name and not by index for the reason `selected` is restored by name: a
    /// batch arriving and rows re-sorting make an index point at a different
    /// file. The focus is never in here — it is always selected, and this is what
    /// ctrl- and shift-presses add to it.
    chosen: Vec<String>,
    /// Where a shift-press starts: the last row picked on its own.
    anchor: Option<String>,
    /// The directory's own modification time, as the last walk saw it.
    ///
    /// A directory's mtime changes when an entry is created, removed or renamed —
    /// which is exactly what a listing cares about — so it is the whole state a
    /// poll needs. It does **not** change when a file's contents change, and a
    /// listing does not show contents.
    listing_seen: Cell<Option<SystemTime>>,
    /// Whether the next frame should bring the selection into view.
    ///
    /// **The selection and the view are two things.** A key or a press moves the
    /// selection, and the view follows it; a wheel moves the view alone, and it
    /// may leave the selection off screen — Nautilus lets you scroll away from
    /// what you picked, and a listing that yanked itself back would make the
    /// wheel useless in a long directory.
    reveal: Cell<bool>,
    /// What the last frame drew and where: the hit test's copy of the layout.
    ///
    /// The painter and the press have to agree about which tile is under a
    /// point, and the pane is painted rather than made of widgets
    /// (`docs/files.md` §13) — so the draw records the numbers it used and the
    /// press asks them back. Two calculations from one snapshot instead of two
    /// snapshots.
    frame: Cell<Option<Frame>>,
    /// The breadcrumb's targets, as the last frame drew them.
    ///
    /// The header is painted, so a crumb is not a widget: the draw records where
    /// each segment landed and the press asks that record back — the same bargain
    /// `frame` makes for the tiles.
    crumbs: RefCell<Vec<(egui::Rect, PathBuf)>>,
    /// The pictures the grid draws, and the theme they come from. Keyed by entry
    /// path, read through a per-frame budget, and replaceable with an empty set
    /// by a test whose baseline may not depend on the machine's icon theme.
    icons: RefCell<IconSet>,
    /// The pictures of the images in view, decoded off this thread and uploaded
    /// when they arrive.
    thumbs: RefCell<Thumbs>,
    /// A name to select once the listing that carries it arrives.
    ///
    /// `dtr --files some/file` names a file the walk has not sent yet, so the
    /// selection is a promise rather than a lookup.
    wanted: Option<String>,
}

/// A listing on its way in.
struct Incoming {
    /// The directory it lists. A batch for another one is a reply to a request
    /// this pane has moved on from, and is dropped.
    dir: PathBuf,
    batches: dtr_vfs::Batches,
}

/// A metadata pass on its way in.
///
/// The rows do not move while it runs: the order is applied **once**, when the
/// pass ends. Re-sorting as each batch lands would shuffle rows under the
/// pointer continuously, which is worse than a second of the previous order.
struct Refining {
    batches: dtr_vfs::Refinements,
    /// Name to row, built once for the pass.
    rows: std::collections::HashMap<String, usize>,
    total: usize,
    answered: usize,
}

/// What a remote pane needs from the SFTP worker.
#[derive(Clone, Debug)]
pub struct Remote {
    /// The ssh alias the listing comes from.
    pub alias: String,
    /// A directory whose listing has been asked for and not sent yet.
    pending: Option<PathBuf>,
    /// The directory the worker is listing now.
    ///
    /// The request is consumed when it is handed to the worker, so this has to
    /// outlive it: matching a reply against `pending` matched nothing, and every
    /// listing was dropped on the floor for a pane that was still loading.
    awaiting: Option<PathBuf>,
    /// The bytes path of a download the user asked for, for the app to start.
    pub download: Option<PathBuf>,
}

impl Browser {
    /// Open at `start`, or at the process's current directory when there is no
    /// start path. A path that names a file shows its parent directory and
    /// selects the file, which is what `dtr --files some/file` should mean.
    /// A pane over another machine. Nothing is read here: the app asks the SFTP
    /// worker for a listing and hands it back with `apply_listing`.
    pub fn remote(alias: &str) -> Self {
        Self::remote_at(alias, PathBuf::from("."))
    }

    /// The same, opening at `path`. `.` is the home directory, because only the
    /// server knows where that is; a path from a link is already absolute.
    pub fn remote_at(alias: &str, path: PathBuf) -> Self {
        let mut browser = Self {
            cwd: path,
            home: PathBuf::from("."),
            entries: Vec::new(),
            hidden: Vec::new(),
            selected: 0,
            scroll: Cell::new(0),
            columns: Cell::new(1),
            page: Cell::new(0),
            view: View::default(),
            chosen: Vec::new(),
            anchor: None,
            listing_seen: Cell::new(None),
            reveal: Cell::new(false),
            crumbs: RefCell::new(Vec::new()),
            frame: Cell::new(None),
            icons: RefCell::new(IconSet::system()),
            thumbs: RefCell::new(Thumbs::default()),
            message: None,
            remote: Some(Remote {
                alias: alias.to_owned(),
                pending: None,
                awaiting: None,
                download: None,
            }),
            local: Local::new(),
            incoming: None,
            refining: None,
            facts_for: None,
            wanted: None,
        };
        browser.remote_request();
        browser
    }

    /// The alias this pane browses, when it browses one.
    pub fn alias(&self) -> Option<&str> {
        self.remote.as_ref().map(|remote| remote.alias.as_str())
    }

    /// The directory the worker should list now, if the pane is waiting.
    pub fn take_request(&mut self) -> Option<(String, PathBuf)> {
        let remote = self.remote.as_mut()?;
        let path = remote.pending.take()?;
        remote.awaiting = Some(path.clone());
        Some((remote.alias.clone(), path))
    }

    /// Is this pane waiting for exactly this listing?
    ///
    /// A reply names the directory it answers, and two panes can browse one host,
    /// so routing is by what the pane asked for — not by which pane is focused,
    /// which is what used to drop every reply to a pane the user was not in.
    pub fn is_waiting_for(&self, path: &Path) -> bool {
        self.remote
            .as_ref()
            .and_then(|remote| remote.awaiting.as_deref())
            == Some(path)
    }

    /// A listing arrived. Ignored when the pane is waiting on another directory,
    /// so a slow reply cannot yank the view back.
    pub fn apply_listing(&mut self, path: PathBuf, entries: Vec<Entry>) {
        let Some(remote) = self.remote.as_mut() else {
            return;
        };
        if remote
            .awaiting
            .as_deref()
            .is_some_and(|awaiting| awaiting != path.as_path())
        {
            return;
        }
        remote.awaiting = None;
        self.cwd = path;
        self.message = None;
        self.adopt(entries);
        self.selected = clamp_selection(self.selected, self.entries.len());
    }

    /// What this pane is sorted by and what it hides.
    /// Whether a row is part of the selection.
    ///
    /// **The focus is not the selection.** The cursor and what is highlighted are
    /// two things — a ctrl-press on the row the cursor is on takes it out of the
    /// pick and leaves the cursor there, which is what "deselect this one" has to
    /// mean — so this asks `chosen` and nothing else.
    pub fn is_selected(&self, index: usize) -> bool {
        self.entries
            .get(index)
            .is_some_and(|entry| self.chosen.iter().any(|name| *name == entry.name))
    }

    /// Move the cursor to a row and make it the whole selection, without asking
    /// the view to follow.
    ///
    /// The version a batch needs: restoring the selection by name after a merge
    /// must not scroll the page to it on every batch of a stream.
    fn focus(&mut self, index: usize) {
        self.selected = index;
        self.chosen = match self.entries.get(index) {
            Some(entry) => vec![entry.name.clone()],
            None => Vec::new(),
        };
        self.anchor = self.entries.get(index).map(|entry| entry.name.clone());
    }

    /// Select exactly one row and bring it into view: what a plain press, a
    /// keyboard move, and a caller that starts from a selection all do.
    pub fn select_only(&mut self, index: usize) {
        self.focus(index);
        self.reveal.set(true);
    }

    /// How the selection reads in a footer: one selection is the path's job, and
    /// several are a number.
    pub fn selection_label(&self) -> Option<String> {
        let total = self.chosen.len();
        (total > 1).then(|| format!("{total} selected"))
    }

    /// Every selected path, in the order the listing shows them.
    pub fn selected_paths(&self) -> Vec<PathBuf> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(index, _)| self.is_selected(*index))
            .filter_map(|(_, entry)| match entry.is_parent() {
                // The parent row is where it goes, not `cwd/..`.
                true => parent_of(&self.cwd),
                false => Some(self.cwd.join(&entry.name)),
            })
            .collect()
    }

    /// The path the selection names, for a copy or a command line.
    ///
    /// The parent row is the directory above, so a press on `..` copies where it
    /// goes rather than the two characters it shows.
    pub fn selected_path(&self) -> Option<PathBuf> {
        let entry = self.selected_entry()?;
        if entry.is_parent() {
            return parent_of(&self.cwd);
        }
        Some(self.cwd.join(&entry.name))
    }

    /// The places a popup offers: home, the desktop's decorated directories, and
    /// the root of the filesystem.
    ///
    /// Built on demand and owned, so the popup holds no borrow of the icon set
    /// while it draws: a `RefCell` borrow across a closure that calls back into
    /// the same browser is a panic waiting for a slow frame.
    pub fn place_list(&self) -> Vec<(String, PathBuf)> {
        let mut places = vec![("home".to_owned(), self.home.clone())];
        for (path, _icon) in self.icons.borrow().places().iter() {
            // A place equal to home is the row above it.
            if path != self.home {
                places.push((display_path(path, &self.home), path.to_path_buf()));
            }
        }
        places.push(("filesystem".to_owned(), PathBuf::from("/")));
        places
    }

    /// Whether a directory is the one this pane is showing, for a tick in a menu.
    pub fn showing(&self, path: &Path) -> bool {
        path == self.cwd
    }

    /// The breadcrumb segment under a point, and where it goes.
    ///
    /// The record the last draw left: the header is painted, so there is no
    /// widget to hit-test and the geometry is the draw's to remember.
    pub fn crumb_at(&self, point: egui::Pos2) -> Option<PathBuf> {
        self.crumbs
            .borrow()
            .iter()
            .find(|(rect, _)| rect.contains(point))
            .map(|(_, path)| path.clone())
    }

    /// Go to a directory the pane already knows about — a breadcrumb's answer.
    ///
    /// A press on the *last* crumb is a press on the directory already open, and
    /// it does nothing rather than re-reading the directory it is showing.
    pub fn navigate(&mut self, path: PathBuf) {
        if path != self.cwd {
            self.cd(path);
        }
    }

    /// Which entry the point in this pane is over.
    ///
    /// `None` is a gap, a margin, the header, or past the last entry — the places
    /// a press selects nothing and must not clear a selection by accident.
    pub fn entry_at(&self, point: egui::Pos2) -> Option<usize> {
        let frame = self.frame.get()?;
        if !frame.body.contains(point) {
            return None;
        }
        match frame.mode {
            Mode::Grid => index_at(
                point,
                frame.across,
                frame.cell,
                frame.gap,
                frame.content.min,
                frame.first,
                self.entries.len(),
            ),
            Mode::List => {
                let row = ((point.y - frame.body.top()) / ROW_HEIGHT).floor();
                if row < 0.0 || row as usize >= frame.visible {
                    return None;
                }
                let index = frame.first + row as usize;
                (index < self.entries.len()).then_some(index)
            }
        }
    }

    /// Pick a row the way a press does.
    ///
    /// Three gestures, and they are the three every file manager has:
    ///
    /// - a **plain** press selects one row and forgets the rest;
    /// - **ctrl** adds the row to the selection or takes it out again, and the row
    ///   that had the focus stays picked;
    /// - **shift** takes everything from the anchor — the last row picked on its
    ///   own — to this one.
    ///
    /// The focus always follows the press, even when the press un-picked the row:
    /// it is the row `Enter` acts on, and a cursor that could not be moved to an
    /// unselected row would make "deselect this one" a gesture with no way back.
    fn pick(&mut self, index: usize, additive: bool, extend: bool) {
        if index >= self.entries.len() {
            return;
        }
        let name = self.entries[index].name.clone();
        if extend {
            let anchor = self
                .anchor
                .clone()
                .unwrap_or_else(|| self.entries[self.selected].name.clone());
            let from = self
                .entries
                .iter()
                .position(|entry| entry.name == anchor)
                .unwrap_or(self.selected);
            let (low, high) = if from <= index {
                (from, index)
            } else {
                (index, from)
            };
            self.chosen = self.entries[low..=high]
                .iter()
                .map(|entry| entry.name.clone())
                .collect();
            self.selected = index;
            self.reveal.set(true);
            return;
        }
        if additive {
            if let Some(at) = self.chosen.iter().position(|picked| *picked == name) {
                // Taking a row out of the pick does not move the cursor: the row
                // `Enter` acts on stays where it was, which is the only way back to
                // it.
                self.chosen.remove(at);
                return;
            }
            let was = self.entries[self.selected].name.clone();
            if !self.chosen.iter().any(|picked| *picked == was) {
                self.chosen.push(was);
            }
            if !self.chosen.iter().any(|picked| *picked == name) {
                self.chosen.push(name.clone());
            }
            self.selected = index;
            self.anchor = Some(name);
            self.reveal.set(true);
            return;
        }
        self.select_only(index);
    }

    /// A press at `point`: select what is under it, and open it on the second
    /// click of the same entry.
    ///
    /// The double-click rule is Nautilus's: the second press has to land on the
    /// same entry, so a press that *moves* the selection never opens anything.
    /// A file is handed back for the caller to put on a command line, exactly as
    /// `Enter` does — a middle click or a menu item is a different promise, and
    /// this is the one the pane has always made.
    pub fn clicked(
        &mut self,
        point: egui::Pos2,
        clicks: u8,
        additive: bool,
        extend: bool,
    ) -> Option<PathBuf> {
        let index = self.entry_at(point)?;
        let same = index == self.selected && !additive && !extend;
        self.pick(index, additive, extend);
        if clicks >= 2 && same {
            return self.handle(Key::Enter);
        }
        None
    }

    /// Scroll the view by `lines`: positive is up, the way a wheel notch is.
    ///
    /// The view moves and the selection does not: a page down and a page up are
    /// how somebody reads a long listing without losing what they had picked.
    pub fn scroll(&mut self, lines: f64) {
        let Some(frame) = self.frame.get() else {
            return;
        };
        let step = lines.abs().round().max(1.0) as usize;
        let rows = match frame.mode {
            Mode::Grid => rows(self.entries.len(), frame.across),
            Mode::List => self.entries.len(),
        };
        let last = rows.saturating_sub(frame.visible);
        // From the *current* first row, not from the frame's: a wheel notch that
        // started from the last drawn frame would be idempotent, and fifty
        // notches would move the view by one.
        let from = self.scroll.get();
        let target = if lines > 0.0 {
            from.saturating_sub(step)
        } else {
            from + step
        };
        self.scroll.set(target.min(last));
        // A wheel is a view change: it does not follow the selection.
        self.reveal.set(false);
    }

    /// Show or hide hidden entries, without touching the order.
    ///
    /// A filter, not a reload: the listing carries every entry it was given and
    /// the view decides what is on screen, so toggling costs a sort.
    pub fn set_hidden(&mut self, hidden: bool) {
        if self.view.show_hidden != hidden {
            self.view.show_hidden = hidden;
            self.rearrange();
        }
    }

    /// Change the layout without touching the order.
    ///
    /// `Ctrl+1`/`Ctrl+2` and `[files] view` are the two writers, and neither
    /// means "re-sort": a mode change is what a person asked for, and a sort
    /// they chose a moment ago must survive it.
    pub fn set_mode(&mut self, mode: Mode) {
        self.view.mode = mode;
    }

    /// Draw drawn glyphs instead of the machine's icons.
    ///
    /// A baseline is an input and must not depend on which icon theme the machine
    /// that ran the test has — nor on whether its raster set stops at 16 points.
    /// The pane snapshots use this; the app never does.
    pub fn without_icons(&mut self) {
        self.icons = RefCell::new(IconSet::empty());
    }

    pub fn view(&self) -> View {
        self.view
    }

    /// Change the order or the filter, and re-sort now.
    ///
    /// The order is the **model's**, not the view's: the rows on screen come from
    /// one `arrange`, so a selector cannot make the grid and the list disagree
    /// about where a file is.
    pub fn set_view(&mut self, view: View) {
        // Turning hidden entries *on* needs a read: they were dropped by the old
        // filter and never kept. Turning it off is a filter over what is here.
        let reveal = view.show_hidden && !self.view.show_hidden;
        self.view = view;
        if reveal {
            self.reload();
            return;
        }
        let selected = self.selected_entry().map(|entry| entry.name.clone());
        let rows = std::mem::take(&mut self.entries)
            .into_iter()
            .filter(|entry| !entry.is_parent())
            .collect();
        self.entries = arrange(rows, &self.cwd, &self.view);
        if let Some(name) = selected
            && let Some(index) = self.entries.iter().position(|entry| entry.name == name)
        {
            // The cursor moves back to its file; the *pick* is kept as it was, so
            // a sort under a multi-selection does not collapse it to one row.
            self.selected = index;
        }
        self.chosen
            .retain(|name| self.entries.iter().any(|entry| entry.name == *name));
        self.selected = clamp_selection(self.selected, self.entries.len());
    }

    /// A worker error, shown the way a filesystem error is.
    pub fn apply_failure(&mut self, message: String) {
        if self.remote.is_some() {
            self.message = Some(message);
        }
    }

    /// A file the user asked to copy down, taken once.
    pub fn take_download(&mut self) -> Option<PathBuf> {
        self.remote.as_mut()?.download.take()
    }

    fn remote_request(&mut self) {
        if let Some(remote) = self.remote.as_mut() {
            remote.pending = Some(self.cwd.clone());
            self.message = Some("loading…".to_owned());
        }
    }

    pub fn new(start: Option<PathBuf>) -> Self {
        let home = dirs::home_dir().unwrap_or_default();
        let mut browser = Self {
            cwd: PathBuf::from("."),
            home,
            entries: Vec::new(),
            hidden: Vec::new(),
            selected: 0,
            scroll: Cell::new(0),
            columns: Cell::new(1),
            page: Cell::new(0),
            view: View::default(),
            chosen: Vec::new(),
            anchor: None,
            listing_seen: Cell::new(None),
            reveal: Cell::new(false),
            crumbs: RefCell::new(Vec::new()),
            frame: Cell::new(None),
            icons: RefCell::new(IconSet::system()),
            thumbs: RefCell::new(Thumbs::default()),
            message: None,
            remote: None,
            local: Local::new(),
            incoming: None,
            refining: None,
            facts_for: None,
            wanted: None,
        };

        let (mut cwd, select) = match start {
            Some(path) if path.is_dir() => (path, None),
            Some(path) if path.is_file() => {
                let name = path.file_name().map(|name| name.to_owned());
                let parent = parent_of(&path).unwrap_or_else(|| PathBuf::from("."));
                (parent, name)
            }
            Some(path) => (path, None),
            None => (
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                None,
            ),
        };

        // Absolute paths make `..`, the header and the status bar agree; a
        // relative path still works when it cannot be canonicalised (the path
        // does not exist, and the error belongs in the pane).
        if let Ok(absolute) = std::fs::canonicalize(&cwd) {
            cwd = absolute;
        }
        browser.cwd = cwd;
        browser.reload();

        // The listing is streamed, so the file a caller names may not have
        // arrived yet: the promise is kept when its batch lands.
        browser.wanted = select.map(|name| name.to_string_lossy().into_owned());
        browser
    }

    /// What the status strip calls this pane. A remote pane names the host,
    /// because the same path means different things on two machines.
    pub fn title(&self) -> String {
        match self.alias() {
            Some(alias) => format!("files  {alias}:{}", display_path(&self.cwd, &self.home)),
            None => format!("files  {}", display_path(&self.cwd, &self.home)),
        }
    }

    /// The host a remote pane browses, for the breadcrumb's first word.
    fn header_alias(&self) -> Option<&str> {
        self.remote.as_ref().map(|remote| remote.alias.as_str())
    }

    /// The header text: the path, prefixed with the host when it is remote.
    fn header(&self) -> String {
        let path = display_path(&self.cwd, &self.home);
        match self.alias() {
            Some(alias) => format!("{alias}:{path}"),
            None => path,
        }
    }

    /// Handle one key. Returns a path when the user asked for a file to be put
    /// on the terminal's command line.
    pub fn handle(&mut self, key: Key) -> Option<PathBuf> {
        let grid = self.view.mode == Mode::Grid;
        // One row is `columns` cells in a grid and one cell in a list, and one
        // page is what the last frame said it could show.
        let row = if grid { self.columns.get().max(1) } else { 1 };
        let page = self.page.get().max(1);
        let last = self.entries.len().saturating_sub(1);
        match key {
            Key::Up => {
                self.selected = self.selected.saturating_sub(row);
            }
            Key::Down => {
                if !self.entries.is_empty() {
                    self.selected = (self.selected + row).min(last);
                }
            }
            // In a grid, Left is the cell to the left — so the parent directory
            // needs a key of its own, which is `Backspace` and `Alt+Up`
            // (`Key::Parent`). In a list, Left keeps meaning "up a directory",
            // which is what it has always meant here.
            Key::Left if grid => {
                self.selected = self.selected.saturating_sub(1);
            }
            Key::Right if grid => {
                if !self.entries.is_empty() {
                    self.selected = (self.selected + 1).min(last);
                }
            }
            // Only `Left` goes up in a list: a right arrow that did the same
            // thing would be a key that lies about its direction.
            Key::Left => self.go_up(),
            Key::Right => {}
            Key::PageUp => {
                self.selected = self.selected.saturating_sub(page);
            }
            Key::PageDown => {
                if !self.entries.is_empty() {
                    self.selected = (self.selected + page).min(last);
                }
            }
            Key::Parent => self.go_up(),
            Key::Top => self.selected = 0,
            Key::Bottom => self.selected = last,
            Key::Reload => self.refresh(),
            Key::Enter => {
                let entry = self.selected_entry()?.clone();
                if entry.is_parent() {
                    self.go_up();
                } else if entry.is_dir {
                    self.cd(self.cwd.join(&entry.name));
                } else if let Some(remote) = self.remote.as_mut() {
                    // A remote file is copied down, not opened: the editor on
                    // this machine has no way to reach it.
                    remote.download = Some(self.cwd.join(&entry.name));
                } else {
                    return Some(self.cwd.join(&entry.name));
                }
            }
        }
        self.selected = clamp_selection(self.selected, self.entries.len());
        // A key moved the cursor and the selection with it: there is no chord
        // that says "keep what the pointer picked", so keeping it would be a
        // selection nobody could see the end of.
        let focus = self.selected;
        self.select_only(focus);
        None
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.entries.get(self.selected)
    }

    fn go_up(&mut self) {
        if let Some(parent) = parent_of(&self.cwd) {
            self.cd(parent);
        }
    }

    fn cd(&mut self, path: PathBuf) {
        self.cwd = path;
        // Whatever was picked belonged to the directory that is going away.
        self.chosen.clear();
        self.anchor = None;
        self.selected = 0;
        self.scroll.set(0);
        self.reload();
    }

    /// Read the directory again. A failure is a message in the pane, not a
    /// panic and not an empty pane: the header and the message both stay.
    fn reload(&mut self) {
        // The pictures belonged to the listing that is going away. A decode
        // already running finishes and its answer is dropped.
        self.thumbs.borrow_mut().clear();
        self.entries.clear();
        self.selected = 0;
        self.scroll.set(0);
        self.message = None;
        self.incoming = None;
        self.refining = None;
        self.facts_for = None;

        if self.remote.is_some() {
            self.remote_request();
            return;
        }
        // Taken *before* the walk, not after: a change during the read is a
        // change the listing may have missed, and a timestamp from after it
        // would hide exactly that.
        self.listing_seen.set(
            std::fs::metadata(&self.cwd)
                .and_then(|meta| meta.modified())
                .ok(),
        );
        // Streamed, off the drawing thread: the first batch is drawn while the
        // rest of a 10 000-entry directory is still being read, and the footer
        // says `reading…` rather than claiming the directory is empty.
        self.incoming = Some(Incoming {
            dir: self.cwd.clone(),
            batches: self.local.list(&self.cwd),
        });
    }

    /// Read the directory again, keeping the selection on its file.
    ///
    /// The refresh a poll and `r` both use: a first batch is clearing the pane,
    /// so a selection that went back to the top would move a person's place in a
    /// directory that only gained a file.
    pub fn refresh(&mut self) {
        let selected = self.selected_entry().map(|entry| entry.name.clone());
        self.reload();
        // A **promise**, not a lookup: the walk is streamed and has not sent a row
        // yet, so a position found now is a position in an empty listing. `wanted`
        // is applied when the walk ends, which is the same mechanism
        // `dtr --files some/file` uses.
        self.wanted = selected.filter(|name| !name.is_empty() && name != "..");
    }

    /// Re-list if the directory changed on disk, and say whether it did.
    ///
    /// Polled rather than watched, for the reason the config file is polled
    /// (`docs/compio.md` §5): a watch per pane is two resources and a thread for
    /// one number, and `notify` is not in this tree. One `stat` of a path the
    /// pane already holds, twice a second, is the cheaper bargain — and a
    /// directory's mtime is the number that answers the question.
    ///
    /// A remote pane is never polled: `Caps::watch` is false there, and the stat
    /// would be a network round trip on a timer.
    pub fn refresh_if_changed(&mut self) -> bool {
        if self.remote.is_some() || self.reading() {
            return false;
        }
        let Ok(modified) = std::fs::metadata(&self.cwd).and_then(|meta| meta.modified()) else {
            return false;
        };
        match self.listing_seen.get() {
            None => {
                self.listing_seen.set(Some(modified));
                false
            }
            Some(seen) if seen == modified => false,
            Some(_) => {
                self.refresh();
                true
            }
        }
    }

    /// Take whatever arrived, and say whether the pane needs a frame because of
    /// it.
    ///
    /// Called once per frame by whoever draws this pane. Every available batch is
    /// merged **once** — a burst of 157 batches of a 10 000-entry directory must
    /// not be 157 sorts, the same rule `publish` applies to read chunks.
    pub fn poll(&mut self) -> bool {
        let mut changed = self.poll_listing();
        changed |= self.poll_facts();
        // The pass starts here rather than in the two places that can make it
        // necessary — a finished walk, and a sort the reader just chose — because
        // that is the one question: is anything arriving, and does the order need
        // what the listing did not carry?
        if !self.reading()
            && self.view.sort.needs_facts()
            && self.facts_for.as_deref() != Some(self.cwd.as_path())
        {
            self.start_facts();
            changed = true;
        }
        if self.reading() {
            // A streamed listing is the one state that has no event to wake it.
            changed = true;
        }
        changed
    }

    fn poll_listing(&mut self) -> bool {
        let Some(incoming) = self.incoming.as_mut() else {
            return false;
        };
        let dir = incoming.dir.clone();
        let mut arrived: Vec<Entry> = Vec::new();
        let mut done = false;
        let mut failure = None;
        loop {
            match incoming.batches.try_recv() {
                Ok(Ok(batch)) => arrived.extend(batch.into_iter().map(Entry::from)),
                Ok(Err(err)) => {
                    failure = Some(format!("{err:#}"));
                    done = true;
                    break;
                }
                Err(async_channel::TryRecvError::Empty) => break,
                Err(async_channel::TryRecvError::Closed) => {
                    done = true;
                    break;
                }
            }
        }
        if done {
            self.incoming = None;
        }
        if let Some(message) = failure {
            self.message = Some(message);
        }
        if !arrived.is_empty() {
            self.merge(arrived);
        } else if !done {
            return false;
        }
        if done {
            // A name the caller asked for outlives the walk that carries it.
            if let Some(name) = self.wanted.take()
                && let Some(index) = self
                    .entries
                    .iter()
                    .position(|entry| entry.name == name && !entry.is_parent())
            {
                // A caller's promise is a selection, not just a cursor:
                // `dtr --files some/file` means that file is the one selected.
                self.select_only(index);
            }
        }
        let _ = dir;
        true
    }

    /// Merge one burst into the view, keeping the reader on the same file.
    ///
    /// A **name** is the identity and an index is the position: a batch landing
    /// above the selection re-sorts the rows under somebody's arrow key, so both
    /// the selection and the first visible row are remembered by name and found
    /// again afterwards.
    fn merge(&mut self, arrived: Vec<Entry>) {
        let selected = self.selected_entry().map(|entry| entry.name.clone());
        let top = self
            .entries
            .get(self.scroll.get())
            .map(|entry| entry.name.clone());

        self.entries.extend(arrived);
        self.rearrange();
        // A batch may name a row the listing already has — a backend refining what
        // it sent is the case `docs/manager.md` §3 allows. `arrange` puts two rows
        // with one name next to each other, so one pass makes them one row, and
        // the later facts win. The **kind** never does: a symlink resolved into a
        // directory in the first batch stays a directory.
        patch_duplicates(&mut self.entries);
        patch_duplicates(&mut self.hidden);

        if let Some(name) = selected
            && let Some(index) = self.entries.iter().position(|entry| entry.name == name)
        {
            self.focus(index);
        }
        if let Some(name) = top
            && let Some(index) = self.entries.iter().position(|entry| entry.name == name)
        {
            self.scroll.set(index);
        }
        self.selected = clamp_selection(self.selected, self.entries.len());
    }

    /// Sort and filter what is here. `..` is synthetic, so it is dropped before
    /// `arrange` and put back by it.
    fn rearrange(&mut self) {
        let mut rows: Vec<Entry> = std::mem::take(&mut self.entries);
        rows.extend(std::mem::take(&mut self.hidden));
        rows.retain(|entry| !entry.is_parent());
        self.adopt(rows);
    }

    /// Sort a complete listing and split what the view hides.
    fn adopt(&mut self, entries: Vec<Entry>) {
        let (shown, hidden) = arrange_split(entries, &self.cwd, &self.view);
        self.entries = shown;
        self.hidden = hidden;
        // A picked file that was deleted while the pane was open is not part of
        // the selection any more, and keeping its name would make a copy of three
        // paths hand back two.
        self.chosen
            .retain(|picked| self.entries.iter().any(|entry| entry.name == *picked));
    }

    /// Ask for the sizes and times the current sort needs.
    ///
    /// Only a `modified` or a `size` sort asks: a name sort of a directory needs
    /// no `stat` at all, which is the whole point of a listing that carries names
    /// and kinds. A remote listing never asks, because SFTP's `READDIR` already
    /// said.
    fn start_facts(&mut self) {
        if self.remote.is_some() || self.refining.is_some() {
            return;
        }
        if self.facts_for.as_deref() == Some(self.cwd.as_path()) {
            return;
        }
        let names: Vec<String> = self
            .entries
            .iter()
            .filter(|entry| !entry.is_parent() && entry.size.is_none() && entry.modified.is_none())
            .map(|entry| entry.name.clone())
            .collect();
        if names.is_empty() {
            self.facts_for = Some(self.cwd.clone());
            return;
        }
        let rows = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| !entry.is_parent())
            .map(|(index, entry)| (entry.name.clone(), index))
            .collect();
        self.refining = Some(Refining {
            total: names.len(),
            answered: 0,
            rows,
            batches: self.local.facts(&self.cwd, names),
        });
    }

    fn poll_facts(&mut self) -> bool {
        let Some(refining) = self.refining.as_mut() else {
            return false;
        };
        let mut done = false;
        let mut touched = false;
        loop {
            match refining.batches.try_recv() {
                Ok(Ok(batch)) => {
                    for facts in batch {
                        refining.answered += 1;
                        let Some(&row) = refining.rows.get(&facts.name) else {
                            continue;
                        };
                        let Some(entry) = self.entries.get_mut(row) else {
                            continue;
                        };
                        entry.size = facts.len;
                        entry.modified = facts.modified;
                        touched = true;
                    }
                }
                Ok(Err(_)) => {
                    done = true;
                    break;
                }
                Err(async_channel::TryRecvError::Empty) => break,
                Err(async_channel::TryRecvError::Closed) => {
                    done = true;
                    break;
                }
            }
        }
        if done {
            // The order is applied **once**, here: re-sorting as each batch
            // landed would move rows under the pointer continuously.
            self.refining = None;
            self.facts_for = Some(self.cwd.clone());
            let selected = self.selected_entry().map(|entry| entry.name.clone());
            self.rearrange();
            if let Some(name) = selected
                && let Some(index) = self.entries.iter().position(|entry| entry.name == name)
            {
                self.selected = index;
            }
            self.selected = clamp_selection(self.selected, self.entries.len());
            touched = true;
        }
        touched
    }

    /// Whether something is still arriving.
    pub fn reading(&self) -> bool {
        self.incoming.is_some() || self.refining.is_some()
    }

    /// How far a metadata pass has got, for a control that says `(reading…)`.
    fn pass_progress(&self) -> Option<(usize, usize)> {
        self.refining
            .as_ref()
            .map(|refining| (refining.answered, refining.total))
    }

    /// The first row to draw. Keeps the selection on screen and never returns
    /// a row past the end of the list.
    fn first_visible(&self, visible: usize) -> usize {
        let len = self.entries.len();
        if len == 0 {
            return 0;
        }
        first_visible(self.selected, self.scroll.get(), visible).min(len - 1)
    }

    /// Paint the pane. `rect` is in logical points and is also the clip.
    fn draw(&self, ui: &mut egui::Ui, skin: &Skin, rect: egui::Rect, focused: bool) {
        if rect.width() < 8.0 || rect.height() < 8.0 {
            return;
        }
        let palette = skin.palette;
        let style = &skin.style;
        let body_font = style.value_font.clone();
        let small = egui::FontId::new(style.label_font.size - 1.5, style.label_font.family.clone());
        let painter = ui.painter().with_clip_rect(rect);

        painter.rect_filled(rect, 0.0, palette.card);

        // Header: the path, how many real entries the listing has, and the close
        // button. The count stops where the button starts — it is placed from
        // the button's own rectangle, so the two cannot be drawn on top of each
        // other when either moves.
        let header = header_rect(rect);
        let header_bottom = header.bottom();
        painter.rect_filled(header, 0.0, palette.window);
        // The breadcrumb: one segment per component, each a press target. What
        // does not fit is dropped from the *front*, and an ellipsis says so: the
        // last segment is the directory being looked at and is never the one to
        // lose.
        let mut x = places_rect(rect).right() + 6.0;
        if let Some(alias) = self.header_alias() {
            let at = egui::pos2(x, header.center().y);
            let galley =
                painter.layout_no_wrap(format!("{alias}:"), body_font.clone(), palette.text);
            let width = galley.size().x;
            painter.galley(
                at - egui::vec2(0.0, galley.size().y / 2.0),
                galley,
                palette.text,
            );
            // A gap, not a colon and a slash touching: `jupyter:/var/log` reads
            // as a URL, and the header is not one.
            x += width + 8.0;
        }
        let crumbs = crumbs(&self.cwd, &self.home);
        let available = (sort_rect(rect).left() - 8.0) - x;
        let widths: Vec<f32> = crumbs
            .iter()
            .map(|(label, _)| {
                painter
                    .layout_no_wrap(label.clone(), body_font.clone(), palette.text)
                    .size()
                    .x
            })
            .collect();
        let separator = painter
            .layout_no_wrap("/".to_owned(), body_font.clone(), palette.dim)
            .size()
            .x
            + 6.0;
        let total = |from: usize| -> f32 {
            widths[from..].iter().sum::<f32>() + separator * (widths.len() - from - 1) as f32
        };
        let mut first = 0;
        while first + 1 < crumbs.len() && total(first) > available {
            first += 1;
        }
        if first > 0 {
            let galley = painter.layout_no_wrap("…".to_owned(), body_font.clone(), palette.dim);
            let width = galley.size().x + 4.0;
            painter.galley(
                egui::pos2(x, header.center().y - galley.size().y / 2.0),
                galley,
                palette.dim,
            );
            x += width;
        }
        let hover = ui.ctx().pointer_hover_pos();
        let mut targets = Vec::new();
        for (index, (label, target)) in crumbs.iter().enumerate().skip(first) {
            if index > first {
                painter.text(
                    egui::pos2(x, header.center().y),
                    Align2::LEFT_CENTER,
                    "/",
                    body_font.clone(),
                    palette.dim,
                );
                x += separator;
            }
            let width = widths[index];
            let hit = egui::Rect::from_min_size(
                egui::pos2(x, header.top()),
                egui::vec2(width, header.height()),
            );
            // A crumb under the pointer says so in colour and not with a box: the
            // header is one band, and a second fill in it reads as a control the
            // row has no room for.
            let colour = match hover.is_some_and(|point| hit.contains(point)) {
                true => palette.accent,
                false => palette.text,
            };
            painter.text(
                egui::pos2(x, header.center().y),
                Align2::LEFT_CENTER,
                label.clone(),
                body_font.clone(),
                colour,
            );
            targets.push((hit, target.clone()));
            x += width;
        }
        *self.crumbs.borrow_mut() = targets;
        let count = self
            .entries
            .iter()
            .filter(|entry| !entry.is_parent())
            .count();
        painter.text(
            egui::pos2(sort_rect(rect).left() - 6.0, header.center().y),
            Align2::RIGHT_CENTER,
            count.to_string(),
            small.clone(),
            palette.dim,
        );
        painter.line_segment(
            [
                egui::pos2(rect.left(), header_bottom),
                egui::pos2(rect.right(), header_bottom),
            ],
            Stroke::new(style.border, palette.divider),
        );

        let footer_top = (rect.bottom() - FOOTER_HEIGHT).max(header_bottom);
        let body = egui::Rect::from_min_max(
            egui::pos2(rect.left(), header_bottom + 1.0),
            egui::pos2(rect.right(), footer_top),
        );
        match self.view.mode {
            Mode::Grid => self.draw_grid(ui, skin, body),
            Mode::List => self.draw_list(ui, skin, body),
        }

        // Footer: a filesystem problem, the selected path, or a quiet hint.
        let footer = egui::Rect::from_min_max(
            egui::pos2(rect.left(), footer_top),
            egui::pos2(rect.right(), rect.bottom()),
        );
        if let Some(message) = &self.message {
            painter.text(
                egui::pos2(footer.left() + 8.0, footer.center().y),
                Align2::LEFT_CENTER,
                message,
                small.clone(),
                palette.accent,
            );
        } else if self.reading() {
            // **A streamed listing must never render its emptiness as a fact.**
            // `empty directory` while the walk is running is a claim the pane
            // cannot make yet; the count is what it does know.
            let said = match self.pass_progress() {
                Some((answered, total)) => format!("reading sizes… {answered}/{total}"),
                None => format!("reading… {} so far", self.entries.len().saturating_sub(1)),
            };
            painter.text(
                egui::pos2(footer.left() + 8.0, footer.center().y),
                Align2::LEFT_CENTER,
                said,
                small.clone(),
                palette.dim,
            );
        } else if let Some(label) = self.selection_label() {
            // Several selected rows are a number and not a path: the footer has one
            // line, and the path of the third of five answers nothing.
            painter.text(
                egui::pos2(footer.left() + 8.0, footer.center().y),
                Align2::LEFT_CENTER,
                label,
                small.clone(),
                palette.dim,
            );
        } else if let Some(entry) = self.selected_entry() {
            let path = if entry.is_parent() {
                parent_of(&self.cwd)
                    .map(|parent| display_path(&parent, &self.home))
                    .unwrap_or_else(|| display_path(&self.cwd, &self.home))
            } else {
                display_join(&self.cwd, &entry.name, &self.home)
            };
            painter.text(
                egui::pos2(footer.left() + 8.0, footer.center().y),
                Align2::LEFT_CENTER,
                path,
                small.clone(),
                palette.dim,
            );
        } else if self.message.is_none() {
            painter.text(
                footer.center(),
                Align2::CENTER_CENTER,
                "empty directory",
                small.clone(),
                palette.dim,
            );
        }

        let border = if focused {
            palette.accent
        } else {
            palette.border
        };
        painter.rect_stroke(
            rect,
            0.0,
            Stroke::new(
                if focused {
                    style.border * 1.5
                } else {
                    style.border
                },
                border,
            ),
            egui::StrokeKind::Inside,
        );
    }
    /// The dense view: one row per entry, a name on the left and a size on the
    /// right.
    ///
    /// Kept rather than replaced by the grid because a pane in a terminal is
    /// often 450 points wide, where four tiles of a short name are fewer items
    /// than ten rows (`docs/manager.md` §3). It is the renderer that shipped
    /// before the grid and it does not move.
    fn draw_list(&self, ui: &mut egui::Ui, skin: &Skin, body: egui::Rect) {
        let palette = skin.palette;
        let style = &skin.style;
        let body_font = style.value_font.clone();
        let small = egui::FontId::new(style.label_font.size - 1.5, style.label_font.family.clone());
        let painter = ui.painter().with_clip_rect(body);

        let visible = (body.height() / ROW_HEIGHT).floor().max(0.0) as usize;
        let first = if self.reveal.replace(false) {
            self.first_visible(visible)
        } else {
            // The wheel moved the view and the selection stayed where it was; the
            // view must not follow it back.
            let last = self.entries.len().saturating_sub(visible);
            self.scroll.get().min(last)
        };
        self.scroll.set(first);
        self.frame.set(Some(Frame {
            mode: Mode::List,
            body,
            content: body,
            cell: egui::vec2(body.width(), ROW_HEIGHT),
            gap: 0.0,
            across: 1,
            first,
            visible,
        }));
        // A list is one column wide and one row a page: the two cells a 2-D key
        // reads are the plain ones.
        self.columns.set(1);
        self.page.set(visible.max(1));

        if self.entries.is_empty() || visible == 0 {
            return;
        }
        for (offset, index) in (first..self.entries.len().min(first + visible)).enumerate() {
            let row = egui::Rect::from_min_size(
                egui::pos2(body.left(), body.top() + offset as f32 * ROW_HEIGHT),
                egui::vec2(body.width(), ROW_HEIGHT),
            );
            let selected = self.is_selected(index);
            // The highlight goes down first; painting a rectangle after
            // the text hides the row it was meant to select.
            if selected {
                painter.rect_filled(
                    row.shrink2(egui::vec2(4.0, 1.0)),
                    2.0,
                    palette.control_active,
                );
            }

            let entry = &self.entries[index];
            let (label, colour) = if entry.is_parent() {
                ("../".to_owned(), palette.dim)
            } else if entry.is_dir {
                (
                    format!("{}/", entry.name),
                    if selected {
                        palette.accent
                    } else {
                        palette.text
                    },
                )
            } else {
                (
                    entry.name.clone(),
                    if selected {
                        palette.accent
                    } else {
                        palette.text
                    },
                )
            };
            painter.text(
                egui::pos2(row.left() + 8.0, row.center().y),
                Align2::LEFT_CENTER,
                label,
                body_font.clone(),
                colour,
            );
            if let Some(size) = entry.size {
                painter.text(
                    egui::pos2(row.right() - 8.0, row.center().y),
                    Align2::RIGHT_CENTER,
                    human_size(size),
                    small.clone(),
                    palette.dim,
                );
            }
        }
    }

    /// The default view: a grid of tiles, laid out by [`crate::grid`]'s functions
    /// and painted by `dtr_widgets::icon_tile`.
    ///
    /// The scroll is a **row** here, like everywhere else in the browser, and the
    /// hit test and the drawer both come from the same `cell_rect`: a grid that
    /// computes its own columns in two places is the `pane_area` bug with a
    /// second dimension (`docs/manager.md` §3).
    fn draw_grid(&self, ui: &mut egui::Ui, skin: &Skin, body: egui::Rect) {
        let palette = skin.palette;
        let style = &skin.style;
        let content = body.shrink2(egui::vec2(TILE_PAD, TILE_PAD));
        if content.width() <= 0.0 || content.height() <= 0.0 {
            return;
        }

        // The row height, measured once from a laid-out line: `Fonts::row_height`
        // needs `&mut Fonts` and a `Ui` hands out `&Fonts`.
        let line = ui
            .painter()
            .layout_no_wrap("M".to_owned(), style.value_font.clone(), palette.text)
            .size()
            .y;
        let cell = egui::vec2(style.tile_icon * 2.0, tile_height(style.tile_icon, line));
        let across = columns(content.width(), cell.x, TILE_GAP);
        let visible = visible_rows(content.height(), cell.y, TILE_GAP);
        self.columns.set(across.max(1));
        self.page.set((visible * across).max(1));
        if across == 0 || visible == 0 {
            return;
        }

        // The window: the selection stays on screen, and the rows are clamped so
        // a listing that shrank cannot leave the grid scrolled past its end.
        let last_row = rows(self.entries.len(), across).saturating_sub(1);
        let first = if self.reveal.replace(false) {
            first_row(row_of(self.selected, across), self.scroll.get(), visible).min(last_row)
        } else {
            self.scroll.get().min(last_row)
        };
        self.scroll.set(first);

        self.frame.set(Some(Frame {
            mode: Mode::Grid,
            body,
            content,
            cell,
            gap: TILE_GAP,
            across,
            first,
            visible,
        }));

        let start = first * across;
        let end = ((first + visible) * across).min(self.entries.len());
        let hovered = ui
            .ctx()
            .pointer_hover_pos()
            .filter(|point| body.contains(*point))
            .and_then(|point| {
                index_at(
                    point,
                    across,
                    cell,
                    TILE_GAP,
                    content.min,
                    first,
                    self.entries.len(),
                )
            });

        // The pictures: the theme first, the drawn glyph when it has none. One
        // frame may read only so many files, so a directory of forty asks for
        // another frame rather than stalling this one.
        let icons = self.icons.borrow();
        icons.start_frame();
        // The size the theme is asked for, in *physical* pixels: a 48-point tile
        // on a 2× screen wants a 96-point picture or it is soft.
        let px = (style.tile_icon * ui.ctx().pixels_per_point())
            .ceil()
            .max(1.0) as u32;
        // Pictures decoded since the last frame become textures here, because a
        // texture needs the drawing context, and the tiles below can then use
        // them in this frame.
        let mut wanted = self.thumbs.borrow_mut().poll(ui.ctx());

        for index in start..end {
            let entry = &self.entries[index];
            let glyph = if entry.is_parent() {
                Glyph::Parent
            } else if entry.is_dir {
                Glyph::Folder
            } else {
                Glyph::File
            };
            // The parent row is not a file and has no picture to ask about.
            let path = self.cwd.join(&entry.name);
            let thumbnail = if entry.is_parent() || entry.is_dir || !looks_like_image(&path) {
                None
            } else {
                // `None` here is "not yet": the type icon is drawn until the
                // decode lands, and another frame is asked for.
                let texture = self.thumbs.borrow_mut().texture(&path, px).cloned();
                if texture.is_none() {
                    wanted = true;
                }
                texture
            };
            let picture = if entry.is_parent() {
                None
            } else {
                icons.picture_for(&path, &entry.name, entry.is_dir, px)
            };
            let icon = match &thumbnail {
                Some(texture) => Icon::Image(texture),
                None => match &picture {
                    Some(picture) => Icon::Bytes {
                        uri: picture.uri(),
                        bytes: picture.bytes(),
                    },
                    None => Icon::Glyph(glyph),
                },
            };
            icon_tile(
                ui,
                skin,
                cell_rect(index, across, cell, TILE_GAP, content.min, first),
                &IconTile {
                    // A folder is not spelled with a slash here: the glyph is
                    // what says it, and a name is a name.
                    label: &entry.name,
                    icon,
                    selected: self.is_selected(index),
                    hovered: hovered == Some(index),
                    // Hidden entries only reach this list when `Ctrl+H` is on,
                    // so nothing here is dimmed *for* being hidden; the flag is
                    // for the filter's own eventual use.
                    dim: false,
                },
            );
        }
        if icons.wants_a_frame() || wanted {
            ui.ctx().request_repaint();
        }
    }
}

/// One file pane as a surface sees it: a rectangle in points, the browser, and
/// whether it currently has focus.
pub struct PaneView<'a> {
    pub id: u64,
    pub rect: egui::Rect,
    pub browser: &'a Browser,
    pub focused: bool,
}

/// The header band: the top of the pane down to [`HEADER_HEIGHT`], or the whole
/// pane when it is shorter than that.
fn header_rect(rect: egui::Rect) -> egui::Rect {
    let bottom = (rect.top() + HEADER_HEIGHT).min(rect.bottom());
    egui::Rect::from_min_max(rect.min, egui::pos2(rect.right(), bottom))
}

/// The close button's target, in the pane's points.
///
/// One function for the painter, the widget and the test: the count is placed
/// against its left edge and the cross is placed *at* it, and two calculations
/// would drift apart the first time the header's inset changed.
fn close_rect(rect: egui::Rect) -> egui::Rect {
    let header = header_rect(rect);
    // Inset from the pane's right edge by the same gap the path has on the
    // left, and as tall as the header allows — a pane too short for the full
    // square gets a smaller one rather than a target outside itself.
    let side = CLOSE_SIDE.min(header.height());
    egui::Rect::from_center_size(
        egui::pos2(header.right() - 8.0 - side / 2.0, header.center().y),
        egui::vec2(side, side),
    )
}

/// What a person did to a pane.
///
/// The pane is painted from a `&Browser` and cannot change it, so an answer
/// travels the way a clicked tab's does: the drawing knows what was pressed and
/// the app knows what it means.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaneOutcome {
    /// The close cross was pressed.
    pub close: bool,
    /// The sort control asked for something.
    pub sort: Option<SortChange>,
    /// The places popup chose a directory.
    pub navigate: Option<PathBuf>,
}

/// What the sort control asked for: one of the four keys, or one of the two
/// switches beside them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortChange {
    Key(Sort),
    /// Flip the direction; the caller keeps the state.
    Reverse,
    FoldersFirst,
}

/// The places button's target: the header's left-hand end, before the path.
fn places_rect(rect: egui::Rect) -> egui::Rect {
    let header = header_rect(rect);
    let height = (header.height() - 6.0).max(0.0);
    egui::Rect::from_min_size(
        egui::pos2(header.left() + 4.0, header.center().y - height / 2.0),
        egui::vec2(PLACES_WIDTH, height),
    )
}

/// The sort control's target: the header's right-hand end, beside the cross.
///
/// One function for the painter, the area and the test — the count is placed
/// against its left edge, so two calculations would drift apart the first time
/// either inset changed (the `close_rect` rule).
fn sort_rect(rect: egui::Rect) -> egui::Rect {
    let header = header_rect(rect);
    let height = (header.height() - 6.0).max(0.0);
    let right = close_rect(rect).left() - 6.0;
    egui::Rect::from_min_size(
        egui::pos2(right - SORT_WIDTH, header.center().y - height / 2.0),
        egui::vec2(SORT_WIDTH, height),
    )
}

/// Draw one file pane in an egui area at its leaf's rectangle.
///
/// `rect` is the pane in **points**, which is egui's unit: a surface that lays
/// panes out in physical pixels divides by its own `pixels_per_point` before
/// asking, because only the surface knows the scale. The area is clipped to the
/// pane so a long listing cannot spill into the terminal next door.
///
/// Returns what was asked for. The drawing code knows that; only the caller can
/// take a leaf out of a layout or change what a listing is ordered by.
pub fn pane_ui(
    ui: &mut egui::Ui,
    id: egui::Id,
    rect: egui::Rect,
    browser: &Browser,
    skin: &Skin,
    focused: bool,
) -> PaneOutcome {
    let ctx = ui.ctx().clone();
    // Prime the area's size: an unprimed area is invisible in the one frame a
    // `--screenshot` renders.
    dtr_widgets::prime_area(&ctx, id, egui::Order::Background, rect, false);
    // The close button is an area of its own, and it is primed as *interactable*
    // or the first frame after the pane appears would have no target in it.
    let close_id = id.with("close");
    let target = close_rect(rect);
    dtr_widgets::prime_area(&ctx, close_id, egui::Order::Background, target, true);
    // The sort control is the second target, for the same reason: a widget inside
    // a non-interactable area is a picture of a button.
    let sort_id = id.with("sort");
    let sort_target = sort_rect(rect);
    dtr_widgets::prime_area(&ctx, sort_id, egui::Order::Background, sort_target, true);
    let label = format!("close {}", browser.header());
    let mut close = false;
    egui::Area::new(id)
        // Below the palette's foreground layer, but above the root background
        // the panels and the grid live in.
        .order(egui::Order::Background)
        .fixed_pos(rect.min)
        .constrain(false)
        // The pane is painted, not clicked: focus and selection are winit's
        // business, so the area must not swallow the pointer. egui skips a
        // non-interactable area in its hit test, which is also why the close
        // button below is an area of its own — a widget in here would never see
        // a press.
        .interactable(false)
        // A capture is one frame; a fade-in would make the whole pane faint.
        .fade_in(false)
        .show(&ctx, |ui| {
            ui.set_min_size(rect.size());
            ui.set_max_size(rect.size());
            ui.set_clip_rect(rect);
            browser.draw(ui, skin, rect, focused);
            close = close_button(&ctx, close_id, target, skin, &label);
        });
    let sort = sort_button(&ctx, sort_id, sort_target, skin, browser.view());
    let places_id = id.with("places");
    let places_target = places_rect(rect);
    dtr_widgets::prime_area(
        &ctx,
        places_id,
        egui::Order::Background,
        places_target,
        true,
    );
    let navigate = places_button(
        &ctx,
        places_id,
        places_target,
        skin,
        browser.place_list(),
        browser,
    );
    PaneOutcome {
        close,
        sort,
        navigate,
    }
}

/// The places button: a house, and a menu of the directories a desktop calls
/// places.
///
/// A pane is too narrow for the sidebar the manager window will have
/// (`docs/manager.md` §6), so the same list is a popup — and it earns its header
/// space because the breadcrumb can reach every place only by way of home.
///
/// The mark is drawn, not borrowed: a house is six strokes, and an SVG would put
/// an image loader between a harness and a snapshot (§The grid, icons).
fn places_button(
    ctx: &egui::Context,
    id: egui::Id,
    target: egui::Rect,
    skin: &Skin,
    places: Vec<(String, PathBuf)>,
    browser: &Browser,
) -> Option<PathBuf> {
    let palette = skin.palette;
    let style = &skin.style;
    let mut chosen = None;
    egui::Area::new(id)
        .order(egui::Order::Background)
        .fixed_pos(target.min)
        .constrain(false)
        .fade_in(false)
        .show(ctx, |ui| {
            let (rect, response) = ui.allocate_exact_size(target.size(), egui::Sense::click());
            let open = egui::Popup::is_id_open(ctx, response.id);
            if response.hovered() || open {
                ui.painter().rect_filled(
                    rect.shrink(2.0),
                    style.control_radius,
                    palette.control_hover,
                );
            }
            house(ui.painter(), rect.shrink(5.0), palette.dim, style.border);
            let name = "places";
            response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, name));
            if response.has_focus() {
                response.surrender_focus();
            }

            let mut popup = egui::Popup::menu(&response).width(210.0);
            if let Some(tip) = style.tip_height {
                popup = dtr_widgets::point_at(ui, popup, rect, tip);
            }
            let popup = popup.show(|ui| {
                ui.set_min_width(210.0);
                for (label, path) in &places {
                    if dtr_widgets::menu_row(ui, skin, label, browser.showing(path)) {
                        chosen = Some(path.clone());
                        ui.close();
                    }
                }
            });
            if let Some(popup) = popup {
                dtr_widgets::menu_tip(&popup.response, skin, rect);
            }
        });
    chosen
}

/// A house, in six strokes: two roof slopes, two walls, a floor and a door.
fn house(painter: &egui::Painter, rect: egui::Rect, colour: egui::Color32, border: f32) {
    let stroke = egui::Stroke::new(border.max(1.2), colour);
    let width = rect.width();
    let height = rect.height();
    let left = rect.left() + width * 0.1;
    let right = rect.right() - width * 0.1;
    let roof = rect.top() + height * 0.12;
    let shoulders = rect.top() + height * 0.46;
    let floor = rect.bottom() - height * 0.1;
    let apex = egui::pos2(rect.center().x, roof);
    painter.line_segment([apex, egui::pos2(left, shoulders)], stroke);
    painter.line_segment([apex, egui::pos2(right, shoulders)], stroke);
    painter.line_segment(
        [egui::pos2(left, shoulders), egui::pos2(left, floor)],
        stroke,
    );
    painter.line_segment(
        [egui::pos2(right, shoulders), egui::pos2(right, floor)],
        stroke,
    );
    painter.line_segment([egui::pos2(left, floor), egui::pos2(right, floor)], stroke);
    // The door, from the floor up: a house without one is a tent.
    let door = rect.center().x;
    let half = width * 0.11;
    painter.line_segment(
        [
            egui::pos2(door - half, floor),
            egui::pos2(door - half, floor - height * 0.22),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(door + half, floor),
            egui::pos2(door + half, floor - height * 0.22),
        ],
        stroke,
    );
}

/// The sort control: the order's name, a drawn triangle for its direction, and
/// the menu the same four keys and two switches are on.
///
/// The pane's second pointer target, and the reason it is an area of its own is
/// the same as the close button's: `interactable(false)` on the pane makes egui
/// skip it in its hit test, so a widget drawn inside would never see a press.
///
/// The direction is a **triangle**, not an arrow: egui's faces have no arrows and
/// a `↓` in a label panics the harness and draws an empty box in the window
/// (`AGENTS.md`, glyphs). Two are enough — up is the key's usual direction and
/// down says it is reversed.
fn sort_button(
    ctx: &egui::Context,
    id: egui::Id,
    target: egui::Rect,
    skin: &Skin,
    view: View,
) -> Option<SortChange> {
    let palette = skin.palette;
    let style = &skin.style;
    let small = egui::FontId::new(style.label_font.size - 1.5, style.label_font.family.clone());
    let mut chosen = None;
    egui::Area::new(id)
        .order(egui::Order::Background)
        .fixed_pos(target.min)
        .constrain(false)
        .fade_in(false)
        .show(ctx, |ui| {
            let (rect, response) = ui.allocate_exact_size(target.size(), egui::Sense::click());
            let open = egui::Popup::is_id_open(ctx, response.id);
            if response.hovered() || open {
                ui.painter().rect_filled(
                    rect.shrink(2.0),
                    style.control_radius,
                    palette.control_hover,
                );
            }

            // The name, elided in its slot: `modified` and `name` share one width.
            let width = (rect.width() - 22.0).max(8.0);
            let mut job = egui::text::LayoutJob::simple(
                view.sort.label().to_owned(),
                small.clone(),
                palette.text,
                width,
            );
            job.wrap = egui::text::TextWrapping {
                max_width: width,
                max_rows: 1,
                break_anywhere: true,
                overflow_character: Some('…'),
            };
            let galley = ui.painter().layout_job(job);
            ui.painter().galley(
                egui::pos2(rect.left() + 6.0, rect.center().y - galley.size().y / 2.0),
                galley,
                palette.text,
            );

            // Up for forwards, down for reversed.
            let centre = egui::pos2(rect.right() - 9.0, rect.center().y);
            let half = 3.5;
            let rise = 2.6;
            let (base, tip) = if view.reverse {
                (centre.y - rise, centre.y + rise)
            } else {
                (centre.y + rise, centre.y - rise)
            };
            ui.painter().add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(centre.x - half, base),
                    egui::pos2(centre.x + half, base),
                    egui::pos2(centre.x, tip),
                ],
                palette.dim,
                egui::Stroke::NONE,
            ));

            // Painted, so the node is declared, and named with the value rather
            // than the row: "sort by" twice is two nodes nothing can address.
            let name = format!("sort by {}", view.sort_label());
            response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &name));
            if response.has_focus() {
                response.surrender_focus();
            }

            let mut popup = egui::Popup::menu(&response).width(190.0);
            if let Some(tip) = style.tip_height {
                popup = dtr_widgets::point_at(ui, popup, rect, tip);
            }
            let popup = popup.show(|ui| {
                ui.set_min_width(190.0);
                let mut pick = |ui: &mut egui::Ui, change: SortChange, label: &str, on: bool| {
                    if dtr_widgets::menu_row(ui, skin, label, on) {
                        chosen = Some(change);
                        ui.close();
                    }
                };
                for sort in Sort::ALL {
                    pick(ui, SortChange::Key(sort), sort.label(), view.sort == sort);
                }
                dtr_widgets::menu_separator(ui, skin);
                pick(ui, SortChange::Reverse, "reverse", view.reverse);
                pick(
                    ui,
                    SortChange::FoldersFirst,
                    "folders first",
                    view.folders_first,
                );
            });
            if let Some(popup) = popup {
                dtr_widgets::menu_tip(&popup.response, skin, rect);
            }
        });
    chosen
}

/// The pane's close button: a cross in the header's right-hand corner.
///
/// It is one small interactable area over one corner rather than a widget in
/// the pane's own area, because egui skips a non-interactable area in its hit
/// test and the pane is painted rather than clicked on purpose — a press on its
/// body is the app's business, and that is what moves the focus to it. This is
/// the whole of the pane's click surface: a press here closes the pane and does
/// not move the focus to it first.
fn close_button(
    ctx: &egui::Context,
    id: egui::Id,
    target: egui::Rect,
    skin: &Skin,
    label: &str,
) -> bool {
    let mut clicked = false;
    egui::Area::new(id)
        // The pane's order, and after it, so the cross is above the header it
        // sits on.
        .order(egui::Order::Background)
        .fixed_pos(target.min)
        .constrain(false)
        .fade_in(false)
        .show(ctx, |ui| {
            // `cross`, not `cross_at`: this area exists for this one square, so
            // there is nothing to place it against. Both declare the same
            // accessibility node and strip the keyboard's synthetic click, so
            // the close button cannot be pressed by typing at the shell.
            clicked = dtr_widgets::cross(ui, skin, label, target.height()).clicked();
        });
    clicked
}

/// Directories first, then case-insensitive name order, with the synthetic
/// `..` row prepended when there is a parent. Hidden entries are dropped unless
/// asked for. No filesystem access: the entries are already read.
pub fn arrange(entries: Vec<Entry>, cwd: &Path, view: &View) -> Vec<Entry> {
    arrange_split(entries, cwd, view).0
}

/// The same, keeping what the view hides instead of dropping it.
///
/// **A filter is not a listing.** Hidden entries are in the directory whether or
/// not they are on screen, and `Ctrl+H` has to be able to show them without
/// reading the directory again: so the sort partitions instead of discarding, and
/// the browser keeps the other half. Dropping them made the key a reload, and a
/// reload is a walk somebody did not ask for.
pub fn arrange_split(entries: Vec<Entry>, cwd: &Path, view: &View) -> (Vec<Entry>, Vec<Entry>) {
    let (mut shown, hidden): (Vec<Entry>, Vec<Entry>) = entries
        .into_iter()
        .partition(|entry| view.show_hidden || !entry.name.starts_with('.'));
    shown.sort_by_cached_key(|entry| SortKey::of(entry, view));
    // After the sort and before nothing else: `..` is synthetic, and it is first
    // whatever the key and whatever the direction.
    if parent_of(cwd).is_some() {
        shown.insert(0, Entry::parent());
    }
    (shown, hidden)
}

/// Collapse rows a backend sent twice, keeping the later facts.
///
/// A backend may name a row it already sent and refine it (the metadata pass is
/// one). `arrange` puts two rows with one name next to each other, so one pass
/// makes them one row and the later facts win. The **kind** never does: a symlink
/// resolved into a directory in the first batch stays a directory.
fn patch_duplicates(rows: &mut Vec<Entry>) {
    rows.dedup_by(|later, earlier| {
        if later.name != earlier.name {
            return false;
        }
        earlier.size = later.size.or(earlier.size);
        earlier.modified = later.modified.or(earlier.modified);
        true
    });
}

/// The key one entry sorts by, built once per entry.
///
/// The comparator this replaces lowercased *both* names on **every comparison** —
/// two `String`s, `O(n log n)` times, which measured 2.77 ms against 1.10 ms for
/// a key built once on 10 000 generated names. The key is the same information,
/// computed once.
#[derive(Debug, PartialEq, Eq)]
struct SortKey {
    /// Carried so `cmp` does not have to be told which key it is comparing;
    /// every key in one sort is built from the same view.
    sort: Sort,
    reverse: bool,
    /// 0 for folders and 1 for the rest when folders come first, 0 for
    /// everything when the grouping is off.
    group: u8,
    /// 0 when the entry's key is known and 1 when it is not, and compared
    /// **before** the key so an unknown sinks in both directions. Sorting by
    /// date with the unknowns on top would be a list of the files you cannot
    /// read, presented as the newest.
    unknown: u8,
    modified: Option<SystemTime>,
    size: Option<u64>,
    /// The extension, lowercased. Empty for a directory: a folder named
    /// `something.d` is not a `.d` file.
    extension: String,
    /// The name, lowercased: the key for a name sort, and the tiebreak that
    /// makes every other sort's order fixed.
    lower: String,
    /// The name itself, last: `A.txt` and `a.txt` have one order whichever
    /// direction the key runs.
    name: String,
}

impl SortKey {
    fn of(entry: &Entry, view: &View) -> Self {
        Self {
            sort: view.sort,
            reverse: view.reverse,
            group: u8::from(view.folders_first && !entry.is_dir),
            unknown: u8::from(match view.sort {
                Sort::Name | Sort::Kind => false,
                Sort::Modified => entry.modified.is_none(),
                Sort::Size => entry.size.is_none(),
            }),
            modified: entry.modified,
            size: entry.size,
            extension: if entry.is_dir {
                String::new()
            } else {
                extension_of(&entry.name)
            },
            lower: entry.name.to_lowercase(),
            name: entry.name.clone(),
        }
    }
}

/// The extension of a file name, lowercased, or an empty string.
fn extension_of(name: &str) -> String {
    Path::new(name)
        .extension()
        .map(|extension| extension.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

impl Ord for SortKey {
    fn cmp(&self, other: &Self) -> Ordering {
        debug_assert_eq!(
            self.sort, other.sort,
            "a key is only compared against keys of the same view"
        );
        self.group
            .cmp(&other.group)
            .then(self.unknown.cmp(&other.unknown))
            .then_with(|| {
                // The whole key chain turns around together — the value, then the
                // name that breaks its ties — because a reader who reverses a
                // date sort expects the folders reversed too. Only the grouping
                // and the knownness stay put.
                let chain = match self.sort {
                    Sort::Name => self.lower.cmp(&other.lower),
                    Sort::Modified => self.modified.cmp(&other.modified),
                    Sort::Size => self.size.cmp(&other.size),
                    Sort::Kind => self.extension.cmp(&other.extension),
                }
                .then_with(|| self.lower.cmp(&other.lower))
                .then_with(|| self.name.cmp(&other.name));
                if self.reverse { chain.reverse() } else { chain }
            })
    }
}

impl PartialOrd for SortKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The directory above `path`, or `None` at the root. Syntactic only: it does
/// not ask the disk, which is what makes it testable and what makes `..`
/// disappear at `/`.
pub fn parent_of(path: &Path) -> Option<PathBuf> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
}

/// Keep a selection inside a list that may have changed underneath it.
pub fn clamp_selection(selected: usize, len: usize) -> usize {
    if len == 0 { 0 } else { selected.min(len - 1) }
}

/// The first row to show: `scroll` when it can, otherwise the window that puts
/// `selected` on screen.
pub fn first_visible(selected: usize, scroll: usize, visible: usize) -> usize {
    let visible = visible.max(1);
    if selected < scroll {
        selected
    } else if selected >= scroll + visible {
        selected + 1 - visible
    } else {
        scroll
    }
}

/// The breadcrumb's segments: a label and where a press on it goes.
///
/// One segment per component, so `~/src/dtr/docs` is four targets and a press on
/// `dtr` is two levels up — the whole point of a breadcrumb over a path string.
/// The root is a segment of its own, because "go to `/`" is otherwise only
/// reachable by pressing the parent several times.
pub fn crumbs(path: &Path, home: &Path) -> Vec<(String, PathBuf)> {
    let mut segments = Vec::new();
    if let Ok(rest) = path.strip_prefix(home) {
        segments.push(("~".to_owned(), home.to_path_buf()));
        let mut at = home.to_path_buf();
        for part in rest.components() {
            at.push(part);
            segments.push((part.as_os_str().to_string_lossy().into_owned(), at.clone()));
        }
        return segments;
    }
    segments.push(("/".to_owned(), PathBuf::from("/")));
    let mut at = PathBuf::from("/");
    for part in path.components() {
        if let std::path::Component::Normal(name) = part {
            at.push(name);
            segments.push((name.to_string_lossy().into_owned(), at.clone()));
        }
    }
    segments
}

/// A path for display: `$HOME` and everything below it becomes `~…`.
pub fn display_path(path: &Path, home: &Path) -> String {
    if home.as_os_str().is_empty() {
        return path.display().to_string();
    }
    if path == home {
        return "~".to_owned();
    }
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// `dir` joined with `name`, as a display path.
pub fn display_join(dir: &Path, name: &str, home: &Path) -> String {
    display_path(&dir.join(name), home)
}

/// A byte count a person can read. Shared with a surface's toasts, which report a
/// save the same way the pane reports a file.
pub fn human_size(bytes: u64) -> String {
    const K: f64 = 1024.0;
    let value = bytes as f64;
    if value < K {
        format!("{bytes} B")
    } else if value < K * K {
        format!("{:.1} kB", value / K)
    } else if value < K * K * K {
        format!("{:.1} MB", value / (K * K))
    } else {
        format!("{:.1} GB", value / (K * K * K))
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        Browser, Entry, Key, Mode, Sort, View, arrange, clamp_selection, crumbs, display_join,
        display_path, first_visible, human_size, parent_of,
    };

    fn entry(name: &str, is_dir: bool) -> Entry {
        Entry {
            name: name.to_owned(),
            is_dir,
            size: None,
            modified: None,
        }
    }

    /// An entry with both facts, for the sorts that need them.
    fn fact(name: &str, size: u64, modified: Option<u64>) -> Entry {
        Entry {
            name: name.to_owned(),
            is_dir: false,
            size: Some(size),
            modified: modified
                .map(|secs| std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)),
        }
    }

    fn view(sort: Sort, reverse: bool) -> View {
        View {
            sort,
            reverse,
            ..View::default()
        }
    }

    /// The selected names in listing order: the focus is always one of them.
    fn selected_names(browser: &Browser) -> Vec<&str> {
        browser
            .entries
            .iter()
            .enumerate()
            .filter(|(index, _)| browser.is_selected(*index))
            .map(|(_, entry)| entry.name.as_str())
            .collect()
    }

    fn names_of(trail: &[(String, PathBuf)]) -> Vec<&str> {
        trail.iter().map(|(label, _)| label.as_str()).collect()
    }

    fn names(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.name.as_str()).collect()
    }

    /// Wait for a local listing to finish.
    ///
    /// The walk is on a thread of its own and the pane is drawn from whatever has
    /// arrived, so a test that asserts about a directory has to let it land — the
    /// same thing a frame does, with a deadline so a broken walk fails instead of
    /// hanging.
    fn settle(browser: &mut Browser) {
        for _ in 0..2_000 {
            browser.poll();
            if !browser.reading() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("the listing never finished");
    }

    /// The row whose name is `name`, as an index — the selection is an index, and
    /// the names are how a test says which one it means.
    fn row_of(browser: &Browser, name: &str) -> usize {
        browser
            .entries
            .iter()
            .position(|entry| entry.name == name)
            .unwrap_or_else(|| panic!("{name} in {:?}", names(&browser.entries)))
    }

    #[test]
    fn directories_come_first_and_names_sort_case_insensitively() {
        let entries = vec![
            entry("beta.txt", false),
            entry("alpha", true),
            entry("Delta.txt", false),
            entry("charlie", true),
        ];
        let arranged = arrange(entries, Path::new("/tmp/work"), &View::default());
        assert_eq!(
            names(&arranged),
            ["..", "alpha", "charlie", "beta.txt", "Delta.txt"]
        );
    }

    #[test]
    fn hidden_entries_need_the_flag_and_the_parent_row_survives_it() {
        let entries = vec![
            entry(".git", true),
            entry(".bashrc", false),
            entry("visible", false),
        ];
        let hidden = arrange(entries.clone(), Path::new("/tmp/work"), &View::default());
        assert_eq!(names(&hidden), ["..", "visible"]);
        let shown = arrange(
            entries,
            Path::new("/tmp/work"),
            &View {
                show_hidden: true,
                ..View::default()
            },
        );
        assert_eq!(names(&shown), ["..", ".git", ".bashrc", "visible"]);
    }

    /// A directory that gained a file is re-listed by the poll, once, and the
    /// selection stays on the file it was on.
    #[test]
    fn a_changed_directory_is_read_again_once() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let root = temporary.path().canonicalize().expect("canonical root");
        std::fs::write(root.join("a.txt"), b"x").expect("file");
        std::fs::write(root.join("b.txt"), b"x").expect("file");

        let mut browser = Browser::new(Some(root.clone()));
        settle(&mut browser);
        assert!(!browser.refresh_if_changed(), "nothing changed yet");
        browser.selected = row_of(&browser, "b.txt");

        // A mtime is compared by value, so the change has to be a different
        // instant: some filesystems have a one-second clock, and this waits for
        // one tick of the coarser kind rather than depending on the finer one.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(root.join("c.txt"), b"x").expect("file");
        assert!(browser.refresh_if_changed(), "the directory changed");
        settle(&mut browser);
        assert!(
            names(&browser.entries).contains(&"c.txt"),
            "the new file is listed"
        );
        assert_eq!(
            browser.selected_entry().map(|entry| entry.name.as_str()),
            Some("b.txt"),
            "the selection stayed on its file"
        );
        // And the same mtime does not re-list a second time.
        assert!(!browser.refresh_if_changed(), "nothing changed again");
    }

    /// The three press gestures: one row, a ctrl-press that adds a row and takes
    /// it away again, and a shift-press that takes everything from the anchor.
    #[test]
    fn a_press_picks_one_row_and_two_modifiers_extend_it() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let root = temporary.path().canonicalize().expect("canonical root");
        for name in ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"] {
            std::fs::write(root.join(name), b"x").expect("file");
        }
        let mut browser = Browser::new(Some(root));
        settle(&mut browser);
        let at = |browser: &Browser, name: &str| row_of(browser, name);

        // One press selects one row.
        let a = at(&browser, "a.txt");
        browser.pick(a, false, false);
        assert_eq!(selected_names(&browser), vec!["a.txt"]);

        // Ctrl adds, and the row that had the focus stays picked.
        let c = at(&browser, "c.txt");
        browser.pick(c, true, false);
        assert_eq!(selected_names(&browser), vec!["a.txt", "c.txt"]);
        assert_eq!(browser.selected, c, "the focus follows the press");

        // Ctrl on a picked row takes it out again — and the focus stays there,
        // because the row `Enter` acts on has to be a row somebody can see.
        browser.pick(c, true, false);
        assert_eq!(selected_names(&browser), vec!["a.txt"]);
        assert_eq!(browser.selected, c);

        // Shift takes the range from the anchor: `a.txt` was the last plain pick.
        let e = at(&browser, "e.txt");
        browser.pick(e, false, false);
        browser.pick(at(&browser, "a.txt"), false, false);
        browser.pick(e, false, true);
        assert_eq!(
            selected_names(&browser),
            vec!["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"]
        );
    }

    /// A keyboard move is a new selection: there is no chord that says "keep what
    /// the pointer picked", so keeping it would be a selection with no visible
    /// end.
    #[test]
    fn a_key_move_drops_what_the_pointer_picked() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let root = temporary.path().canonicalize().expect("canonical root");
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(root.join(name), b"x").expect("file");
        }
        let mut browser = Browser::new(Some(root));
        settle(&mut browser);
        let b = row_of(&browser, "b.txt");
        browser.pick(b, false, false);
        browser.pick(row_of(&browser, "c.txt"), true, false);
        assert_eq!(selected_names(&browser).len(), 2);
        browser.handle(Key::Up);
        assert_eq!(
            selected_names(&browser).len(),
            1,
            "one row, the focus's own"
        );
        assert_eq!(browser.selection_label(), None, "one row is not a count");
    }

    /// The pick is kept by **name**, so a sort or a new batch cannot move it to a
    /// different file.
    #[test]
    fn a_sort_keeps_the_pick_on_its_files() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let root = temporary.path().canonicalize().expect("canonical root");
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(root.join(name), b"x").expect("file");
        }
        let mut browser = Browser::new(Some(root));
        settle(&mut browser);
        browser.pick(row_of(&browser, "a.txt"), false, false);
        browser.pick(row_of(&browser, "c.txt"), true, false);
        let mut view = browser.view();
        view.reverse = true;
        browser.set_view(view);
        settle(&mut browser);
        // The listing is reversed, so the selected rows are reported in its order
        // now: the pick followed the *names*, which is the whole point.
        assert_eq!(selected_names(&browser), vec!["c.txt", "a.txt"]);
        assert_eq!(
            browser.selected_paths().len(),
            2,
            "and the paths follow the names"
        );
        assert_eq!(browser.selection_label().as_deref(), Some("2 selected"));
    }

    /// The selected path is the file's, and on the parent row it is where that
    /// row goes rather than the two characters it shows.
    #[test]
    fn the_selected_path_is_the_one_a_command_line_wants() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let root = temporary.path().canonicalize().expect("canonical root");
        std::fs::write(root.join("a.txt"), b"x").expect("file");
        let mut browser = Browser::new(Some(root.clone()));
        settle(&mut browser);
        browser.selected = row_of(&browser, "a.txt");
        assert_eq!(browser.selected_path(), Some(root.join("a.txt")));
        browser.selected = 0;
        assert!(browser.entries[0].is_parent(), "the fixture starts on `..`");
        assert_eq!(
            browser.selected_path(),
            root.parent().map(Path::to_path_buf)
        );
    }

    /// The places popup starts at home and ends at the filesystem root, and it
    /// holds no borrow of the icon set: the list is owned before the popup draws.
    #[test]
    fn a_place_list_starts_at_home_and_ends_at_the_root() {
        let mut browser = Browser::new(Some(PathBuf::from("/tmp")));
        browser.without_icons();
        let places = browser.place_list();
        assert_eq!(
            places.first().map(|(label, _)| label.as_str()),
            Some("home")
        );
        assert_eq!(
            places.last().map(|(label, _)| label.as_str()),
            Some("filesystem")
        );
        assert_eq!(
            places.last().map(|(_, path)| path.clone()),
            Some(PathBuf::from("/"))
        );
    }

    /// The breadcrumb is one target per component, and the home directory is a
    /// segment of its own — the whole point of it over a path string.
    #[test]
    fn a_breadcrumb_is_one_target_per_component() {
        let home = Path::new("/home/ada");
        let trail = crumbs(Path::new("/home/ada/src/dtr/docs"), home);
        assert_eq!(
            names_of(&trail),
            vec!["~", "src", "dtr", "docs"],
            "the labels are the components"
        );
        assert_eq!(trail[0].1, PathBuf::from("/home/ada"));
        assert_eq!(trail[2].1, PathBuf::from("/home/ada/src/dtr"));
        // A path outside home starts at the root, which is a target too: "go to
        // /" is otherwise only reachable by pressing the parent four times.
        let elsewhere = crumbs(Path::new("/var/log"), home);
        assert_eq!(names_of(&elsewhere), vec!["/", "var", "log"]);
        assert_eq!(elsewhere[0].1, PathBuf::from("/"));
        assert_eq!(elsewhere[2].1, PathBuf::from("/var/log"));
        // Home itself is one segment and goes nowhere surprising.
        assert_eq!(names_of(&crumbs(home, home)), vec!["~"]);
    }

    /// `Ctrl+H` is a redraw, not a walk: the hidden entries are kept beside the
    /// shown ones, so showing them and hiding them again costs a sort each way
    /// and no directory read at all.
    #[test]
    fn showing_hidden_files_is_a_redraw_and_not_a_reload() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let root = temporary.path().canonicalize().expect("canonical root");
        std::fs::write(root.join("a.txt"), b"x").expect("file");
        std::fs::write(root.join(".bashrc"), b"x").expect("hidden file");
        std::fs::create_dir(root.join(".config")).expect("hidden directory");

        let mut browser = Browser::new(Some(root));
        settle(&mut browser);
        assert!(!names(&browser.entries).contains(&".bashrc"));
        assert!(!names(&browser.entries).contains(&".config"));
        assert_eq!(browser.hidden.len(), 2, "the two are kept beside the shown");

        browser.set_hidden(true);
        assert!(names(&browser.entries).contains(&".bashrc"));
        assert!(names(&browser.entries).contains(&".config"));
        assert!(browser.hidden.is_empty());

        browser.set_hidden(false);
        assert!(!names(&browser.entries).contains(&".bashrc"));
        assert_eq!(
            browser.hidden.len(),
            2,
            "and still kept, so it can come back"
        );
        // The listing was never re-read: the files are the same ones.
        assert!(names(&browser.entries).contains(&"a.txt"));
    }

    #[test]
    fn the_root_has_no_parent_row() {
        let entries = vec![entry("etc", true), entry("tmp", true)];
        assert_eq!(
            names(&arrange(entries.clone(), Path::new("/"), &View::default())),
            ["etc", "tmp"]
        );
        assert!(
            names(&arrange(
                entries,
                Path::new("/home/barnabas"),
                &View::default()
            ))
            .first()
                == Some(&"..")
        );
    }

    #[test]
    fn parent_resolution_stops_at_the_root() {
        assert_eq!(
            parent_of(Path::new("/home/barnabas/project")),
            Some(PathBuf::from("/home/barnabas"))
        );
        assert_eq!(parent_of(Path::new("/")), None);
        // A bare relative name has nowhere useful to go; no synthetic `..`.
        assert_eq!(parent_of(Path::new("file")), None);
    }

    /// The bug everyone ships once: a reverse that also turns the *grouping*
    /// around, so a folder list sorted by date puts every file on top of every
    /// folder.
    #[test]
    fn reversing_flips_the_key_and_not_the_grouping() {
        let entries = vec![
            entry("src", true),
            entry("docs", true),
            fact("old.txt", 10, Some(1_000)),
            fact("new.txt", 10, Some(9_000)),
        ];
        let down = arrange(
            entries.clone(),
            Path::new("/work"),
            &view(Sort::Modified, true),
        );
        assert_eq!(names(&down), ["..", "src", "docs", "new.txt", "old.txt"]);

        let up = arrange(entries, Path::new("/work"), &view(Sort::Modified, false));
        assert_eq!(names(&up), ["..", "docs", "src", "old.txt", "new.txt"]);
    }

    /// An entry whose key is unknown sinks whichever way the key runs. Sorting by
    /// date with the unreadable files on top is a list of what you cannot open,
    /// presented as the newest.
    #[test]
    fn unknown_keys_sink_in_both_directions() {
        let entries = vec![
            fact("known-old", 10, Some(1_000)),
            fact("unknown", 10, None),
            fact("known-new", 10, Some(9_000)),
        ];
        let up = arrange(
            entries.clone(),
            Path::new("/work"),
            &view(Sort::Modified, false),
        );
        assert_eq!(names(&up), ["..", "known-old", "known-new", "unknown"]);

        let down = arrange(entries, Path::new("/work"), &view(Sort::Modified, true));
        assert_eq!(names(&down), ["..", "known-new", "known-old", "unknown"]);
    }

    /// A directory's size is not its inode's length: `Caps::dir_size` is false, so
    /// the key is unknown and sinks — which is why the folders-go-last question
    /// cannot be answered by a size sort alone.
    #[test]
    fn a_directory_has_no_size_to_sort_by() {
        let entries = vec![
            entry("src", true),
            fact("small", 10, None),
            fact("big", 9_000, None),
        ];
        let view = View {
            sort: Sort::Size,
            folders_first: false,
            ..View::default()
        };
        assert_eq!(
            names(&arrange(entries, Path::new("/work"), &view)),
            ["..", "small", "big", "src"]
        );
    }

    /// `..` is synthetic and sorts after the fact: no key and no direction may
    /// sink it.
    #[test]
    fn the_parent_row_stays_first_however_the_key_runs() {
        let entries = vec![fact("a", 1, Some(9_000)), fact("b", 1, Some(1_000))];
        for sort in Sort::ALL {
            for reverse in [false, true] {
                let arranged = arrange(entries.clone(), Path::new("/work"), &view(sort, reverse));
                assert_eq!(
                    arranged.first().map(|entry| entry.name.as_str()),
                    Some(".."),
                    "{sort:?} reverse={reverse}"
                );
            }
        }
    }

    /// Two names that differ only by case have one order, and it does not depend
    /// on which way the key runs: an unstable tiebreak makes a directory jump
    /// between frames.
    #[test]
    fn names_that_differ_only_by_case_still_have_one_order() {
        let entries = vec![entry("readme", false), entry("README", false)];
        let up = arrange(
            entries.clone(),
            Path::new("/work"),
            &view(Sort::Name, false),
        );
        assert_eq!(names(&up), ["..", "README", "readme"]);
        let down = arrange(entries, Path::new("/work"), &view(Sort::Name, true));
        assert_eq!(names(&down), ["..", "readme", "README"]);
    }

    /// A type sort groups by extension, and two `.pdf`s are ordered by name
    /// inside it.
    #[test]
    fn a_type_sort_groups_by_extension() {
        let entries = vec![
            fact("b.pdf", 1, None),
            fact("notes.txt", 1, None),
            fact("a.pdf", 1, None),
            fact("Makefile", 1, None),
        ];
        assert_eq!(
            names(&arrange(
                entries,
                Path::new("/work"),
                &view(Sort::Kind, false)
            )),
            ["..", "Makefile", "a.pdf", "b.pdf", "notes.txt"]
        );
    }

    /// A batch that names a row the listing already has refines it: one row, and
    /// the later facts win. A streamed listing that forgets it is one list draws
    /// the same file twice.
    #[test]
    fn a_later_batch_patches_its_row_instead_of_duplicating_it() {
        let mut browser = Browser::remote_at("jupyter", PathBuf::from("/var/log"));
        browser.apply_listing(PathBuf::from("/var/log"), vec![entry("syslog", false)]);
        assert_eq!(browser.entries[1].size, None);

        browser.merge(vec![fact("syslog", 4_096, Some(2_000))]);
        assert_eq!(
            names(&browser.entries),
            ["..", "syslog"],
            "one row, not two"
        );
        assert_eq!(browser.entries[1].size, Some(4_096));
        assert!(browser.entries[1].modified.is_some());
    }

    /// A local directory streams: the first batch is drawn while the rest is
    /// still being read, and the pane says it is reading rather than claiming to
    /// be empty.
    #[test]
    fn a_local_listing_arrives_in_batches_and_ends() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let root = temporary.path().canonicalize().expect("canonical root");
        for index in 0..200 {
            std::fs::write(root.join(format!("file_{index:03}.log")), b"x").expect("file");
        }

        let mut browser = Browser::new(Some(root.clone()));
        assert!(browser.reading(), "the walk has started");
        settle(&mut browser);
        assert!(!browser.reading(), "and finished");
        // 200 files, the synthetic parent, and no duplicates: one row per name.
        assert_eq!(browser.entries.len(), 201);
        assert_eq!(names(&browser.entries).first().copied(), Some(".."));
        // A name sort asks for no metadata at all, which is the point of a listing
        // that carries names and kinds.
        assert!(
            browser.entries.iter().all(|entry| entry.size.is_none()),
            "a name sort took a stat it did not need"
        );
    }

    /// A size sort is the case that *does* need a stat per file, and the pane says
    /// so while it takes them.
    #[test]
    fn a_size_sort_reads_the_sizes_and_then_orders_by_them() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let root = temporary.path().canonicalize().expect("canonical root");
        std::fs::write(root.join("small"), b"x").expect("small");
        std::fs::write(root.join("large"), vec![0u8; 4096]).expect("large");

        let mut browser = Browser::new(Some(root.clone()));
        settle(&mut browser);
        browser.set_view(View {
            sort: Sort::Size,
            ..View::default()
        });
        // The next frame is what starts the pass, because the sort is what makes
        // it necessary.
        browser.poll();
        assert!(browser.reading(), "the pass started on the view change");
        settle(&mut browser);
        assert_eq!(names(&browser.entries), ["..", "small", "large"]);
        assert_eq!(browser.entries[1].size, Some(1));
        assert_eq!(browser.entries[2].size, Some(4_096));
    }

    /// A selector re-sorts what is already there, and keeps the reader on the same
    /// file while doing it.
    #[test]
    fn changing_the_sort_keeps_the_selection_on_its_file() {
        let mut browser = Browser::remote_at("jupyter", PathBuf::from("/var/log"));
        browser.apply_listing(
            PathBuf::from("/var/log"),
            vec![fact("b.log", 1, Some(1_000)), fact("a.log", 1, Some(9_000))],
        );
        browser.selected = 2;
        assert_eq!(
            browser.selected_entry().map(|e| e.name.clone()),
            Some("b.log".to_owned())
        );

        browser.set_view(View {
            sort: Sort::Modified,
            ..View::default()
        });
        assert_eq!(
            browser.selected_entry().map(|e| e.name.clone()),
            Some("b.log".to_owned()),
            "the reader is still looking at b.log"
        );
        assert_eq!(names(&browser.entries), ["..", "b.log", "a.log"]);
    }

    #[test]
    fn selection_is_clamped_after_the_list_shrinks() {
        assert_eq!(clamp_selection(9, 3), 2);
        assert_eq!(clamp_selection(1, 3), 1);
        assert_eq!(clamp_selection(9, 0), 0);
        assert_eq!(clamp_selection(0, 0), 0);
    }

    #[test]
    fn the_visible_window_follows_the_selection() {
        // Selection above the window pulls the window up.
        assert_eq!(first_visible(2, 6, 5), 2);
        // Selection below the window pulls it down.
        assert_eq!(first_visible(10, 0, 5), 6);
        // Inside the window, the stored scroll is kept.
        assert_eq!(first_visible(7, 5, 5), 5);
        // A zero-height viewport still shows one row rather than dividing by 0.
        assert_eq!(first_visible(4, 0, 0), 4);
    }

    #[test]
    fn home_and_its_children_display_with_a_tilde() {
        let home = Path::new("/home/barnabas");
        assert_eq!(display_path(home, home), "~");
        assert_eq!(
            display_path(Path::new("/home/barnabas/src/dtr"), home),
            "~/src/dtr"
        );
        assert_eq!(display_path(Path::new("/etc"), home), "/etc");
        // A sibling with the same prefix is not under home.
        assert_eq!(
            display_path(Path::new("/home/barnabassoon/x"), home),
            "/home/barnabassoon/x"
        );
    }

    #[test]
    fn joining_for_display_uses_the_same_tilde_rule() {
        let home = Path::new("/home/barnabas");
        assert_eq!(
            display_join(Path::new("/home/barnabas/src"), "dtr", home),
            "~/src/dtr"
        );
        assert_eq!(display_join(Path::new("/etc"), "hosts", home), "/etc/hosts");
    }

    #[test]
    fn sizes_are_human_readable() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1536), "1.5 kB");
        assert_eq!(human_size(2 * 1024 * 1024), "2.0 MB");
    }

    #[test]
    fn enter_descends_into_a_directory_and_hands_back_a_file() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let root = temporary.path().canonicalize().expect("canonical root");
        std::fs::create_dir(root.join("sub")).expect("subdir");
        std::fs::write(root.join("note.txt"), b"x").expect("file");

        let mut browser = Browser::new(Some(root.clone()));
        settle(&mut browser);
        browser.selected = row_of(&browser, "note.txt");
        assert_eq!(browser.handle(Key::Enter), Some(root.join("note.txt")));

        browser.selected = row_of(&browser, "sub");
        assert_eq!(browser.handle(Key::Enter), None);
        assert_eq!(browser.cwd, root.join("sub"));

        // `Backspace`/`Alt+Up` go back up through the same parent resolution the
        // listing uses. `Left` cannot: in a grid it is the cell to the left, and
        // the parent has a key of its own because of it.
        browser.handle(Key::Parent);
        assert_eq!(browser.cwd, root);
    }

    /// A grid moves in two dimensions, and "a row" is the column count the last
    /// frame measured: nothing in the model may assume a pane's width.
    #[test]
    fn a_grid_moves_by_a_row_and_a_cell() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let root = temporary.path().canonicalize().expect("canonical root");
        for name in ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"] {
            std::fs::write(root.join(name), b"x").expect("file");
        }

        let mut browser = Browser::new(Some(root));
        settle(&mut browser);
        assert_eq!(browser.entries.len(), 6, "five files and the parent row");

        // A pane that fits two cells across. The frame writes this; here it is
        // written by hand, because there is no frame.
        browser.columns.set(2);
        browser.selected = 0;
        browser.handle(Key::Down);
        assert_eq!(browser.selected, 2, "down is a row, not a cell");
        browser.handle(Key::Down);
        assert_eq!(browser.selected, 4);
        browser.handle(Key::Down);
        assert_eq!(browser.selected, 5, "clamped to the last cell, not past it");
        browser.handle(Key::Right);
        assert_eq!(browser.selected, 5, "the end of the list is the end");
        browser.handle(Key::Up);
        assert_eq!(browser.selected, 3);
        browser.handle(Key::Left);
        assert_eq!(browser.selected, 2);

        // Page keys are the frame's answer to "how much fits".
        browser.page.set(4);
        browser.selected = 0;
        browser.handle(Key::PageDown);
        assert_eq!(browser.selected, 4);
        browser.handle(Key::PageUp);
        assert_eq!(browser.selected, 0);
    }

    /// The list keeps the meaning it always had, because a dense view is what
    /// the pane shipped with and `Ctrl+2` must not change what a key does as
    /// well as what the pane looks like.
    #[test]
    fn the_list_still_moves_one_row_at_a_time() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let root = temporary.path().canonicalize().expect("canonical root");
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(root.join(name), b"x").expect("file");
        }

        let parent = root.parent().expect("the tempdir has a parent").to_owned();
        let mut browser = Browser::new(Some(root));
        settle(&mut browser);
        let mut view = browser.view();
        view.mode = Mode::List;
        browser.set_view(view);
        // A stale column count from the grid must not leak into a list.
        browser.columns.set(8);
        browser.selected = 0;
        browser.handle(Key::Down);
        assert_eq!(browser.selected, 1, "a list row is one cell");
        // Left is still the parent in a list, which is what a dense view has
        // always meant here.
        browser.handle(Key::Left);
        assert_eq!(browser.cwd, parent);
    }

    #[test]
    fn a_directory_that_cannot_be_read_is_a_message_not_a_blank_pane() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let missing = temporary.path().join("gone");
        let mut browser = Browser::new(Some(missing));
        // The failure arrives the way the listing does: as a batch.
        settle(&mut browser);
        assert!(browser.entries.is_empty());
        let message = browser.message.expect("a message");
        assert!(
            message.contains("list "),
            "the path is in the message: {message}"
        );
    }

    /// A remote pane reads nothing itself: it asks, once, and waits.
    #[test]
    fn a_remote_pane_asks_for_its_directory_once() {
        let mut browser = Browser::remote("jupyter");
        assert_eq!(
            browser.take_request(),
            Some(("jupyter".to_owned(), PathBuf::from(".")))
        );
        assert_eq!(browser.take_request(), None, "one request per navigation");
        assert!(browser.entries.is_empty());
    }

    /// A link names a directory, and the pane has to open *there* rather than at
    /// the home directory.
    #[test]
    fn a_remote_pane_can_open_at_a_linked_directory() {
        let mut browser = Browser::remote_at("jupyter", PathBuf::from("/var/log"));
        assert_eq!(browser.alias(), Some("jupyter"));
        assert_eq!(
            browser.take_request(),
            Some(("jupyter".to_owned(), PathBuf::from("/var/log")))
        );
    }

    /// The reply is matched to the request that asked for it, and the request is
    /// consumed when it is sent — so "waiting" has to outlive it. Matching on the
    /// pending request matched nothing, and every listing was dropped for a pane
    /// that then sat on "loading…" for ever.
    #[test]
    fn a_pane_waits_for_the_listing_it_sent() {
        let mut browser = Browser::remote_at("jupyter", PathBuf::from("/var/log"));
        assert!(
            !browser.is_waiting_for(Path::new("/var/log")),
            "not sent yet"
        );
        browser.take_request();
        assert!(browser.is_waiting_for(Path::new("/var/log")));
        assert!(!browser.is_waiting_for(Path::new("/etc")));
        browser.apply_listing(PathBuf::from("/var/log"), Vec::new());
        assert!(!browser.is_waiting_for(Path::new("/var/log")), "answered");
        // A local pane waits for nothing: it reads the disk when it is built.
        let local = Browser::new(Some(PathBuf::from(".")));
        assert!(!local.is_waiting_for(Path::new(".")));
    }

    /// A reply for a directory the pane is not waiting on cannot yank it away.
    #[test]
    fn a_reply_for_another_directory_is_ignored() {
        let mut browser = Browser::remote_at("jupyter", PathBuf::from("/var/log"));
        browser.take_request();
        browser.apply_listing(PathBuf::from("/etc"), Vec::new());
        assert_eq!(browser.cwd, PathBuf::from("/var/log"));
        assert!(
            browser.is_waiting_for(Path::new("/var/log")),
            "still waiting"
        );
    }
}

#[cfg(test)]
mod kittest_tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    const WIDTH: f32 = 640.0;
    // Tall enough for two rows of tiles: a snapshot of a grid that shows one row
    // proves the arithmetic and nothing about the view.
    const HEIGHT: f32 = 360.0;

    fn pane_rect() -> egui::Rect {
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(WIDTH, HEIGHT))
    }

    fn entry(name: &str, is_dir: bool, size: Option<u64>) -> Entry {
        Entry {
            name: name.to_owned(),
            is_dir,
            size,
            // No dates: the snapshot is of the rows, and a date column is F1b.
            modified: None,
        }
    }

    /// A pane whose listing is written down rather than read. A snapshot of a
    /// real `/var/log` would be a snapshot of the machine that ran the test.
    fn pane() -> Browser {
        let mut browser = Browser::remote_at("jupyter", PathBuf::from("/var/log"));
        browser.apply_listing(
            PathBuf::from("/var/log"),
            vec![
                entry("apt", true, None),
                entry("nginx", true, None),
                entry("auth.log", false, Some(1536)),
                entry("btmp", false, Some(0)),
                entry("dpkg.log", false, Some(2_621_440)),
                entry("kern.log", false, Some(933_888)),
                entry("syslog", false, Some(4_194_304)),
                entry("wtmp.1", false, Some(10_485_760)),
            ],
        );
        let at = browser
            .entries
            .iter()
            .position(|entry| entry.name == "syslog")
            .expect("syslog in the fixture");
        browser.select_only(at);
        // A baseline may not depend on the icon theme of the machine that made
        // it, so these panes draw the glyphs this crate carries.
        browser.without_icons();
        browser
    }

    /// The skin these snapshots are drawn in: the terminal's own, over the
    /// built-in theme's colours.
    ///
    /// Written out rather than asked of a window, because a baseline is an input:
    /// a browser test that resolved the desktop's palette would compare against
    /// whatever machine ran it.
    fn skin() -> Skin {
        Skin::dtr(dtr_widgets::Palette::dtr(
            [0x0b, 0x0f, 0x0e],
            [0xdc, 0xdf, 0xdd],
            [0xff, 0xb0, 0x00],
        ))
    }

    /// The header's close button: where it is, that it is the pane's only click
    /// target, and that it reports the press.
    ///
    /// The body being click-through is not an accident of the layout — it is why
    /// the button is its own area. A press on the listing is the surface's (that
    /// is what moves the focus to the pane), and an area egui can hit-test would
    /// take that press before the app ever saw it.
    #[test]
    fn the_header_carries_a_close_button() {
        let skin = skin();
        let browser = pane();
        let label = format!("close {}", browser.header());
        let pressed = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let sink = std::rc::Rc::clone(&pressed);
        let mut harness = Harness::builder()
            .with_size(egui::vec2(WIDTH, HEIGHT))
            .build_ui(move |ui| {
                if pane_ui(
                    ui,
                    egui::Id::unique("dtr-files-test"),
                    pane_rect(),
                    &browser,
                    &skin,
                    true,
                )
                .close
                {
                    sink.set(sink.get() + 1);
                }
            });
        harness.run();

        // The button is in the header, inside the pane, and to the right of
        // everything else the header draws.
        let target = close_rect(pane_rect());
        assert!(
            pane_rect().contains_rect(target),
            "the target escapes the pane"
        );
        assert!(header_rect(pane_rect()).contains_rect(target));
        assert!(
            target.left() > pane_rect().left() + WIDTH / 2.0,
            "the cross belongs at the right-hand end"
        );

        // One square is a target; the listing is not. The window's root layer is
        // everywhere and is not a surface, which is the question
        // `dtr_widgets::above_the_root` answers and the one a surface asks before
        // handing a press to whatever is behind the pane.
        let over_body = harness
            .ctx
            .layer_id_at(pane_rect().center())
            .map(dtr_widgets::above_the_root);
        assert_eq!(
            over_body,
            Some(false),
            "the pane's body must stay click-through"
        );
        let over_cross = harness
            .ctx
            .layer_id_at(target.center())
            .map(dtr_widgets::above_the_root);
        assert_eq!(over_cross, Some(true), "the cross has no target");

        harness.snapshot("file_pane_close");

        harness.get_by_label(&label).click();
        harness.run();
        assert_eq!(pressed.get(), 1, "the cross did not report the press");
    }

    /// A press selects what is under it, and the second press of the *same*
    /// entry opens it — Nautilus's rule, and the reason a press that moves the
    /// selection never opens anything.
    ///
    /// Drawn first, because the hit test is the last frame's layout: the pane is
    /// painted, so there is nothing for egui to hit-test and the numbers come
    /// from the draw that put the rows where they are.
    #[test]
    fn a_press_selects_and_the_second_press_opens() {
        let skin = skin();
        let browser = std::rc::Rc::new(std::cell::RefCell::new(pane()));
        let mut view = browser.borrow().view();
        view.mode = Mode::List;
        browser.borrow_mut().set_view(view);
        {
            let browser = std::rc::Rc::clone(&browser);
            let mut harness = Harness::builder()
                .with_size(egui::vec2(WIDTH, HEIGHT))
                .build_ui(move |ui| {
                    let browser = browser.borrow();
                    pane_ui(
                        ui,
                        egui::Id::unique("dtr-files-press"),
                        pane_rect(),
                        &browser,
                        &skin,
                        true,
                    );
                });
            harness.run();
        }

        // The fourth row of a list: `..`, the two directories, then `auth.log`.
        let body_top = HEADER_HEIGHT + 1.0;
        let point = egui::pos2(40.0, body_top + 3.0 * ROW_HEIGHT + ROW_HEIGHT / 2.0);
        assert_eq!(browser.borrow().entry_at(point), Some(3));
        assert_eq!(browser.borrow().entries[3].name, "auth.log");

        let mut browser = browser.borrow_mut();
        assert_eq!(
            browser.clicked(point, 1, false, false),
            None,
            "a press only selects"
        );
        assert_eq!(browser.selected, 3);
        // The second press of the same entry is `Enter`. This fixture is a
        // *remote* pane, where Enter asks for a copy rather than handing a path
        // to the shell — the pane has always made that distinction.
        assert_eq!(browser.clicked(point, 2, false, false), None);
        assert_eq!(
            browser.take_download(),
            Some(PathBuf::from("/var/log/auth.log"))
        );
        // A press on another entry selects that one and opens nothing.
        let other = egui::pos2(40.0, body_top + 6.0 * ROW_HEIGHT + ROW_HEIGHT / 2.0);
        assert_eq!(browser.clicked(other, 2, false, false), None);
        assert_eq!(browser.selected, 6);
    }

    /// The pointer path carries the modifiers through: ctrl adds a row and a
    /// plain press forgets the rest.
    #[test]
    fn a_control_press_on_the_pointer_picks_a_second_row() {
        let skin = skin();
        let browser = std::rc::Rc::new(std::cell::RefCell::new(pane()));
        {
            let mut browser = browser.borrow_mut();
            let mut view = browser.view();
            view.mode = Mode::List;
            browser.set_view(view);
        }
        {
            let browser = std::rc::Rc::clone(&browser);
            let drawn = skin.clone();
            let mut harness = Harness::builder()
                .with_size(egui::vec2(WIDTH, HEIGHT))
                .build_ui(move |ui| {
                    let browser = browser.borrow();
                    pane_ui(
                        ui,
                        egui::Id::unique("dtr-files-pick"),
                        pane_rect(),
                        &browser,
                        &drawn,
                        true,
                    );
                });
            harness.run();
        }
        let row = |index: usize| {
            let top = HEADER_HEIGHT + 1.0;
            egui::pos2(40.0, top + index as f32 * ROW_HEIGHT + ROW_HEIGHT / 2.0)
        };
        let mut browser = browser.borrow_mut();
        assert_eq!(browser.clicked(row(2), 1, false, false), None);
        assert!(browser.is_selected(2));
        assert_eq!(browser.clicked(row(4), 1, true, false), None);
        assert!(
            browser.is_selected(2) && browser.is_selected(4),
            "the pick is both rows"
        );
        assert_eq!(browser.selection_label().as_deref(), Some("2 selected"));
        // A plain press is a new selection, not an addition.
        assert_eq!(browser.clicked(row(6), 1, false, false), None);
        assert!(!browser.is_selected(2) && browser.is_selected(6));
        assert_eq!(browser.selection_label(), None);
    }

    /// A grid's press lands on the tile under the point and on nothing in the gap
    /// between two of them — and the drawer and the hit test agree, because both
    /// ask the layout the draw recorded.
    #[test]
    fn a_grid_press_lands_on_a_tile_and_not_in_the_gap() {
        let skin = skin();
        let browser = std::rc::Rc::new(std::cell::RefCell::new(pane()));
        {
            let browser = std::rc::Rc::clone(&browser);
            let drawn = skin.clone();
            let mut harness = Harness::builder()
                .with_size(egui::vec2(WIDTH, HEIGHT))
                .build_ui(move |ui| {
                    let browser = browser.borrow();
                    pane_ui(
                        ui,
                        egui::Id::unique("dtr-files-grid-press"),
                        pane_rect(),
                        &browser,
                        &drawn,
                        true,
                    );
                });
            harness.run();
        }
        let browser = browser.borrow();
        // The first cell: the body inset by `TILE_PAD`, half a cell in.
        let first = egui::pos2(
            pane_rect().left() + TILE_PAD + skin.style.tile_icon,
            pane_rect().top() + HEADER_HEIGHT + 1.0 + TILE_PAD + skin.style.tile_icon,
        );
        assert_eq!(browser.entry_at(first), Some(0));
        // The gap between the first and second cells belongs to nobody.
        let gap = egui::pos2(first.x + skin.style.tile_icon + TILE_GAP / 2.0, first.y);
        assert_eq!(browser.entry_at(gap), None);
        // And the header is not the listing.
        assert_eq!(browser.entry_at(egui::pos2(first.x, 8.0)), None);
    }

    /// A wheel notch moves the view and not the selection, and cannot scroll past
    /// the end of the listing.
    #[test]
    fn the_wheel_scrolls_the_view_without_moving_the_selection() {
        let skin = skin();
        let browser = std::rc::Rc::new(std::cell::RefCell::new(pane()));
        {
            // Long enough to need scrolling: the fixture's nine entries all fit.
            let names: Vec<Entry> = (0..40)
                .map(|index| entry(&format!("file{index:02}.log"), false, Some(index)))
                .collect();
            let mut browser = browser.borrow_mut();
            browser.apply_listing(PathBuf::from("/var/log"), names);
            let mut view = browser.view();
            view.mode = Mode::List;
            browser.set_view(view);
        }
        {
            let browser = std::rc::Rc::clone(&browser);
            let drawn = skin.clone();
            let mut harness = Harness::builder()
                .with_size(egui::vec2(WIDTH, HEIGHT))
                .build_ui(move |ui| {
                    let browser = browser.borrow();
                    pane_ui(
                        ui,
                        egui::Id::unique("dtr-files-wheel"),
                        pane_rect(),
                        &browser,
                        &drawn,
                        true,
                    );
                });
            harness.run();
        }
        let mut browser = browser.borrow_mut();
        let selected = browser.selected;
        let rows = browser.entries.len();
        assert!(rows > 20, "the fixture is meant to be long: {rows}");
        assert_eq!(browser.scroll.get(), 0, "a fresh listing starts at the top");

        // A notch down moves the view by a row.
        browser.scroll(-1.0);
        assert_eq!(browser.scroll.get(), 1);

        // Many notches stop at the last page: the view never scrolls into blank
        // space below the listing.
        for _ in 0..50 {
            browser.scroll(-3.0);
        }
        let visible = browser.frame.get().expect("a frame").visible;
        assert_eq!(browser.scroll.get(), rows - visible);
        assert_eq!(browser.selected, selected, "the selection did not move");

        // And up again returns to the top and no further.
        for _ in 0..50 {
            browser.scroll(3.0);
        }
        assert_eq!(browser.scroll.get(), 0);
        assert_eq!(browser.selected, selected);
    }

    /// A crumb is a press target, and the draw is what remembers where it landed:
    /// the header is painted, so there is no widget to hit-test.
    #[test]
    fn a_crumb_knows_where_it_is() {
        let skin = skin();
        let browser = std::rc::Rc::new(std::cell::RefCell::new(pane()));
        {
            let browser = std::rc::Rc::clone(&browser);
            let drawn = skin.clone();
            let mut harness = Harness::builder()
                .with_size(egui::vec2(WIDTH, HEIGHT))
                .build_ui(move |ui| {
                    let browser = browser.borrow();
                    pane_ui(
                        ui,
                        egui::Id::unique("dtr-files-crumb"),
                        pane_rect(),
                        &browser,
                        &drawn,
                        true,
                    );
                });
            harness.run();
        }
        let browser = browser.borrow();
        // Scan the header strip: the remote fixture browses `/var/log`, so the
        // crumbs are `jupyter:` then `/`, `var`, `log` — three targets, in that
        // order, and nothing between them.
        let mut found: Vec<PathBuf> = Vec::new();
        let y = HEADER_HEIGHT / 2.0;
        let mut x = pane_rect().left() + 8.0;
        while x < pane_rect().right() - 8.0 {
            if let Some(path) = browser.crumb_at(egui::pos2(x, y)) {
                if found.last() != Some(&path) {
                    found.push(path);
                }
            }
            x += 2.0;
        }
        assert_eq!(
            found,
            vec![
                PathBuf::from("/"),
                PathBuf::from("/var"),
                PathBuf::from("/var/log")
            ],
            "one target per component, in the order the path reads"
        );
        // The body is not the header.
        assert_eq!(browser.crumb_at(pane_rect().center()), None);
    }

    /// The places button opens a menu of the directories a desktop calls places,
    /// and answers with one.
    #[test]
    fn the_places_button_answers_with_a_directory() {
        let skin = skin();
        let browser = pane();
        let answer = std::rc::Rc::new(std::cell::RefCell::new(None));
        let seen = std::rc::Rc::clone(&answer);
        let mut harness = Harness::builder()
            .with_size(egui::vec2(WIDTH, HEIGHT))
            .build_ui(move |ui| {
                let outcome = pane_ui(
                    ui,
                    egui::Id::unique("dtr-files-places"),
                    pane_rect(),
                    &browser,
                    &skin,
                    true,
                );
                if outcome.navigate.is_some() {
                    *seen.borrow_mut() = outcome.navigate.clone();
                }
            });
        harness.run();
        harness.get_by_label("places").click();
        harness.run();
        for label in ["home", "filesystem"] {
            assert!(
                harness.query_by_label(label).is_some(),
                "{label} is missing from the places menu"
            );
        }
        harness.get_by_label("filesystem").click();
        harness.run();
        assert_eq!(
            *answer.borrow(),
            Some(PathBuf::from("/")),
            "the menu's answer never reached the caller"
        );
    }

    /// Two picked tiles and a footer that counts them: the state ctrl-press is
    /// for, and the only place it is seen.
    #[test]
    fn two_picked_tiles_read_as_a_count() {
        let skin = skin();
        let mut browser = pane();
        browser.select_only(3);
        browser.pick(5, true, false);
        assert_eq!(browser.selection_label().as_deref(), Some("2 selected"));
        let mut harness = Harness::builder()
            .with_size(egui::vec2(WIDTH, HEIGHT))
            .build_ui(move |ui| {
                pane_ui(
                    ui,
                    egui::Id::unique("dtr-files-picked"),
                    pane_rect(),
                    &browser,
                    &skin,
                    true,
                );
            });
        harness.run();
        harness.snapshot("file_pane_picked");
    }

    /// The order's own control: a name, a direction, and a menu that is the same
    /// four keys and two switches the keyboard has.
    ///
    /// Clicked through the accessibility node, which is also the check that the
    /// control *has* one: a painted button with no node is invisible to a screen
    /// reader and unaddressable from a test.
    #[test]
    fn the_sort_control_opens_its_menu_and_answers() {
        let skin = skin();
        let browser = pane();
        let answer = std::rc::Rc::new(std::cell::RefCell::new(None));
        let seen = std::rc::Rc::clone(&answer);
        let mut harness = Harness::builder()
            .with_size(egui::vec2(WIDTH, HEIGHT))
            .build_ui(move |ui| {
                let outcome = pane_ui(
                    ui,
                    egui::Id::unique("dtr-files-sort"),
                    pane_rect(),
                    &browser,
                    &skin,
                    true,
                );
                if outcome.sort.is_some() {
                    *seen.borrow_mut() = outcome.sort;
                }
            });
        harness.run();

        harness.get_by_label("sort by name").click();
        harness.run();
        // The menu names every key the pane can be ordered by, and the two
        // switches under them.
        for label in [
            "name",
            "modified",
            "size",
            "type",
            "reverse",
            "folders first",
        ] {
            assert!(
                harness.query_by_label(label).is_some(),
                "{label} is missing from the menu"
            );
        }
        // A picture of the menu, because a popup is the one thing a pure-function
        // test cannot check: the ticks, the order of the rows and the separator
        // are all a drawing.
        harness.snapshot("file_pane_sort_menu");
        harness.get_by_label("modified").click();
        harness.run();
        assert_eq!(
            *answer.borrow(),
            Some(SortChange::Key(Sort::Modified)),
            "the menu's answer never reached the caller"
        );
    }

    /// The list view, kept rather than replaced: a dense pane shows eight rows
    /// where it shows six tiles, and `Ctrl+2` is the escape hatch for a narrow
    /// pane rather than a matter of taste (`docs/manager.md` §3).
    #[test]
    fn the_list_view_is_kept() {
        let skin = skin();
        let mut browser = pane();
        let mut view = browser.view();
        view.mode = Mode::List;
        browser.set_view(view);
        let mut harness = Harness::builder()
            .with_size(egui::vec2(WIDTH, HEIGHT))
            .build_ui(move |ui| {
                pane_ui(
                    ui,
                    egui::Id::unique("dtr-files-list"),
                    pane_rect(),
                    &browser,
                    &skin,
                    true,
                );
            });
        harness.run();
        harness.snapshot("file_pane_list");
    }
}
