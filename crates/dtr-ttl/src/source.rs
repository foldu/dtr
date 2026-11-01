//! Reading a macro file: its encoding and its lines.
//!
//! §3.1 of the manual fixes the encoding rules, and they are unusually short:
//! a BOM decides among UTF-8, UTF-16BE and UTF-16LE; with no BOM the file is
//! read as UTF-8 if it decodes as UTF-8 and as the ANSI code page if it does
//! not. §3.2 fixes the lines: one statement per line, CR / LF / CR LF all
//! ending one, and a line holds at most 1023 characters with the rest silently
//! dropped.

use std::fmt;

/// §3.2: characters per line, beyond which the line is truncated.
pub const MAX_LINE_CHARS: usize = 1023;

/// How a file was decoded, for reporting and for tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    /// UTF-8 with a byte-order mark, which is stripped before use.
    Utf8Bom,
    Utf16Be,
    Utf16Le,
    /// The fallback: an ASCII-compatible single- or double-byte code page.
    ///
    /// The real answer is `CP_ACP` on Windows, which this cannot know. It is
    /// decoded here as Latin-1 — **not** the right text — because that is
    /// lexically faithful for the files a macro is written in: every ANSI code
    /// page is ASCII-compatible, and CP932's trail bytes are `0x40..=0xFC`, so
    /// no quote, semicolon, bracket, equals sign or newline can be hiding in
    /// the second byte of a double-byte character. The text needs a real
    /// decoder before any command may see it; the *structure* does not.
    Ansi,
}

#[derive(Debug)]
pub enum DecodeError {
    Utf8(std::str::Utf8Error),
    /// A UTF-16 file with an odd length or an unpaired surrogate.
    Utf16,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Utf8(e) => write!(f, "invalid UTF-8: {e}"),
            Self::Utf16 => write!(f, "invalid UTF-16"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl From<std::str::Utf8Error> for DecodeError {
    fn from(e: std::str::Utf8Error) -> Self {
        Self::Utf8(e)
    }
}

/// Decode a macro file's bytes, returning the text and how it was read.
pub fn decode(bytes: &[u8]) -> Result<(String, Encoding), DecodeError> {
    match bytes {
        [0xEF, 0xBB, 0xBF, rest @ ..] => Ok((std::str::from_utf8(rest)?.to_owned(), Encoding::Utf8Bom)),
        [0xFE, 0xFF, rest @ ..] => Ok((utf16(rest, true)?, Encoding::Utf16Be)),
        [0xFF, 0xFE, rest @ ..] => Ok((utf16(rest, false)?, Encoding::Utf16Le)),
        _ => match std::str::from_utf8(bytes) {
            Ok(text) => Ok((text.to_owned(), Encoding::Utf8)),
            // See `Encoding::Ansi`: a byte-preserving decode keeps the
            // structure legible, which is all this stage needs.
            Err(_) => Ok((bytes.iter().map(|&b| b as char).collect(), Encoding::Ansi)),
        },
    }
}

fn utf16(bytes: &[u8], big_endian: bool) -> Result<String, DecodeError> {
    if bytes.len() % 2 != 0 {
        return Err(DecodeError::Utf16);
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| {
            if big_endian {
                u16::from_be_bytes([c[0], c[1]])
            } else {
                u16::from_le_bytes([c[0], c[1]])
            }
        })
        .collect();
    String::from_utf16(&units).map_err(|_| DecodeError::Utf16)
}

/// Split a decoded file into the lines §3.2 describes, each truncated to
/// [`MAX_LINE_CHARS`].
///
/// A trailing newline does not produce a final empty line; CR, LF and CR LF all
/// end a line, so a file written on any system reads the same.
pub fn lines(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push(std::mem::take(&mut cur));
            }
            '\n' => out.push(std::mem::take(&mut cur)),
            _ => {
                if cur.chars().count() < MAX_LINE_CHARS {
                    cur.push(c);
                }
                // Past the limit the character is dropped: §3.2 says the excess
                // is ignored, and ignoring it silently is the documented
                // behaviour rather than an error.
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bom_selects_the_encoding() {
        assert_eq!(decode(b"\xEF\xBB\xBF sendln 'x'\r\n").unwrap().1, Encoding::Utf8Bom);
        assert_eq!(decode(b"\xFE\xFF\0a\0b").unwrap().1, Encoding::Utf16Be);
        assert_eq!(decode(b"\xFF\xFEa\0b\0").unwrap().1, Encoding::Utf16Le);
        assert_eq!(decode(b"sendln 'x'").unwrap().1, Encoding::Utf8);
    }

    #[test]
    fn utf16_text_is_decoded_not_guessed() {
        let (text, _) = decode(b"\xFF\xFEa\0b\0").unwrap();
        assert_eq!(text, "ab");
        let (text, _) = decode(b"\xFE\xFF\0a\0b").unwrap();
        assert_eq!(text, "ab");
    }

    #[test]
    fn a_file_that_is_not_utf8_falls_back_and_keeps_its_shape() {
        // "表" in CP932 is 0x95 0x5C: the trail byte is a backslash, which is
        // exactly the byte that would break a naive lexer if it were special.
        let (text, enc) = decode(b"send '\x95\x5C'\r\n").unwrap();
        assert_eq!(enc, Encoding::Ansi);
        assert_eq!(text.chars().count(), 11, "two bytes become two characters");
        assert!(text.starts_with("send '"));
        assert_eq!(text.chars().nth(6), Some('\u{95}'));
        assert_eq!(text.chars().nth(7), Some('\u{5C}'));
        assert_eq!(text.chars().nth(8), Some('\''), "the closing quote survives");
    }

    #[test]
    fn the_three_newlines_all_end_a_line() {
        assert_eq!(lines("a\r\nb\rc\nd"), ["a", "b", "c", "d"]);
    }

    #[test]
    fn a_trailing_newline_does_not_add_an_empty_line() {
        assert_eq!(lines("a\r\n"), ["a"]);
        assert_eq!(lines(""), Vec::<String>::new());
        assert_eq!(lines("a\n\n"), ["a", ""]);
    }

    #[test]
    fn a_line_is_cut_at_1023_characters() {
        let long = "x".repeat(MAX_LINE_CHARS + 10);
        assert_eq!(lines(&long)[0].chars().count(), MAX_LINE_CHARS);
    }

    #[test]
    fn counting_characters_not_bytes_matters_for_a_cut() {
        // 600 two-byte characters, then more: the cut is at 1023 characters,
        // which is 2046 bytes, so a byte-based cut would split a character.
        let text = "é".repeat(1100);
        assert_eq!(lines(&text)[0].chars().count(), MAX_LINE_CHARS);
    }
}
