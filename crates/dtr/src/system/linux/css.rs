//! What the user's `gtk.css` says, resolved the way GTK resolves it.
//!
//! GTK loads exactly one user stylesheet — `$XDG_CONFIG_HOME/gtk-4.0/gtk.css`,
//! at `GTK_STYLE_PROVIDER_PRIORITY_USER`, above libadwaita's own sheet and above
//! the accent libadwaita injects at theme priority (`adw-style-manager.c`). So a
//! customised GNOME is a file, and the colours a libadwaita window shows are that
//! file's definitions resolved through the provider cascade, not the library's
//! constants. This reads the same chain GTK does: the file, its `@import`s in
//! document order, and the `@media (prefers-color-scheme: …)` blocks the decided
//! scheme selects.
//!
//! The syntax is `cssparser`'s (the tokenizer Servo ships), so comments, strings,
//! escapes, urls and numbers are not this file's problem. What is here is the
//! GTK-specific grammar — `@define-color`, `@import`, `@media`, `:root { --x }` —
//! and the one policy only dtr can hold: a token it cannot resolve keeps the
//! measured constant and says so in `-v`, because a half-resolved palette is a
//! third look that matches neither the desktop nor dtr.
//!
//! `docs/gtk-palette.md` §8 has the account: why the base stays vendored rather
//! than parsed, and why `@define-color` and `:root` variables are both read.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use cssparser::{
    AtRuleParser, CowRcStr, DeclarationParser, ParseError, Parser, ParserState,
    QualifiedRuleParser, RuleBodyItemParser, RuleBodyParser, StyleSheetParser, Token,
};
use dtr_widgets::Overrides;
use egui::Color32;

/// How deep an `@import` chain is followed. GTK has no limit; a chain this deep
/// is a cycle or a mistake, and the cap is what keeps one file's read bounded.
const MAX_DEPTH: usize = 8;

/// How many dropped tokens `-v` will quote, so a broken file cannot flood the log.
const MAX_DROPPED: usize = 8;

/// One rule of a stylesheet, in the order the file writes it.
#[derive(Debug)]
enum Item {
    Define { name: String, value: String },
    Import(String),
    Media { query: String, items: Vec<Item> },
    Root(Vec<(String, String)>),
}

/// What the chain said, in the shape the caller needs.
pub(super) struct Report {
    pub overrides: Overrides,
    /// Colour tokens the file set and dtr applied.
    pub applied: usize,
    /// Tokens the file set that could not be resolved, quoted for `-v`.
    pub dropped: Vec<String>,
    /// Imports and parse errors worth a log line, quoted for `-v`.
    pub notes: Vec<String>,
    /// Stylesheets read, including imports.
    pub files: usize,
}

impl Default for Report {
    fn default() -> Self {
        Self {
            overrides: Overrides::default(),
            applied: 0,
            dropped: Vec::new(),
            notes: Vec::new(),
            files: 0,
        }
    }
}

impl Report {
    /// Whether the chain repainted anything; `-v` uses it to tell a session with
    /// a `gtk.css` from one without.
    pub fn is_empty(&self) -> bool {
        self.overrides.is_empty()
    }
}

/// Read the session's user stylesheet, if it has one.
pub(super) fn read(dark: bool) -> Report {
    match user_sheet() {
        Some(path) => read_at(&path, dark),
        None => Report::default(),
    }
}

/// `$XDG_CONFIG_HOME/gtk-4.0/gtk.css`, the one file GTK calls the user theme.
fn user_sheet() -> Option<PathBuf> {
    let path = dirs::config_dir()?.join("gtk-4.0").join("gtk.css");
    path.is_file().then_some(path)
}

fn read_at(path: &Path, dark: bool) -> Report {
    let mut state = State::default();
    let mut seen = Vec::new();
    load(path, dark, &mut state, 0, &mut seen);
    state.into_report()
}

