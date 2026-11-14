//! What a picker is asked for, and what a person's answer is.
//!
//! This is F2 of `docs/manager.md`: the same browser with a footer and an answer.
//! The mode is *what the pane is for* — the grid-or-list question is a
//! [`Layout`](crate::Layout) — and the interesting part is not the drawing but
//! the decisions:
//!
//! - **Which of the two things a press can mean** ("open this", "go in there")
//!   is the same in both modes; only the footer differs.
//! - **A filter is a promise about the answer**, not a decoration: a file that
//!   the chosen filter would not show is refused out loud rather than handed
//!   back, because a picker whose button returns something the caller cannot use
//!   is the one outcome that must not happen (`docs/files.md` §8 is the same rule
//!   for the platform dialog).
//! - **A save whose name exists is a question, not a replacement**: `Replace` is
//!   an answer of its own, and the second press is what accepts it.
//!
//! Everything here that can be decided from values is a free function or a
//! method with no I/O in it, so the answers are tested without a window and
//! without a disk. [`Pick::accept`] is the one place that asks the filesystem —
//! "does this name exist, and may anything be written here" — and it asks only
//! through [`exists`] and [`writable`], two functions a test can read.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

/// What the pane is for.
///
/// `Manage` is the browser as it has always been — a pane in a tab, later the
/// manager window. `Pick` is the same browser answering a question, which is why
/// it is a mode of one widget rather than a second widget
/// (`docs/manager.md` §4).
#[derive(Debug, Default)]
pub enum Mode {
    #[default]
    Manage,
    Pick(Pick),
}

impl Mode {
    pub fn pick(&self) -> Option<&Pick> {
        match self {
            Mode::Manage => None,
            Mode::Pick(pick) => Some(pick),
        }
    }
}

/// How many answers a request wants, and where they come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Select {
    /// The file under the cursor.
    File,
    /// Every selected file, the cursor included. The picker supplies at least
    /// one, because the cursor is always selected.
    Files,
    /// A folder: the one being shown, as every desktop's folder chooser means it.
    Directory,
    /// A name typed into the footer, joined onto the folder being shown.
    Save,
}

/// One line of the filter list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filter {
    /// What the footer calls it: "text files", "PNG images".
    pub name: String,
    /// The portal's patterns: `*.[ch]`, `*.png`, `*`. `fnmatch` with `*`, `?`
    /// and `[…]` classes, matched against the whole name and ignoring case.
    pub patterns: Vec<String>,
}

impl Filter {
    pub fn new(name: impl Into<String>, patterns: impl IntoIterator<Item = String>) -> Self {
        Self {
            name: name.into(),
            patterns: patterns.into_iter().collect(),
        }
    }

    /// The everyday shape: a name and one pattern.
    pub fn of(name: &str, pattern: &str) -> Self {
        Self::new(name, [pattern.to_owned()])
    }

    /// Whether `name` is one of this filter's kinds.
    ///
    /// Case-insensitive on purpose: a file called `Photo.PNG` is a PNG, and the
    /// portal's patterns are the same globs the desktop's own dialogs use.
    pub fn matches(&self, name: &str) -> bool {
        self.patterns.iter().any(|pattern| glob(pattern, name))
    }

    /// The extension this filter saves with, when it has exactly one.
    ///
    /// Read from the first pattern's last `*`: `*.png` has one, `*` and `*.[ch]`
    /// do not — a class of extensions has no single suffix to append.
    pub fn extension(&self) -> Option<String> {
        let pattern = self.patterns.first()?;
        let tail = &pattern[pattern.rfind('*')? + 1..];
        let extension = tail.strip_prefix('.')?;
        // `*.tar.gz` is one suffix and not two: the whole tail after the star is
        // what a save appends. A class or a wildcard in it is not an extension.
        let plain = !extension.is_empty() && !extension.contains(['*', '?', '[', ']']);
        plain.then(|| extension.to_owned())
    }
}

