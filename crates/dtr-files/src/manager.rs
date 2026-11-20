//! The manager window's chrome: the toolbar, the sidebar and the status bar.
//!
//! The manager is the same browser as a file pane, in a window of its own
//! (`docs/manager.md` §6). What the window adds over the pane is chrome, and
//! only chrome: this module paints that chrome around a rectangle the caller
//! gives to a [`crate::Browser`]. There is no window here — no winit, no wgpu, no
//! file I/O on a timer. Everything is data in and painted `egui::Ui` plus a
//! reported [`ManagerAnswer`] out, which is the same bargain `pane_ui` makes, so
//! the whole thing is testable in a kittest harness.
//!
//! **One layout, computed once.** [`ManagerLayout`] splits the client rectangle
//! into the toolbar, the sidebar, the content and the status bar, and the window
//! that owns the `Browser` asks the same struct where to put it. A window that
//! measured its own toolbar and a painter that measured another is the
//! `pane_area` bug in `AGENTS.md`: two consumers of one rectangle disagreeing
//! puts the picture where the target is not.
//!
//! # What the caller supplies
//!
//! Nothing here reads the machine. The window (F4) reads the places through
//! [`crate::icons::Places`], the mounts through `/proc/mounts`, and this
//! session's ssh hosts through `~/.ssh/config`, then hands the rows in. The data
//! types are deliberate:
//!
//! - [`Drive`] and [`Host`] are what a caller has, before a row is built.
//! - [`SidebarEntry`] is a drawn row: a label, a [`SidebarChoice`] to report, and
//!   an optional caller-resolved picture. The manager never resolves an icon
//!   itself; [`SidebarEntry::icon_name`] is the freedesktop name the caller may
//!   use to find one.
//! - [`ManagerState`] is what the listing currently says: where it is, how it is
//!   viewed, what was typed in the search box, how many entries there are and
//!   what is selected. A struct rather than a dozen arguments, and a value the
//!   caller owns between frames.
//!
//! # What is deliberately not here
//!
//! - **No browser.** The window draws the `Browser` into
//!   [`ManagerLayout::content`]; the chrome only reserves it. A chrome module
//!   that also painted listings would need the browser's frame state and would
//!   be a second implementation of the pane.
//! - **No history.** Back and forward are buttons that report a press; a
//!   history stack is the window's, and the buttons are drawn disabled from
//!   [`ManagerState::can_go_back`] / `can_go_forward`.
//! - **No transfer queue figure.** The status bar leaves the right-hand slot
//!   empty ([`ManagerLayout::status_slots`] says where it is); the queue (F5)
//!   paints it.
//! - **A fixed sidebar.** [`SIDEBAR_WIDTH`] is a number of points, not a
//!   splitter: a draggable split is a stored ratio, a hit test and a second
//!   geometry consumer, and the milestone asks for the rows first.
//! - **Mouse only.** Rows are buttons and fields; there is no sidebar focus ring
//!   or arrow-key walk yet. The toolbar is mouse-only for the same reason the
//!   preferences card is (`docs/manager.md` §6).
//!
//! # Icons, and the one thing this crate cannot do
//!
//! The chrome's controls are `dtr-widgets` primitives — [`Button`],
//! [`dtr_widgets::segmented`], [`dtr_widgets::menu_row`], and the rows'
//! [`Mark`]. Back, forward and up are therefore *word* buttons, not Lucide
//! pictures. The tree has the SVGs (`crates/dtr/assets/icons/`), but drawing one
//! needs `egui_extras`' image loader, and `dtr-files` deliberately does not
//! depend on `egui_extras` (a picker process links this crate and nothing else).
//! The alternative would be a hand-drawn glyph, which `AGENTS.md` rules out.
//! The one glyph painted here is the burger: its response has to anchor a
//! `Popup`, and no `dtr-widgets` primitive hands a response back, so it is
//! painted from the skin's tokens the way the pane's house already is. Moving
//! both the burger and an icon button into `dtr-widgets` is the follow-up
//! (`docs/manager.md` §6).
//!
//! # Geometry
//!
//! [`ManagerLayout`], [`ManagerLayout::band_at`] and
//! [`ManagerLayout::status_slots`] are pure functions with unit tests; the paint
//! path reads the same rectangles rather than deriving its own.

use std::path::{Path, PathBuf};

use egui::{
    Align2, Color32, FontId, Rect, Sense, Stroke, Ui, pos2, text::LayoutJob, text::TextWrapping,
    vec2,
};

use dtr_widgets::{Button, Mark, Skin, Style};

use crate::browser::{Layout, View, crumbs, display_path, human_size, plain};
use crate::icons::Places;

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

/// The sidebar's width, in points.
///
/// Fixed rather than a splitter, and a constant rather than a `Style` token: a
/// draggable split is an interaction, a stored ratio and a second consumer of
/// the geometry, and the milestone asks for the rows before the split. A skin
/// that changed it would change where content starts, which is a layout the
/// window cannot re-derive — so it is one number both sides read.
pub const SIDEBAR_WIDTH: f32 = 196.0;

/// The narrowest window the chrome is laid out for, in points.
///
/// Below it the search field and the breadcrumb start sharing width; the window
/// should ask for at least this. It is a layout hint, not a clamp: the chrome
/// still paints in a smaller rectangle, it just elides more.
pub const MIN_WIDTH: f32 = 640.0;

/// The shortest window the chrome is laid out for, in points.
pub const MIN_HEIGHT: f32 = 360.0;

/// Which band of the window a point is in.
///
/// The window asks this for focus: a press in the content belongs to the
/// browser, a press in the sidebar does not, and neither should have to
/// re-derive the bands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Band {
    Toolbar,
    Sidebar,
    Content,
    Status,
}

/// The window's skeleton, in points.
///
/// Computed **once** from the client rectangle and handed to both the painter
/// and the window that places the `Browser`, so the toolbar's height cannot
/// disagree with the content's top edge (`AGENTS.md`, `pane_area`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ManagerLayout {
    /// The whole client area the chrome was computed for.
    pub client: Rect,
    /// The top band: navigation, location, search, view and menu.
    pub toolbar: Rect,
    /// The left band: places, drives and this session's hosts.
    pub sidebar: Rect,
    /// The middle: where the caller draws the `Browser`.
    pub content: Rect,
    /// The bottom band: counts, selection, path and the transfer slot.
    pub status: Rect,
}

impl ManagerLayout {
    /// Split `client` into the four bands of `skin`'s chrome.
    ///
    /// A client too short or too narrow for the chrome gets clamped bands rather
    /// than inverted rectangles: a window that is being resized down paints
    /// garbage for one frame and then closes, and a negative-height rectangle
    /// panics some of egui's painters.
    pub fn new(client: Rect, skin: &Skin) -> Self {
        Self::with_style(client, &skin.style)
    }