/// Everything the chain accumulated. Definitions and variables are kept as the
/// source text GTK wrote, because a value may be a reference to another name and
/// resolving early would make document order matter when it does not.
#[derive(Default)]
struct State {
    defs: HashMap<String, String>,
    vars: HashMap<String, String>,
    notes: Vec<String>,
    files: usize,
}

impl State {
    fn into_report(self) -> Report {
        let mut dropped = Vec::new();
        let overrides = Overrides {
            window: take(&self, &["window_bg_color"], &mut dropped),
            // A raised container, not the dialog: GNOME themes routinely make
            // `dialog_bg_color` the window's own colour, which would flatten
            // every card dtr draws, while `popover_bg_color` stays distinct.
            card: take(
                &self,
                &["popover_bg_color", "dialog_bg_color", "card_bg_color"],
                &mut dropped,
            ),
            well: take(&self, &["view_bg_color"], &mut dropped),
            headerbar: take(&self, &["headerbar_bg_color"], &mut dropped),
            text: take(&self, &["window_fg_color"], &mut dropped),
            accent: take(&self, &["accent_bg_color"], &mut dropped),
            on_accent: take(&self, &["accent_fg_color"], &mut dropped),
            danger: take(&self, &["destructive_bg_color"], &mut dropped),
            on_danger: take(&self, &["destructive_fg_color"], &mut dropped),
        };
        let applied = [
            overrides.window,
            overrides.card,
            overrides.well,
            overrides.headerbar,
            overrides.text,
            overrides.accent,
            overrides.on_accent,
            overrides.danger,
            overrides.on_danger,
        ]
        .iter()
        .filter(|colour| colour.is_some())
        .count();
        dropped.truncate(MAX_DROPPED);
        Report {
            overrides,
            applied,
            dropped,
            notes: self.notes,
            files: self.files,
        }
    }
}

/// The first name that has a definition, resolved. A `:root` variable of the same
/// token wins over the named colour: it is the spelling libadwaita documents and
/// the one GTK's own deprecation points at, so a file that writes both means the
/// variable.
fn take(state: &State, names: &[&str], dropped: &mut Vec<String>) -> Option<Color32> {
    for name in names {
        let variable = name.replace('_', "-");
        let Some(raw) = state.vars.get(&variable).or_else(|| state.defs.get(*name)) else {
            continue;
        };
        match colour(raw, &state.defs, &state.vars, 0) {
            Some(colour) => return Some(colour),
            None => dropped.push(format!("{name}={raw}")),
        }
    }
    None
}

/// A colour value, following `@name` and `var(--name)` references.
///
/// `csscolorparser` is the value grammar: `#rrggbb`, `rgb()`/`rgba()` in both
/// comma and space/slash form (GTK's own sheet writes `RGB(0 0 6 / 80%)`),
/// `hsl()`, named colours, and the wide-gamut functions. Anything else — a
/// `color-mix()`, an `oklab(from …)`, a percentage — is not a colour this reader
/// will guess at, so the token is dropped and the constant stays.
fn colour(
    raw: &str,
    defs: &HashMap<String, String>,
    vars: &HashMap<String, String>,
    depth: usize,
) -> Option<Color32> {
    if depth > MAX_DEPTH {
        return None;
    }
    let value = raw.trim();
    if let Some(name) = value.strip_prefix('@') {
        let next = defs.get(name.trim())?;
        return colour(next, defs, vars, depth + 1);
    }
    if let Some(inner) = value
        .strip_prefix("var(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let name = inner.trim().trim_start_matches("--");
        let next = vars.get(name)?;
        return colour(next, defs, vars, depth + 1);
    }
    let colour = csscolorparser::Color::from_str(value).ok()?;
    let [r, g, b, a] = colour.to_rgba8();
    Some(Color32::from_rgba_unmultiplied(r, g, b, a))
}