/// What a picker was asked for. Every field is the caller's, so the widget
/// cannot invent a question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub select: Select,
    /// The footer's confirm button: "open", "save", "select".
    pub accept: String,
    /// Where to start. `None` is the process's current directory.
    pub folder: Option<PathBuf>,
    /// The filters a person may choose between; empty means no filter at all.
    pub filters: Vec<Filter>,
    /// Save mode's starting name — the selected file's, or an empty field.
    pub name: String,
}

impl Request {
    /// One file, opened.
    pub fn open() -> Self {
        Self {
            select: Select::File,
            accept: "open".to_owned(),
            folder: None,
            filters: Vec::new(),
            name: String::new(),
        }
    }

    /// Several files, opened.
    pub fn open_many() -> Self {
        Self {
            select: Select::Files,
            ..Self::open()
        }
    }

    /// A folder.
    pub fn folder() -> Self {
        Self {
            select: Select::Directory,
            accept: "select".to_owned(),
            ..Self::open()
        }
    }

    /// A name, written into the folder being shown.
    pub fn save(name: impl Into<String>) -> Self {
        Self {
            select: Select::Save,
            accept: "save".to_owned(),
            name: name.into(),
            ..Self::open()
        }
    }

    pub fn at(mut self, folder: impl Into<PathBuf>) -> Self {
        self.folder = Some(folder.into());
        self
    }

    pub fn with(mut self, filters: impl IntoIterator<Item = Filter>) -> Self {
        self.filters = filters.into_iter().collect();
        self
    }

    pub fn accept_label(mut self, label: impl Into<String>) -> Self {
        self.accept = label.into();
        self
    }
}

/// The answer, or the reason there is not one yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The paths to hand back, in listing order.
    Chosen(Vec<PathBuf>),
    /// The name exists: `Replace` says which, and the next accept is the
    /// confirmation. It is an outcome rather than a silent overwrite because the
    /// memo asks for a question, and because a picker that deletes a file on the
    /// way to answering a form is not one anybody would trust twice.
    Replace(PathBuf),
    /// Something only the person can fix, said in the footer.
    Refused(String),
    /// A folder pressed in a mode that wants a file.
    ///
    /// The press is the browser's own gesture — open the folder — and not an
    /// answer, which is what every desktop's Open button does with a folder
    /// selected. The picker stays open, one level down.
    Navigate(PathBuf),
}

/// A picker in progress: the request, and what has been typed or chosen since.
///
/// The mutable parts are cells because the pane is drawn from a `&Browser` — the
/// same bargain the layout, the breadcrumbs and the icon set make. The request
/// itself never changes: a picker that rewrote its own question would be one the
/// caller cannot predict.
#[derive(Debug)]
pub struct Pick {
    pub request: Request,
    /// The name in the footer's field, in save mode.
    draft: RefCell<String>,
    /// Which filter is chosen, as an index into `request.filters`.
    filter: Cell<usize>,
    /// The last refusal, shown in the footer until the next answer.
    problem: RefCell<Option<String>>,
    /// The name waiting for a confirmation.
    asking: RefCell<Option<PathBuf>>,
    /// Whether the footer has still to ask for the keyboard.
    ///
    /// A save picker exists to be typed into, and a field that needs a click
    /// first is a picker whose first keystroke goes somewhere else. The request
    /// is made once and then taken, so a later frame does not steal focus back
    /// from a listing somebody is scrolling.
    focus: Cell<bool>,
}

impl Pick {
    pub fn new(request: Request) -> Self {
        let draft = request.name.clone();
        Self {
            request,
            draft: RefCell::new(draft),
            filter: Cell::new(0),
            problem: RefCell::new(None),
            asking: RefCell::new(None),
            focus: Cell::new(true),
        }
    }

    pub fn select(&self) -> Select {
        self.request.select
    }

    /// The confirm button's word: the caller's, unless a name is waiting to be
    /// replaced.
    pub fn accept_label(&self) -> &str {
        match self.asking.borrow().is_some() {
            true => "replace",
            false => &self.request.accept,
        }
    }

