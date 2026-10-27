//! The parsed configuration, its blocks, and OpenSSH-style resolution.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::pattern::matches_host_patterns;

/// One `Host` line from the configuration, in file order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEntry {
    /// Patterns exactly as written, including a leading `!` for negations.
    pub patterns: Vec<String>,
    /// One-based line number of the `Host` keyword.
    pub line: usize,
    /// File containing the `Host` line; `<input>` for [`crate::parse_str`].
    pub file: PathBuf,
}

impl HostEntry {
    /// The concrete aliases on this line, in order.
    ///
    /// Negated patterns and patterns containing `*` or `?` are skipped. Nothing
    /// is expanded against a network or the local host list; the result is
    /// exactly what the file said, suitable for a picker.
    pub fn aliases(&self) -> Vec<&str> {
        self.patterns
            .iter()
            .filter(|pattern| !pattern.starts_with('!') && !pattern.contains(['*', '?']))
            .map(String::as_str)
            .collect()
    }
}

/// A `Match` block whose criteria are not `all`.
///
/// These blocks are parsed and retained so a picker can show them, but
/// [`Config::resolve`] never applies their directives: evaluating `exec`,
/// `host`, `localuser` and friends is out of scope here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchEntry {
    /// Criteria as written after the `Match` keyword, split into words.
    pub criteria: Vec<String>,
    /// One-based line number of the `Match` keyword.
    pub line: usize,
    /// File containing the `Match` line; `<input>` for [`crate::parse_str`].
    pub file: PathBuf,
}

/// The result of resolving one alias against a [`Config`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedHost {
    /// The alias that was resolved.
    pub alias: String,
    /// The first `HostName` seen in a matching block.
    pub hostname: Option<String>,
    /// The first `User` seen in a matching block.
    pub user: Option<String>,
    /// The first `Port` seen in a matching block.
    pub port: Option<u16>,
    /// Every `IdentityFile` seen in matching blocks, in file order.
    ///
    /// Paths are returned as written: no `~` or `%` token expansion is applied.
    pub identity_files: Vec<PathBuf>,
    /// The first `ProxyJump` seen in a matching block.
    pub proxy_jump: Option<String>,
    /// Directives without a first-class field, keyed by canonical name.
    ///
    /// Values are in file order and first-value-wins has already been applied,
    /// except for the directives OpenSSH treats as lists (`SendEnv`, `SetEnv`,
    /// `LocalForward`, `RemoteForward`, `DynamicForward`, `CertificateFile`,
    /// `UserKnownHostsFile`, `GlobalKnownHostsFile`, `IgnoreUnknown`,
    /// `PermitRemoteOpen`, `Tag`), where every occurrence is kept.
    ///
    /// Known directive names are normalised to OpenSSH's spelling (`Hostname`
    /// becomes `HostName`); unknown names are kept as written, so two casings
    /// of an unknown directive are two entries.
    pub options: BTreeMap<String, Vec<String>>,
}

impl ResolvedHost {
    fn for_alias(alias: &str) -> Self {
        Self {
            alias: alias.to_string(),
            hostname: None,
            user: None,
            port: None,
            identity_files: Vec::new(),
            proxy_jump: None,
            options: BTreeMap::new(),
        }
    }

    /// The name to connect to: `HostName` if one applied, otherwise the alias.
    pub fn effective_hostname(&self) -> &str {
        self.hostname.as_deref().unwrap_or(&self.alias)
    }

    /// The port to connect to, defaulting to SSH's 22.
    pub fn effective_port(&self) -> u16 {
        self.port.unwrap_or(22)
    }
}

/// A parsed OpenSSH client configuration with includes expanded in place.
///
/// Blocks are kept in file order; see the crate docs for the resolution rules.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub(crate) scopes: Vec<Scope>,
    pub(crate) hosts: Vec<HostEntry>,
    pub(crate) match_blocks: Vec<MatchEntry>,
}