/// Read one stylesheet and apply it to `state`, following its imports at the
/// point they appear (which is what GTK does: a later definition in the importing
/// file wins over an imported one).
fn load(path: &Path, dark: bool, state: &mut State, depth: usize, seen: &mut Vec<(u64, u64)>) {
    if depth > MAX_DEPTH {
        state.notes.push(format!(
            "@import deeper than {MAX_DEPTH}: {}",
            path.display()
        ));
        return;
    }
    let Ok(meta) = std::fs::metadata(path) else {
        state
            .notes
            .push(format!("no such file: {}", path.display()));
        return;
    };
    #[cfg(unix)]
    let id = {
        use std::os::unix::fs::MetadataExt;
        (meta.dev(), meta.ino())
    };
    #[cfg(not(unix))]
    let id = (0, 0);
    if seen.contains(&id) {
        state
            .notes
            .push(format!("@import cycle: {}", path.display()));
        return;
    }
    let Ok(css) = std::fs::read_to_string(path) else {
        state
            .notes
            .push(format!("not valid UTF-8: {}", path.display()));
        return;
    };
    seen.push(id);
    state.files += 1;
    let (items, errors) = parse(&css);
    if errors > 0 {
        state
            .notes
            .push(format!("{errors} parse error(s) in {}", path.display()));
    }
    apply(
        &items,
        path.parent().unwrap_or(Path::new(".")),
        dark,
        state,
        depth,
        seen,
    );
    seen.pop();
}

fn apply(
    items: &[Item],
    base: &Path,
    dark: bool,
    state: &mut State,
    depth: usize,
    seen: &mut Vec<(u64, u64)>,
) {
    for item in items {
        match item {
            Item::Define { name, value } => {
                state.defs.insert(name.clone(), value.clone());
            }
            Item::Root(vars) => {
                for (name, value) in vars {
                    state.vars.insert(name.clone(), value.clone());
                }
            }
            Item::Import(url) => match import_path(base, url) {
                Some(path) => load(&path, dark, state, depth + 1, seen),
                None => state.notes.push(format!("ignored @import {url}")),
            },
            Item::Media { query, items } => {
                if media_matches(query, dark) {
                    apply(items, base, dark, state, depth, seen);
                }
            }
        }
    }
}

/// Where an `@import` points: a relative name against the importing file's own
/// directory, or an absolute `file:` path. Anything else is not a file we can
/// read — `resource://` is libadwaita's gresource, which the constants are.
///
/// The path is *not* canonicalised: GTK resolves a relative import against the
/// stylesheet path it was given, and `~/.config/gtk-4.0/gtk.css` is usually a
/// symlink into the store, so canonicalising first would look for the import
/// beside the store copy instead of beside the symlink.
fn import_path(base: &Path, url: &str) -> Option<PathBuf> {
    let url = url.trim();
    if let Some(rest) = url.strip_prefix("file://") {
        if !rest.starts_with('/') {
            return None;
        }
        return Some(PathBuf::from(percent_decode(rest)?));
    }
    if url.contains("://") {
        return None;
    }
    Some(base.join(percent_decode(url)?))
}

/// `%20`-style escapes in a `file:` url, which is how a path with a space is
/// written. Not a general URI decoder: only what a stylesheet's `@import` needs.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            let hex = std::str::from_utf8(hex).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Whether a media block applies to the scheme dtr decided.
///
/// `prefers-color-scheme` is the one feature GTK's own sheet and the themes that
/// customise it use. `prefers-contrast` and the motion features are deliberately
/// not evaluated: dtr has no high-contrast style, so applying that block's
/// colours would claim an accessibility mode the rest of the chrome does not
/// follow. Any other feature (`width`, `resolution`) is not knowable here.
fn media_matches(query: &str, dark: bool) -> bool {
    let query = query.to_ascii_lowercase();
    if query.contains("prefers-contrast")
        || query.contains("prefers-reduced-motion")
        || query.contains("prefers-reduced-transparency")
    {
        return false;
    }
    if let Some((_, rest)) = query.split_once("prefers-color-scheme") {
        if rest.contains("dark") {
            return dark;
        }
        if rest.contains("light") {
            return !dark;
        }
        return false;
    }
    !query.contains('(')
}

