//! Extract URLs from a Markdown body and match them against the URL
//! blocklist snapshot.
//!
//! We deliberately use a simple regex (`https?://...`) here rather than
//! plumbing through the full pulldown-cmark parser:
//!
//! * The check has to run at edit time, before the page hits storage, so
//!   the body is just a `&str`. There's no DOM yet.
//! * Operators want to block bare-URL spam too — the regex catches plain
//!   URLs in addition to inline `[label](url)` Markdown links, which the
//!   parser would miss.
//! * False positives (e.g. URLs inside a code block) are acceptable for
//!   v1 — the alternative is letting moderation-evading edits past the
//!   gate.

use std::sync::OnceLock;

use regex::Regex;

use crate::blocklist::state::BlocklistSnapshot;

/// Compiled URL extractor. Initialised lazily.
///
/// The pattern matches `http://` / `https://` followed by any run of
/// non-whitespace characters that are also not common terminators
/// (`)`, `>`, `"`, `'`, `]`). That conservative tail trim keeps the
/// captured URL out of trailing Markdown / HTML punctuation.
fn url_extractor() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    #[allow(
        clippy::expect_used,
        reason = "the pattern is a compile-time string literal that we control; a parse \
                  failure here is a programmer error and should surface loudly"
    )]
    REGEX.get_or_init(|| {
        Regex::new(r#"https?://[^\s)>"'\]]+"#).expect("URL extractor regex compiles")
    })
}

/// Extract every URL from `body`. Returns owned `String`s so callers can
/// keep them around past the lifetime of the input slice.
#[must_use]
pub fn extract_urls(body: &str) -> Vec<String> {
    url_extractor()
        .find_iter(body)
        .map(|m| m.as_str().to_string())
        .collect()
}

/// One offending URL paired with the patterns it matched. Returned by
/// [`check_body_against_snapshot`] so the API layer can render a
/// useful 400 error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlBlockMatch {
    /// The URL extracted from the body.
    pub url: String,
    /// Pattern source strings (verbatim from `url_blocklist.pattern`) that
    /// matched.
    pub matched_patterns: Vec<String>,
}

/// Check every URL in `body` against `snapshot`. Returns the first batch of
/// matches; callers reject the edit if the return is `Some(...)`.
#[must_use]
pub fn check_body_against_snapshot(
    body: &str,
    snapshot: &BlocklistSnapshot,
) -> Option<Vec<UrlBlockMatch>> {
    if snapshot.url_regex_set.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for url in extract_urls(body) {
        if let Some(patterns) = snapshot.matching_patterns(&url) {
            out.push(UrlBlockMatch {
                url,
                matched_patterns: patterns.into_iter().map(str::to_string).collect(),
            });
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use regex::RegexSet;

    use super::*;

    fn snapshot(patterns: &[&str]) -> BlocklistSnapshot {
        let owned: Vec<String> = patterns.iter().map(|s| (*s).to_string()).collect();
        BlocklistSnapshot {
            ip_nets: Vec::new(),
            url_regex_set: RegexSet::new(&owned).unwrap(),
            url_patterns: owned,
        }
    }

    #[test]
    fn extracts_plain_urls() {
        let urls = extract_urls("see https://example.com/path and http://x.test/y");
        assert_eq!(urls, vec!["https://example.com/path", "http://x.test/y"]);
    }

    #[test]
    fn extracts_markdown_link_target_without_paren() {
        let urls = extract_urls("a [link](https://example.com/foo) here");
        assert_eq!(urls, vec!["https://example.com/foo"]);
    }

    #[test]
    fn extracts_no_urls_from_plain_text() {
        assert!(extract_urls("nothing to see here").is_empty());
    }

    #[test]
    fn check_returns_none_on_no_match() {
        let snap = snapshot(&[r"^https://evil\.example/"]);
        assert!(check_body_against_snapshot("https://good.example/path", &snap).is_none());
    }

    #[test]
    fn check_returns_match_with_pattern() {
        let snap = snapshot(&[r"^https://evil\.example/"]);
        let hits = check_body_against_snapshot("link: https://evil.example/x", &snap).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://evil.example/x");
        assert_eq!(hits[0].matched_patterns, vec![r"^https://evil\.example/".to_string()]);
    }

    #[test]
    fn empty_snapshot_returns_none() {
        let snap = snapshot(&[]);
        let _hits = check_body_against_snapshot("https://x.test/", &snap);
        assert!(check_body_against_snapshot("https://x.test/", &snap).is_none());
    }
}