impl Config {
    /// The `Host` entries in file order, ready to display in a picker.
    ///
    /// Patterns are reported exactly as written (wildcards and negations
    /// included); no expansion is performed against a network or the local
    /// host list. Use [`HostEntry::aliases`] for the concrete names.
    pub fn hosts(&self) -> Vec<HostEntry> {
        self.hosts.clone()
    }

    /// The `Match` blocks that were retained but are never applied.
    pub fn unapplied_match_blocks(&self) -> Vec<MatchEntry> {
        self.match_blocks.clone()
    }

    /// Apply OpenSSH resolution semantics for one alias.
    ///
    /// Matching blocks are visited in file order; the first value seen for a
    /// scalar directive wins and `IdentityFile` accumulates. Blocks that do not
    /// match, including every `Match` block except `Match all`, contribute
    /// nothing. Resolving an alias that matches no block yields the alias
    /// itself with empty options.
    pub fn resolve(&self, alias: &str) -> ResolvedHost {
        let mut resolved = ResolvedHost::for_alias(alias);
        for scope in &self.scopes {
            if !scope.matches(alias) {
                continue;
            }
            for directive in &scope.directives {
                directive.apply(&mut resolved);
            }
        }
        resolved
    }
}

/// How often a directive may contribute a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Arity {
    /// Only the first value is kept.
    Single,
    /// Every value is kept, in file order.
    Multi,
}

/// The directives that resolve into [`ResolvedHost`] fields rather than
/// [`ResolvedHost::options`], plus the `options` catch-all.
#[derive(Debug, Clone, Copy)]
pub(crate) enum DirectiveKind {
    HostName,
    User,
    Port(u16),
    ProxyJump,
    IdentityFile,
    Option(Arity),
}

/// One directive in the block where it appeared.
#[derive(Debug, Clone)]
pub(crate) struct Directive {
    pub(crate) kind: DirectiveKind,
    pub(crate) name: String,
    pub(crate) value: String,
}

impl Directive {
    fn apply(&self, resolved: &mut ResolvedHost) {
        match self.kind {
            DirectiveKind::HostName => set_first(&mut resolved.hostname, &self.value),
            DirectiveKind::User => set_first(&mut resolved.user, &self.value),
            DirectiveKind::ProxyJump => set_first(&mut resolved.proxy_jump, &self.value),
            DirectiveKind::Port(port) => {
                if resolved.port.is_none() {
                    resolved.port = Some(port);
                }
            }
            DirectiveKind::IdentityFile => {
                resolved.identity_files.push(PathBuf::from(&self.value));
            }
            DirectiveKind::Option(arity) => {
                let values = resolved.options.entry(self.name.clone()).or_default();
                if arity == Arity::Multi || values.is_empty() {
                    values.push(self.value.clone());
                }
            }
        }
    }
}

fn set_first(slot: &mut Option<String>, value: &str) {
    if slot.is_none() {
        *slot = Some(value.to_string());
    }
}

/// One `Host` or `Match` block (or the implicit global context) receiving the
/// directives that follow it.
#[derive(Debug, Clone)]
pub(crate) struct Scope {
    pub(crate) kind: ScopeKind,
    pub(crate) file: PathBuf,
    pub(crate) line: usize,
    pub(crate) directives: Vec<Directive>,
}

impl Scope {
    fn matches(&self, alias: &str) -> bool {
        match &self.kind {
            ScopeKind::Global => true,
            ScopeKind::Host(patterns) => matches_host_patterns(patterns, alias),
            ScopeKind::Match => false,
        }
    }
}

/// What a [`Scope`] was opened by.
#[derive(Debug, Clone)]
pub(crate) enum ScopeKind {
    /// Directives before any block, and `Match all`: apply to every alias.
    Global,
    /// A `Host` block, with its patterns as written.
    Host(Vec<String>),
    /// A `Match` block that is never applied.
    Match,
}