/// The stylesheet, as rules in document order.
fn parse(css: &str) -> (Vec<Item>, usize) {
    let mut parser = Parser::new(css);
    let mut sheet = Sheet;
    let mut items = Vec::new();
    let mut errors = 0;
    {
        let mut rules = StyleSheetParser::new(&mut parser, &mut sheet);
        for rule in &mut rules {
            match rule {
                Ok(Some(item)) => items.push(item),
                Ok(None) => {}
                Err(_) => errors += 1,
            }
        }
    }
    (items, errors)
}

// The syntax is `cssparser`'s; these four types are the GTK grammar on top of it.

/// The rule-list parser's own state; the items are the iterator's output, not
/// this type's, so it holds nothing.
struct Sheet;

enum Prelude {
    Define(String, String),
    Import(String),
    Media(String),
    Other,
}

impl<'i> AtRuleParser<'i> for Sheet {
    type Prelude = Prelude;
    type AtRule = Option<Item>;
    type Error = ();

    fn parse_prelude(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i>,
    ) -> Result<Prelude, ParseError<()>> {
        if name.eq_ignore_ascii_case("define-color") {
            let name = input.expect_ident_cloned()?.to_string();
            Ok(Prelude::Define(name, rest(input)))
        } else if name.eq_ignore_ascii_case("import") {
            Ok(Prelude::Import(input.expect_url_or_string()?.to_string()))
        } else if name.eq_ignore_ascii_case("media") {
            Ok(Prelude::Media(rest(input)))
        } else {
            // The prelude still has to be consumed, or the leftover tokens read
            // as a parse error and the rule is lost rather than skipped.
            let _ = rest(input);
            Ok(Prelude::Other)
        }
    }

    fn rule_without_block(
        &mut self,
        prelude: Prelude,
        _start: &ParserState,
    ) -> Result<Option<Item>, ()> {
        Ok(match prelude {
            Prelude::Define(name, value) => Some(Item::Define { name, value }),
            Prelude::Import(url) => Some(Item::Import(url)),
            _ => None,
        })
    }

    fn parse_block(
        &mut self,
        prelude: Prelude,
        _start: &ParserState,
        input: &mut Parser<'i>,
    ) -> Result<Option<Item>, ParseError<()>> {
        match prelude {
            Prelude::Media(query) => {
                let mut nested = Sheet;
                let mut items = Vec::new();
                {
                    let mut body = RuleBodyParser::new(input, &mut nested);
                    for rule in &mut body {
                        if let Some(item) = rule.map_err(|error| error.0)? {
                            items.push(item);
                        }
                    }
                }
                Ok(Some(Item::Media { query, items }))
            }
            // `@keyframes`, `@font-face`, `@supports`: no colour dtr maps comes
            // from one, and the block still has to be parsed structurally or a
            // nested `{}` reads as a stray token and the rest of the file is
            // lost.
            _ => {
                skip(input)?;
                Ok(None)
            }
        }
    }
}

impl<'i> QualifiedRuleParser<'i> for Sheet {
    type Prelude = String;
    type QualifiedRule = Option<Item>;
    type Error = ();

    fn parse_prelude(&mut self, input: &mut Parser<'i>) -> Result<String, ParseError<()>> {
        Ok(rest(input))
    }

    fn parse_block(
        &mut self,
        prelude: String,
        _start: &ParserState,
        input: &mut Parser<'i>,
    ) -> Result<Option<Item>, ParseError<()>> {
        if prelude.trim() != ":root" {
            skip(input)?;
            return Ok(None);
        }
        let mut vars = Vec::new();
        {
            let mut declarations = Decls;
            let mut body = RuleBodyParser::new(input, &mut declarations);
            for declaration in &mut body {
                let (name, value) = declaration.map_err(|error| error.0)?;
                if !name.is_empty() {
                    vars.push((name, value));
                }
            }
        }
        Ok(Some(Item::Root(vars)))
    }
}

impl<'i> DeclarationParser<'i> for Sheet {
    type Declaration = Option<Item>;
    type Error = ();