    /// The same split, for a caller that has the [`Style`] rather than a
    /// [`Skin`] — a test building the skeleton without a palette.
    pub fn with_style(client: Rect, style: &Style) -> Self {
        let width = client.width().max(0.0);
        let height = client.height().max(0.0);
        let toolbar_height = Self::toolbar_height(style).min(height);
        let status_height = Self::status_height(style).min(height - toolbar_height);
        let sidebar_width = SIDEBAR_WIDTH.min(width);

        let toolbar = Rect::from_min_size(client.min, vec2(width, toolbar_height));
        let status = Rect::from_min_size(
            pos2(client.left(), client.bottom() - status_height),
            vec2(width, status_height),
        );
        let middle = Rect::from_min_max(
            pos2(client.left(), toolbar.bottom()),
            pos2(client.right(), status.top()),
        );
        let sidebar = Rect::from_min_size(middle.min, vec2(sidebar_width, middle.height()));
        let content = Rect::from_min_max(pos2(sidebar.right(), middle.top()), middle.max);
        Self {
            client,
            toolbar,
            sidebar,
            content,
            status,
        }
    }

    /// The toolbar's height: the skin's row over a gap at each edge.
    ///
    /// The toolbar *is* one row of controls in a band with air above and below,
    /// so a skin that makes its controls taller makes its toolbar taller, and
    /// the content moves down with it. A constant here would put an Adwaita
    /// button through the top of the content.
    pub fn toolbar_height(style: &Style) -> f32 {
        style.row + style.gap * 2.0
    }

    /// The status bar's height: one row of text, with the same air as the
    /// toolbar.
    pub fn status_height(style: &Style) -> f32 {
        style.row + style.gap * 2.0
    }

    /// The sidebar's width, in points.
    pub fn sidebar_width(_style: &Style) -> f32 {
        SIDEBAR_WIDTH
    }

    /// Which band `at` falls in, or `None` outside the client.
    ///
    /// The bands share their edges, so the comparisons are half-open — a point
    /// on the toolbar's floor belongs to what is below it, and a point on the
    /// sidebar's right edge belongs to the content. `Rect::contains` includes
    /// the boundary, which would make a shared edge answer twice; this is why the
    /// question is asked by coordinate.
    pub fn band_at(&self, at: egui::Pos2) -> Option<Band> {
        let inside = self.client.left() <= at.x
            && at.x < self.client.right()
            && self.client.top() <= at.y
            && at.y < self.client.bottom();
        if !inside {
            return None;
        }
        if at.y < self.toolbar.bottom() {
            Some(Band::Toolbar)
        } else if at.y >= self.status.top() {
            Some(Band::Status)
        } else if at.x < self.sidebar.right() {
            Some(Band::Sidebar)
        } else {
            Some(Band::Content)
        }
    }

    /// The status bar's four slots: counts, selection, path and the transfer
    /// queue's figure.
    ///
    /// Pure, because the queue (F5) and the bar have to agree about where the
    /// right-hand figure goes, and a queue that measured its own slot would grow
    /// a second answer. The transfer slot is deliberately empty today: this task
    /// reserves the room, the queue fills it.
    pub fn status_slots(&self, style: &Style) -> StatusSlots {
        let inner = self.status.shrink2(vec2(style.gap, 0.0));
        let transfer_width = (inner.width() * 0.18)
            .clamp(96.0, 200.0)
            .min(inner.width() * 0.4);
        let transfer = Rect::from_min_size(
            pos2(inner.right() - transfer_width, inner.top()),
            vec2(transfer_width, inner.height()),
        );
        let rest = (inner.width() - transfer_width).max(0.0);
        let counts = Rect::from_min_size(inner.min, vec2(rest * 0.24, inner.height()));
        let selection = Rect::from_min_size(
            pos2(counts.right(), inner.top()),
            vec2(rest * 0.24, inner.height()),
        );
        let path = Rect::from_min_max(pos2(selection.right(), inner.top()), transfer.min);
        StatusSlots {
            counts,
            selection,
            path,
            transfer,
        }
    }
}

/// The status bar's slots, in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StatusSlots {
    /// The item count, or how far a walk has got.
    pub counts: Rect,
    /// How many entries are selected, and their total size.
    pub selection: Rect,
    /// The directory being shown.
    pub path: Rect,
    /// The transfer queue's figure: reserved, and painted by the queue.
    pub transfer: Rect,
}

// ---------------------------------------------------------------------------
// What the caller supplies
// ---------------------------------------------------------------------------

/// Whether the toolbar's location is a breadcrumb or a text field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LocationMode {
    /// One target per path component, as a pane's header draws it.
    #[default]
    Breadcrumb,
    /// A text field: type a path and press Enter.
    Edit,
}

/// How much is selected, and how big it is when the sizes are known.
///
/// `bytes` is an `Option` and not a `u64` for the reason the listing's sizes are:
/// a remote backend may not know a directory's size, and a sum with a made-up
/// zero in it is a smaller number than the truth. `None` means "at least one
/// selected entry has no known size", so the bar says the count and no total.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SelectionSummary {
    pub count: usize,
    pub bytes: Option<u64>,
}

/// What the listing is doing right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EntryCount {
    /// Entries in the directory, or as many as have arrived so far.
    pub total: usize,
    /// Entries the search/filter left on screen.
    pub shown: usize,
    /// Whether a walk is still in flight: the bar says `reading… n so far`
    /// rather than reporting an empty directory as a fact.
    pub reading: bool,
}

/// The manager's own state: what the window tells the chrome each frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ManagerState {
    /// The directory the content is showing.
    pub location: PathBuf,
    /// The account's home, for `~` in the breadcrumb, the path and the places.
    pub home: PathBuf,
    /// The ssh alias the content is browsing, when it is remote.
    ///
    /// A local directory and a host's directory can have the same path, so the
    /// path alone cannot say which machine a place row is showing.
    pub remote: Option<String>,
    /// The listing's view: the grid/list toggle and the sort's switches.
    pub view: View,
    /// Whether the location is a breadcrumb or a field.
    pub location_mode: LocationMode,
    /// What is typed in the search box.
    pub search: String,
    /// The listing's counts.
    pub entries: EntryCount,
    /// The selection's count and total size.
    pub selection: SelectionSummary,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub can_go_up: bool,
}

impl ManagerState {
    /// The minimum a window must fill in: where the content is, whose home it
    /// is, and the view it is shown in.
    pub fn new(location: impl Into<PathBuf>, home: impl Into<PathBuf>, view: View) -> Self {
        Self {
            location: location.into(),
            home: home.into(),
            view,
            ..Self::default()
        }
    }
}

/// A real mount, as a sidebar row's data: the caller reads the mounts.
///
/// A drive's `label` is what the desktop calls it (`Filesystem`, `Data`,
/// `C:`); `path` is where it is mounted. The manager does not read
/// `/proc/mounts`, because a crate that draws a window must not also decide
/// what a mount is (§6: another stream owns the mount list).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drive {
    pub label: String,
    pub path: PathBuf,
}

