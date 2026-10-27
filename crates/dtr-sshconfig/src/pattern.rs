//! OpenSSH's shell-style host patterns (`*` and `?`), implemented locally so
//! the crate does not pull in a general glob engine.

/// Matches one OpenSSH pattern against `text`, ignoring ASCII case.
///
/// OpenSSH's host patterns know only `*` (any run, including empty) and `?`
/// (exactly one character); every other character is literal.
fn pattern_matches(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().map(|c| c.to_ascii_lowercase()).collect();
    let text: Vec<char> = text.chars().map(|c| c.to_ascii_lowercase()).collect();

    let mut p = 0;
    let mut t = 0;
    // Position of the most recent `*` and the text offset it was tried at.
    let mut star: Option<(usize, usize)> = None;

    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, t));
            p += 1;
        } else if let Some((star_pattern, star_text)) = star {
            p = star_pattern + 1;
            t = star_text + 1;
            star = Some((star_pattern, star_text + 1));
        } else {
            return false;
        }
    }

    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

/// Applies a `Host` line's pattern list to `alias`.
///
/// The block matches when some positive pattern matches and no negated pattern
/// does, which also means a block containing only negations never matches.
pub(crate) fn matches_host_patterns(patterns: &[String], alias: &str) -> bool {
    let mut positive_match = false;
    for pattern in patterns {
        let (negated, pattern) = match pattern.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, pattern.as_str()),
        };
        if pattern_matches(pattern, alias) {
            if negated {
                return false;
            }
            positive_match = true;
        }
    }
    positive_match
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patterns(patterns: &[&str]) -> Vec<String> {
        patterns.iter().map(|p| (*p).to_string()).collect()
    }

    #[test]
    fn star_matches_any_run_including_empty() {
        assert!(pattern_matches("web*", "web"));
        assert!(pattern_matches("web*", "web01"));
        assert!(pattern_matches("*host", "myhost"));
        assert!(pattern_matches("a*b*c", "axbyc"));
        assert!(pattern_matches("*", "anything"));
        assert!(!pattern_matches("web*", "wib"));
        assert!(!pattern_matches("*.internal", "internal.example"));
    }

    #[test]
    fn question_mark_matches_exactly_one_character() {
        assert!(pattern_matches("db?", "db1"));
        assert!(!pattern_matches("db?", "db"));
        assert!(!pattern_matches("db?", "db12"));
        assert!(pattern_matches("a?c", "abc"));
        assert!(pattern_matches("a?c", "axc"));
        assert!(!pattern_matches("a?c", "ac"));
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert!(pattern_matches("WEB*", "web1"));
        assert!(pattern_matches("web*", "WEB1"));
        assert!(matches_host_patterns(
            &patterns(&["Web.Example"]),
            "web.example"
        ));
    }

    #[test]
    fn multiple_patterns_match_if_any_positive_does() {
        let patterns = patterns(&["web", "db", "cache*"]);
        assert!(matches_host_patterns(&patterns, "db"));
        assert!(matches_host_patterns(&patterns, "cache-1"));
        assert!(!matches_host_patterns(&patterns, "mail"));
    }

    #[test]
    fn negation_vetoes_a_positive_match() {
        let patterns = patterns(&["web*", "!web-prod"]);
        assert!(matches_host_patterns(&patterns, "web-dev"));
        assert!(!matches_host_patterns(&patterns, "web-prod"));
        assert!(!matches_host_patterns(&patterns, "mail"));
    }

    #[test]
    fn only_negative_patterns_never_match() {
        let patterns = patterns(&["!web"]);
        assert!(!matches_host_patterns(&patterns, "web"));
        assert!(!matches_host_patterns(&patterns, "db"));
    }
}
