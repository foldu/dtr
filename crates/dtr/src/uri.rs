//! `file:` URIs, and which machine they name.
//!
//! Two features ask the same question — the cwd sniffer reading OSC 7 and the
//! pointer opening an OSC 8 link — and they answered it separately: the sniffer
//! knew that `file://<this machine>/path` is local, the link opener accepted
//! only `localhost` and refused everything else. So every `ls --hyperlink=always`
//! result was refused as another host, which is exactly what that program
//! prints: `file://jupiter/home/x`, never `file:///home/x`.
//!
//! The rule lives here once. A host is this machine when it is empty, when it is
//! `localhost`, or when it is this machine's own name.

/// This machine's name, read once.
///
/// `/etc/hostname` is the authority on Linux; `HOSTNAME` covers a container
/// without one, and `COMPUTERNAME` is what Windows calls the same thing.
pub fn local_hostname() -> Option<&'static str> {
    static NAME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        std::fs::read_to_string("/etc/hostname")
            .ok()
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .or_else(|| {
                std::env::var("HOSTNAME")
                    .ok()
                    .filter(|name| !name.trim().is_empty())
            })
            .or_else(|| std::env::var("COMPUTERNAME").ok())
    })
    .as_deref()
}

/// Is `host` this machine?
///
/// Empty and `localhost` are; so is the machine's own name, which is what a
/// program printing a `file:` URI for a file it just listed will use.
pub fn is_local_host(host: &str) -> bool {
    host.is_empty()
        || host.eq_ignore_ascii_case("localhost")
        || local_hostname().is_some_and(|name| host.eq_ignore_ascii_case(name))
}

/// The host and the path of a `file:` URI.
///
/// Both `file:///path` and `file:/path` name a path with no host; `file:x` is a
/// relative reference and names nothing this can open. The host is not
/// percent-decoded — a machine name with a `%` in it is not a thing a shell
/// reports — but the path is, because that is where `%20` lives.
pub fn split(uri: &str) -> Option<(&str, String)> {
    let rest = uri.strip_prefix("file:")?;
    let (host, path) = match rest.strip_prefix("//") {
        Some(after) => {
            let slash = after.find('/')?;
            (&after[..slash], &after[slash..])
        }
        None => ("", rest),
    };
    if !path.starts_with('/') {
        return None;
    }
    Some((host, drive_letter(percent_decode(path))))
}

/// `file:///C:/Users/x` is a Windows path, not a directory called `C:` at the
/// root of the filesystem, and `PathBuf::from("/C:/x")` is not the file there.
fn drive_letter(path: String) -> String {
    let bytes = path.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':' {
        return path[1..].to_owned();
    }
    path
}

/// `%20` and friends.
///
/// The bytes are collected before they are turned into a string, so an escape
/// that spells a whole UTF-8 character (`%C3%A9`) decodes to that character
/// rather than to two halves of one. An escape that is not valid hex stays
/// literal, because a path with a stray `%` in it should survive.
pub fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' && at + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[at + 1..at + 3]) {
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    out.push(byte);
                    at += 3;
                    continue;
                }
            }
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::{drive_letter, is_local_host, local_hostname, percent_decode, split};

    fn path(uri: &str) -> Option<String> {
        split(uri).map(|(_, path)| path)
    }

    #[test]
    fn a_file_uri_names_a_path() {
        assert_eq!(path("file:///tmp/notes.txt").as_deref(), Some("/tmp/notes.txt"));
        assert_eq!(
            path("file://localhost/tmp/a%20b.png").as_deref(),
            Some("/tmp/a b.png")
        );
        // A URI may leave the `//` out; the path still has to be absolute.
        assert_eq!(path("file:/tmp/x").as_deref(), Some("/tmp/x"));
        assert_eq!(path("file:tmp/x"), None);
        assert_eq!(path("file://"), None);
        assert_eq!(path("https://example.com/x"), None);
        assert_eq!(path("/tmp/relative"), None);
    }

    #[test]
    fn the_host_is_kept_apart_from_the_path() {
        assert_eq!(split("file://jupyter/etc/hosts"), Some(("jupyter", "/etc/hosts".to_owned())));
        assert_eq!(split("file:///etc/hosts"), Some(("", "/etc/hosts".to_owned())));
    }

    /// An escape that spells one character must decode to one character: the
    /// byte-at-a-time version of this turned `%C3%A9` into two Latin-1 letters.
    #[test]
    fn an_escape_decodes_whole_characters() {
        assert_eq!(percent_decode("caf%C3%A9"), "café");
        assert_eq!(percent_decode("%2F"), "/");
        // A stray % is a literal: dropping it would silently change the path.
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    /// Empty, `localhost` and this machine are the only hosts whose paths mean
    /// anything here; anything else is somebody else's disk.
    #[test]
    fn this_machine_answers_to_three_names() {
        assert!(is_local_host(""));
        assert!(is_local_host("LOCALHOST"));
        if let Some(name) = local_hostname() {
            assert!(is_local_host(name), "{name} is this machine");
        }
        assert!(!is_local_host("some-other-host.invalid"));
    }

    /// A Windows path in a URI starts with a slash that is not a directory.
    #[test]
    fn a_drive_letter_is_not_a_directory() {
        assert_eq!(drive_letter("/C:/Users/x".to_owned()), "C:/Users/x");
        assert_eq!(drive_letter("/tmp/x".to_owned()), "/tmp/x");
        assert_eq!(drive_letter("/".to_owned()), "/");
    }
}