/// This session's ssh host, as a sidebar row's data.
///
/// `home` is the far end's home directory when the caller has asked the SFTP
/// client for it (`dtr_sftp::Session::home`); a host whose home is not known
/// yet still gets a row, and opens at `.`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Host {
    pub alias: String,
    pub home: Option<PathBuf>,
}

/// What choosing a sidebar row means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidebarChoice {
    /// A directory on this machine: a place or a drive.
    Path(PathBuf),
    /// Another machine's directory, through its ssh alias.
    Host {
        alias: String,
        home: Option<PathBuf>,
    },
}

impl SidebarChoice {
    /// Whether this row is the one the content is showing.
    ///
    /// A path row is current when the content is local and on that path; a host
    /// row is current when the content is browsing that alias. The two are
    /// separate questions because `"~/src"` and `"jupyter:~/src"` are different
    /// directories.
    pub fn is_current(&self, location: &Path, remote: Option<&str>) -> bool {
        match (self, remote) {
            (SidebarChoice::Path(path), None) => path == location,
            (SidebarChoice::Host { alias, .. }, Some(open)) => alias == open,
            _ => false,
        }
    }
}

/// One drawn sidebar row.
///
/// The label is the row's text, the choice is what a press reports, and the
/// picture is the caller's: the manager does not resolve a theme icon (it has no
/// image loader, and a sidebar is not worth a machine-dependent snapshot). A row
/// with no picture still reserves the picture's column, so labels line up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarEntry {
    pub label: String,
    pub choice: SidebarChoice,
    /// The freedesktop icon name a theme could answer with, for the caller's
    /// resolver. The manager never reads it, and the snapshot never depends on
    /// it.
    pub icon_name: Option<&'static str>,
    /// The picture the caller resolved and uploaded, if it has one.
    pub icon: Option<egui::TextureId>,
    /// Whether this row is where the content is.
    pub selected: bool,
}

impl SidebarEntry {
    /// A row that opens a directory on this machine.
    pub fn path(label: impl Into<String>, path: impl Into<PathBuf>, selected: bool) -> Self {
        Self {
            label: label.into(),
            choice: SidebarChoice::Path(path.into()),
            icon_name: None,
            icon: None,
            selected,
        }
    }

    /// A row that opens an ssh host's home directory.
    pub fn host(alias: impl Into<String>, home: Option<PathBuf>, selected: bool) -> Self {
        let alias = alias.into();
        Self {
            label: alias.clone(),
            choice: SidebarChoice::Host { alias, home },
            icon_name: None,
            icon: None,
            selected,
        }
    }

    /// The icon name a theme should answer with (`folder-download`, `user-home`
    /// …), for the caller's resolver.
    pub fn named(mut self, icon_name: &'static str) -> Self {
        self.icon_name = Some(icon_name);
        self
    }

    /// The picture the caller uploaded for this row.
    pub fn pictured(mut self, icon: egui::TextureId) -> Self {
        self.icon = Some(icon);
        self
    }
}

/// The sidebar's three sections.
///
/// Empty sections are not drawn: a machine with no ssh config should not read as
/// a machine with an empty "This session" heading.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sidebar {
    pub places: Vec<SidebarEntry>,
    pub drives: Vec<SidebarEntry>,
    pub hosts: Vec<SidebarEntry>,
}

impl Sidebar {
    /// The *Places* section: home, the directories the desktop decorated, and
    /// the filesystem root.
    ///
    /// The order is the desktop's own list, which is what `user-dirs.dirs`
    /// promises (`docs/manager.md` §3), then home first and the root last — the
    /// two every desktop puts at the ends. `location` decides which row is
    /// selected; the caller can override the pictures afterwards with
    /// [`SidebarEntry::pictured`].
    pub fn places(places: &Places, home: &Path, location: &Path) -> Vec<SidebarEntry> {
        let mut entries =
            vec![SidebarEntry::path("home", home, location == home).named("user-home")];
        for (path, icon) in places.iter() {
            let label = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            entries.push(SidebarEntry::path(label, path, location == path).named(icon));
        }
        // The root of the volume home is on: `/` on Unix, `C:\` on Windows. A
        // caller on a machine with no home gets `/`, which is still one target.
        let root = home
            .ancestors()
            .last()
            .filter(|root| !root.as_os_str().is_empty())
            .unwrap_or(Path::new("/"));
        if !entries
            .iter()
            .any(|entry| matches!(&entry.choice, SidebarChoice::Path(path) if path == root))
        {
            let label = plain(root).display().to_string();
            entries.push(SidebarEntry::path(label, root, location == root).named("drive-harddisk"));
        }
        entries
    }

    /// The *Drives* section, in the caller's order: the mounts it read.
    pub fn drives(drives: &[Drive], location: &Path) -> Vec<SidebarEntry> {
        drives
            .iter()
            .map(|drive| {
                SidebarEntry::path(
                    drive.label.clone(),
                    drive.path.clone(),
                    location == drive.path,
                )
            })
            .collect()
    }

    /// The *This session* section: the ssh hosts the caller found, and which one
    /// the content is browsing.
    pub fn hosts(hosts: &[Host], remote: Option<&str>) -> Vec<SidebarEntry> {
        hosts
            .iter()
            .map(|host| {
                SidebarEntry::host(
                    host.alias.clone(),
                    host.home.clone(),
                    remote == Some(host.alias.as_str()),
                )
            })
            .collect()
    }

    /// Whether every section is empty: a sidebar with nothing in it is still
    /// painted (it is the window's shape), but a caller can skip it.
    pub fn is_empty(&self) -> bool {
        self.places.is_empty() && self.drives.is_empty() && self.hosts.is_empty()
    }
}

// ---------------------------------------------------------------------------
// What the chrome reports
// ---------------------------------------------------------------------------

/// A row of the burger menu, as the caller acts on it.
///
/// The values are *requests*, not state: the window decides whether a grid/list
/// toggle writes the config, whether a location edit starts a navigation, and
/// what "show hidden" means for the listing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuChoice {
    /// Show the grid (`Ctrl+1`).
    Grid,
    /// Show the list (`Ctrl+2`).
    List,
    /// Turn the breadcrumb into a text field.
    EditLocation,
    /// Turn the location field back into a breadcrumb.
    BreadcrumbLocation,
    /// Flip whether hidden entries are shown (`Ctrl+H`).
    ToggleHidden,
}

