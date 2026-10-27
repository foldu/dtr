//! End-to-end tests for parsing, resolution and include expansion.

use std::fs;
use std::path::{Path, PathBuf};

use dtr_sshconfig::{Config, Error, parse_file, parse_str};
use tempfile::TempDir;

fn config(input: &str) -> Config {
    parse_str(input, Path::new(".")).expect("configuration parses")
}

fn write(dir: &Path, name: &str, content: &str) -> PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directory");
    }
    fs::write(&path, content).expect("write file");
    path
}

fn tempdir() -> TempDir {
    tempfile::tempdir().expect("temporary directory")
}

#[test]
fn single_host_block_resolves_every_scalar() {
    let config = config(
        "\
Host web
    HostName web.example.com
    User alice
    Port 2222
    IdentityFile ~/.ssh/id_web
    ProxyJump bastion
",
    );

    let hosts = config.hosts();
    assert_eq!(hosts.len(), 1);
    assert_eq!(hosts[0].patterns, ["web"]);
    assert_eq!(hosts[0].line, 1);
    assert_eq!(hosts[0].file, PathBuf::from("<input>"));

    let resolved = config.resolve("web");
    assert_eq!(resolved.alias, "web");
    assert_eq!(resolved.hostname.as_deref(), Some("web.example.com"));
    assert_eq!(resolved.user.as_deref(), Some("alice"));
    assert_eq!(resolved.port, Some(2222));
    assert_eq!(
        resolved.identity_files,
        vec![PathBuf::from("~/.ssh/id_web")]
    );
    assert_eq!(resolved.proxy_jump.as_deref(), Some("bastion"));
    assert_eq!(resolved.effective_hostname(), "web.example.com");
    assert_eq!(resolved.effective_port(), 2222);
    assert!(resolved.options.is_empty());
}

#[test]
fn multiple_aliases_share_one_block() {
    let config = config("Host web db cache\n    User ops\n");

    assert_eq!(config.hosts()[0].patterns, ["web", "db", "cache"]);
    assert_eq!(config.hosts()[0].aliases(), ["web", "db", "cache"]);
    for alias in ["web", "db", "cache"] {
        assert_eq!(config.resolve(alias).user.as_deref(), Some("ops"));
    }
    assert_eq!(config.resolve("mail").user, None);
}

#[test]
fn wildcard_and_question_mark_patterns_match() {
    let config = config(
        "\
Host web*
    User webuser
Host db?
    User dbuser
Host *.internal
    User internal
",
    );

    assert_eq!(config.resolve("web1").user.as_deref(), Some("webuser"));
    assert_eq!(config.resolve("web").user.as_deref(), Some("webuser"));
    assert_eq!(config.resolve("WEB1").user.as_deref(), Some("webuser"));
    assert_eq!(config.resolve("db7").user.as_deref(), Some("dbuser"));
    assert_eq!(config.resolve("db77").user, None);
    assert_eq!(config.resolve("db").user, None);
    assert_eq!(
        config.resolve("box.internal").user.as_deref(),
        Some("internal")
    );
    assert_eq!(config.resolve("internal").user, None);
}

#[test]
fn negated_patterns_veto_a_match() {
    let negated = config("Host web* !web-prod\n    User shared\n");

    assert_eq!(negated.resolve("web-dev").user.as_deref(), Some("shared"));
    assert_eq!(negated.resolve("web-prod").user, None);

    let only_negative = config("Host !web\n    User nobody\n");
    assert_eq!(only_negative.resolve("web").user, None);
    assert_eq!(only_negative.resolve("other").user, None);
}

#[test]
fn first_value_wins_when_the_specific_block_comes_first() {
    let config = config(
        "\
Host web
    User specific
    Port 2200
Host *
    User default
    Port 22
",
    );

    let resolved = config.resolve("web");
    assert_eq!(resolved.user.as_deref(), Some("specific"));
    assert_eq!(resolved.port, Some(2200));
}

