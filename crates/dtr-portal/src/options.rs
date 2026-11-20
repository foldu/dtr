//! The `a{sv}` a frontend sends, decoded onto the picker's own question.
//!
//! The portal passes a vardict of options; `dtr_files::pick::Request` is what the
//! browser understands. This module is the one translation, and the reason it is
//! a module rather than a dozen `if let`s in the interface method is that most of
//! it is decisions:
//!
//! - **`directory` wins over `multiple`** for an open: a folder picker answers
//!   with the folder on screen, and "several folders" is not a shape the picker
//!   has. (GTK's backend does select several folders; ours says so in the docs
//!   rather than inventing a second selection model.)
//! - **`current_folder` is bytes, not a string** (`ay`, NUL-terminated). A
//!   suggested folder is a path, and a path is not promised to be UTF-8, so it is
//!   decoded with the same care the answer's `uris` are encoded with.
//! - **A filter is a promise about the answer**, and the picker's `Filter` can
//!   only promise things it can match against a *name*. The portal's
//!   `a(sa(us))` rows may be either a glob (`0`) or a MIME type (`1`); MIME types
//!   cannot be matched by a name (`docs/manager.md` §4, "cannot tell"). So a
//!   filter keeps its glob patterns, and one that has only MIME types is
//!   **dropped** rather than handed to the picker as a filter that refuses every
//!   file. The raw rows are kept, because `current_filter` must be echoed back
//!   exactly as the application sent it.
//! - **`current_filter` selects by name**, which is GTK's heuristic and the
//!   portal's own advice (the application's filter need not be pointer-identical
//!   to one in the list). If the list is empty, the portal allows it to be
//!   applied unconditionally, which is what happens.
//! - **Every recognised key is typed strictly**: a frontend that sends
//!   `multiple` as a string has a bug, and answering `InvalidArgs` says so
//!   instead of silently opening the wrong kind of dialog. Unrecognised keys are
//!   ignored, because the interface may grow.
//!
//! The answer needs one thing this type keeps and `Request` does not: the raw
//! filter rows, and the mapping from a request-filter index back to a portal row.

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

use dtr_files::pick::{Filter, Request};
use zbus::zvariant::OwnedValue;

/// The portal's pattern kinds, as `a(us)` encodes them.
const GLOB: u32 = 0;
#[allow(dead_code)]
const MIME: u32 = 1;

/// The interface method a request arrived on.
///
/// The three differ in what they ask for, not in how they are answered, and the
/// `Kind` is what carries that difference into the option decode and the result
/// encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    OpenFile,
    SaveFile,
    SaveFiles,
}

impl Kind {
    /// The D-Bus method name, for logs and errors.
    pub fn method(self) -> &'static str {
        match self {
            Kind::OpenFile => "OpenFile",
            Kind::SaveFile => "SaveFile",
            Kind::SaveFiles => "SaveFiles",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.method())
    }
}

/// One row of the portal's `filters`: a name and its `(kind, pattern)` list.
///
/// Kept in the portal's own shape because `current_filter` is returned to the
/// application, and an answer that dropped the MIME rows would not be the filter
/// that was offered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortalFilter {
    /// The user-visible name ("Images", "Text files").
    pub name: String,
    /// `(0, "*.png")` is a glob, `(1, "image/png")` a MIME type.
    pub patterns: Vec<(u32, String)>,
}

impl PortalFilter {
    /// A filter with only globs, which is how the tests and a `current_filter`
    /// with no list are built.
    pub fn globs(name: impl Into<String>, patterns: impl IntoIterator<Item = String>) -> Self {
        Self {
            name: name.into(),
            patterns: patterns.into_iter().map(|p| (GLOB, p)).collect(),
        }
    }

    /// The patterns the browser can actually test a name against.
    fn glob_patterns(&self) -> Vec<String> {
        self.patterns
            .iter()
            .filter(|(kind, _)| *kind == GLOB)
            .map(|(_, pattern)| pattern.clone())
            .collect()
    }
}