/// Everything the chrome was asked to do this frame.
///
/// Fields rather than one enum because two answers can arrive in the same frame
/// — a menu closes while the search field reports a keystroke — and a caller
/// that had to pick one would drop the other.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ManagerAnswer {
    /// The back button was pressed.
    pub back: bool,
    /// The forward button was pressed.
    pub forward: bool,
    /// The up button was pressed.
    pub up: bool,
    /// A breadcrumb segment (or the editable field's Enter) chose a directory.
    pub navigate: Option<PathBuf>,
    /// A sidebar row was pressed.
    pub sidebar: Option<SidebarChoice>,
    /// The search field's text changed.
    pub search: Option<String>,
    /// The grid/list toggle chose the other view.
    pub layout: Option<Layout>,
    /// The location changed between breadcrumb and field.
    pub location_mode: Option<LocationMode>,
    /// The location field's text changed.
    pub typed_location: Option<String>,
    /// The location field was submitted (Enter while it had the focus).
    pub location_submitted: bool,
    /// A burger menu row was chosen.
    pub menu: Option<MenuChoice>,
}

impl ManagerAnswer {
    /// Whether anything was asked for this frame.
    pub fn is_empty(&self) -> bool {
        !self.back
            && !self.forward
            && !self.up
            && self.navigate.is_none()
            && self.sidebar.is_none()
            && self.search.is_none()
            && self.layout.is_none()
            && self.location_mode.is_none()
            && self.typed_location.is_none()
            && !self.location_submitted
            && self.menu.is_none()
    }

    /// Fold one frame's answer into another.
    ///
    /// A press and its release can land in two frames — a harness runs as many
    /// frames as a click needs, and a window that asks for a repaint does the
    /// same — so a caller that kept only the last frame's answer would lose the
    /// press. `Option` fields keep the newer answer when both frames carried
    /// one, which is what a field being typed into wants: the whole text.
    pub fn merge(&mut self, other: &Self) {
        self.back |= other.back;
        self.forward |= other.forward;
        self.up |= other.up;
        self.location_submitted |= other.location_submitted;
        if other.navigate.is_some() {
            self.navigate.clone_from(&other.navigate);
        }
        if other.sidebar.is_some() {
            self.sidebar.clone_from(&other.sidebar);
        }
        if other.search.is_some() {
            self.search.clone_from(&other.search);
        }
        if other.layout.is_some() {
            self.layout = other.layout;
        }
        if other.location_mode.is_some() {
            self.location_mode = other.location_mode;
        }
        if other.typed_location.is_some() {
            self.typed_location.clone_from(&other.typed_location);
        }
        if other.menu.is_some() {
            self.menu = other.menu;
        }
    }
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// Paint the manager's chrome into `ui`, reporting what was pressed.
///
/// `layout` must be the one from [`ManagerLayout::new`] for the same client
/// rectangle — the window keeps it and places the `Browser` in
/// [`ManagerLayout::content`], so the two cannot disagree about where the chrome
/// ends. `ui` is the window's root `Ui`; the chrome uses the skin's tokens for
/// every colour, radius and stroke width.
pub fn manager_ui(
    ui: &mut Ui,
    layout: &ManagerLayout,
    skin: &Skin,
    state: &ManagerState,
    sidebar: &Sidebar,
) -> ManagerAnswer {
    // The stock widgets in the fields and popups read the context style, and a
    // manager window is where they must look like the window's chrome.
    skin.apply(ui.ctx());
    let palette = skin.palette;
    let style = &skin.style;
    let mut answer = ManagerAnswer::default();

    // The window behind everything, and the content's own card. The browser
    // paints over the content rectangle after this, so the card is the colour of
    // an empty one.
    let painter = ui.painter().with_clip_rect(layout.client);
    painter.rect_filled(layout.client, 0.0, palette.window);
    painter.rect_filled(layout.content, 0.0, palette.card);

    toolbar_band(ui, layout, skin, state, &mut answer);
    if let Some(choice) = sidebar_band(ui, layout, skin, sidebar) {
        answer.sidebar = Some(choice);
    }
    status_band(ui, layout, skin, state);

    // The rules between the bands, drawn last so a band's own fill cannot cover
    // them: the toolbar's floor, the sidebar's right edge and the status bar's
    // ceiling.
    let rule = Stroke::new(style.border, palette.divider);
    let painter = ui.painter().with_clip_rect(layout.client);
    painter.line_segment(
        [
            pos2(
                layout.client.left(),
                layout.toolbar.bottom() - style.border / 2.0,
            ),
            pos2(
                layout.client.right(),
                layout.toolbar.bottom() - style.border / 2.0,
            ),
        ],
        rule,
    );
    painter.line_segment(
        [
            pos2(
                layout.sidebar.right() - style.border / 2.0,
                layout.sidebar.top(),
            ),
            pos2(
                layout.sidebar.right() - style.border / 2.0,
                layout.sidebar.bottom(),
            ),
        ],
        rule,
    );
    painter.line_segment(
        [
            pos2(
                layout.client.left(),
                layout.status.top() + style.border / 2.0,
            ),
            pos2(
                layout.client.right(),
                layout.status.top() + style.border / 2.0,
            ),
        ],
        rule,
    );
    answer
}

/// The toolbar: navigation, location, search, view and menu.
fn toolbar_band(
    ui: &mut Ui,
    layout: &ManagerLayout,
    skin: &Skin,
    state: &ManagerState,
    answer: &mut ManagerAnswer,
) {
    let style = &skin.style;
    let gap = style.gap;
    let band = layout.toolbar;
    let inner = Rect::from_min_size(
        pos2(band.left() + gap, band.center().y - style.row / 2.0),
        vec2((band.width() - gap * 2.0).max(0.0), style.row),
    );
    if inner.width() < 1.0 {
        return;
    }

    // Measured first: the slots are rectangles, and every control is drawn into
    // the one it was given. The alternative — a flow that lets each widget take
    // what it needs — puts the menu wherever the breadcrumb ended, which is
    // exactly the drift a toolbar is noticed for.
    let back_width = Button::measured_width(ui, skin, "back");
    let forward_width = Button::measured_width(ui, skin, "forward");
    let up_width = Button::measured_width(ui, skin, "up");
    // `segmented` lays its chips out with 4 points between them
    // (`crates/dtr-widgets/src/widgets/segmented.rs`); the slot has to include
    // that or the last chip is clipped.
    const SEGMENT_GAP: f32 = 4.0;
    let view_width = Button::measured_width(ui, skin, Layout::Grid.label())
        + Button::measured_width(ui, skin, Layout::List.label())
        + SEGMENT_GAP;
    let menu_width = style.row;

    let left = back_width + gap + forward_width + gap + up_width;
    let right = view_width + gap + menu_width;
    let middle = (inner.width() - left - right - gap * 3.0).max(0.0);
    // The search box gets a share of the middle and the breadcrumb the rest,
    // with floors so neither vanishes on a narrow window. The floors can
    // overflow a window below `MIN_WIDTH`; the painter clips rather than
    // overlapping the menu.
    let location_floor = (middle * 0.45).clamp(80.0, 260.0);
    let search_width = (middle - location_floor - gap)
        .clamp(70.0, 200.0)
        .min(middle);
    let location_width = (middle - search_width - gap).max(0.0);

    let mut x = inner.left();
    let back = slot(&mut x, inner.top(), back_width, style.row, gap);
    let forward = slot(&mut x, inner.top(), forward_width, style.row, gap);
    let up = slot(&mut x, inner.top(), up_width, style.row, gap);
    let location = slot(&mut x, inner.top(), location_width, style.row, gap);
    let search = slot(&mut x, inner.top(), search_width, style.row, gap);
    let view = slot(&mut x, inner.top(), view_width, style.row, gap);
    let menu = slot(&mut x, inner.top(), menu_width, style.row, 0.0);

    let button = |ui: &mut Ui, rect: Rect, label: &str, enabled: bool| {
        let mut pressed = false;
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            pressed = Button::new(label).enabled(enabled).show(ui, skin);
        });
        pressed
    };
    answer.back = button(ui, back, "back", state.can_go_back);
    answer.forward = button(ui, forward, "forward", state.can_go_forward);
    answer.up = button(ui, up, "up", state.can_go_up);

    match state.location_mode {
        LocationMode::Breadcrumb => crumb_bar(ui, skin, location, state, answer),
        LocationMode::Edit => location_field(ui, location, state, answer),
    }
    search_field(ui, search, state, answer);
    if let Some(layout) = view_toggle(ui, skin, view, state) {
        answer.layout = Some(layout);
    }
    menu_button(ui, skin, menu, state, answer);
}