    fn parse_value(
        &mut self,
        _name: CowRcStr<'i>,
        input: &mut Parser<'i>,
        _start: &ParserState,
    ) -> Result<Option<Item>, ParseError<()>> {
        skip(input)?;
        Ok(None)
    }
}

impl<'i> RuleBodyItemParser<'i, Option<Item>, ()> for Sheet {
    fn parse_declarations(&self) -> bool {
        false
    }

    fn parse_qualified(&self) -> bool {
        true
    }
}

/// The declarations of a `:root` block. Only the custom properties are kept;
/// every other declaration is consumed so the next one still parses.
struct Decls;

impl<'i> DeclarationParser<'i> for Decls {
    type Declaration = (String, String);
    type Error = ();

    fn parse_value(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i>,
        _start: &ParserState,
    ) -> Result<(String, String), ParseError<()>> {
        if let Some(variable) = name.strip_prefix("--") {
            return Ok((variable.to_string(), rest(input)));
        }
        skip(input)?;
        Ok((String::new(), String::new()))
    }
}

impl<'i> QualifiedRuleParser<'i> for Decls {
    type Prelude = ();
    type QualifiedRule = (String, String);
    type Error = ();

    fn parse_prelude(&mut self, input: &mut Parser<'i>) -> Result<(), ParseError<()>> {
        skip(input)?;
        Ok(())
    }

    fn parse_block(
        &mut self,
        _prelude: (),
        _start: &ParserState,
        input: &mut Parser<'i>,
    ) -> Result<(String, String), ParseError<()>> {
        skip(input)?;
        Ok((String::new(), String::new()))
    }
}

impl<'i> AtRuleParser<'i> for Decls {
    type Prelude = ();
    type AtRule = (String, String);
    type Error = ();

    fn parse_prelude(
        &mut self,
        _name: CowRcStr<'i>,
        input: &mut Parser<'i>,
    ) -> Result<(), ParseError<()>> {
        skip(input)?;
        Ok(())
    }

    fn rule_without_block(
        &mut self,
        _prelude: (),
        _start: &ParserState,
    ) -> Result<(String, String), ()> {
        Ok((String::new(), String::new()))
    }

    fn parse_block(
        &mut self,
        _prelude: (),
        _start: &ParserState,
        input: &mut Parser<'i>,
    ) -> Result<(String, String), ParseError<()>> {
        skip(input)?;
        Ok((String::new(), String::new()))
    }
}

impl<'i> RuleBodyItemParser<'i, (String, String), ()> for Decls {
    fn parse_declarations(&self) -> bool {
        true
    }

    fn parse_qualified(&self) -> bool {
        false
    }
}

/// The source text of the rest of the given parser, which is how a value is kept
/// verbatim for the resolver — a colour, a reference, or a function this version
/// will drop.
fn rest(input: &mut Parser<'_>) -> String {
    let start = input.position();
    while input.next_including_whitespace().is_ok() {}
    input.slice_from(start).trim().to_owned()
}

