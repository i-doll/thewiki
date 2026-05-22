//! Wire types for `GET /api/v1/search`.
//!
//! The shape mirrors the GraphQL `SearchResults` / `SearchHit` types in
//! [`crate::graphql::types`] so a frontend can share the same render code
//! between REST and GraphQL clients (the SPA uses REST today).

use serde::{Deserialize, Serialize};
use thewiki_core::PageId;
use time::OffsetDateTime;
use utoipa::ToSchema;

/// One ranked hit returned by the search endpoint.
///
/// Constructed from a [`thewiki_search::SearchHit`] in the handler. The
/// snippet carries HTML — clients are expected to sanitise it before
/// rendering (the SPA pipes it through DOMPurify).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SearchHitView {
    /// Page primary key (UUIDv7).
    pub page_id: PageId,
    /// Slug of the namespace the hit lives in.
    pub namespace_slug: String,
    /// URL slug of the matched page.
    pub slug: String,
    /// Page title.
    pub title: String,
    /// HTML snippet of the body with matched terms wrapped in
    /// `<mark>…</mark>`. May be empty when the match only touched the
    /// title — the SPA renders the title alone in that case.
    pub snippet: String,
    /// BM25-derived relevance score (higher is better). Useful only as a
    /// relative ranking within the same response.
    pub score: f32,
    /// Last-edited timestamp of the matched page.
    #[serde(with = "time::serde::rfc3339::option")]
    pub updated_at: Option<OffsetDateTime>,
}

/// Response body for `GET /api/v1/search`.
///
/// `next_cursor` is reserved for relevance-cursor pagination; today every
/// response carries `null` and `total_estimate` reflects the number of hits
/// materialised in this response. The shape stays stable so a follow-up
/// can wire cursors without changing the wire surface.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SearchResponse {
    /// Ranked hits, descending by score.
    pub items: Vec<SearchHitView>,
    /// Opaque cursor for the next batch — currently always `null`.
    pub next_cursor: Option<String>,
    /// Best-effort estimate of the total matching document count.
    pub total_estimate: u64,
}

/// Query parameters for `GET /api/v1/search`.
///
/// `q` is required and may not be empty; the handler returns `400` when the
/// trimmed value has length zero. `limit` is clamped to `1..=MAX_LIMIT` and
/// defaults to `DEFAULT_LIMIT` when missing or `0`.
#[derive(Debug, Clone, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SearchParams {
    /// Free-text query. Empty / whitespace-only values are rejected with
    /// `400 invalid_input`.
    pub q: String,
    /// Optional namespace filter (slug). Unknown namespaces simply return
    /// no hits — we deliberately do not 404 here because the index may
    /// lag the canonical namespace list.
    #[serde(default)]
    pub namespace: Option<String>,
    /// Optional tag filter. Currently matches against the empty multi-valued
    /// `tags` field, so always returns nothing until #29 lands.
    #[serde(default)]
    pub tag: Option<String>,
    /// Page size. `0` / missing falls back to the route default (10).
    /// Clamped to the route maximum (50).
    #[serde(default)]
    pub limit: Option<u32>,
    /// Opaque cursor returned by a previous call. Reserved for a follow-up
    /// — accepted today but ignored, so clients can pass it through
    /// without breaking when pagination lands.
    #[serde(default)]
    pub cursor: Option<String>,
}