/// A slot in the toolbar's row, advancing the cursor past it.
fn slot(x: &mut f32, top: f32, width: f32, height: f32, gap: f32) -> Rect {
    let rect = Rect::from_min_size(pos2(*x, top), vec2(width.max(0.0), height));
    *x += width + gap;
    rect
}

/// The breadcrumb: one target per component, the tail always kept.
///
/// The segments come from [`crumbs`], the pane's own function, so the window and
/// a pane cannot spell one path two ways. What does not fit is dropped from the
/// *front* — the directory being looked at is the one that matters — and a `…`
/// says so. A target is painted rather than made from `Button`s because a
/// breadcrumb is a row of words with a hover, not a row of boxes; its hover fill
/// and radius come from the skin like everything else.
fn crumb_bar(
    ui: &mut Ui,
    skin: &Skin,
    rect: Rect,
    state: &ManagerState,
    answer: &mut ManagerAnswer,
) {
    if rect.width() < 4.0 {
        return;
    }
    let palette = skin.palette;
    let style = &skin.style;
    let segments = crumbs(&state.location, &state.home);
    let font = style.value_font.clone();
    let pad = style.padding.x;
    let painter = ui.painter().with_clip_rect(rect);

    let widths: Vec<f32> = segments
        .iter()
        .map(|(label, _)| {
            painter
                .layout_no_wrap(label.clone(), font.clone(), palette.text)
                .size()
                .x
                + pad * 2.0
        })
        .collect();
    let separator_width = painter
        .layout_no_wrap(
            "/".to_owned(),
            FontId::new(style.label_font.size - 1.5, style.label_font.family.clone()),
            palette.dim,
        )
        .size()
        .x
        .max(6.0)
        + style.gap / 2.0;

    // Fit from the end: the current directory never loses, and the home segment
    // is the first thing to go when the path is deeper than the toolbar.
    let mut first = segments.len();
    let mut used = 0.0;
    for index in (0..segments.len()).rev() {
        let needed = widths[index]
            + if first < segments.len() {
                separator_width
            } else {
                0.0
            };
        if used + needed > rect.width() && first < segments.len() {
            break;
        }
        used += needed;
        first = index;
    }
    let elided = first > 0;
    let mut x = rect.left();
    if elided {
        let ellipsis = painter.layout_no_wrap("…".to_owned(), font.clone(), palette.dim);
        let width = ellipsis.size().x;
        painter.galley(
            pos2(x + pad / 2.0, rect.center().y - ellipsis.size().y / 2.0),
            ellipsis,
            palette.dim,
        );
        x += width + style.gap / 2.0;
        if !segments.is_empty() {
            x += separator_width;
        }
    }

    for (index, (label, path)) in segments.iter().enumerate().skip(first) {
        let width = widths[index].min((rect.right() - x).max(0.0));
        let target = Rect::from_min_size(pos2(x, rect.top()), vec2(width, rect.height()));
        if width < 2.0 {
            break;
        }
        let response = ui.interact(
            target,
            ui.make_persistent_id(("dtr-files-manager-crumb", index)),
            Sense::click(),
        );
        let hovered = response.hovered();
        if hovered {
            painter.rect_filled(target.shrink(1.0), style.chip_radius, palette.control_hover);
        }
        let galley = painter.layout_no_wrap(label.clone(), font.clone(), palette.text);
        painter.galley(
            pos2(
                target.left() + pad,
                target.center().y - galley.size().y / 2.0,
            ),
            galley,
            palette.text,
        );
        let name = format!("open {label}");
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &name));
        if response.has_focus() {
            response.surrender_focus();
        }
        // Enter is the shell's key on a pane and the manager's window key
        // elsewhere; strip the synthetic click like every painted control.
        let mut response = response;
        response
            .flags
            .remove(egui::response::Flags::FAKE_PRIMARY_CLICKED);
        if response.clicked() {
            answer.navigate = Some(path.clone());
        }
        x += width;
        if index + 1 < segments.len() {
            let separator = painter.layout_no_wrap(
                "/".to_owned(),
                FontId::new(style.label_font.size - 1.5, style.label_font.family.clone()),
                palette.dim,
            );
            painter.galley(
                pos2(
                    x + style.gap / 2.0,
                    rect.center().y - separator.size().y / 2.0,
                ),
                separator,
                palette.dim,
            );
            x += separator_width;
        }
    }
}

/// The location as a text field, when the window is in
/// [`LocationMode::Edit`].
///
/// The text starts as the path a person would type — not `~`, not a verbatim
/// Windows path (`plain`) — because the field is what gets edited and sent back.
fn location_field(ui: &mut Ui, rect: Rect, state: &ManagerState, answer: &mut ManagerAnswer) {
    if rect.width() < 8.0 {
        return;
    }
    let mut text = plain(&state.location).display().to_string();
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.spacing_mut().interact_size.y = rect.height();
        let response = ui.add_sized(
            rect.size(),
            egui::TextEdit::singleline(&mut text)
                .id(egui::Id::unique("dtr-files-manager-location"))
                .hint_text("a path")
                .desired_width(rect.width())
                .align(egui::Align2::LEFT_CENTER),
        );
        if response.changed() {
            answer.typed_location = Some(text);
        }
        if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            answer.location_submitted = true;
        }
    });
}