/// What the options said, before they are a [`Request`].
///
/// The fields are the ones the interface documents for these methods; a key
/// that is not here is either not applicable (a `current_name` on `OpenFile`) or
/// deliberately unsupported (`choices`, which the picker has no widget for — the
/// interface says the portal may ignore it, and a combo box invented for one
/// application would be a control `dtr-widgets` has never seen).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// `multiple` (`b`, default false).
    pub multiple: bool,
    /// `directory` (`b`, default false).
    pub directory: bool,
    /// `modal` (`b`, default true).
    pub modal: bool,
    /// `accept_label` (`s`), the confirm button's text.
    pub accept_label: Option<String>,
    /// `filters` (`a(sa(us))`), as sent.
    pub filters: Vec<PortalFilter>,
    /// `current_filter` (`(sa(us))`), as sent.
    pub current_filter: Option<PortalFilter>,
    /// `current_folder` (`ay`, NUL-terminated bytes).
    pub current_folder: Option<PathBuf>,
    /// `current_name` (`s`).
    pub current_name: Option<String>,
    /// `current_file` (`ay`, NUL-terminated bytes).
    pub current_file: Option<PathBuf>,
    /// `files` (`aay`), the names `SaveFiles` appends to the chosen folder.
    pub files: Vec<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            multiple: false,
            directory: false,
            // The interface's default for `modal` is yes.
            modal: true,
            accept_label: None,
            filters: Vec::new(),
            current_filter: None,
            current_folder: None,
            current_name: None,
            current_file: None,
            files: Vec::new(),
        }
    }
}

/// An option that is present but not of the type the interface promises.
///
/// A [`DecodeError`] is answered as `InvalidArgs`: the request cannot be shown
/// as asked, and showing something else would be a dialog about a different
/// question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeError {
    key: &'static str,
    detail: String,
}

impl DecodeError {
    fn new(key: &'static str, detail: impl Into<String>) -> Self {
        Self {
            key,
            detail: detail.into(),
        }
    }

    /// Which option was wrong. Logged; not part of the `InvalidArgs` message
    /// alone, which carries the whole sentence.
    pub fn key(&self) -> &str {
        self.key
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the `{}` option is not what the FileChooser interface says: {}",
            self.key, self.detail
        )
    }
}

impl std::error::Error for DecodeError {}

/// Options, the question they make, and the filter bookkeeping the answer needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoded {
    /// The options exactly as they arrived (minus the unrecognised keys).
    pub options: Options,
    /// The question, as the browser's picker takes it.
    pub request: Request,
    /// The `current_filter` option as an index into `request.filters`.
    pub filter: Option<usize>,
    /// `map[n]` is the portal filter that `request.filters[n]` came from.
    map: Vec<usize>,
}

impl Decoded {
    /// The portal filter to echo as `current_filter` for a chosen request-filter
    /// index, or `None` if the picker is on no filter (or reports an index that
    /// is not one of its own).
    pub fn selected_filter(&self, chosen: Option<usize>) -> Option<&PortalFilter> {
        let raw = *self.map.get(chosen?)?;
        self.options.filters.get(raw)
    }

    /// The raw filter rows, for a caller that wants to see what was offered.
    pub fn portal_filters(&self) -> &[PortalFilter] {
        &self.options.filters
    }
}

/// Decode a method's `options` vardict for `kind`.
pub fn decode(kind: Kind, values: HashMap<String, OwnedValue>) -> Result<Decoded, DecodeError> {
    Ok(Options::decode(values)?.into_request(kind))
}

impl Options {
    /// Decode the vardict. Unrecognised keys are ignored.
    pub fn decode(mut values: HashMap<String, OwnedValue>) -> Result<Self, DecodeError> {
        let multiple = bool_option(&mut values, "multiple")?.unwrap_or(false);
        let directory = bool_option(&mut values, "directory")?.unwrap_or(false);
        let modal = bool_option(&mut values, "modal")?.unwrap_or(true);
        let accept_label = string_option(&mut values, "accept_label")?;
        let filters = filters_option(&mut values, "filters")?.unwrap_or_default();
        let current_filter = filter_option(&mut values, "current_filter")?;
        let current_folder = bytes_option(&mut values, "current_folder")?.map(path_from_bytes);
        let current_name = string_option(&mut values, "current_name")?;
        let current_file = bytes_option(&mut values, "current_file")?.map(path_from_bytes);
        let files = files_option(&mut values, "files")?.unwrap_or_default();
        Ok(Self {
            multiple,
            directory,
            modal,
            accept_label,
            filters,
            current_filter,
            current_folder,
            current_name,
            current_file,
            files,
        })
    }

