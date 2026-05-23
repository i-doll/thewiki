//! Watchlist HTTP handlers (#46).

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::json;
use thewiki_core::PageId;
use thewiki_storage::StorageError;
use thewiki_storage::repo::{
    AuditLogRepository, NewAuditLogEntry, PageRepository, WatchRepository,
};

use crate::auth::AuthSession;
use crate::error::ApiError;
use crate::state::{AppState, AppStorage};
use crate::watchlist::dto::{
    AddWatchRequest, WatchStatus, WatchedPageView, WatchlistResponse,
};

/// Page-list cap. Sized to comfortably cover an active user; the watchlist
/// is not expected to grow beyond a few hundred entries in practice and we
/// don't paginate the JSON read for simplicity. The Atom feed has its own
/// (smaller) cap in [`crate::feeds::FEED_LIMIT`].
const WATCHLIST_PAGE_LIMIT: u32 = 500;

/// `GET /api/v1/watchlist` — return every page the caller watches, newest
/// subscription first.
#[utoipa::path(
    get,
    path = "",
    responses(
        (status = 200, description = "Watchlist", body = WatchlistResponse),
        (status = 401, description = "Missing or expired session", body = crate::auth::error::AuthErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    security(("SessionCookie" = [])),
    tag = "watchlist",
)]
pub async fn list_watchlist<S: AppStorage>(
    State(state): State<AppState<S>>,
    session: AuthSession,
) -> Result<Json<WatchlistResponse>, ApiError> {
    let rows = state
        .storage
        .watches()
        .list_for_user(session.user.id, WATCHLIST_PAGE_LIMIT)
        .await?;
    let items = rows
        .into_iter()
        .map(|w| WatchedPageView {
            page_id: w.page_id,
            namespace: w.namespace_slug,
            slug: w.page_slug,
            title: w.page_title,
            watched_at: w.watched_at,
        })
        .collect();
    Ok(Json(WatchlistResponse { items }))
}

/// `POST /api/v1/watchlist` — add a page to the caller's watchlist.
///
/// Idempotent: re-watching a page already on the watchlist returns the same
/// `201 { watched: true }` body without bumping the original timestamp, and
/// without writing a second `watchlist.add` audit row.
#[utoipa::path(
    post,
    path = "",
    request_body = AddWatchRequest,
    responses(
        (status = 201, description = "Added to watchlist", body = WatchStatus),
        (status = 400, description = "Invalid input", body = crate::error::ErrorBody),
        (status = 401, description = "Missing or expired session", body = crate::auth::error::AuthErrorBody),
        (status = 404, description = "Page not found", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    security(("SessionCookie" = []), ("CsrfToken" = [])),
    tag = "watchlist",
)]
pub async fn add_to_watchlist<S: AppStorage>(
    State(state): State<AppState<S>>,
    session: AuthSession,
    Json(req): Json<AddWatchRequest>,
) -> Result<(StatusCode, Json<WatchStatus>), ApiError> {
    // Reject up front when the target page doesn't exist so we can return a
    // clean 404 with a meaningful audit label instead of letting the FK
    // violation bubble up as a 500. The storage trait's FK is "on insert,
    // fail" — the explicit check is also useful for the audit row.
    let page = state.storage.pages().get_by_id(req.page_id).await?;

    let inserted = state
        .storage
        .watches()
        .watch(session.user.id, req.page_id)
        .await?;

    // Audit only on the state-changing path. A duplicate POST (e.g. a retry)
    // hits `INSERT OR IGNORE` and returns `false` — we don't want to clutter
    // the audit log with bogus "watchlist.add" rows that didn't reflect an
    // actual change.
    if inserted {
        let audit = NewAuditLogEntry {
            actor_id: session.user.id,
            actor_username: session.user.username.as_str().to_owned(),
            action: "watchlist.add".to_owned(),
            target_kind: "page".to_owned(),
            target_id: page.id.into_uuid(),
            target_label: Some(format!("{}/{}", page.namespace_id.into_uuid(), page.slug)),
            metadata: json!({
                "slug": page.slug,
            }),
        };
        state.storage.audit_log().create(audit).await?;
    }

    Ok((StatusCode::CREATED, Json(WatchStatus { watched: true })))
}

/// `DELETE /api/v1/watchlist/{page_id}` — remove a page from the caller's
/// watchlist.
///
/// Idempotent — removing a page the user wasn't watching returns
/// `204 No Content` just the same. Returns `404` only when the page itself
/// doesn't exist (so callers can distinguish "typo'd ID" from "already
/// removed").
#[utoipa::path(
    delete,
    path = "/{page_id}",
    params(("page_id" = String, Path, description = "Page UUID to remove")),
    responses(
        (status = 204, description = "Removed (or wasn't watched)"),
        (status = 401, description = "Missing or expired session", body = crate::auth::error::AuthErrorBody),
        (status = 404, description = "Page not found", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    security(("SessionCookie" = []), ("CsrfToken" = [])),
    tag = "watchlist",
)]
pub async fn remove_from_watchlist<S: AppStorage>(
    State(state): State<AppState<S>>,
    session: AuthSession,
    Path(page_id): Path<PageId>,
) -> Result<StatusCode, ApiError> {
    // Existence check so a bad ID returns 404 rather than silently 204ing.
    let page = match state.storage.pages().get_by_id(page_id).await {
        Ok(page) => page,
        Err(StorageError::NotFound) => return Err(ApiError::NotFound),
        Err(err) => return Err(err.into()),
    };

    let removed = state
        .storage
        .watches()
        .unwatch(session.user.id, page_id)
        .await?;

    // Same rationale as `add_to_watchlist`: only audit when the DELETE
    // actually changed state. Removing a page the user wasn't watching is a
    // silent 204 with no audit entry.
    if removed {
        let audit = NewAuditLogEntry {
            actor_id: session.user.id,
            actor_username: session.user.username.as_str().to_owned(),
            action: "watchlist.remove".to_owned(),
            target_kind: "page".to_owned(),
            target_id: page.id.into_uuid(),
            target_label: Some(format!("{}/{}", page.namespace_id.into_uuid(), page.slug)),
            metadata: json!({
                "slug": page.slug,
            }),
        };
        state.storage.audit_log().create(audit).await?;
    }

    Ok(StatusCode::NO_CONTENT)
}