/// The search/filter field.
///
/// The text is a local copy handed back on change, which is the only shape a
/// `TextEdit` has in an immediate-mode frame: the caller's [`ManagerState`] stays
/// immutable and the window writes what it is told.
fn search_field(ui: &mut Ui, rect: Rect, state: &ManagerState, answer: &mut ManagerAnswer) {
    if rect.width() < 8.0 {
        return;
    }
    let mut text = state.search.clone();
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.spacing_mut().interact_size.y = rect.height();
        let response = ui.add_sized(
            rect.size(),
            egui::TextEdit::singleline(&mut text)
                .id(egui::Id::unique("dtr-files-manager-search"))
                .hint_text("search")
                .desired_width(rect.width())
                .align(egui::Align2::LEFT_CENTER),
        );
        if response.changed() {
            answer.search = Some(text);
        }
    });
}

/// The grid/list toggle: a [`dtr_widgets::segmented`] over [`Layout::ALL`].
fn view_toggle(ui: &mut Ui, skin: &Skin, rect: Rect, state: &ManagerState) -> Option<Layout> {
    if rect.width() < 8.0 {
        return None;
    }
    let mut picked = None;
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        picked = dtr_widgets::segmented(
            ui,
            skin,
            "view",
            state.view.layout,
            &[
                (Layout::Grid, Layout::Grid.label()),
                (Layout::List, Layout::List.label()),
            ],
        );
    });
    picked
}

/// The burger at the right-hand end: the window's own menu.
///
/// The button is painted rather than made from [`Button`] for one reason: a
/// `Popup` is anchored to a `Response`, and `Button::show` reports only a `bool`.
/// The fill, radius, glyph colour and stroke width are the skin's; the glyph is
/// three lines, the same way the pane's house is six.
fn menu_button(
    ui: &mut Ui,
    skin: &Skin,
    rect: Rect,
    state: &ManagerState,
    answer: &mut ManagerAnswer,
) {
    if rect.width() < 4.0 {
        return;
    }
    let palette = skin.palette;
    let style = &skin.style;
    let mut picked = None;
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        let response = ui.interact(
            rect,
            egui::Id::unique("dtr-files-manager-menu"),
            Sense::click(),
        );
        let hovered = response.hovered() || egui::Popup::is_id_open(ui.ctx(), response.id);
        if hovered {
            ui.painter().rect_filled(
                rect.shrink(2.0),
                style.control_radius,
                palette.control_hover,
            );
        }
        paint_burger(
            ui.painter(),
            rect,
            if hovered { palette.text } else { palette.dim },
            style.border.max(1.2),
        );
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "menu"));
        if response.has_focus() {
            response.surrender_focus();
        }

        let rows = [
            dtr_widgets::RowSpec {
                label: "grid",
                icon: false,
            },
            dtr_widgets::RowSpec {
                label: "list",
                icon: false,
            },
            dtr_widgets::RowSpec {
                label: "show hidden",
                icon: false,
            },
            dtr_widgets::RowSpec {
                label: if state.location_mode == LocationMode::Breadcrumb {
                    "edit location"
                } else {
                    "breadcrumb location"
                },
                icon: false,
            },
        ];
        let width = dtr_widgets::menu_width(ui, skin, &rows);
        let mut popup = egui::Popup::menu(&response).width(width);
        if let Some(tip) = style.tip_height {
            popup = dtr_widgets::point_at(ui, popup, rect, tip);
        }
        let popup = popup.show(|ui| {
            ui.set_min_width(width);
            let layout = state.view.layout;
            if dtr_widgets::menu_row(ui, skin, "grid", layout == Layout::Grid) {
                picked = Some(MenuChoice::Grid);
                ui.close();
            }
            if dtr_widgets::menu_row(ui, skin, "list", layout == Layout::List) {
                picked = Some(MenuChoice::List);
                ui.close();
            }
            dtr_widgets::menu_separator(ui, skin);
            let edit = state.location_mode == LocationMode::Breadcrumb;
            let label = if edit {
                "edit location"
            } else {
                "breadcrumb location"
            };
            if dtr_widgets::menu_row(ui, skin, label, false) {
                picked = Some(if edit {
                    MenuChoice::EditLocation
                } else {
                    MenuChoice::BreadcrumbLocation
                });
                ui.close();
            }
            dtr_widgets::menu_separator(ui, skin);
            if dtr_widgets::menu_row(ui, skin, "show hidden", state.view.show_hidden) {
                picked = Some(MenuChoice::ToggleHidden);
                ui.close();
            }
        });
        if let Some(popup) = popup {
            dtr_widgets::menu_tip(&popup.response, skin, rect);
        }
    });
    if let Some(choice) = picked {
        answer.menu = Some(choice);
        // The mode travels as its own answer too: the window can act on "the
        // location is a field now" without matching the menu's row, and a menu
        // that only said which row was pressed would make every caller write
        // that match.
        answer.location_mode = match choice {
            MenuChoice::EditLocation => Some(LocationMode::Edit),
            MenuChoice::BreadcrumbLocation => Some(LocationMode::Breadcrumb),
            _ => None,
        };
    }
}

/// Three lines: the burger the tree's own strip uses, drawn here because an
/// embedded SVG needs an image loader this crate does not carry.
fn paint_burger(painter: &egui::Painter, rect: Rect, colour: Color32, width: f32) {
    let arm = rect.width().min(rect.height()) * 0.22;
    let stroke = Stroke::new(width, colour);
    for step in [-1.0_f32, 0.0, 1.0] {
        let y = rect.center().y + step * arm;
        painter.line_segment(
            [
                pos2(rect.center().x - arm, y),
                pos2(rect.center().x + arm, y),
            ],
            stroke,
        );
    }
}

/// The sidebar: Places, Drives, and this session's hosts.
fn sidebar_band(
    ui: &mut Ui,
    layout: &ManagerLayout,
    skin: &Skin,
    sidebar: &Sidebar,
) -> Option<SidebarChoice> {
    let band = layout.sidebar;
    let palette = skin.palette;
    let style = &skin.style;
    if band.width() < 8.0 || band.height() < 8.0 {
        return None;
    }
    let painter = ui.painter().with_clip_rect(band);
    painter.rect_filled(band, 0.0, palette.window);

    let inner = band.shrink2(vec2(style.gap, style.gap));
    let mut chosen = None;
    ui.scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| {
        ui.set_clip_rect(inner);
        ui.spacing_mut().item_spacing.y = style.gap / 2.0;
        for (title, entries) in [
            ("Places", &sidebar.places),
            ("Drives", &sidebar.drives),
            ("This session", &sidebar.hosts),
        ] {
            if entries.is_empty() {
                continue;
            }
            // A window shorter than its sidebar stops drawing rows rather than
            // letting them fall behind the status bar: a clipped row is gone,
            // but a row painted under the bar is a control that is visible in
            // the wrong place.
            if ui.next_widget_position().y + style.row > inner.bottom() {
                break;
            }
            dtr_widgets::section(ui, skin, title);
            for entry in entries.iter() {
                if ui.next_widget_position().y + style.row > inner.bottom() {
                    break;
                }
                let mark = entry.icon.map(Mark::Texture);
                if dtr_widgets::menu_row_icon(ui, skin, mark, &entry.label, entry.selected) {
                    chosen = Some(entry.choice.clone());
                }
            }
        }
    });
    chosen
}