    pub fn filters(&self) -> &[Filter] {
        &self.request.filters
    }

    pub fn filter(&self) -> usize {
        self.filter
            .get()
            .min(self.request.filters.len().saturating_sub(1))
    }

    /// Choose a filter. The draft is kept: a filter narrows the answer, it does
    /// not retype what somebody wrote.
    pub fn set_filter(&self, index: usize) {
        if index < self.request.filters.len() {
            self.filter.set(index);
        }
    }

    /// The chosen filter, if the request has any.
    pub fn chosen_filter(&self) -> Option<&Filter> {
        self.request.filters.get(self.filter())
    }

    /// The name being typed, for a field that writes it back in place.
    pub fn draft_mut(&self) -> std::cell::RefMut<'_, String> {
        self.draft.borrow_mut()
    }

    pub fn set_draft(&self, text: String) {
        *self.draft.borrow_mut() = text;
    }

    /// Is there still a request for the keyboard? Answered once per picker.
    pub fn take_focus(&self) -> bool {
        let wanted = self.focus.get();
        self.focus.set(false);
        wanted
    }

    /// The refusal to say, if the last answer was refused.
    pub fn problem(&self) -> Option<String> {
        self.problem.borrow().clone()
    }

    /// The name being confirmed, if a save is asking.
    pub fn asking(&self) -> Option<PathBuf> {
        self.asking.borrow().clone()
    }

    /// Give up on the question, not on the picker: the footer's cancel inside a
    /// `replace` confirmation returns to the form.
    pub fn clear_asking(&self) {
        *self.asking.borrow_mut() = None;
    }

    /// The accept button, with the listing's answer folded in.
    ///
    /// `cursor` is the file under the selection's focus (`None` for the `..`
    /// row); `picked` is every selected file in listing order. Both are the
    /// pane's business, and both are arguments rather than reads so that this is
    /// testable without a browser.
    pub fn accept(&self, folder: &Path, cursor: Option<PathBuf>, picked: &[PathBuf]) -> Outcome {
        // A name waiting to be replaced makes this press the confirmation. It is
        // the same button: the question and the answer are one gesture, and a
        // second dialog for "are you sure" is a third window.
        if let Some(target) = self.asking.borrow_mut().take() {
            *self.problem.borrow_mut() = None;
            return Outcome::Chosen(vec![target]);
        }
        let outcome = self.decide(folder, cursor, picked);
        match &outcome {
            Outcome::Refused(why) => *self.problem.borrow_mut() = Some(why.clone()),
            Outcome::Replace(target) => {
                *self.problem.borrow_mut() = None;
                *self.asking.borrow_mut() = Some(target.clone());
            }
            Outcome::Chosen(_) | Outcome::Navigate(_) => *self.problem.borrow_mut() = None,
        }
        outcome
    }

    fn decide(&self, folder: &Path, cursor: Option<PathBuf>, picked: &[PathBuf]) -> Outcome {
        match self.request.select {
            // A folder chooser answers with the folder being looked at: that is
            // how the portal's `directory` option reads, and how GTK's own
            // chooser behaves once you stop navigating.
            Select::Directory => Outcome::Chosen(vec![folder.to_path_buf()]),
            Select::File => match cursor {
                Some(path) if is_dir(&path) => Outcome::Navigate(path),
                Some(path) if self.passes(&path) => Outcome::Chosen(vec![path]),
                Some(_) => Outcome::Refused(self.filter_refusal()),
                None => Outcome::Refused("choose a file".to_owned()),
            },
            Select::Files => {
                if picked.is_empty() {
                    return Outcome::Refused("choose at least one file".to_owned());
                }
                // One folder is a way in; a mixture is not an answer a file
                // picker can give.
                if picked.len() == 1 && is_dir(&picked[0]) {
                    return Outcome::Navigate(picked[0].clone());
                }
                if picked.iter().any(|path| is_dir(path)) {
                    return Outcome::Refused("a folder is not a file".to_owned());
                }
                if picked.iter().any(|path| !self.passes(path)) {
                    return Outcome::Refused(self.filter_refusal());
                }
                Outcome::Chosen(picked.to_vec())
            }
            Select::Save => self.save(folder),
        }
    }

    /// A save name: joined onto the folder, given the filter's extension, and
    /// asked about when it already exists.
    fn save(&self, folder: &Path) -> Outcome {
        let typed = self.draft.borrow().trim().to_owned();
        if typed.is_empty() {
            return Outcome::Refused("type a name".to_owned());
        }
        // A name, not a path: `..` and a slash are the file manager's, and a
        // picker that resolves them is a picker that can be talked into writing
        // somewhere the person is not looking.
        if typed == "." || typed == ".." || typed.contains(['/', '\\']) {
            return Outcome::Refused("a name, not a path".to_owned());
        }
        let name = match self.chosen_filter() {
            Some(filter) if !filter.matches(&typed) => match filter.extension() {
                // No extension at all: the filter supplies one.
                Some(extension) if Path::new(&typed).extension().is_none() => {
                    format!("{typed}.{extension}")
                }
                _ => return Outcome::Refused(self.filter_refusal()),
            },
            _ => typed,
        };
        if !writable(folder) {
            return Outcome::Refused("this folder cannot be written to".to_owned());
        }
        let target = folder.join(&name);
        match exists(&target) {
            true if is_dir(&target) => Outcome::Refused("a folder has that name".to_owned()),
            true => Outcome::Replace(target),
            false => Outcome::Chosen(vec![target]),
        }
    }

    /// Whether a path is one the chosen filter would show.
    fn passes(&self, path: &Path) -> bool {
        let Some(filter) = self.chosen_filter() else {
            return true;
        };
        match path.file_name().and_then(|name| name.to_str()) {
            Some(name) => filter.matches(name),
            // A name that is not text cannot be matched, so it cannot be shown:
            // the same answer the listing would give.
            None => false,
        }
    }

    fn filter_refusal(&self) -> String {
        match self.chosen_filter() {
            Some(filter) => format!("only {} can be picked", filter.name),
            None => "that cannot be picked".to_owned(),
        }
    }
}

