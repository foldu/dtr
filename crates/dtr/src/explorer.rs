//! A file-manager pane.
//!
//! The pane is deliberately small: a directory listing, a selection, and the
//! one thing a terminal wants from a file manager — "put this path on the
//! command line". It is not an editor, and it does not shell out to `ls`.
//!
//! The filesystem-facing part is a thin layer over [`std::fs::read_dir`]. The
//! decisions that are easy to get wrong (ordering, selection clamping, what
//! `..` means, how a path under `$HOME` is shown) live in plain functions that
//! take values, so their tests never touch a disk.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use egui::{Align2, FontId, Stroke};

use crate::ui::Look;

const ROW_HEIGHT: f32 = 18.0;
const HEADER_HEIGHT: f32 = 26.0;
const FOOTER_HEIGHT: f32 = 18.0;

/// One line in the listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    /// File size in bytes; `None` for directories and for entries the OS would
    /// not report one for.
    pub size: Option<u64>,
}

impl Entry {
    fn parent() -> Self {
        Self {
            name: "..".to_owned(),
            is_dir: true,
            size: None,
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
    Left,
    Enter,
    Reload,
    Top,
    Bottom,
}

/// A file pane: a directory, its entries, and where the selection is.
pub struct Explorer {
    cwd: PathBuf,
    home: PathBuf,
    entries: Vec<Entry>,
    selected: usize,
    /// First row the user scrolled to. The drawing code raises it if the
    /// selection would otherwise be off-screen, so this is a hint rather than
    /// an invariant.
    scroll: Cell<usize>,
    show_hidden: bool,
    /// A filesystem problem to show in the pane itself. The pane stays usable
    /// and its header stays visible; it never goes blank.
    message: Option<String>,
    /// Set when this pane browses another machine over SFTP. `None` is the
    /// local filesystem, which is the only case that reads `std::fs`.
    remote: Option<Remote>,
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

impl Explorer {
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
        let mut explorer = Self {
            cwd: path,
            home: PathBuf::from("."),
            entries: Vec::new(),
            selected: 0,
            scroll: Cell::new(0),
            show_hidden: false,
            message: None,
            remote: Some(Remote {
                alias: alias.to_owned(),
                pending: None,
                awaiting: None,
                download: None,
            }),
        };
        explorer.remote_request();
        explorer
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
        self.entries = arrange(entries, &self.cwd, self.show_hidden);
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
        let mut explorer = Self {
            cwd: PathBuf::from("."),
            home,
            entries: Vec::new(),
            selected: 0,
            scroll: Cell::new(0),
            show_hidden: false,
            message: None,
            remote: None,
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
        explorer.cwd = cwd;
        explorer.reload();

        if let Some(name) = select {
            let name = name.to_string_lossy();
            if let Some(index) = explorer.entries.iter().position(|entry| entry.name == name) {
                explorer.selected = index;
            }
        }
        explorer
    }

    /// What the status strip calls this pane. A remote pane names the host,
    /// because the same path means different things on two machines.
    pub fn title(&self) -> String {
        match self.alias() {
            Some(alias) => format!("files  {alias}:{}", display_path(&self.cwd, &self.home)),
            None => format!("files  {}", display_path(&self.cwd, &self.home)),
        }
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
        match key {
            Key::Up => {
                self.selected = self.selected.saturating_sub(1);
            }
            Key::Down => {
                if !self.entries.is_empty() {
                    self.selected = (self.selected + 1).min(self.entries.len() - 1);
                }
            }
            Key::Top => self.selected = 0,
            Key::Bottom => self.selected = self.entries.len().saturating_sub(1),
            Key::Reload => {
                let selected = self.selected_entry().map(|entry| entry.name.clone());
                self.reload();
                if let Some(name) = selected {
                    if let Some(index) = self
                        .entries
                        .iter()
                        .position(|entry| entry.name == name && !entry.is_parent())
                    {
                        self.selected = index;
                    }
                }
            }
            Key::Left => self.go_up(),
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
        self.selected = 0;
        self.scroll.set(0);
        self.reload();
    }

    /// Read the directory again. A failure is a message in the pane, not a
    /// panic and not an empty pane: the header and the message both stay.
    fn reload(&mut self) {
        if self.remote.is_some() {
            let selected = self.selected_entry().map(|entry| entry.name.clone());
            let _ = selected;
            self.entries.clear();
            self.selected = 0;
            self.scroll.set(0);
            self.remote_request();
            return;
        }
        self.message = None;
        self.entries.clear();
        self.selected = 0;
        self.scroll.set(0);

        let read = match std::fs::read_dir(&self.cwd) {
            Ok(read) => read,
            Err(err) => {
                self.message = Some(format!(
                    "cannot open {}: {err}",
                    display_path(&self.cwd, &self.home)
                ));
                return;
            }
        };

        let mut unreadable = 0usize;
        let mut entries = Vec::new();
        for item in read {
            match item {
                Ok(item) => {
                    let name = item.file_name().to_string_lossy().into_owned();
                    let is_dir = item.file_type().is_ok_and(|kind| kind.is_dir());
                    let size = (!is_dir)
                        .then(|| item.metadata().ok().map(|meta| meta.len()))
                        .flatten();
                    entries.push(Entry {
                        name,
                        is_dir,
                        size,
                    });
                }
                Err(_) => unreadable += 1,
            }
        }

        self.entries = arrange(entries, &self.cwd, self.show_hidden);
        if unreadable > 0 {
            self.message = Some(format!(
                "{unreadable} entr{} could not be read",
                if unreadable == 1 { "y" } else { "ies" }
            ));
        }
        self.selected = clamp_selection(self.selected, self.entries.len());
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
    fn draw(&self, ui: &mut egui::Ui, look: &Look, rect: egui::Rect, focused: bool) {
        if rect.width() < 8.0 || rect.height() < 8.0 {
            return;
        }
        let painter = ui.painter().with_clip_rect(rect);

        painter.rect_filled(rect, 0.0, Look::colour(look.surface));

        // Header: the path, and how many real entries the listing has.
        let header_bottom = (rect.top() + HEADER_HEIGHT).min(rect.bottom());
        let header = egui::Rect::from_min_max(rect.min, egui::pos2(rect.right(), header_bottom));
        painter.rect_filled(header, 0.0, Look::colour(look.background));
        painter.text(
            egui::pos2(header.left() + 8.0, header.center().y),
            Align2::LEFT_CENTER,
            self.header(),
            FontId::monospace(12.0),
            Look::colour(look.foreground),
        );
        let count = self
            .entries
            .iter()
            .filter(|entry| !entry.is_parent())
            .count();
        painter.text(
            egui::pos2(header.right() - 8.0, header.center().y),
            Align2::RIGHT_CENTER,
            count.to_string(),
            FontId::monospace(10.0),
            Look::colour(look.dim),
        );
        painter.line_segment(
            [
                egui::pos2(rect.left(), header_bottom),
                egui::pos2(rect.right(), header_bottom),
            ],
            Stroke::new(1.0, Look::colour(look.dim).gamma_multiply(0.35)),
        );

        let footer_top = (rect.bottom() - FOOTER_HEIGHT).max(header_bottom);
        let body = egui::Rect::from_min_max(
            egui::pos2(rect.left(), header_bottom + 1.0),
            egui::pos2(rect.right(), footer_top),
        );
        let visible = (body.height() / ROW_HEIGHT).floor().max(0.0) as usize;
        let first = self.first_visible(visible);
        self.scroll.set(first);

        if !self.entries.is_empty() && visible > 0 {
            for (offset, index) in (first..self.entries.len().min(first + visible)).enumerate() {
                let row = egui::Rect::from_min_size(
                    egui::pos2(body.left(), body.top() + offset as f32 * ROW_HEIGHT),
                    egui::vec2(body.width(), ROW_HEIGHT),
                );
                let selected = index == self.selected;
                // The highlight goes down first; painting a rectangle after
                // the text hides the row it was meant to select.
                if selected {
                    painter.rect_filled(
                        row.shrink2(egui::vec2(4.0, 1.0)),
                        2.0,
                        Look::colour(look.accent).gamma_multiply(0.28),
                    );
                }

                let entry = &self.entries[index];
                let (label, colour) = if entry.is_parent() {
                    ("../".to_owned(), Look::colour(look.dim))
                } else if entry.is_dir {
                    (
                        format!("{}/", entry.name),
                        if selected {
                            Look::colour(look.accent)
                        } else {
                            Look::colour(look.foreground)
                        },
                    )
                } else {
                    (
                        entry.name.clone(),
                        if selected {
                            Look::colour(look.accent)
                        } else {
                            Look::colour(look.foreground)
                        },
                    )
                };
                painter.text(
                    egui::pos2(row.left() + 8.0, row.center().y),
                    Align2::LEFT_CENTER,
                    label,
                    FontId::monospace(12.0),
                    colour,
                );
                if let Some(size) = entry.size {
                    painter.text(
                        egui::pos2(row.right() - 8.0, row.center().y),
                        Align2::RIGHT_CENTER,
                        human_size(size),
                        FontId::monospace(10.0),
                        Look::colour(look.dim),
                    );
                }
            }
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
                FontId::monospace(11.0),
                Look::colour(look.accent),
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
                FontId::monospace(10.0),
                Look::colour(look.dim).gamma_multiply(0.8),
            );
        } else if self.message.is_none() {
            painter.text(
                footer.center(),
                Align2::CENTER_CENTER,
                "empty directory",
                FontId::monospace(10.0),
                Look::colour(look.dim),
            );
        }

        let border = if focused {
            Look::colour(look.accent).gamma_multiply(0.8)
        } else {
            Look::colour(look.dim).gamma_multiply(0.4)
        };
        painter.rect_stroke(
            rect,
            0.0,
            Stroke::new(if focused { 1.5 } else { 1.0 }, border),
            egui::StrokeKind::Inside,
        );
    }
}

/// One file pane as the renderer sees it: a physical rectangle, the explorer,
/// and whether it currently has focus.
pub struct PaneView<'a> {
    pub id: u64,
    pub rect: crate::layout::Rect,
    pub explorer: &'a Explorer,
    pub focused: bool,
}

/// Draw one file pane in an egui area at its leaf's rectangle.
///
/// `rect` is physical pixels; egui works in logical points, so the conversion
/// is `physical / pixels_per_point`. The area is clipped to the pane so a long
/// listing cannot spill into the terminal next door.
pub fn pane_ui(
    ui: &mut egui::Ui,
    id: egui::Id,
    rect: crate::layout::Rect,
    explorer: &Explorer,
    look: &Look,
    focused: bool,
) {
    let pixels_per_point = ui.ctx().pixels_per_point();
    let rect = egui::Rect::from_min_size(
        egui::pos2(rect.x / pixels_per_point, rect.y / pixels_per_point),
        egui::vec2(
            rect.width / pixels_per_point,
            rect.height / pixels_per_point,
        ),
    );
    let ctx = ui.ctx().clone();
    // Prime the area's size: an unprimed area is invisible in the one frame a
    // `--screenshot` renders.
    crate::ui::prime_area(&ctx, id, egui::Order::Background, rect, false);
    egui::Area::new(id)
        // Below the palette's foreground layer, but above the root background
        // the panels and the grid live in.
        .order(egui::Order::Background)
        .fixed_pos(rect.min)
        .constrain(false)
        // The pane is painted, not clicked: focus and selection are winit's
        // business, so the area must not swallow the pointer.
        .interactable(false)
        // A capture is one frame; a fade-in would make the whole pane faint.
        .fade_in(false)
        .show(&ctx, |ui| {
            ui.set_min_size(rect.size());
            ui.set_max_size(rect.size());
            ui.set_clip_rect(rect);
            explorer.draw(ui, look, rect, focused);
        });
}

/// Directories first, then case-insensitive name order, with the synthetic
/// `..` row prepended when there is a parent. Hidden entries are dropped unless
/// asked for. No filesystem access: the entries are already read.
pub fn arrange(entries: Vec<Entry>, cwd: &Path, show_hidden: bool) -> Vec<Entry> {
    let mut entries: Vec<Entry> = entries
        .into_iter()
        .filter(|entry| show_hidden || !entry.name.starts_with('.'))
        .collect();
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            // Equal ignoring case still needs a stable order.
            .then_with(|| a.name.cmp(&b.name))
    });
    if parent_of(cwd).is_some() {
        entries.insert(0, Entry::parent());
    }
    entries
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
    if len == 0 {
        0
    } else {
        selected.min(len - 1)
    }
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

/// A byte count a person can read. Shared with the app's toasts, which report a
/// save the same way the pane reports a file.
pub(crate) fn human_size(bytes: u64) -> String {
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
        Entry, Key, arrange, clamp_selection, display_join, display_path, first_visible,
        human_size, parent_of, Explorer,
    };

    fn entry(name: &str, is_dir: bool) -> Entry {
        Entry {
            name: name.to_owned(),
            is_dir,
            size: None,
        }
    }

    fn names(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.name.as_str()).collect()
    }