/// The status bar: counts on the left, the selection, the path, and the queue's
/// reserved slot on the right.
fn status_band(ui: &mut Ui, layout: &ManagerLayout, skin: &Skin, state: &ManagerState) {
    let band = layout.status;
    if band.width() < 8.0 || band.height() < 8.0 {
        return;
    }
    let palette = skin.palette;
    let style = &skin.style;
    let painter = ui.painter().with_clip_rect(band);
    painter.rect_filled(band, 0.0, palette.window);

    let slots = layout.status_slots(style);
    let font = style.value_font.clone();
    paint_elided(
        ui,
        slots.counts,
        &counts_label(state),
        font.clone(),
        palette.dim,
        Align2::LEFT_CENTER,
    );
    let selected = selection_label(state.selection);
    if !selected.is_empty() {
        paint_elided(
            ui,
            slots.selection,
            &selected,
            font.clone(),
            palette.text,
            Align2::LEFT_CENTER,
        );
    }
    paint_elided(
        ui,
        slots.path,
        &location_label(state),
        font,
        palette.dim,
        Align2::RIGHT_CENTER,
    );
    // `slots.transfer` is deliberately not painted: it is the queue's (F5).
}

/// How many entries there are, or how far a walk has got.
///
/// A streamed listing must not render its emptiness as a fact
/// (`docs/manager.md` §3), so `reading` wins over the count and says `so far`.
fn counts_label(state: &ManagerState) -> String {
    let entries = state.entries;
    if entries.reading {
        return format!("reading… {} so far", entries.total);
    }
    if !state.search.is_empty() && entries.shown != entries.total {
        return format!("{} of {}", entries.shown, entries.total);
    }
    match entries.total {
        1 => "1 item".to_owned(),
        total => format!("{total} items"),
    }
}

/// How much is selected, when anything is.
fn selection_label(selection: SelectionSummary) -> String {
    if selection.count == 0 {
        return String::new();
    }
    let count = match selection.count {
        1 => "1 selected".to_owned(),
        count => format!("{count} selected"),
    };
    match selection.bytes {
        Some(bytes) => format!("{count} · {}", human_size(bytes)),
        None => count,
    }
}

/// The content's path, with the ssh alias in front when it is remote.
///
/// `jupyter:/srv` and `/srv` are different directories, and a status bar that
/// dropped the machine would be the pane header's old lie (`docs/files.md`).
fn location_label(state: &ManagerState) -> String {
    let path = display_path(&state.location, &state.home);
    match &state.remote {
        Some(alias) => format!("{alias}:{path}"),
        None => path,
    }
}