#[test]
fn first_value_wins_when_the_wildcard_block_comes_first() {
    // `Host *` is positional, not special-cased: because it is obtained first,
    // it wins even against a later, more specific block.
    let config = config(
        "\
Host *
    User default
    Port 22
Host web
    User specific
    Port 2200
",
    );

    let resolved = config.resolve("web");
    assert_eq!(resolved.user.as_deref(), Some("default"));
    assert_eq!(resolved.port, Some(22));
}

#[test]
fn later_defaults_fill_keys_the_specific_block_did_not_set() {
    let config = config(
        "\
Host web
    User specific
Host *
    User default
    Port 2222
    IdentityFile ~/.ssh/default
",
    );

    let resolved = config.resolve("web");
    assert_eq!(resolved.user.as_deref(), Some("specific"));
    assert_eq!(resolved.port, Some(2222));
    assert_eq!(
        resolved.identity_files,
        vec![PathBuf::from("~/.ssh/default")]
    );
}

#[test]
fn first_value_wins_within_a_single_block() {
    let config = config("Host web\n    User first\n    User second\n");
    assert_eq!(config.resolve("web").user.as_deref(), Some("first"));
}

#[test]
fn single_valued_options_are_first_wins() {
    let config = config(
        "\
Host web
    Compression yes
Host *
    Compression no
",
    );
    assert_eq!(config.resolve("web").options["Compression"], ["yes"]);
}

#[test]
fn identity_files_accumulate_in_file_order() {
    let config = config(
        "\
Host web
    IdentityFile ~/.ssh/one
Host *
    IdentityFile ~/.ssh/two
    IdentityFile \"~/.ssh/three with space\"
",
    );

    assert_eq!(
        config.resolve("web").identity_files,
        vec![
            PathBuf::from("~/.ssh/one"),
            PathBuf::from("~/.ssh/two"),
            PathBuf::from("~/.ssh/three with space"),
        ]
    );
}

#[test]
fn keyword_value_forms_are_equivalent() {
    let config = config(
        "\
Host=web
    HostName=web.example.com
    user = alice
    Port=2222
",
    );

    assert_eq!(config.hosts()[0].patterns, ["web"]);
    let resolved = config.resolve("web");
    assert_eq!(resolved.hostname.as_deref(), Some("web.example.com"));
    assert_eq!(resolved.user.as_deref(), Some("alice"));
    assert_eq!(resolved.port, Some(2222));
}

#[test]
fn quoted_values_preserve_spaces_and_escapes() {
    let config = config(
        r#"Host web
    IdentityFile "/path with space/id key"
    LocalCommand echo "hello \"world\""
    SetEnv FOO="a b"
    IdentityFile "/hash#kept"
"#,
    );

    let resolved = config.resolve("web");
    assert_eq!(
        resolved.identity_files,
        vec![
            PathBuf::from("/path with space/id key"),
            PathBuf::from("/hash#kept"),
        ]
    );
    assert_eq!(resolved.options["LocalCommand"], ["echo hello \"world\""]);
    assert_eq!(resolved.options["SetEnv"], ["FOO=a b"]);
}

#[test]
fn comments_and_blank_lines_are_ignored() {
    let config = config(
        "\
# leading comment

Host web   # trailing comment
    User alice
    # an indented comment
    Port 22
",
    );

    let resolved = config.resolve("web");
    assert_eq!(resolved.user.as_deref(), Some("alice"));
    assert_eq!(resolved.port, Some(22));
}

#[test]
fn hash_terminates_an_unquoted_token() {
    let config = config("Host web\n    IdentityFile /keys/id#backup\n");
    assert_eq!(
        config.resolve("web").identity_files,
        vec![PathBuf::from("/keys/id")]
    );
}

