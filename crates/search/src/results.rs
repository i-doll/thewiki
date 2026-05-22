//! Ranked-hit shape returned by [`SearchIndex::search`](crate::SearchIndex::search).
//!
//! These types are plain `Debug + Clone` structs. Serialising them on the
//! wire is the API crate's job — keeping `serde` out of this crate means
//! callers can decide whether to expose `score` (typically yes) and
//! `total_estimate` (typically yes, for "showing 1–10 of ~N").

use thewiki_core::PageId;
use time::OffsetDateTime;

/// Hit list returned by a successful search.
#[derive(Debug, Clone)]
pub struct SearchResults {
    /// Ranked hits in descending relevance order.
    pub hits: Vec<SearchHit>,
    /// Opaque cursor for the next page of results, or `None` when the hit
    /// set is exhausted. Today this is always `None` — relevance-cursor
    /// pagination lands in a follow-up.
    pub next_cursor: Option<String>,
    /// Best-effort estimate of the total matching document count. Tantivy
    /// does not give us an exact value without a separate `Count` collector,
    /// so this is the number of hits we materialised — fine for "more
    /// results available" UI affordances.
    pub total_estimate: u64,
}

/// One ranked hit.
#[derive(Debug, Clone)]
pub struct SearchHit {
    /// Page primary key.
    pub page_id: PageId,
    /// Namespace slug of the page (joined for display).
    pub namespace_slug: String,
    /// URL slug of the page (joined for display).
    pub slug: String,
    /// Page title.
    pub title: String,
    /// HTML snippet with the matched terms wrapped in `<mark>…</mark>`.
    /// Empty when the matched terms only appear outside the body field
    /// (e.g. a hit purely on the title).
    pub snippet: String,
    /// Tantivy's BM25-derived score. Higher is better; the absolute value is
    /// meaningful only relative to other hits in the same result set.
    pub score: f32,
    /// Last-edited timestamp of the matched page, if the indexed document
    /// carried one (it always should — the field is mandatory in the
    /// schema). `None` is a degenerate case we still tolerate gracefully.
    pub updated_at: Option<OffsetDateTime>,
}
