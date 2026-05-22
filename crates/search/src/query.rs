//! Query input for [`SearchIndex::search`](crate::SearchIndex::search).
//!
//! Kept as a plain struct with `Option` filters rather than a builder — the
//! API layer constructs these from query parameters so the deserialise path
//! stays trivial.
//!
//! The `cursor` field is reserved for future opaque relevance-cursor
//! pagination. Today every search returns `next_cursor: None`; the
//! placeholder keeps the wire shape stable so a follow-up that wires
//! Tantivy's `TopDocs::tweak_score` style cursor doesn't need an API change.

use thewiki_core::NamespaceId;

/// A typed search request.
#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    /// Free-text query, parsed by Tantivy's classic query parser against
    /// `title`, `body`, and `tags`. May contain Tantivy syntax (boost,
    /// phrase, field qualifier). Empty queries return no results.
    pub text: String,
    /// Optional filter: only return hits in this namespace.
    ///
    /// Surfaced as `namespace_id` so call sites that already have a
    /// resolved namespace don't have to look up its slug; the query
    /// path translates it to a slug-based filter against the indexed
    /// `namespace_slug` (`namespace_id` is not yet round-trip indexed by
    /// the schema beyond a bytes-only filter — see #28 for cross-namespace
    /// routing).
    pub namespace_id: Option<NamespaceId>,
    /// Optional filter: only return hits with this exact namespace slug.
    /// Mutually exclusive with [`namespace_id`](Self::namespace_id) in
    /// practice — when both are set, slug wins.
    pub namespace_slug: Option<String>,
    /// Optional filter: only return hits tagged with `tag`. Tags ship in
    /// #29; until then this filter matches against an empty multi-valued
    /// field, which means it returns nothing.
    pub tag: Option<String>,
    /// Maximum number of hits to return. Clamped to `>= 1`.
    pub limit: u32,
    /// Opaque relevance cursor. Reserved for the next pagination PR.
    pub cursor: Option<String>,
    /// Multiplier applied to the `title` field during BM25 scoring. Values
    /// above 1.0 favour title matches over body matches; `0.0` disables the
    /// boost entirely (every field weighted equally). The API layer reads
    /// this from `Config::search.title_boost` (default 2.0).
    pub title_boost: f32,
}

impl SearchQuery {
    /// Convenience constructor for the common "text only" case.
    #[must_use]
    pub fn text(text: impl Into<String>, limit: u32) -> Self {
        Self {
            text: text.into(),
            limit,
            ..Self::default()
        }
    }
}
