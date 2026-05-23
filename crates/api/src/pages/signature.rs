//! Sign-with-timestamp expansion for talk-namespace pages (#43).
//!
//! When a page lives in a discussion ("talk") namespace, the API rewrites
//! occurrences of the MediaWiki-compatible marker `~~~~` to
//! `[[User:<username>]] <ISO-8601 UTC timestamp>` server-side, before the
//! revision is persisted. The expansion runs only on the talk codepath —
//! subject pages keep their `~~~~` literal so authors can document the
//! convention without it being eaten by the editor.
//!
//! The SPA shipping with #43 can preview the same expansion client-side
//! via the [`SignatureConvention`](crate::pages::dto::SignatureConvention)
//! block on the page-view response. Keeping the server expansion
//! authoritative means a queued edit (e.g. via the API directly) always
//! lands with the canonical signature; the preview is purely a UX nicety.

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// MediaWiki-compatible marker that triggers signature expansion.
pub const SIGNATURE_MARKER: &str = "~~~~";

/// Replace every `~~~~` in `body` with `[[User:<username>]] <ISO-8601 UTC timestamp>`.
///
/// Pure function — no I/O — so call sites can run it in either the live or
/// queued-edit branches. The timestamp is computed *once* per call so all
/// markers in a single revision share the same stamp; that matches the
/// MediaWiki convention and makes diffs readable.
///
/// `now` is taken as a parameter rather than read from the system clock so
/// tests can drive deterministic timestamps without monkey-patching.
#[must_use]
pub fn expand_signatures_with(body: &str, username: &str, now: OffsetDateTime) -> String {
    if !body.contains(SIGNATURE_MARKER) {
        return body.to_owned();
    }
    // RFC 3339 covers the "ISO-8601 UTC timestamp" spec the design constraints
    // call for. We use the well-known format so it round-trips through the
    // search index and audit log without bespoke parsing.
    let timestamp = now
        .format(&Rfc3339)
        .unwrap_or_else(|_| now.unix_timestamp().to_string());
    let replacement = format!("[[User:{username}]] {timestamp}");
    body.replace(SIGNATURE_MARKER, &replacement)
}

/// Convenience: expand against `OffsetDateTime::now_utc()`.
#[must_use]
pub fn expand_signatures(body: &str, username: &str) -> String {
    expand_signatures_with(body, username, OffsetDateTime::now_utc())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn fixed_ts() -> OffsetDateTime {
        OffsetDateTime::parse("2026-05-23T12:34:56Z", &Rfc3339).expect("valid ts")
    }

    #[test]
    fn no_marker_returns_body_unchanged() {
        let body = "hello world";
        assert_eq!(expand_signatures_with(body, "alice", fixed_ts()), body);
    }

    #[test]
    fn single_marker_expands_to_user_and_timestamp() {
        let body = "Looks good to me ~~~~";
        let out = expand_signatures_with(body, "alice", fixed_ts());
        assert_eq!(out, "Looks good to me [[User:alice]] 2026-05-23T12:34:56Z");
    }

    #[test]
    fn multiple_markers_share_one_timestamp() {
        let body = "Reply one ~~~~\n\n> nested\n\nReply two ~~~~";
        let out = expand_signatures_with(body, "bob", fixed_ts());
        let stamp = "[[User:bob]] 2026-05-23T12:34:56Z";
        assert_eq!(
            out,
            format!("Reply one {stamp}\n\n> nested\n\nReply two {stamp}")
        );
    }

    #[test]
    fn marker_at_start_of_line_works() {
        let body = "~~~~ posting first";
        let out = expand_signatures_with(body, "carol", fixed_ts());
        assert!(out.starts_with("[[User:carol]] 2026-05-23T12:34:56Z"));
    }

    #[test]
    fn five_tildes_only_expands_first_four() {
        // `~~~~~` is "marker plus a trailing tilde" — the replace finds the
        // first 4-char window and leaves the trailing `~` alone. Documenting
        // the behaviour so future-us doesn't regress to a stricter regex.
        let body = "edge case ~~~~~";
        let out = expand_signatures_with(body, "alice", fixed_ts());
        assert_eq!(out, "edge case [[User:alice]] 2026-05-23T12:34:56Z~");
    }
}