    /// The question these options ask, plus the filter index it starts on.
    pub fn into_request(mut self, kind: Kind) -> Decoded {
        let name = match kind {
            Kind::SaveFile => self.save_name(),
            _ => String::new(),
        };
        let mut request = match kind {
            Kind::OpenFile if self.directory => Request::folder(),
            Kind::OpenFile if self.multiple => Request::open_many(),
            Kind::OpenFile => Request::open(),
            Kind::SaveFile => Request::save(name),
            // `SaveFiles` asks for a folder; the names are appended to it after
            // the person answers, not before.
            Kind::SaveFiles => Request::folder(),
        };
        // `current_file` without a `current_folder` still says where to start:
        // the folder the file is in.
        request.folder = self.current_folder.clone().or_else(|| {
            self.current_file
                .as_ref()
                .and_then(|file| file.parent())
                .map(PathBuf::from)
        });
        request.accept = self
            .accept_label
            .clone()
            .unwrap_or_else(|| default_accept(kind, self.multiple).to_owned());

        // Only the filters the picker can promise are handed to it; `map` keeps
        // the way back to the row the application sent.
        let mut filters = Vec::new();
        let mut map = Vec::new();
        for (raw, portal) in self.filters.iter().enumerate() {
            let patterns = portal.glob_patterns();
            if patterns.is_empty() {
                tracing::debug!(
                    filter = %portal.name,
                    "dropping a MIME-only filter: the picker promises by name"
                );
                continue;
            }
            filters.push(Filter::new(portal.name.clone(), patterns));
            map.push(raw);
        }

        let mut filter = None;
        if let Some(current) = self.current_filter.take() {
            if self.filters.is_empty() {
                // The portal allows a `current_filter` with no list: it applies
                // unconditionally, and that is exactly a picker with one filter.
                let patterns = current.glob_patterns();
                if !patterns.is_empty() {
                    filters.push(Filter::new(current.name.clone(), patterns));
                    self.filters.push(current);
                    map.push(self.filters.len() - 1);
                    filter = Some(0);
                } else {
                    tracing::debug!(
                        filter = %current.name,
                        "an unconditional MIME-only filter cannot be promised by name"
                    );
                }
            } else if let Some(raw) = self
                .filters
                .iter()
                .position(|candidate| candidate.name == current.name)
            {
                filter = map.iter().position(|mapped| *mapped == raw);
                if filter.is_none() {
                    tracing::debug!(
                        filter = %current.name,
                        "the requested default filter has no name patterns and is not offered"
                    );
                }
            } else {
                tracing::debug!(
                    filter = %current.name,
                    "the requested default filter is not in the list; starting on the first"
                );
            }
        }

        request.filters = filters;
        Decoded {
            options: self,
            request,
            filter,
            map,
        }
    }

