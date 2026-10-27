//! The error type returned by [`crate::parse_file`] and [`crate::parse_str`].

use std::fmt;
use std::io;
use std::path::PathBuf;

use crate::parser::MAX_INCLUDE_DEPTH;

/// A failure while reading or parsing an OpenSSH client configuration.
#[derive(Debug)]
pub enum Error {
    /// A configuration file could not be read.
    Io {
        /// The file that could not be read.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// A non-glob `Include` path does not exist.
    MissingInclude {
        /// The path named by the directive.
        path: PathBuf,
        /// The file containing the directive.
        file: PathBuf,
        /// One-based line of the directive in `file`.
        line: usize,
    },
    /// An `Include` glob could not be turned into a pattern.
    InvalidIncludePattern {
        /// The pattern as written.
        pattern: String,
        /// The file containing the directive.
        file: PathBuf,
        /// One-based line of the directive in `file`.
        line: usize,
    },
    /// Includes are nested deeper than [`MAX_INCLUDE_DEPTH`].
    IncludeDepth {
        /// The include being expanded when the cap was reached.
        path: PathBuf,
    },
    /// An include cycle was detected.
    IncludeCycle {
        /// The file that was already being expanded.
        path: PathBuf,
    },
    /// A `Port` directive was not an integer in `1..=65535`.
    InvalidPort {
        /// The file containing the directive.
        file: PathBuf,
        /// One-based line of the directive in `file`.
        line: usize,
        /// The offending value.
        value: String,
    },
    /// A line could not be parsed.
    Syntax {
        /// The file containing the line.
        file: PathBuf,
        /// One-based line number.
        line: usize,
        /// What is wrong with it.
        message: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::MissingInclude { path, file, line } => write!(
                f,
                "{}:{line}: included file does not exist: {}",
                file.display(),
                path.display()
            ),
            Self::InvalidIncludePattern {
                pattern,
                file,
                line,
            } => write!(
                f,
                "{}:{line}: invalid include pattern {pattern:?}",
                file.display()
            ),
            Self::IncludeDepth { path } => write!(
                f,
                "{}: includes nested deeper than {MAX_INCLUDE_DEPTH} levels",
                path.display()
            ),
            Self::IncludeCycle { path } => {
                write!(f, "{}: include cycle detected", path.display())
            }
            Self::InvalidPort { file, line, value } => write!(
                f,
                "{}:{line}: invalid Port {value:?} (expected 1-65535)",
                file.display()
            ),
            Self::Syntax {
                file,
                line,
                message,
            } => write!(f, "{}:{line}: {message}", file.display()),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