    #[test]
    fn directories_come_first_and_names_sort_case_insensitively() {
        let entries = vec![
            entry("beta.txt", false),
            entry("alpha", true),
            entry("Delta.txt", false),
            entry("charlie", true),
        ];
        let arranged = arrange(entries, Path::new("/tmp/work"), false);
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
        let hidden = arrange(entries.clone(), Path::new("/tmp/work"), false);
        assert_eq!(names(&hidden), ["..", "visible"]);
        let shown = arrange(entries, Path::new("/tmp/work"), true);
        assert_eq!(names(&shown), ["..", ".git", ".bashrc", "visible"]);
    }

    #[test]
    fn the_root_has_no_parent_row() {
        let entries = vec![entry("etc", true), entry("tmp", true)];
        assert_eq!(
            names(&arrange(entries.clone(), Path::new("/"), false)),
            ["etc", "tmp"]
        );
        assert!(
            names(&arrange(entries, Path::new("/home/barnabas"), false)).first() == Some(&"..")
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

        let mut explorer = Explorer::new(Some(root.clone()));
        explorer.selected = explorer
            .entries
            .iter()
            .position(|entry| entry.name == "note.txt")
            .expect("note.txt in the listing");
        assert_eq!(explorer.handle(Key::Enter), Some(root.join("note.txt")));

        explorer.selected = explorer
            .entries
            .iter()
            .position(|entry| entry.name == "sub")
            .expect("sub in the listing");
        assert_eq!(explorer.handle(Key::Enter), None);
        assert_eq!(explorer.cwd, root.join("sub"));

        // `h` goes back up through the same parent resolution the listing uses.
        explorer.handle(Key::Left);
        assert_eq!(explorer.cwd, root);
    }

    #[test]
    fn a_directory_that_cannot_be_read_is_a_message_not_a_blank_pane() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let missing = temporary.path().join("gone");
        let explorer = Explorer::new(Some(missing));
        assert!(explorer.entries.is_empty());
        let message = explorer.message.expect("a message");
        assert!(message.contains("cannot open"), "{message}");
    }

