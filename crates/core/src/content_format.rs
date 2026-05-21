//! Content formats that the renderer pipeline understands.
//!
//! v1 ships with Markdown only. `#[non_exhaustive]` keeps the door open for
//! AsciiDoc, MediaWiki wikitext, and reStructuredText to land post-v1
//! without breaking downstream `match` expressions.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The source format a page is authored in.
///
/// The renderer crate dispatches on this value to pick an implementation of
/// the `Renderer` trait (defined in M0's issue #3). Storage round-trips the
/// value as a lowercase string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ContentFormat {
    /// CommonMark Markdown plus the GitHub-flavoured extensions selected in
    /// ADR-0001. The only v1 format.
    Markdown,
}

impl ContentFormat {
    /// Stable identifier used in storage columns and the OpenAPI surface.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
        }
    }
}

impl core::fmt::Display for ContentFormat {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "ergonomic tests")]
mod tests {
    use super::*;

    #[test]
    fn markdown_serialises_lowercase() {
        let json = serde_json::to_string(&ContentFormat::Markdown).expect("serialise");
        assert_eq!(json, "\"markdown\"");
    }

    #[test]
    fn round_trip_serde() {
        let parsed: ContentFormat = serde_json::from_str("\"markdown\"").expect("deserialise");
        assert_eq!(parsed, ContentFormat::Markdown);
    }

    #[test]
    fn display_matches_as_str() {
        assert_eq!(ContentFormat::Markdown.to_string(), "markdown");
    }
}