#[test]
fn include_glob_expands_in_place() {
    let dir = tempdir();
    // Written out of order to prove the expansion is sorted.
    write(
        dir.path(),
        "conf.d/20-two.conf",
        "Host two\n    User two-user\n",
    );
    write(
        dir.path(),
        "conf.d/10-one.conf",
        "Host one\n    User one-user\n",
    );
    let main = write(
        dir.path(),
        "config",
        "\
Host alpha
    User alpha-user
Include conf.d/*.conf
Host omega
    User omega-user
",
    );

    let config = parse_file(&main).expect("configuration parses");
    let hosts = config.hosts();
    let patterns: Vec<&str> = hosts.iter().map(|host| host.patterns[0].as_str()).collect();
    assert_eq!(patterns, ["alpha", "one", "two", "omega"]);
    assert_eq!(config.resolve("one").user.as_deref(), Some("one-user"));
    assert_eq!(config.resolve("omega").user.as_deref(), Some("omega-user"));

    let included = &hosts[1];
    assert_eq!(included.line, 1);
    assert!(
        included.file.ends_with("conf.d/10-one.conf"),
        "unexpected file: {}",
        included.file.display()
    );
}

#[test]
fn nested_includes_apply_in_place() {
    let dir = tempdir();
    write(dir.path(), "a.conf", "Include b.conf\n");
    write(dir.path(), "b.conf", "Host deep\n    User deepest\n");
    let main = write(
        dir.path(),
        "config",
        "Host top\n    User top-user\nInclude a.conf\n",
    );

    let config = parse_file(&main).expect("configuration parses");
    assert_eq!(config.resolve("deep").user.as_deref(), Some("deepest"));
    assert_eq!(config.resolve("top").user.as_deref(), Some("top-user"));
}

#[test]
fn parse_str_resolves_relative_includes_against_base_dir() {
    let dir = tempdir();
    write(dir.path(), "inc.conf", "Host included\n    User inc-user\n");

    let config = parse_str("Include inc.conf\n", dir.path()).expect("configuration parses");
    assert_eq!(config.resolve("included").user.as_deref(), Some("inc-user"));
}

#[test]
fn absolute_include_paths_work() {
    let dir = tempdir();
    let extra = write(
        dir.path(),
        "extra.conf",
        "Host absolute\n    User abs-user\n",
    );
    let main = write(
        dir.path(),
        "config",
        &format!("Include \"{}\"\n", extra.display()),
    );

    let config = parse_file(&main).expect("configuration parses");
    assert_eq!(config.resolve("absolute").user.as_deref(), Some("abs-user"));
}

#[test]
fn include_inside_host_block_applies_to_that_host() {
    let dir = tempdir();
    write(
        dir.path(),
        "extra.conf",
        "Port 2222\nIdentityFile ~/.ssh/extra\n",
    );
    let main = write(
        dir.path(),
        "config",
        "\
Host web
    User web-user
    Include extra.conf
",
    );

    let config = parse_file(&main).expect("configuration parses");
    let resolved = config.resolve("web");
    assert_eq!(resolved.user.as_deref(), Some("web-user"));
    assert_eq!(resolved.port, Some(2222));
    assert_eq!(resolved.identity_files, vec![PathBuf::from("~/.ssh/extra")]);
    assert_eq!(config.resolve("other").port, None);
}

#[test]
fn include_inside_host_block_resumes_the_enclosing_block() {
    let dir = tempdir();
    write(
        dir.path(),
        "extra.conf",
        "Host other\n    User other-user\n",
    );
    let main = write(
        dir.path(),
        "config",
        "\
Host web
    User web-before
    Include extra.conf
    Port 2222
",
    );

    let config = parse_file(&main).expect("configuration parses");
    let resolved = config.resolve("web");
    assert_eq!(resolved.user.as_deref(), Some("web-before"));
    assert_eq!(resolved.port, Some(2222));
    assert_eq!(config.resolve("other").user.as_deref(), Some("other-user"));
    // `Port` after the include still belongs to `web`, not to `other`.
    assert_eq!(config.resolve("other").port, None);
}

#[test]
fn missing_non_glob_include_is_an_error() {
    let dir = tempdir();
    let main = write(
        dir.path(),
        "config",
        "Host web\n    User alice\nInclude missing.conf\n",
    );

    let error = parse_file(&main).expect_err("missing include fails");
    match &error {
        Error::MissingInclude { path, line, .. } => {
            assert!(path.ends_with("missing.conf"));
            assert_eq!(*line, 3);
        }
        other => panic!("unexpected error: {other:?}"),
    }
    let message = error.to_string();
    assert!(message.contains("missing.conf"), "{message}");
    assert!(message.contains(":3:"), "{message}");
}

#[test]
fn glob_matching_nothing_is_not_an_error() {
    let dir = tempdir();
    let main = write(
        dir.path(),
        "config",
        "Host web\n    User alice\nInclude conf.d/*.conf\n",
    );

    let config = parse_file(&main).expect("configuration parses");
    assert_eq!(config.resolve("web").user.as_deref(), Some("alice"));
}

#[test]
fn include_cycle_is_detected() {
    let dir = tempdir();
    write(dir.path(), "a.conf", "Include b.conf\n");
    write(dir.path(), "b.conf", "Include a.conf\n");

    let error = parse_file(&dir.path().join("a.conf")).expect_err("cycle fails");
    assert!(
        matches!(error, Error::IncludeCycle { .. }),
        "unexpected error: {error:?}"
    );
}

/// Write `f1.conf` .. `f<count>.conf`, each including the next.
fn write_include_chain(dir: &Path, count: usize) -> PathBuf {
    for index in 1..=count {
        let content = if index == count {
            "Host deep\n    User deepest\n".to_string()
        } else {
            format!("Include f{}.conf\n", index + 1)
        };
        write(dir, &format!("f{index}.conf"), &content);
    }
    dir.join("f1.conf")
}

#[test]
fn include_depth_is_capped() {
    let allowed = tempdir();
    let root = write_include_chain(allowed.path(), 17);
    let config = parse_file(&root).expect("17 levels are within the cap");
    assert_eq!(config.resolve("deep").user.as_deref(), Some("deepest"));

    let too_deep = tempdir();
    let root = write_include_chain(too_deep.path(), 18);
    let error = parse_file(&root).expect_err("18 levels exceed the cap");
    assert!(
        matches!(error, Error::IncludeDepth { .. }),
        "unexpected error: {error:?}"
    );
}

#[test]
fn match_all_applies_and_other_match_criteria_do_not() {
    let config = config(
        "\
Match all
    SendEnv LANG
Match host web
    User never
Host web
    User web-user
",
    );

    let resolved = config.resolve("web");
    assert_eq!(resolved.user.as_deref(), Some("web-user"));
    assert_eq!(resolved.options["SendEnv"], ["LANG"]);
    assert_eq!(config.resolve("elsewhere").options["SendEnv"], ["LANG"]);

    let unapplied = config.unapplied_match_blocks();
    assert_eq!(unapplied.len(), 1);
    assert_eq!(unapplied[0].criteria, ["host", "web"]);
    assert_eq!(unapplied[0].line, 3);
    assert_eq!(unapplied[0].file, PathBuf::from("<input>"));
}

#[test]
fn match_all_ends_the_previous_host_context() {
    let config = config(
        "\
Host web
    User web-user
Match all
    Compression yes
Host db
    User db-user
",
    );

    assert_eq!(config.resolve("db").user.as_deref(), Some("db-user"));
    assert_eq!(config.resolve("db").options["Compression"], ["yes"]);
    assert_eq!(config.resolve("web").options["Compression"], ["yes"]);
}

#[test]
fn unknown_directives_are_preserved() {
    let config = config(
        "\
Host web
    sendenv LANG
    SendEnv LC_*
    LocalForward 8080 localhost:80
    Frobnicate yes
",
    );

    let resolved = config.resolve("web");
    assert_eq!(resolved.options["SendEnv"], ["LANG", "LC_*"]);
    assert_eq!(resolved.options["LocalForward"], ["8080 localhost:80"]);
    assert_eq!(resolved.options["Frobnicate"], ["yes"]);
}

#[test]
fn unknown_directives_keep_the_first_value() {
    let config = config(
        "\
Host web
    Frobnicate first
Host *
    Frobnicate second
",
    );
    assert_eq!(config.resolve("web").options["Frobnicate"], ["first"]);
}

#[test]
fn hosts_before_the_first_block_apply_globally() {
    let config = config(
        "\
User global-user
Compression yes
Host web
    Port 2200
",
    );

    assert_eq!(config.hosts().len(), 1);
    let web = config.resolve("web");
    assert_eq!(web.user.as_deref(), Some("global-user"));
    assert_eq!(web.port, Some(2200));
    assert_eq!(
        config.resolve("unlisted").user.as_deref(),
        Some("global-user")
    );
    assert_eq!(config.hosts()[0].line, 3);
}

#[test]
fn invalid_port_carries_file_and_line() {
    for value in ["0", "70000", "ssh"] {
        let input = format!("Host web\n    User alice\n    Port {value}\n");
        let error = parse_str(&input, Path::new(".")).expect_err("invalid port fails");
        match &error {
            Error::InvalidPort {
                file,
                line,
                value: offending,
            } => {
                assert_eq!(file, &PathBuf::from("<input>"));
                assert_eq!(*line, 3);
                assert_eq!(offending, value);
            }
            other => panic!("unexpected error: {other:?}"),
        }
        let message = error.to_string();
        assert!(message.contains("<input>:3"), "{message}");
        assert!(message.contains(value), "{message}");
    }
}

#[test]
fn invalid_port_in_an_included_file_reports_that_file() {
    let dir = tempdir();
    write(dir.path(), "extra.conf", "Port 99999\n");
    let main = write(dir.path(), "config", "Host web\nInclude extra.conf\n");

    let error = parse_file(&main).expect_err("invalid port fails");
    let message = error.to_string();
    assert!(
        message.contains("extra.conf:1"),
        "unexpected message: {message}"
    );
}

#[test]
fn host_entry_aliases_skip_patterns() {
    let config = config("Host web db? !web-prod *.internal\n    User ops\n");
    assert_eq!(config.hosts()[0].aliases(), ["web"]);
}

#[test]
fn empty_configuration_resolves_to_the_alias() {
    let config = config("\n# nothing here\n");
    assert!(config.hosts().is_empty());

    let resolved = config.resolve("web");
    assert_eq!(resolved.alias, "web");
    assert_eq!(resolved.effective_hostname(), "web");
    assert_eq!(resolved.effective_port(), 22);
    assert!(resolved.options.is_empty());
}

#[test]
fn missing_config_file_is_an_io_error() {
    let error = parse_file(Path::new("/nonexistent/dtr-sshconfig-test-config"))
        .expect_err("missing file fails");
    assert!(
        matches!(error, Error::Io { .. }),
        "unexpected error: {error:?}"
    );
}

#[test]
fn unterminated_quote_is_a_syntax_error_with_location() {
    let error = parse_str("Host web\n    IdentityFile \"/no end\n", Path::new("."))
        .expect_err("unterminated quote fails");
    match &error {
        Error::Syntax { line, message, .. } => {
            assert_eq!(*line, 2);
            assert!(message.contains("unterminated"), "{message}");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn tilde_user_expansion_is_rejected() {
    let error = parse_str("Include ~someone/config\n", Path::new("."))
        .expect_err("~user expansion is unsupported");
    assert!(
        matches!(error, Error::Syntax { .. }),
        "unexpected error: {error:?}"
    );
    assert!(error.to_string().contains("~user"), "{error}");
}
