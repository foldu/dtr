//! Paths to `file://` URIs, from bytes.
//!
//! The portal asks backends to normalise the locations a person selected into
//! `file://` URIs. The trap is the word "path": on Unix a path is a byte string,
//! not a `str` — a filename may be any bytes but `/` and NUL — so
//! `to_string_lossy` is not an encoder, it is a rewriter that turns a perfectly
//! good path into a URI naming a different file. Every byte outside the
//! unreserved set is percent-encoded here, which is also what makes the result a
//! valid D-Bus string: percent-encoded text is ASCII by construction.
//!
//! `/` stays literal, because a URI path is made of segments; encoding it would
//! give one segment with `%2F`s in it and a document portal that cannot tell a
//! directory from a file. Everything else outside `A-Za-z0-9-._~` — space, `+`,
//! `#`, `?`, `%`, `:`, and every byte of a non-UTF-8 name — becomes `%XX`.

use std::path::Path;

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

/// The URI for an absolute path, or `None` for one that cannot be normalised.
///
/// "Cannot be normalised" is a relative path: `file:relative` is not a URI a
/// caller can open, and the portal's instruction is to discard locations that
/// cannot be made into `file://` URIs rather than to invent a base for them.
pub fn file_uri(path: &Path) -> Option<String> {
    if !path.is_absolute() {
        return None;
    }
    let mut uri = String::with_capacity(7 + path.as_os_str().len());
    uri.push_str("file://");
    for byte in bytes(path) {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                uri.push(byte as char)
            }
            _ => {
                uri.push('%');
                uri.push(HEX[(byte >> 4) as usize] as char);
                uri.push(HEX[(byte & 0x0f) as usize] as char);
            }
        }
    }
    Some(uri)
}

/// The raw bytes of a path.
///
/// Unix has them. Elsewhere (this crate is Linux's, but the workspace is
/// compiled by tooling that does not know that) a non-Unicode path is best
/// effort — the URI is still well formed, and the alternative is not compiling.
#[cfg(unix)]
fn bytes(path: &Path) -> Vec<u8> {
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().into_owned().into_bytes()
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn a_plain_path_is_itself_after_the_scheme() {
        assert_eq!(
            file_uri(Path::new("/home/barnabas/notes.txt")).as_deref(),
            Some("file:///home/barnabas/notes.txt")
        );
        // The unreserved set survives, including the ones that look punctuation.
        assert_eq!(
            file_uri(Path::new("/tmp/a-b_c.d~e")).as_deref(),
            Some("file:///tmp/a-b_c.d~e")
        );
    }

    /// A space is `%20` and `+` is not: form encoding is the bug this avoids.
    /// `#` and `?` would end the path and start a fragment or a query in any URI
    /// parser, so they have to go too.
    #[test]
    fn a_name_that_would_change_the_uri_is_percent_encoded() {
        assert_eq!(
            file_uri(Path::new("/tmp/a b/c+d#e?f%g")).as_deref(),
            Some("file:///tmp/a%20b/c%2Bd%23e%3Ff%25g")
        );
        // A `:` in a segment is legal in a URI path but encoding it is still
        // correct, and this is the rule: unreserved and `/`, nothing else.
        assert_eq!(
            file_uri(Path::new("/tmp/a:b")).as_deref(),
            Some("file:///tmp/a%3Ab")
        );
    }

    /// The reason this takes bytes: a name that is not UTF-8 encodes exactly,
    /// where `to_string_lossy` would have replaced it with U+FFFD and pointed the
    /// caller at a file that does not exist.
    #[cfg(unix)]
    #[test]
    fn a_non_utf8_name_encodes_each_byte() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let path = PathBuf::from(OsStr::from_bytes(b"/tmp/caf\xc3\xa9\x80"));
        assert_eq!(
            file_uri(&path).as_deref(),
            Some("file:///tmp/caf%C3%A9%80"),
            "UTF-8 bytes encode byte by byte, and so does the invalid one"
        );
    }

    #[test]
    fn a_relative_path_is_discarded_rather_than_given_a_base() {
        assert_eq!(file_uri(Path::new("notes.txt")), None);
        assert_eq!(file_uri(Path::new("../notes.txt")), None);
    }
}