    /// A remote pane reads nothing itself: it asks, once, and waits.
    #[test]
    fn a_remote_pane_asks_for_its_directory_once() {
        let mut explorer = Explorer::remote("jupyter");
        assert_eq!(
            explorer.take_request(),
            Some(("jupyter".to_owned(), PathBuf::from(".")))
        );
        assert_eq!(explorer.take_request(), None, "one request per navigation");
        assert!(explorer.entries.is_empty());
    }

    /// A link names a directory, and the pane has to open *there* rather than at
    /// the home directory.
    #[test]
    fn a_remote_pane_can_open_at_a_linked_directory() {
        let mut explorer = Explorer::remote_at("jupyter", PathBuf::from("/var/log"));
        assert_eq!(explorer.alias(), Some("jupyter"));
        assert_eq!(
            explorer.take_request(),
            Some(("jupyter".to_owned(), PathBuf::from("/var/log")))
        );
    }

    /// The reply is matched to the request that asked for it, and the request is
    /// consumed when it is sent — so "waiting" has to outlive it. Matching on the
    /// pending request matched nothing, and every listing was dropped for a pane
    /// that then sat on "loading…" for ever.
    #[test]
    fn a_pane_waits_for_the_listing_it_sent() {
        let mut explorer = Explorer::remote_at("jupyter", PathBuf::from("/var/log"));
        assert!(!explorer.is_waiting_for(Path::new("/var/log")), "not sent yet");
        explorer.take_request();
        assert!(explorer.is_waiting_for(Path::new("/var/log")));
        assert!(!explorer.is_waiting_for(Path::new("/etc")));
        explorer.apply_listing(PathBuf::from("/var/log"), Vec::new());
        assert!(!explorer.is_waiting_for(Path::new("/var/log")), "answered");
        // A local pane waits for nothing: it reads the disk when it is built.
        let local = Explorer::new(Some(PathBuf::from(".")));
        assert!(!local.is_waiting_for(Path::new(".")));
    }

    /// A reply for a directory the pane is not waiting on cannot yank it away.
    #[test]
    fn a_reply_for_another_directory_is_ignored() {
        let mut explorer = Explorer::remote_at("jupyter", PathBuf::from("/var/log"));
        explorer.take_request();
        explorer.apply_listing(PathBuf::from("/etc"), Vec::new());
        assert_eq!(explorer.cwd, PathBuf::from("/var/log"));
        assert!(explorer.is_waiting_for(Path::new("/var/log")), "still waiting");
    }
}