    /// A save's starting name: the application's name, else the existing file's.
    fn save_name(&self) -> String {
        if let Some(name) = &self.current_name {
            return name.clone();
        }
        self.current_file
            .as_ref()
            .and_then(|file| file.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// The button word when the application did not supply `accept_label`.
///
/// These are GTK's defaults, because they are the words a desktop's own dialog
/// uses: `Select` for one file, `Open` for several, `Save` for a save.
fn default_accept(kind: Kind, multiple: bool) -> &'static str {
    match kind {
        Kind::OpenFile if multiple => "open",
        Kind::OpenFile => "select",
        Kind::SaveFile | Kind::SaveFiles => "save",
    }
}

/// The bytes of a NUL-terminated `ay` as a path.
///
/// The NUL is a terminator, not part of the name: everything after it is
/// padding. On Unix the bytes become an `OsString` unchanged, which is the whole
/// point — a suggested folder is a path like any other.
fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    let mut bytes = bytes;
    if let Some(nul) = bytes.iter().position(|byte| *byte == 0) {
        bytes.truncate(nul);
    }
    #[cfg(unix)]
    {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(OsString::from_vec(bytes))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
    }
}

fn take(values: &mut HashMap<String, OwnedValue>, key: &'static str) -> Option<OwnedValue> {
    values.remove(key)
}

fn bool_option(
    values: &mut HashMap<String, OwnedValue>,
    key: &'static str,
) -> Result<Option<bool>, DecodeError> {
    take(values, key)
        .map(|value| {
            bool::try_from(value)
                .map_err(|err| DecodeError::new(key, format!("expected `b`: {err}")))
        })
        .transpose()
}

fn string_option(
    values: &mut HashMap<String, OwnedValue>,
    key: &'static str,
) -> Result<Option<String>, DecodeError> {
    take(values, key)
        .map(|value| {
            String::try_from(value)
                .map_err(|err| DecodeError::new(key, format!("expected `s`: {err}")))
        })
        .transpose()
}

fn bytes_option(
    values: &mut HashMap<String, OwnedValue>,
    key: &'static str,
) -> Result<Option<Vec<u8>>, DecodeError> {
    take(values, key)
        .map(|value| {
            Vec::<u8>::try_from(value)
                .map_err(|err| DecodeError::new(key, format!("expected `ay`: {err}")))
        })
        .transpose()
}

/// `a(sa(us))` as the interface defines it.
fn filters_option(
    values: &mut HashMap<String, OwnedValue>,
    key: &'static str,
) -> Result<Option<Vec<PortalFilter>>, DecodeError> {
    take(values, key)
        .map(|value| {
            let rows = Vec::<(String, Vec<(u32, String)>)>::try_from(value)
                .map_err(|err| DecodeError::new(key, format!("expected `a(sa(us))`: {err}")))?;
            Ok(rows
                .into_iter()
                .map(|(name, patterns)| PortalFilter { name, patterns })
                .collect())
        })
        .transpose()
}

/// `(sa(us))`, the shape `current_filter` shares with one row of `filters`.
fn filter_option(
    values: &mut HashMap<String, OwnedValue>,
    key: &'static str,
) -> Result<Option<PortalFilter>, DecodeError> {
    take(values, key)
        .map(|value| {
            let (name, patterns) = <(String, Vec<(u32, String)>)>::try_from(value)
                .map_err(|err| DecodeError::new(key, format!("expected `(sa(us))`: {err}")))?;
            Ok(PortalFilter { name, patterns })
        })
        .transpose()
}

/// `aay`, each a NUL-terminated file name.
fn files_option(
    values: &mut HashMap<String, OwnedValue>,
    key: &'static str,
) -> Result<Option<Vec<PathBuf>>, DecodeError> {
    take(values, key)
        .map(|value| {
            let rows = Vec::<Vec<u8>>::try_from(value)
                .map_err(|err| DecodeError::new(key, format!("expected `aay`: {err}")))?;
            Ok(rows.into_iter().map(path_from_bytes).collect())
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dtr_files::pick::Select;
    use zbus::zvariant::Value;

    /// A value the way a sender would put it in the vardict.
    ///
    /// One helper per shape rather than one generic helper: `Value::from` is
    /// implemented both per concrete type and through `Into<Structure>`, and a
    /// generic `T: Into<Value>` leaves the compiler unable to choose between
    /// them.
    fn owned(value: Value<'static>) -> OwnedValue {
        OwnedValue::try_from(value).expect("an owned value")
    }

    fn a_bool(value: bool) -> OwnedValue {
        owned(Value::from(value))
    }

    fn a_string(value: &str) -> OwnedValue {
        owned(Value::from(value.to_owned()))
    }

    fn bytes(value: Vec<u8>) -> OwnedValue {
        owned(Value::from(value))
    }

    fn filters(value: Vec<(String, Vec<(u32, String)>)>) -> OwnedValue {
        owned(Value::from(value))
    }

    fn filter(value: (String, Vec<(u32, String)>)) -> OwnedValue {
        owned(Value::from(value))
    }

    fn file_names(value: Vec<Vec<u8>>) -> OwnedValue {
        owned(Value::from(value))
    }

    fn pair(name: &str, patterns: &[(u32, &str)]) -> (String, Vec<(u32, String)>) {
        (
            name.to_owned(),
            patterns
                .iter()
                .map(|(kind, pattern)| (*kind, (*pattern).to_owned()))
                .collect(),
        )
    }

    fn decode(values: Vec<(&str, OwnedValue)>, kind: Kind) -> Decoded {
        let values: HashMap<String, OwnedValue> = values
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect();
        super::decode(kind, values).expect("the options decode")
    }

    #[test]
    fn the_defaults_are_a_single_file_open() {
        let decoded = decode(Vec::new(), Kind::OpenFile);
        assert_eq!(decoded.request.select, Select::File);
        assert_eq!(decoded.request.accept, "select");
        assert_eq!(decoded.request.folder, None);
        assert!(decoded.request.filters.is_empty());
        assert_eq!(decoded.filter, None);
        assert!(decoded.options.modal, "modal defaults to yes");
    }

    #[test]
    fn a_multiple_open_asks_for_several_files_and_says_open() {
        let decoded = decode(vec![("multiple", a_bool(true))], Kind::OpenFile);
        assert_eq!(decoded.request.select, Select::Files);
        assert_eq!(decoded.request.accept, "open");
    }

    #[test]
    fn a_directory_open_asks_for_the_folder_on_screen() {
        let decoded = decode(vec![("directory", a_bool(true))], Kind::OpenFile);
        assert_eq!(decoded.request.select, Select::Directory);
        assert_eq!(decoded.request.accept, "select");
    }

    /// The whole realistic OpenFile: options from a GTK-style application, and
    /// the three parts of it that the picker has a field for.
    #[test]
    fn a_real_open_file_maps_onto_the_pickers_request() {
        let decoded = decode(
            vec![
                ("multiple", a_bool(true)),
                (
                    "filters",
                    filters(vec![
                        pair(
                            "Images",
                            &[(GLOB, "*.png"), (GLOB, "*.jpg"), (MIME, "image/png")],
                        ),
                        pair("Text", &[(GLOB, "*.txt")]),
                        pair("Documents", &[(MIME, "application/pdf")]),
                    ]),
                ),
                (
                    "current_filter",
                    filter(pair("Images", &[(GLOB, "*.png"), (MIME, "image/png")])),
                ),
                ("current_folder", bytes(b"/home/barnabas/Pictures".to_vec())),
                ("accept_label", a_string("_Pick")),
                ("modal", a_bool(false)),
            ],
            Kind::OpenFile,
        );
        assert_eq!(decoded.request.select, Select::Files);
        assert_eq!(decoded.request.accept, "_Pick");
        assert_eq!(
            decoded.request.folder,
            Some(PathBuf::from("/home/barnabas/Pictures"))
        );
        // The MIME-only "Documents" filter is dropped; the other two keep only
        // their glob patterns.
        assert_eq!(
            decoded.request.filters,
            vec![
                Filter::new("Images", ["*.png".to_owned(), "*.jpg".to_owned()]),
                Filter::new("Text", ["*.txt".to_owned()]),
            ]
        );
        // `current_filter` selects by name, and the answer echoes the row the
        // application sent — MIME row included.
        assert_eq!(decoded.filter, Some(0));
        let echoed = decoded.selected_filter(Some(0)).expect("the echoed filter");
        assert_eq!(echoed.name, "Images");
        assert_eq!(
            echoed.patterns,
            vec![
                (GLOB, "*.png".to_owned()),
                (GLOB, "*.jpg".to_owned()),
                (MIME, "image/png".to_owned())
            ],
            "a `(sa(us))` row is returned as it arrived"
        );
        // The second filter's index maps through the dropped row.
        let echoed = decoded.selected_filter(Some(1)).expect("the echoed filter");
        assert_eq!(echoed.name, "Text");
        assert_eq!(decoded.selected_filter(None), None);
        assert_eq!(decoded.selected_filter(Some(2)), None);
    }

    /// A `current_filter` and no `filters` is the portal's "apply it
    /// unconditionally", which is a picker that offers one filter.
    #[test]
    fn an_unconditional_current_filter_becomes_the_only_filter() {
        let decoded = decode(
            vec![("current_filter", filter(pair("Images", &[(GLOB, "*.png")])))],
            Kind::OpenFile,
        );
        assert_eq!(
            decoded.request.filters,
            vec![Filter::new("Images", ["*.png".to_owned()])]
        );
        assert_eq!(decoded.filter, Some(0));
    }

    #[test]
    fn a_default_filter_that_is_not_in_the_list_starts_on_the_first() {
        let decoded = decode(
            vec![
                ("filters", filters(vec![pair("Images", &[(GLOB, "*.png")])])),
                ("current_filter", filter(pair("Sounds", &[(GLOB, "*.wav")]))),
            ],
            Kind::OpenFile,
        );
        assert_eq!(decoded.filter, None);
        assert_eq!(decoded.selected_filter(Some(0)).unwrap().name, "Images");
    }

    #[test]
    fn a_save_takes_its_name_and_folder_from_the_options() {
        let decoded = decode(
            vec![
                ("current_name", a_string("report")),
                (
                    "current_folder",
                    bytes(b"/home/barnabas/Documents".to_vec()),
                ),
            ],
            Kind::SaveFile,
        );
        assert_eq!(decoded.request.select, Select::Save);
        assert_eq!(decoded.request.accept, "save");
        assert_eq!(decoded.request.name, "report");
        assert_eq!(
            decoded.request.folder,
            Some(PathBuf::from("/home/barnabas/Documents"))
        );
    }

    /// "Save as" on an existing file: the folder comes from the file when the
    /// frontend sent no folder, and the name from its basename.
    #[test]
    fn a_save_on_an_existing_file_starts_where_that_file_is() {
        let decoded = decode(
            vec![("current_file", bytes(b"/tmp/old name.txt".to_vec()))],
            Kind::SaveFile,
        );
        assert_eq!(decoded.request.name, "old name.txt");
        assert_eq!(decoded.request.folder, Some(PathBuf::from("/tmp")));
    }

    #[test]
    fn a_save_files_request_is_a_folder_and_keeps_the_names() {
        let decoded = decode(
            vec![
                (
                    "current_folder",
                    bytes(b"/home/barnabas/Downloads".to_vec()),
                ),
                (
                    "files",
                    file_names(vec![b"one.txt".to_vec(), b"two.bin".to_vec()]),
                ),
            ],
            Kind::SaveFiles,
        );
        assert_eq!(decoded.request.select, Select::Directory);
        assert_eq!(decoded.request.accept, "save");
        assert_eq!(
            decoded.options.files,
            vec![PathBuf::from("one.txt"), PathBuf::from("two.bin")]
        );
    }

    /// A folder is a path, not a string: the bytes after the NUL are padding and
    /// the ones before it need not be UTF-8.
    #[cfg(unix)]
    #[test]
    fn the_current_folder_is_decoded_from_its_bytes() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let decoded = decode(
            vec![(
                "current_folder",
                bytes(b"/tmp/caf\xc3\xa9\x80\0ignored".to_vec()),
            )],
            Kind::OpenFile,
        );
        assert_eq!(
            decoded.request.folder,
            Some(PathBuf::from(OsStr::from_bytes(b"/tmp/caf\xc3\xa9\x80")))
        );
    }

    #[test]
    fn an_unknown_option_is_ignored_because_the_interface_may_grow() {
        let decoded = decode(
            vec![("some_future_option", a_string("whatever"))],
            Kind::OpenFile,
        );
        // The only difference from `Request::open()` is the accept word: a
        // portal's single-file default label is "Select", not "Open".
        assert_eq!(decoded.request, Request::open().accept_label("select"));
    }

    #[test]
    fn a_wrongly_typed_option_is_refused_with_the_key_in_the_message() {
        let values = vec![("filters", a_bool(true))]
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect();
        let err = super::decode(Kind::OpenFile, values).expect_err("a bool is not a filter list");
        assert_eq!(err.key(), "filters");
        assert!(err.to_string().contains("a(sa(us))"), "{err}");
    }
}