/// Consume a block's contents without interpreting them, including nested
/// functions and `{}` blocks.
fn skip(input: &mut Parser<'_>) -> Result<(), ParseError<()>> {
    while let Ok(token) = input.next_including_whitespace() {
        let nested = matches!(
            token,
            Token::CurlyBracketBlock
                | Token::SquareBracketBlock
                | Token::ParenthesisBlock
                | Token::Function(_)
        );
        if nested {
            input.parse_nested_block(|input| skip(input))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_css(css: &str, dark: bool) -> Report {
        let mut state = State::default();
        let (items, errors) = parse(css);
        assert_eq!(errors, 0, "test stylesheet should parse: {css}");
        apply(&items, Path::new("."), dark, &mut state, 0, &mut Vec::new());
        state.into_report()
    }

    fn rgb(colour: Color32) -> [u8; 3] {
        [colour.r(), colour.g(), colour.b()]
    }

    #[test]
    fn a_user_file_repaints_every_slot_dtr_maps() {
        let report = read_css(
            r#"
            @define-color window_bg_color #271e1a;
            @define-color popover_bg_color #322824;
            @define-color view_bg_color #231a16;
            @define-color headerbar_bg_color #271e1a;
            @define-color window_fg_color #f0dfd8;
            @define-color accent_bg_color #ff6d00;
            @define-color accent_fg_color #000000;
            @define-color destructive_bg_color #f2b8b5;
            @define-color destructive_fg_color #601410;
            "#,
            true,
        );
        assert_eq!(report.applied, 9);
        let o = report.overrides;
        assert_eq!(rgb(o.window.unwrap()), [0x27, 0x1E, 0x1A]);
        assert_eq!(rgb(o.card.unwrap()), [0x32, 0x28, 0x24]);
        assert_eq!(rgb(o.well.unwrap()), [0x23, 0x1A, 0x16]);
        assert_eq!(rgb(o.headerbar.unwrap()), [0x27, 0x1E, 0x1A]);
        assert_eq!(rgb(o.text.unwrap()), [0xF0, 0xDF, 0xD8]);
        assert_eq!(rgb(o.accent.unwrap()), [0xFF, 0x6D, 0x00]);
        assert_eq!(rgb(o.on_accent.unwrap()), [0x00, 0x00, 0x00]);
        assert_eq!(rgb(o.danger.unwrap()), [0xF2, 0xB8, 0xB5]);
        assert_eq!(rgb(o.on_danger.unwrap()), [0x60, 0x14, 0x10]);
        assert!(report.dropped.is_empty());
    }

    /// GTK resolves a named colour lazily, so a definition may name one that the
    /// file declares later; a reader that resolved as it parsed would lose it.
    #[test]
    fn a_reference_may_be_defined_after_its_use() {
        let report = read_css(
            r#"
            @define-color brand #3584e4;
            @define-color accent_bg_color @brand;
            "#,
            true,
        );
        assert_eq!(rgb(report.overrides.accent.unwrap()), [0x35, 0x84, 0xE4]);
    }

    /// The documented override — `:root { --accent-bg-color: … }` — is the
    /// spelling GTK is moving to, so it wins over the named colour when a file
    /// writes both.
    #[test]
    fn a_root_variable_wins_over_the_named_colour() {
        let report = read_css(
            r#"
            @define-color accent_bg_color #ff0000;
            :root { --accent-bg-color: #00ff00; }
            "#,
            true,
        );
        assert_eq!(rgb(report.overrides.accent.unwrap()), [0x00, 0xFF, 0x00]);
    }

    /// A value this reader cannot resolve keeps the measured constant rather than
    /// becoming a guess, and `-v` can name it.
    #[test]
    fn an_unsupported_value_is_dropped_not_guessed() {
        let report = read_css(
            r#"
            @define-color window_bg_color color-mix(in srgb, #000 50%, #fff);
            @define-color accent_bg_color #ff6d00;
            "#,
            true,
        );
        assert!(report.overrides.window.is_none());
        assert_eq!(rgb(report.overrides.accent.unwrap()), [0xFF, 0x6D, 0x00]);
        assert_eq!(report.dropped.len(), 1);
        assert!(report.dropped[0].starts_with("window_bg_color=color-mix"));
    }

    /// `@media` is evaluated against the scheme dtr decided, which is what GTK
    /// does with the portal's `color-scheme`.
    #[test]
    fn media_blocks_follow_the_decided_scheme() {
        let css = r#"
            @define-color accent_bg_color #ff0000;
            @media (prefers-color-scheme: dark) {
                @define-color accent_bg_color #ff6d00;
            }
        "#;
        assert_eq!(
            rgb(read_css(css, true).overrides.accent.unwrap()),
            [0xFF, 0x6D, 0x00]
        );
        assert_eq!(
            rgb(read_css(css, false).overrides.accent.unwrap()),
            [0xFF, 0x00, 0x00]
        );
    }

    /// A high-contrast block is not applied: dtr has no high-contrast style, and
    /// applying its colours would claim one.
    #[test]
    fn a_contrast_block_is_not_applied() {
        let css = r#"
            @media (prefers-contrast: more) {
                @define-color window_bg_color #000000;
            }
        "#;
        assert!(read_css(css, true).overrides.window.is_none());
    }

    /// GTK's own sheet writes `RGB(0 0 6 / 80%)`, and the alpha has to survive to
    /// the palette, which composites it over the window.
    #[test]
    fn a_translucent_foreground_keeps_its_alpha() {
        let report = read_css("@define-color window_fg_color RGB(0 0 6 / 80%);", false);
        let [_, _, _, alpha] = report.overrides.text.unwrap().to_srgba_unmultiplied();
        assert_eq!(alpha, 204);
    }

    /// A file that only sets a colour for one component leaves the rest at the
    /// constants — a partial palette is not a failure.
    #[test]
    fn a_partial_file_keeps_the_rest() {
        let report = read_css("@define-color accent_bg_color #ff6d00;", true);
        assert_eq!(report.applied, 1);
        assert!(report.overrides.window.is_none());
        assert!(report.overrides.card.is_none());
    }

    /// The real shape of a customised session: a symlinked `gtk.css` that imports
    /// a generated palette, one dangling store path (a rebuild moved it), and
    /// selector rules after the definitions. The dangling import must cost nothing
    /// but a note.
    #[test]
    fn a_dangling_import_does_not_lose_the_palette() {
        let path = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/gtk/gtk.css"
        ));
        let report = read_at(path, true);
        assert_eq!(report.files, 2, "gtk.css and the palette it imports");
        assert_eq!(rgb(report.overrides.window.unwrap()), [0x27, 0x1E, 0x1A]);
        assert_eq!(rgb(report.overrides.card.unwrap()), [0x32, 0x28, 0x24]);
        assert_eq!(rgb(report.overrides.accent.unwrap()), [0xFF, 0x6D, 0x00]);
        assert_eq!(rgb(report.overrides.on_accent.unwrap()), [0x00, 0x00, 0x00]);
        assert_eq!(rgb(report.overrides.on_danger.unwrap()), [0x60, 0x14, 0x10]);
        assert!(
            report
                .notes
                .iter()
                .any(|note| note.contains("no such file")),
            "the dangling import should be noted: {:?}",
            report.notes
        );
        assert!(report.dropped.is_empty(), "{:?}", report.dropped);
    }

    /// A relative import resolves beside the file that wrote it, not beside its
    /// canonical target, because `~/.config/gtk-4.0/gtk.css` is usually a
    /// symlink into the store.
    #[test]
    fn a_relative_import_resolves_beside_the_symlink() {
        let path = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/gtk/gtk.css"
        ));
        let report = read_at(path, true);
        assert_eq!(report.files, 2);
        assert_eq!(rgb(report.overrides.well.unwrap()), [0x23, 0x1A, 0x16]);
    }

    /// A block at-rule is skipped structurally: a nested `{}` consumed as a flat
    /// token stream would swallow the rest of the file, and the definition after it
    /// is the one a person sees.
    #[test]
    fn a_block_rule_does_not_eat_the_rest_of_the_file() {
        let report = read_css(
            r#"
            @keyframes spin { from { color: red; } to { color: blue; } }
            @font-face { font-family: X; src: url(x.woff2); }
            @define-color accent_bg_color #ff6d00;
            "#,
            true,
        );
        assert_eq!(rgb(report.overrides.accent.unwrap()), [0xFF, 0x6D, 0x00]);
        assert!(report.dropped.is_empty(), "{:?}", report.dropped);
    }

    #[test]
    fn a_value_that_references_an_undefined_name_is_dropped() {
        let report = read_css("@define-color accent_bg_color @blue_3;", true);
        assert!(report.overrides.accent.is_none());
        assert_eq!(report.dropped, vec!["accent_bg_color=@blue_3".to_string()]);
    }
}
