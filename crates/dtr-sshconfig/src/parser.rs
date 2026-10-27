//! Line parsing, `Include` expansion and directive classification.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use glob::glob;

use crate::config::{
    Arity, Config, Directive, DirectiveKind, HostEntry, MatchEntry, Scope, ScopeKind,
};
use crate::error::Error;

/// Maximum number of nested `Include` files.
///
/// The root configuration file is depth 0; each included file adds one level.
/// An include that would open level 17 fails with [`Error::IncludeDepth`].
pub const MAX_INCLUDE_DEPTH: usize = 16;

/// File name reported for text passed to [`parse_str`], which has no path.
const INPUT_FILE: &str = "<input>";

/// Parse an OpenSSH client configuration from `path`.
///
/// Relative `Include` paths are resolved against the directory containing
/// `path`, mirroring OpenSSH's single config directory (see the crate docs).
pub fn parse_file(path: &Path) -> Result<Config, Error> {
    let canonical = fs::canonicalize(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let base_dir = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let input = read_config(&canonical)?;

    let mut parser = Parser::new(base_dir);
    let mut stack = vec![canonical.clone()];
    parser.parse_source(&input, &canonical, 0, &mut stack)?;
    Ok(parser.finish())
}

/// Parse an OpenSSH client configuration from an in-memory string.
///
/// `base_dir` is the directory that relative `Include` paths resolve against,
/// for this input and for everything it includes; pass the directory the text
/// would have lived in (usually `~/.ssh`). Host entries parsed from `input`
/// itself report the file `<input>`.
pub fn parse_str(input: &str, base_dir: &Path) -> Result<Config, Error> {
    let mut parser = Parser::new(base_dir.to_path_buf());
    let mut stack = Vec::new();
    parser.parse_source(input, Path::new(INPUT_FILE), 0, &mut stack)?;
    Ok(parser.finish())
}

fn read_config(path: &Path) -> Result<String, Error> {
    fs::read_to_string(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// State collected while walking the root file and its includes.
struct Parser {
    base_dir: PathBuf,
    scopes: Vec<Scope>,
    hosts: Vec<HostEntry>,
    match_blocks: Vec<MatchEntry>,
    /// Scope currently receiving directives, if any.
    current: Option<usize>,
}

impl Parser {
    fn new(base_dir: PathBuf) -> Self {
        Self {
            base_dir,
            scopes: Vec::new(),
            hosts: Vec::new(),
            match_blocks: Vec::new(),
            current: None,
        }
    }

    fn finish(self) -> Config {
        Config {
            scopes: self.scopes,
            hosts: self.hosts,
            match_blocks: self.match_blocks,
        }
    }

    /// Parse one file's text, appending to the already-open scopes.
    ///
    /// `depth` is the include depth of `file` itself (0 for the root file).
    fn parse_source(
        &mut self,
        input: &str,
        file: &Path,
        depth: usize,
        stack: &mut Vec<PathBuf>,
    ) -> Result<(), Error> {
        // A UTF-8 BOM at the start of a file is not a keyword.
        let input = input.strip_prefix('\u{feff}').unwrap_or(input);

        for (index, raw) in input.lines().enumerate() {
            let line = index + 1;
            let tokens = tokenize(raw).map_err(|message| Error::Syntax {
                file: file.to_path_buf(),
                line,
                message,
            })?;
            let Some((keyword, args)) = split_keyword(&tokens) else {
                continue;
            };
            if keyword.is_empty() {
                return Err(Error::Syntax {
                    file: file.to_path_buf(),
                    line,
                    message: "missing keyword".to_string(),
                });
            }

            let lower = keyword.to_ascii_lowercase();
            match lower.as_str() {
                "host" => self.start_host(args, file, line)?,
                "match" => self.start_match(args, file, line)?,
                "include" => self.include(&args, file, line, depth, stack)?,
                _ => self.directive(&lower, &keyword, &args, file, line)?,
            }
        }
        Ok(())
    }

    fn start_host(&mut self, args: Vec<String>, file: &Path, line: usize) -> Result<(), Error> {
        if args.is_empty() {
            return Err(Error::Syntax {
                file: file.to_path_buf(),
                line,
                message: "Host needs at least one pattern".to_string(),
            });
        }
        self.hosts.push(HostEntry {
            patterns: args.clone(),
            line,
            file: file.to_path_buf(),
        });
        self.push_scope(ScopeKind::Host(args), file, line);
        Ok(())
    }

    fn start_match(&mut self, args: Vec<String>, file: &Path, line: usize) -> Result<(), Error> {
        if args.is_empty() {
            return Err(Error::Syntax {
                file: file.to_path_buf(),
                line,
                message: "Match needs a criterion".to_string(),
            });
        }
        if args.len() == 1 && args[0].eq_ignore_ascii_case("all") {
            self.push_scope(ScopeKind::Global, file, line);
        } else {
            self.match_blocks.push(MatchEntry {
                criteria: args,
                line,
                file: file.to_path_buf(),
            });
            self.push_scope(ScopeKind::Match, file, line);
        }
        Ok(())
    }

    fn push_scope(&mut self, kind: ScopeKind, file: &Path, line: usize) {
        let index = self.scopes.len();
        self.scopes.push(Scope {
            kind,
            file: file.to_path_buf(),
            line,
            directives: Vec::new(),
        });
        self.current = Some(index);
    }

    /// Append a copy of `index`'s block so directives after an interrupted
    /// include keep their original context but the right position in the file.
    fn continuation(&mut self, index: usize) -> Option<usize> {
        let scope = self.scopes.get(index)?;
        let (kind, file, line) = (scope.kind.clone(), scope.file.clone(), scope.line);
        let next = self.scopes.len();
        self.scopes.push(Scope {
            kind,
            file,
            line,
            directives: Vec::new(),
        });
        Some(next)
    }

    fn directive(
        &mut self,
        lower: &str,
        written: &str,
        args: &[String],
        file: &Path,
        line: usize,
    ) -> Result<(), Error> {
        if args.is_empty() {
            return Err(Error::Syntax {
                file: file.to_path_buf(),
                line,
                message: format!("{written} needs a value"),
            });
        }
        let value = args.join(" ");

        let (name, kind) = match classify(lower) {
            None => (written.to_string(), DirectiveKind::Option(Arity::Single)),
            Some((canonical, class)) => {
                let kind = match class {
                    Class::HostName => DirectiveKind::HostName,
                    Class::User => DirectiveKind::User,
                    Class::ProxyJump => DirectiveKind::ProxyJump,
                    Class::IdentityFile => DirectiveKind::IdentityFile,
                    Class::Port => {
                        let port = parse_port(&value).ok_or_else(|| Error::InvalidPort {
                            file: file.to_path_buf(),
                            line,
                            value: value.clone(),
                        })?;
                        DirectiveKind::Port(port)
                    }
                    Class::Option(arity) => DirectiveKind::Option(arity),
                };
                (canonical.to_string(), kind)
            }
        };

        self.push_directive(Directive { kind, name, value }, file, line);
        Ok(())
    }

    /// Append `directive` to the open scope, opening the implicit global scope
    /// for directives that appear before the first `Host`/`Match` line.
    fn push_directive(&mut self, directive: Directive, file: &Path, line: usize) {
        let index = match self.current {
            Some(index) => index,
            None => {
                let index = self.scopes.len();
                self.scopes.push(Scope {
                    kind: ScopeKind::Global,
                    file: file.to_path_buf(),
                    line,
                    directives: Vec::new(),
                });
                self.current = Some(index);
                index
            }
        };
        if let Some(scope) = self.scopes.get_mut(index) {
            scope.directives.push(directive);
        }
    }

    fn include(
        &mut self,
        args: &[String],
        file: &Path,
        line: usize,
        depth: usize,
        stack: &mut Vec<PathBuf>,
    ) -> Result<(), Error> {
        if args.len() != 1 {
            return Err(Error::Syntax {
                file: file.to_path_buf(),
                line,
                message: "Include takes exactly one path or pattern".to_string(),
            });
        }
        let raw = &args[0];
        let mut path = expand_tilde(raw).map_err(|message| Error::Syntax {
            file: file.to_path_buf(),
            line,
            message: message.to_string(),
        })?;
        if !path.is_absolute() {
            path = self.base_dir.join(&path);
        }

        if is_glob_pattern(raw) {
            let pattern = path.to_str().ok_or_else(|| Error::InvalidIncludePattern {
                pattern: path.to_string_lossy().into_owned(),
                file: file.to_path_buf(),
                line,
            })?;
            let entries = glob(pattern).map_err(|_| Error::InvalidIncludePattern {
                pattern: pattern.to_string(),
                file: file.to_path_buf(),
                line,
            })?;
            let mut matches = Vec::new();
            for entry in entries {
                match entry {
                    Ok(found) => matches.push(found),
                    Err(error) => {
                        let path = error.path().to_path_buf();
                        return Err(Error::Io {
                            path,
                            source: error.into(),
                        });
                    }
                }
            }
            // glob(3) sorts by default; do the same so ordering is explicit
            // and independent of the filesystem.
            matches.sort();
            for found in matches {
                self.include_file(&found, file, line, depth, stack)?;
            }
        } else {
            self.include_file(&path, file, line, depth, stack)?;
        }
        Ok(())
    }

    fn include_file(
        &mut self,
        path: &Path,
        site: &Path,
        line: usize,
        depth: usize,
        stack: &mut Vec<PathBuf>,
    ) -> Result<(), Error> {
        if depth >= MAX_INCLUDE_DEPTH {
            return Err(Error::IncludeDepth {
                path: path.to_path_buf(),
            });
        }

        let canonical = match fs::canonicalize(path) {
            Ok(canonical) => canonical,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Err(Error::MissingInclude {
                    path: path.to_path_buf(),
                    file: site.to_path_buf(),
                    line,
                });
            }
            Err(source) => {
                return Err(Error::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        if stack.contains(&canonical) {
            return Err(Error::IncludeCycle { path: canonical });
        }

        let input = read_config(&canonical)?;
        let inherited = self.current;
        let scope_count = self.scopes.len();

        stack.push(canonical.clone());
        let result = self.parse_source(&input, &canonical, depth + 1, stack);
        stack.pop();
        result?;

        // If the included file opened blocks of its own, the enclosing block
        // has to be resumed as a new scope so later directives stay in order.
        if self.scopes.len() != scope_count {
            self.current = inherited.and_then(|index| self.continuation(index));
        }
        Ok(())
    }
}

/// Split a tokenized line into its keyword and value words.
///
/// Handles `Keyword value`, `Keyword=value` and `Keyword = value`. Returns
/// `None` for a line with no tokens.
fn split_keyword(tokens: &[String]) -> Option<(String, Vec<String>)> {
    let first = tokens.first()?;
    let mut values: Vec<String> = Vec::new();

    let keyword = if let Some((keyword, rest)) = first.split_once('=') {
        if !rest.is_empty() {
            values.push(rest.to_string());
        }
        values.extend(tokens.iter().skip(1).cloned());
        keyword.to_string()
    } else {
        values.extend(tokens.iter().skip(1).cloned());
        if values.first().is_some_and(|value| value.as_str() == "=") {
            values.remove(0);
        }
        first.clone()
    };

    Some((keyword, values))
}

/// Split one line into words, honouring double quotes and comments.
///
/// Double quotes group whitespace and may be concatenated with unquoted runs.
/// Inside quotes, `\"` and `\\` are escapes; any other backslash is literal.
/// `#` outside quotes starts a comment that runs to the end of the line.
fn tokenize(line: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut chars = line.chars();

    while let Some(character) = chars.next() {
        match character {
            '"' => {
                in_token = true;
                loop {
                    match chars.next() {
                        None => return Err("unterminated double quote".to_string()),
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(escaped @ ('"' | '\\')) => current.push(escaped),
                            Some(other) => {
                                current.push('\\');
                                current.push(other);
                            }
                            None => return Err("unterminated escape in double quote".to_string()),
                        },
                        Some(other) => current.push(other),
                    }
                }
            }
            '#' => {
                if in_token {
                    tokens.push(std::mem::take(&mut current));
                    in_token = false;
                }
                break;
            }
            character if character.is_whitespace() => {
                if in_token {
                    tokens.push(std::mem::take(&mut current));
                    in_token = false;
                }
            }
            character => {
                in_token = true;
                current.push(character);
            }
        }
    }

    if in_token {
        tokens.push(current);
    }
    Ok(tokens)
}

fn parse_port(value: &str) -> Option<u16> {
    value.parse::<u16>().ok().filter(|port| *port != 0)
}

/// Whether an `Include` argument should be treated as a glob.
fn is_glob_pattern(raw: &str) -> bool {
    raw.contains(['*', '?', '['])
}

/// Expand a leading `~` (current user only) into a filesystem path.
fn expand_tilde(raw: &str) -> Result<PathBuf, &'static str> {
    let Some(rest) = raw.strip_prefix('~') else {
        return Ok(PathBuf::from(raw));
    };
    if !(rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\')) {
        return Err("~user expansion is not supported; use ~ or an absolute path");
    }
    let home = home_dir().ok_or("cannot expand ~: neither HOME nor USERPROFILE is set")?;
    if rest.is_empty() {
        return Ok(home);
    }
    Ok(home.join(rest.trim_start_matches(['/', '\\'])))
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|home| !home.as_os_str().is_empty())
}

/// How a known directive is resolved.
#[derive(Debug, Clone, Copy)]
enum Class {
    HostName,
    User,
    Port,
    ProxyJump,
    IdentityFile,
    Option(Arity),
}

fn single() -> Class {
    Class::Option(Arity::Single)
}

fn multi() -> Class {
    Class::Option(Arity::Multi)
}

/// Map a lowercased keyword to its canonical spelling and how it resolves.
///
/// Directives that are not listed here are preserved verbatim in
/// [`ResolvedHost::options`](crate::ResolvedHost::options) with first-value-wins.
fn classify(lower: &str) -> Option<(&'static str, Class)> {
    let (name, class) = match lower {
        // Directives with first-class fields.
        "hostname" => ("HostName", Class::HostName),
        "user" => ("User", Class::User),
        "port" => ("Port", Class::Port),
        "proxyjump" => ("ProxyJump", Class::ProxyJump),
        "identityfile" => ("IdentityFile", Class::IdentityFile),

        // Single-valued options.
        "addkeystoagent" => ("AddKeysToAgent", single()),
        "batchmode" => ("BatchMode", single()),
        "bindaddress" => ("BindAddress", single()),
        "bindinterface" => ("BindInterface", single()),
        "canonicaldomains" => ("CanonicalDomains", single()),
        "canonicalizehostname" => ("CanonicalizeHostname", single()),
        "casignaturealgorithms" => ("CASignatureAlgorithms", single()),
        "channeltimeout" => ("ChannelTimeout", single()),
        "checkhostip" => ("CheckHostIP", single()),
        "ciphers" => ("Ciphers", single()),
        "compression" => ("Compression", single()),
        "connecttimeout" => ("ConnectTimeout", single()),
        "connectionattempts" => ("ConnectionAttempts", single()),
        "controlmaster" => ("ControlMaster", single()),
        "controlpath" => ("ControlPath", single()),
        "controlpersist" => ("ControlPersist", single()),
        "enableescapecommandline" => ("EnableEscapeCommandline", single()),
        "escapechar" => ("EscapeChar", single()),
        "exitonforwardfailure" => ("ExitOnForwardFailure", single()),
        "forkafterauthentication" => ("ForkAfterAuthentication", single()),
        "forwardagent" => ("ForwardAgent", single()),
        "forwardx11" => ("ForwardX11", single()),
        "forwardx11trusted" => ("ForwardX11Trusted", single()),
        "gatewayports" => ("GatewayPorts", single()),
        "hashknownhosts" => ("HashKnownHosts", single()),
        "hostbasedacceptedalgorithms" => ("HostbasedAcceptedAlgorithms", single()),
        "hostbasedauthentication" => ("HostbasedAuthentication", single()),
        "hostkeyalgorithms" => ("HostKeyAlgorithms", single()),
        "identityagent" => ("IdentityAgent", single()),
        "identitiesonly" => ("IdentitiesOnly", single()),
        "kbdinteractiveauthentication" => ("KbdInteractiveAuthentication", single()),
        "kexalgorithms" => ("KexAlgorithms", single()),
        "knownhostscommand" => ("KnownHostsCommand", single()),
        "localcommand" => ("LocalCommand", single()),
        "loglevel" => ("LogLevel", single()),
        "macs" => ("MACs", single()),
        "passwordauthentication" => ("PasswordAuthentication", single()),
        "permitlocalcommand" => ("PermitLocalCommand", single()),
        "preferredauthentications" => ("PreferredAuthentications", single()),
        "proxycommand" => ("ProxyCommand", single()),
        "proxyusefdpass" => ("ProxyUseFdpass", single()),
        "pubkeyacceptedalgorithms" => ("PubkeyAcceptedAlgorithms", single()),
        "pubkeyauthentication" => ("PubkeyAuthentication", single()),
        "remotecommand" => ("RemoteCommand", single()),
        "requesttty" => ("RequestTTY", single()),
        "requiredrsasize" => ("RequiredRSASize", single()),
        "rdomain" => ("RDomain", single()),
        "serveralivecountmax" => ("ServerAliveCountMax", single()),
        "serveraliveinterval" => ("ServerAliveInterval", single()),
        "sessiontype" => ("SessionType", single()),
        "stdinnull" => ("StdinNull", single()),
        "streamlocalbindmask" => ("StreamLocalBindMask", single()),
        "streamlocalbindunlink" => ("StreamLocalBindUnlink", single()),
        "stricthostkeychecking" => ("StrictHostKeyChecking", single()),
        "syslogfacility" => ("SyslogFacility", single()),
        "tcpkeepalive" => ("TCPKeepAlive", single()),
        "updatehostkeys" => ("UpdateHostKeys", single()),
        "verifyhostkeydns" => ("VerifyHostKeyDNS", single()),
        "visualhostkey" => ("VisualHostKey", single()),

        // Directives OpenSSH accumulates over every occurrence.
        "certificatefile" => ("CertificateFile", multi()),
        "dynamicforward" => ("DynamicForward", multi()),
        "globalknownhostsfile" => ("GlobalKnownHostsFile", multi()),
        "ignoreunknown" => ("IgnoreUnknown", multi()),
        "localforward" => ("LocalForward", multi()),
        "permitremoteopen" => ("PermitRemoteOpen", multi()),
        "remoteforward" => ("RemoteForward", multi()),
        "sendenv" => ("SendEnv", multi()),
        "setenv" => ("SetEnv", multi()),
        "tag" => ("Tag", multi()),
        "userknownhostsfile" => ("UserKnownHostsFile", multi()),

        _ => return None,
    };
    Some((name, class))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(input: &str) -> Vec<String> {
        tokenize(input).expect("tokenizes")
    }

    #[test]
    fn splits_keyword_and_value_forms() {
        assert_eq!(
            split_keyword(&words("Host web")).expect("keyword"),
            ("Host".to_string(), vec!["web".to_string()])
        );
        assert_eq!(
            split_keyword(&words("Host=web")).expect("keyword"),
            ("Host".to_string(), vec!["web".to_string()])
        );
        assert_eq!(
            split_keyword(&words("Port = 2222")).expect("keyword"),
            ("Port".to_string(), vec!["2222".to_string()])
        );
        assert_eq!(
            split_keyword(&words("HostName=web.example.com")).expect("keyword"),
            ("HostName".to_string(), vec!["web.example.com".to_string()])
        );
    }

    #[test]
    fn quotes_group_whitespace_and_concatenate() {
        assert_eq!(
            words(r#"IdentityFile "/path with space/key""#),
            vec!["IdentityFile", "/path with space/key"]
        );
        assert_eq!(words(r#""/a b"/c"#), vec!["/a b/c"]);
        assert_eq!(words("SetEnv FOO=\"a b\""), vec!["SetEnv", "FOO=a b"]);
    }

    #[test]
    fn backslash_escapes_only_quotes_and_backslashes() {
        assert_eq!(
            words(r#"LocalCommand echo "say \"hi\"""#),
            vec!["LocalCommand", "echo", r#"say "hi""#]
        );
        assert_eq!(words(r#"X "a\\b""#), vec!["X", r"a\b"]);
        assert_eq!(words(r#"X "a\nb""#), vec!["X", r"a\nb"]);
    }

    #[test]
    fn hash_starts_a_comment_outside_quotes() {
        assert_eq!(words("Host web # comment"), vec!["Host", "web"]);
        assert_eq!(words("# whole line"), Vec::<String>::new());
        assert_eq!(words("Port 22#tail"), vec!["Port", "22"]);
        assert_eq!(words(r#"X "a#b""#), vec!["X", "a#b"]);
    }

    #[test]
    fn rejects_unterminated_quote() {
        assert!(tokenize("Host \"web").is_err());
        assert!(tokenize("Host \"web\\").is_err());
    }

    #[test]
    fn keyword_classification_is_case_insensitive() {
        let (name, class) = classify("hostname").expect("known");
        assert_eq!(name, "HostName");
        assert!(matches!(class, Class::HostName));
        let (name, _) = classify("sendenv").expect("known");
        assert_eq!(name, "SendEnv");
        assert!(classify("frobnicate").is_none());
    }
}