/// Does anything live at `path`?
///
/// `symlink_metadata` rather than `exists`: a dangling symlink is a name that is
/// taken, and a save that overwrote it would be a surprise.
pub fn exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

pub fn is_dir(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.is_dir())
        .unwrap_or(false)
}

/// May anything be written into `dir`?
///
/// The question a save has to ask before it offers a name. On Unix it is
/// `access(W_OK)` — the check the kernel itself would make — because the
/// permission bit says `0644` about a file that an ACL still forbids, and
/// `/usr/bin` is not marked read-only either. Windows has no `access`, so there
/// the answer is the read-only attribute, which is weaker; the write itself
/// still decides, and the picker says so in `docs/manager.md` §4.
pub fn writable(dir: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let Ok(path) = CString::new(dir.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: `path` is a NUL-terminated C string that outlives the call, and
        // `access` reads nothing else.
        unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
    }
    #[cfg(not(unix))]
    {
        std::fs::metadata(dir)
            .map(|meta| !meta.permissions().readonly())
            .unwrap_or(false)
    }
}

/// `fnmatch` for the portal's patterns: `*`, `?` and `[…]`, whole-name and
/// case-insensitive.
///
/// Hand-written rather than a dependency because the patterns arrive from a
/// D-Bus dictionary and the grammar is nine lines; a regex engine for `*.png`
/// would be a dependency, a compile per filter and a denial-of-service surface
/// (`docs/manager.md` §5 has the same worry about the portal's options).
pub fn glob(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().flat_map(char::to_lowercase).collect();
    let name: Vec<char> = name.chars().flat_map(char::to_lowercase).collect();
    matches_from(&pattern, &name)
}

