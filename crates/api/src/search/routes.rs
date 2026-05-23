//! Axum handler for `GET /api/v1/search`.
//!
//! Thin wrapper around [`thewiki_search::Searcher::search`]. Handles input
//! validation (empty query → `400`, limit clamp), wires the operator-tunable
//! title boost from `Config::search.title_boost`, and re-shapes the search
//! crate's [`thewiki_search::SearchHit`] into the wire DTO.

use axum::Json;
use axum::extract::{Query, State};
use thewiki_search::SearchQuery;

use crate::error::ApiError;
use crate::search::dto::{SearchHitView, SearchParams, SearchResponse};
use crate::state::{AppState, AppStorage};

/// Default `limit` when the caller omits it.
pub const DEFAULT_LIMIT: u32 = 10;

/// Hard upper bound on `limit`. Anything above is silently clamped.
pub const MAX_LIMIT: u32 = 50;

/// `GET /api/v1/search` — ranked full-text search across every indexed page.
///
/// Returns `400 invalid_input` when `q` is missing or whitespace-only.
/// Otherwise responds with a hit list ordered by Tantivy BM25 score
/// (with the configured title-boost applied to title-field matches).
/// Snippets carry HTML highlights wrapped in `<mark>…</mark>`; clients must
/// sanitise before rendering.
#[utoipa::path(
    get,
    path = "",
    params(SearchParams),
    responses(
        (status = 200, description = "Ranked hits", body = SearchResponse),
        (status = 400, description = "Empty / malformed query", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
        (status = 500, description = "Index unavailable", body = crate::error::ErrorBody),
    ),
    tag = "search",
)]
pub async fn search<S: AppStorage>(
    State(state): State<AppState<S>>,
    Query(params): Query<SearchParams>,
) -> Result<Json<SearchResponse>, ApiError> {
    let q = params.q.trim();
    if q.is_empty() {
        return Err(ApiError::InvalidInput("q must not be empty".to_string()));
    }

    let limit = clamp_limit(params.limit);
    let query = SearchQuery {
        text: q.to_string(),
        namespace_id: None,
        namespace_slug: params.namespace.clone(),
        tag: params.tag.clone(),
        limit,
        cursor: params.cursor.clone(),
        title_boost: state.search_title_boost,
        talk_boost: state.search_talk_boost,
    };

    // The searcher is sync (Tantivy reads are cheap and don't yield), so
    // running it on the runtime is fine — no need for `spawn_blocking`.
    let results = state
        .searcher
        .search(&query)
        .map_err(|err| ApiError::Internal(format!("search: {err}")))?;

    let items: Vec<SearchHitView> = results
        .hits
        .into_iter()
        .map(|h| SearchHitView {
            page_id: h.page_id,
            namespace_slug: h.namespace_slug,
            slug: h.slug,
            title: h.title,
            snippet: h.snippet,
            score: h.score,
            updated_at: h.updated_at,
        })
        .collect();

    Ok(Json(SearchResponse {
        items,
        next_cursor: results.next_cursor,
        total_estimate: results.total_estimate,
    }))
}

/// Clamp the caller-supplied limit into `1..=MAX_LIMIT`, defaulting `None`
/// / `Some(0)` to [`DEFAULT_LIMIT`].
fn clamp_limit(raw: Option<u32>) -> u32 {
    match raw {
        Some(0) | None => DEFAULT_LIMIT,
        Some(n) => n.min(MAX_LIMIT),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn limit_clamps_to_default_and_max() {
        assert_eq!(clamp_limit(None), DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some(0)), DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some(5)), 5);
        assert_eq!(clamp_limit(Some(MAX_LIMIT)), MAX_LIMIT);
        assert_eq!(clamp_limit(Some(MAX_LIMIT + 100)), MAX_LIMIT);
    }
}
