//! Parser and resolver for OpenSSH client configuration.
//!
//! The intended consumer is a host picker: [`Config::hosts`] lists the `Host`
//! entries for display, and [`Config::resolve`] answers "what would ssh do for
//! this alias", including `Include` files that were expanded in place.
//!
//! ```
//! use std::path::Path;
//!
//! let config = dtr_sshconfig::parse_str("Host web\n    User alice\n", Path::new(".")).unwrap();
//! assert_eq!(config.resolve("web").user.as_deref(), Some("alice"));
//! ```
//!
//! # Semantics
//!
//! * Keywords are case-insensitive; `Keyword value`, `Keyword=value` and
//!   `Keyword = value` are equivalent. `#` starts a comment outside double
//!   quotes. Inside quotes, `\"` and `\\` are the only escapes, and quoted runs
//!   concatenate with unquoted ones.
//! * A `Host` block matches when at least one positive pattern matches and no
//!   negated (`!`) pattern does. Patterns support `*` and `?`, matched
//!   case-insensitively. A `Host *` is positional, like every other block.
//! * Resolution walks matching blocks in file order and keeps the **first**
//!   value seen for each directive. `IdentityFile` accumulates instead.
//! * Directives that appear before the first `Host`/`Match` line apply to every
//!   alias, as they do in OpenSSH.
//! * `Match all` participates in resolution; every other `Match` block is
//!   parsed, kept in [`Config::unapplied_match_blocks`], and never applied.
//! * Unknown directives are preserved verbatim in
//!   [`ResolvedHost::options`] so a picker does not drop `SendEnv` or
//!   `LocalForward`.
//!
//! # Include handling
//!
//! `Include` is expanded in place. Absolute paths, `~/` paths and relative
//! paths are accepted, and glob metacharacters (`*`, `?`, `[`) select the
//! `glob` crate's expansion, sorted for deterministic ordering. A non-glob
//! include that does not exist is an error; a glob that matches nothing is not.
//!
//! OpenSSH resolves relative includes against the user's config directory
//! (`~/.ssh`), *not* against the directory of the file that contains the
//! directive. To match that, both [`parse_file`] and [`parse_str`] resolve
//! every relative include, at any nesting depth, against a single directory:
//! the directory of the root file for [`parse_file`], and the supplied
//! `base_dir` for [`parse_str`]. Callers reading `~/.ssh/config` should pass
//! `~/.ssh`.
//!
//! After an included file returns, the including file resumes its own
//! `Host`/`Match` context. OpenSSH instead keeps whatever context the included
//! file last opened; resuming is deliberate (contexts stay in their file) and
//! is the only intentional divergence in include handling.
//!
//! # Not implemented
//!
//! Token expansion (`%h`, `%p`, `%d`, ...), `~user` expansion, `Match` criteria
//! evaluation (`exec`, `host`, `localuser`, `canonical`, `final`, ...),
//! `CanonicalizeHostname` rewriting, `Host` pattern escaping, and any form of
//! network wildcard expansion. [`ResolvedHost::identity_files`] carries paths as
//! written; no tilde expansion is applied there because a picker displays them.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod config;
mod error;
mod parser;
mod pattern;

pub use config::{Config, HostEntry, MatchEntry, ResolvedHost};
pub use error::Error;
pub use parser::{MAX_INCLUDE_DEPTH, parse_file, parse_str};