fn matches_from(pattern: &[char], name: &[char]) -> bool {
    let Some((&head, rest)) = pattern.split_first() else {
        return name.is_empty();
    };
    match head {
        '*' => {
            // The epsilon case, then the case that eats one character: the
            // classic two-branch star, and with the epsilon first it does not
            // blow up on `*a*a*a`.
            matches_from(rest, name) || (!name.is_empty() && matches_from(pattern, &name[1..]))
        }
        '?' => !name.is_empty() && matches_from(rest, &name[1..]),
        '[' => {
            let Some((matched, after)) = class(rest, name.first().copied()) else {
                // An unterminated class is a literal `[`: a pattern is a pattern,
                // and a filter that refused everything because of a typo would be
                // a filter nobody could use.
                return name.first() == Some(&'[') && matches_from(rest, &name[1..]);
            };
            matched && matches_from(after, &name[1..])
        }
        literal => name.first() == Some(&literal) && matches_from(rest, &name[1..]),
    }
}

/// One `[…]` class: the pattern after the `[`, and the character to test.
///
/// Returns whether it matched and the pattern after the closing `]`. `None` for
/// an unterminated class.
fn class<'a>(pattern: &'a [char], at: Option<char>) -> Option<(bool, &'a [char])> {
    let mut matched = false;
    let mut index = 0;
    let mut negate = false;
    if pattern.first() == Some(&'!') || pattern.first() == Some(&'^') {
        negate = true;
        index = 1;
    }
    let mut any = false;
    while index < pattern.len() {
        let low = pattern[index];
        if low == ']' && any {
            return Some((matched != negate, &pattern[index + 1..]));
        }
        any = true;
        // A range, `a-z`, but a trailing `-` (or one before `]`) is a literal.
        if pattern.get(index + 1) == Some(&'-')
            && pattern.get(index + 2).is_some_and(|high| *high != ']')
        {
            let high = pattern[index + 2];
            if let Some(at) = at {
                matched |= at >= low && at <= high;
            }
            index += 3;
        } else {
            if Some(low) == at {
                matched = true;
            }
            index += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str) -> PathBuf {
        PathBuf::from(name)
    }

    #[test]
    fn a_star_matches_anything_and_a_question_mark_one_character() {
        assert!(glob("*.png", "photo.png"));
        assert!(!glob("*.png", "photo.jpg"));
        assert!(glob("*.png", ".png"));
        assert!(glob("a?c.txt", "abc.txt"));
        assert!(!glob("a?c.txt", "ac.txt"));
        assert!(glob("*", "anything at all"));
        assert!(glob("*", ".hidden"));
        // The whole name: a pattern is not a substring search.
        assert!(!glob("png", "photo.png"));
    }

    #[test]
    fn the_class_in_the_portals_own_example_matches_each_letter() {
        assert!(glob("*.[ch]", "main.c"));
        assert!(glob("*.[ch]", "main.h"));
        assert!(!glob("*.[ch]", "main.o"));
        assert!(glob("*.[ch]", "MAIN.C"), "case-insensitive, like a desktop");
        // A range, and the negation the shell has.
        assert!(glob("file[0-9].txt", "file7.txt"));
        assert!(!glob("file[0-9].txt", "filex.txt"));
        assert!(glob("file[!0-9].txt", "filex.txt"));
    }

    #[test]
    fn an_unterminated_class_is_the_literal_character() {
        assert!(glob("a[b", "a[b"));
        assert!(!glob("a[b", "ab"));
    }

    /// A filter's extension comes from its pattern, and a class has none: there
    /// is no single suffix to append for `*.[ch]`.
    #[test]
    fn an_extension_is_only_there_when_the_pattern_names_one() {
        assert_eq!(
            Filter::of("images", "*.png").extension().as_deref(),
            Some("png")
        );
        assert_eq!(
            Filter::of("archives", "*.tar.gz").extension().as_deref(),
            Some("tar.gz")
        );
        assert_eq!(Filter::of("all", "*").extension(), None);
        assert_eq!(Filter::of("source", "*.[ch]").extension(), None);
        assert_eq!(
            Filter::of("both", "*.jpg").extension().as_deref(),
            Some("jpg"),
            "the first pattern names the suffix"
        );
    }

    fn scratch() -> tempfile::TempDir {
        tempfile::tempdir().expect("a temporary directory")
    }

    #[test]
    fn a_filter_refuses_what_it_would_not_show() {
        let dir = scratch();
        let pick = Pick::new(Request::open().with([Filter::of("PNG images", "*.png")]));
        let png = dir.path().join("photo.png");
        let text = dir.path().join("notes.txt");
        assert_eq!(
            pick.accept(dir.path(), Some(png.clone()), &[png]),
            Outcome::Chosen(vec![dir.path().join("photo.png")])
        );
        assert_eq!(
            pick.accept(dir.path(), Some(text.clone()), &[]),
            Outcome::Refused("only PNG images can be picked".to_owned())
        );
        // And the refusal is what the footer shows until the next answer.
        assert_eq!(
            pick.problem().as_deref(),
            Some("only PNG images can be picked")
        );
    }

    #[test]
    fn one_file_mode_refuses_an_empty_listing_and_the_parent_row() {
        let dir = scratch();
        let pick = Pick::new(Request::open());
        assert_eq!(
            pick.accept(dir.path(), None, &[]),
            Outcome::Refused("choose a file".to_owned())
        );
    }

    #[test]
    fn several_files_are_the_whole_selection_or_nothing() {
        let dir = scratch();
        let pick = Pick::new(Request::open_many());
        let one = dir.path().join("a.txt");
        let two = dir.path().join("b.txt");
        assert_eq!(
            pick.accept(dir.path(), Some(one.clone()), &[one.clone(), two.clone()]),
            Outcome::Chosen(vec![one, two])
        );
        assert_eq!(
            pick.accept(dir.path(), None, &[]),
            Outcome::Refused("choose at least one file".to_owned())
        );
    }

    /// A folder pressed in a file picker is a way in, not an answer — the
    /// browser's own gesture, with the question still open.
    #[test]
    fn a_folder_in_a_file_picker_is_a_way_in() {
        let dir = scratch();
        let inside = dir.path().join("sub");
        std::fs::create_dir(&inside).expect("mkdir");
        let pick = Pick::new(Request::open());
        assert_eq!(
            pick.accept(dir.path(), Some(inside.clone()), &[]),
            Outcome::Navigate(inside.clone())
        );
        // One folder chosen by a multiple picker is the same way in; a mixture
        // is not an answer a file picker can give.
        let many = Pick::new(Request::open_many());
        assert_eq!(
            many.accept(dir.path(), Some(inside.clone()), &[inside.clone()]),
            Outcome::Navigate(inside.clone())
        );
        let file = dir.path().join("a.txt");
        assert_eq!(
            many.accept(dir.path(), Some(inside.clone()), &[file, inside]),
            Outcome::Refused("a folder is not a file".to_owned())
        );
    }

    /// A folder chooser answers with the folder it is showing, whatever is
    /// selected inside it.
    #[test]
    fn a_folder_is_the_one_on_screen() {
        let dir = scratch();
        let pick = Pick::new(Request::folder());
        assert_eq!(
            pick.accept(dir.path(), None, &[]),
            Outcome::Chosen(vec![dir.path().to_path_buf()])
        );
    }

    #[test]
    fn a_save_needs_a_name_that_is_a_name() {
        let dir = scratch();
        let pick = Pick::new(Request::save(""));
        assert_eq!(
            pick.accept(dir.path(), None, &[]),
            Outcome::Refused("type a name".to_owned())
        );
        pick.set_draft("  ".to_owned());
        assert!(matches!(
            pick.accept(dir.path(), None, &[]),
            Outcome::Refused(_)
        ));
        for hostile in ["../escape", "sub/file", "..", "."] {
            pick.set_draft(hostile.to_owned());
            assert_eq!(
                pick.accept(dir.path(), None, &[]),
                Outcome::Refused("a name, not a path".to_owned()),
                "{hostile}"
            );
        }
    }

    /// The filter supplies the suffix when the name has none, and refuses a name
    /// of a kind the filter would not show — the two halves of "the chosen filter
    /// decides the extension".
    #[test]
    fn a_save_takes_the_filters_extension_but_not_its_name() {
        let dir = scratch();
        let pick = Pick::new(Request::save("").with([Filter::of("PNG images", "*.png")]));
        pick.set_draft("diagram".to_owned());
        assert_eq!(
            pick.accept(dir.path(), None, &[]),
            Outcome::Chosen(vec![dir.path().join("diagram.png")])
        );
        pick.set_draft("diagram.txt".to_owned());
        assert_eq!(
            pick.accept(dir.path(), None, &[]),
            Outcome::Refused("only PNG images can be picked".to_owned())
        );
        // A name that already matches is kept as it is, extension and all.
        pick.set_draft("diagram.PNG".to_owned());
        assert_eq!(
            pick.accept(dir.path(), None, &[]),
            Outcome::Chosen(vec![dir.path().join("diagram.PNG")])
        );
    }

    /// An existing name is a question, and the same button answers it: the first
    /// press asks, the second returns the path.
    #[test]
    fn an_existing_name_is_asked_about_before_it_is_answered() {
        let dir = scratch();
        let target = dir.path().join("taken.txt");
        std::fs::write(&target, b"here first").expect("write");
        let pick = Pick::new(Request::save("taken.txt"));
        assert_eq!(
            pick.accept(dir.path(), None, &[]),
            Outcome::Replace(target.clone())
        );
        assert_eq!(pick.asking(), Some(target.clone()));
        assert_eq!(pick.accept_label(), "replace");
        assert_eq!(
            pick.accept(dir.path(), None, &[]),
            Outcome::Chosen(vec![target.clone()]),
            "the second press is the confirmation"
        );
        assert_eq!(pick.asking(), None);
        assert_eq!(pick.accept_label(), "save");
    }

    #[test]
    fn a_folder_with_the_name_is_not_a_file_to_replace() {
        let dir = scratch();
        std::fs::create_dir(dir.path().join("sub")).expect("mkdir");
        let pick = Pick::new(Request::save("sub"));
        assert_eq!(
            pick.accept(dir.path(), None, &[]),
            Outcome::Refused("a folder has that name".to_owned())
        );
    }

    /// A folder nobody may write to is said out loud rather than offered.
    #[test]
    #[cfg(unix)]
    fn a_folder_that_cannot_be_written_to_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch();
        let closed = dir.path().join("closed");
        std::fs::create_dir(&closed).expect("mkdir");
        std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o500)).expect("chmod");
        // Root ignores the mode bits, and then the check cannot be tested: skip
        // rather than assert something the kernel does not do.
        if writable(&closed) {
            return;
        }
        let pick = Pick::new(Request::save("new.txt"));
        assert_eq!(
            pick.accept(&closed, None, &[]),
            Outcome::Refused("this folder cannot be written to".to_owned())
        );
    }

    #[test]
    fn a_name_that_is_taken_by_a_dangling_link_is_still_taken() {
        let dir = scratch();
        let link = dir.path().join("gone");
        std::os::unix::fs::symlink(dir.path().join("nothing"), &link).expect("symlink");
        assert!(exists(&link));
        let pick = Pick::new(Request::save("gone"));
        assert_eq!(pick.accept(dir.path(), None, &[]), Outcome::Replace(link));
    }

    #[test]
    fn a_save_without_a_filter_takes_the_name_as_written() {
        let dir = scratch();
        let pick = Pick::new(Request::save("plain"));
        assert_eq!(
            pick.accept(dir.path(), None, &[]),
            Outcome::Chosen(vec![dir.path().join("plain")])
        );
    }
}