/// One line of text in a slot: elided at the slot's width, never wrapped and
/// never spilling into the next slot.
fn paint_elided(ui: &Ui, rect: Rect, text: &str, font: FontId, colour: Color32, align: Align2) {
    if text.is_empty() || rect.width() < 2.0 {
        return;
    }
    let mut job = LayoutJob::simple(text.to_owned(), font, colour, rect.width());
    job.wrap = TextWrapping {
        max_width: rect.width(),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = ui.painter().layout_job(job);
    let position = if align == Align2::RIGHT_CENTER {
        pos2(
            rect.right() - galley.size().x,
            rect.center().y - galley.size().y / 2.0,
        )
    } else {
        pos2(rect.left(), rect.center().y - galley.size().y / 2.0)
    };
    ui.painter().galley(position, galley, colour);
}

// ---------------------------------------------------------------------------
// Tests: geometry and the bar's words
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use dtr_widgets::{Palette, Style};

    fn skin() -> Skin {
        Skin::dtr(Palette::dtr(
            [0x0b, 0x0f, 0x0e],
            [0xdc, 0xdf, 0xdd],
            [0xff, 0xb0, 0x00],
        ))
    }

    fn client() -> Rect {
        Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 620.0))
    }

    #[test]
    fn the_bands_tile_the_client_without_a_gap_or_an_overlap() {
        let skin = skin();
        let layout = ManagerLayout::new(client(), &skin);
        assert_eq!(layout.toolbar.width(), 900.0);
        assert_eq!(
            layout.toolbar,
            Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, layout.toolbar.height()))
        );
        assert_eq!(layout.status.bottom(), 620.0);
        assert_eq!(layout.status.width(), 900.0);
        // The sidebar starts where the toolbar ends and stops where the status
        // bar starts; the content starts where the sidebar ends.
        assert_eq!(layout.sidebar.top(), layout.toolbar.bottom());
        assert_eq!(layout.sidebar.bottom(), layout.status.top());
        assert_eq!(layout.content.top(), layout.sidebar.top());
        assert_eq!(layout.content.bottom(), layout.sidebar.bottom());
        assert_eq!(layout.content.left(), layout.sidebar.right());
        assert_eq!(layout.content.right(), 900.0);
        // Their areas sum to the client's.
        let area = |rect: Rect| rect.width() * rect.height();
        assert_eq!(
            area(layout.toolbar)
                + area(layout.sidebar)
                + area(layout.content)
                + area(layout.status),
            area(layout.client)
        );
    }

    #[test]
    fn the_heights_follow_the_skin_row() {
        for style in [Style::dtr(), Style::adwaita(), Style::egui()] {
            assert_eq!(
                ManagerLayout::toolbar_height(&style),
                style.row + style.gap * 2.0
            );
            assert_eq!(
                ManagerLayout::status_height(&style),
                style.row + style.gap * 2.0
            );
            assert_eq!(ManagerLayout::sidebar_width(&style), SIDEBAR_WIDTH);
        }
    }

    #[test]
    fn every_band_is_hit_where_it_is_and_nowhere_else() {
        let layout = ManagerLayout::new(client(), &skin());
        assert_eq!(layout.band_at(pos2(450.0, 1.0)), Some(Band::Toolbar));
        assert_eq!(layout.band_at(pos2(50.0, 300.0)), Some(Band::Sidebar));
        assert_eq!(layout.band_at(pos2(450.0, 300.0)), Some(Band::Content));
        assert_eq!(layout.band_at(pos2(450.0, 619.0)), Some(Band::Status));
        assert_eq!(layout.band_at(pos2(-1.0, 300.0)), None);
        assert_eq!(layout.band_at(pos2(450.0, 621.0)), None);
        // A shared edge belongs to the band below (or to the right), never to
        // both: exactly one answer, so a press is never claimed twice.
        assert_eq!(
            layout.band_at(pos2(450.0, layout.toolbar.bottom())),
            Some(Band::Content)
        );
        assert_eq!(
            layout.band_at(pos2(layout.sidebar.right(), 300.0)),
            Some(Band::Content)
        );
        assert_eq!(
            layout.band_at(pos2(450.0, layout.status.top())),
            Some(Band::Status)
        );
    }

    #[test]
    fn a_client_too_short_for_its_chrome_still_has_four_bands() {
        let tiny = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(40.0, 20.0));
        let layout = ManagerLayout::new(tiny, &skin());
        for band in [
            layout.toolbar,
            layout.sidebar,
            layout.content,
            layout.status,
        ] {
            assert!(band.width() >= 0.0 && band.height() >= 0.0, "{band:?}");
            assert!(
                layout.client.contains_rect(band) || band.width() == 0.0 || band.height() == 0.0,
                "{band:?}"
            );
        }
        assert_eq!(layout.toolbar.top(), 0.0);
        assert_eq!(layout.status.bottom(), 20.0);
    }

    #[test]
    fn the_status_slots_are_disjoint_and_the_transfer_slot_is_the_right_end() {
        let style = Style::dtr();
        let layout = ManagerLayout::new(client(), &skin());
        let slots = layout.status_slots(&style);
        for (name, rect) in [
            ("counts", slots.counts),
            ("selection", slots.selection),
            ("path", slots.path),
            ("transfer", slots.transfer),
        ] {
            assert!(
                layout.status.contains_rect(rect),
                "{name} escapes the bar: {rect:?}"
            );
            assert!(rect.width() > 0.0, "{name} has no width");
        }
        let ordered = [slots.counts, slots.selection, slots.path, slots.transfer];
        for pair in ordered.windows(2) {
            assert!(
                pair[0].right() <= pair[1].left() + 0.01,
                "slots overlap: {pair:?}"
            );
        }
        assert_eq!(slots.transfer.right(), layout.status.right() - style.gap);
        assert_eq!(slots.transfer.height(), layout.status.height());
    }

    #[test]
    fn the_status_bar_says_what_the_listing_is_doing() {
        let mut state = ManagerState::new("/home/ana/src", "/home/ana", View::default());
        state.entries = EntryCount {
            total: 12,
            shown: 12,
            reading: false,
        };
        assert_eq!(counts_label(&state), "12 items");
        state.entries.total = 1;
        assert_eq!(counts_label(&state), "1 item");
        state.entries.reading = true;
        state.entries.total = 64;
        assert_eq!(counts_label(&state), "reading… 64 so far");
        state.entries.reading = false;
        state.search = "log".to_owned();
        state.entries = EntryCount {
            total: 128,
            shown: 3,
            reading: false,
        };
        assert_eq!(counts_label(&state), "3 of 128");
        // A search that hides nothing does not pretend to filter.
        state.entries.shown = 128;
        assert_eq!(counts_label(&state), "128 items");
    }

    #[test]
    fn the_selection_summary_counts_and_weighs() {
        assert_eq!(selection_label(SelectionSummary::default()), "");
        assert_eq!(
            selection_label(SelectionSummary {
                count: 1,
                bytes: Some(1536)
            }),
            "1 selected · 1.5 kB"
        );
        assert_eq!(
            selection_label(SelectionSummary {
                count: 3,
                bytes: Some(4_509_696)
            }),
            "3 selected · 4.3 MB"
        );
        // An unknown size is not a zero: the bar keeps the count and drops the
        // total.
        assert_eq!(
            selection_label(SelectionSummary {
                count: 2,
                bytes: None
            }),
            "2 selected"
        );
    }

    #[test]
    fn a_remote_path_names_its_machine() {
        let mut state = ManagerState::new("/srv/logs", "/home/ana", View::default());
        assert_eq!(location_label(&state), "/srv/logs");
        state.remote = Some("jupyter".to_owned());
        assert_eq!(location_label(&state), "jupyter:/srv/logs");
        state.location = PathBuf::from("/home/ana/notes");
        assert_eq!(location_label(&state), "jupyter:~/notes");
    }

    #[test]
    fn an_answer_merges_across_the_frames_of_one_gesture() {
        let mut pressed = ManagerAnswer::default();
        pressed.back = true;
        pressed.search = Some("l".to_owned());
        let mut released = ManagerAnswer::default();
        released.search = Some("log".to_owned());
        released.layout = Some(Layout::List);
        pressed.merge(&released);
        assert!(pressed.back, "the press survived the release's frame");
        assert_eq!(
            pressed.search.as_deref(),
            Some("log"),
            "the newer text wins"
        );
        assert_eq!(pressed.layout, Some(Layout::List));
        // An empty frame is not a reason to forget anything.
        pressed.merge(&ManagerAnswer::default());
        assert!(pressed.back && pressed.layout.is_some());
    }

    #[test]
    fn a_sidebars_current_row_is_the_content_and_only_the_content() {
        let local = SidebarChoice::Path(PathBuf::from("/home/ana"));
        assert!(local.is_current(Path::new("/home/ana"), None));
        assert!(!local.is_current(Path::new("/home/ana"), Some("jupyter")));
        let remote = SidebarChoice::Host {
            alias: "jupyter".to_owned(),
            home: None,
        };
        assert!(remote.is_current(Path::new("/srv"), Some("jupyter")));
        assert!(!remote.is_current(Path::new("/srv"), None));
        assert!(
            !SidebarChoice::Path(PathBuf::from("/srv"))
                .is_current(Path::new("/srv"), Some("jupyter"))
        );
    }

    #[test]
    fn the_places_section_is_home_decorations_root() {
        let places = Places::parse(
            "XDG_DOWNLOAD_DIR=\"$HOME/Downloads\"\nXDG_PICTURES_DIR=\"$HOME/Pictures\"\n",
            Path::new("/home/ana"),
        );
        let entries = Sidebar::places(
            &places,
            Path::new("/home/ana"),
            Path::new("/home/ana/Downloads"),
        );
        let labels: Vec<&str> = entries.iter().map(|entry| entry.label.as_str()).collect();
        assert_eq!(labels, ["home", "Downloads", "Pictures", "/"]);
        assert_eq!(entries[0].icon_name, Some("user-home"));
        assert_eq!(entries[1].icon_name, Some("folder-download"));
        assert_eq!(entries[3].icon_name, Some("drive-harddisk"));
        assert!(!entries[0].selected);
        assert!(
            entries[1].selected,
            "the decorated directory is the one on screen"
        );
        assert!(!entries[3].selected);
    }

    #[test]
    fn the_host_rows_are_selected_by_alias_not_by_path() {
        let hosts = [
            Host {
                alias: "jupyter".to_owned(),
                home: Some(PathBuf::from("/home/ana")),
            },
            Host {
                alias: "build".to_owned(),
                home: None,
            },
        ];
        let entries = Sidebar::hosts(&hosts, Some("build"));
        assert_eq!(entries.len(), 2);
        assert!(!entries[0].selected);
        assert!(entries[1].selected);
        assert_eq!(entries[0].label, "jupyter");
        assert_eq!(
            entries[0].choice,
            SidebarChoice::Host {
                alias: "jupyter".to_owned(),
                home: Some(PathBuf::from("/home/ana"))
            }
        );
    }
}
