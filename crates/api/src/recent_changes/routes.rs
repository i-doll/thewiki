//! Axum handler for the recent-changes feed.
//!
//! One handler, `GET /api/v1/recent-changes`. Reads are open today (no
//! authentication required), matching the page-listing endpoints.

use axum::Json;
use axum::extract::{Query, State};
use thewiki_core::{NamespaceSlug, Username};
use thewiki_storage::repo::{
    Cursor, NamespaceRepository, RecentChangesFilter, RecentChangesRepository, UserRepository,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::ApiError;
use crate::recent_changes::dto::{RecentChangeView, RecentChangesQuery, RecentChangesResponse};
use crate::state::{AppState, AppStorage};

/// `GET /api/v1/recent-changes` — chronological wiki-wide edit feed.
///
/// Cursor-paginated, newest first. Filterable by `since`, `namespace`, and
/// `actor`. Unknown namespaces / actors render as `404`; an unset filter
/// simply drops the corresponding predicate.
#[utoipa::path(
    get,
    path = "",
    params(RecentChangesQuery),
    responses(
        (status = 200, description = "Recent changes", body = RecentChangesResponse),
        (status = 400, description = "Malformed query", body = crate::error::ErrorBody),
        (status = 404, description = "Namespace or actor not found", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "recent-changes",
)]
pub async fn list_recent_changes<S: AppStorage>(
    State(state): State<AppState<S>>,
    Query(query): Query<RecentChangesQuery>,
) -> Result<Json<RecentChangesResponse>, ApiError> {
    // ── Parse the timestamp filter ───────────────────────────────────────
    let since = match query.since.as_deref() {
        Some(raw) => Some(
            OffsetDateTime::parse(raw, &Rfc3339)
                .map_err(|err| ApiError::InvalidInput(format!("since: {err}")))?,
        ),
        None => None,
    };

    // ── Resolve the namespace filter (slug → id), 404 if missing ─────────
    let namespace_id = match query.namespace.as_deref() {
        Some(raw) => {
            let slug = NamespaceSlug::new(raw)
                .map_err(|err| ApiError::InvalidInput(format!("namespace: {err}")))?;
            let ns = state
                .storage
                .namespaces()
                .get_by_slug(&slug)
                .await
                .map_err(ApiError::from)?;
            Some(ns.id)
        }
        None => None,
    };

    // ── Resolve the actor filter (username → id), 404 if missing ─────────
    let actor_id = match query.actor.as_deref() {
        Some(raw) => {
            let username = Username::new(raw)
                .map_err(|err| ApiError::InvalidInput(format!("actor: {err}")))?;
            let user = state
                .storage
                .users()
                .get_by_username(&username)
                .await
                .map_err(ApiError::from)?;
            Some(user.id)
        }
        None => None,
    };

    let limit = match query.limit {
        Some(0) | None => state.route_config.default_page_size,
        Some(n) => n,
    };

    let cursor = query.cursor.map(Cursor);
    let filter = RecentChangesFilter {
        since,
        namespace_id,
        actor_id,
    };

    let slice = state
        .storage
        .recent_changes()
        .list(filter, cursor, limit)
        .await?;

    let items = slice
        .items
        .into_iter()
        .map(|rc| RecentChangeView {
            revision_id: rc.revision_id,
            page_id: rc.page_id,
            page_slug: rc.page_slug,
            namespace_id: rc.namespace_id,
            namespace_slug: rc.namespace_slug,
            author_id: rc.author_id,
            author_username: rc.author_username,
            edit_summary: rc.edit_summary,
            created_at: rc.created_at,
        })
        .collect();

    Ok(Json(RecentChangesResponse {
        items,
        next_cursor: slice.next.map(|c| c.0),
    }))
}
