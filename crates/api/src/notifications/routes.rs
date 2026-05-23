//! Axum handlers for the in-app inbox endpoints (#40).

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use thewiki_core::Notification;
use thewiki_storage::repo::{Cursor, NotificationRepository};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::ApiError;
use crate::extractors::RequireAuth;
use crate::notifications::dto::{
    ListNotificationsQuery, NotificationListResponse, NotificationView,
};
use crate::state::{AppState, AppStorage};

fn view_of(row: Notification) -> NotificationView {
    NotificationView {
        id: row.id,
        user_id: row.user_id,
        kind: row.kind,
        payload: row.payload,
        read_at: row.read_at,
        created_at: row.created_at,
    }
}

/// `GET /api/v1/notifications` — list the current user's notifications.
#[utoipa::path(
    get,
    path = "",
    params(ListNotificationsQuery),
    responses(
        (status = 200, description = "Inbox listing", body = NotificationListResponse),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "notifications",
)]
pub async fn list_notifications<S: AppStorage>(
    State(state): State<AppState<S>>,
    actor: RequireAuth,
    Query(query): Query<ListNotificationsQuery>,
) -> Result<Json<NotificationListResponse>, ApiError> {
    let limit = match query.limit {
        Some(0) | None => state.route_config.default_page_size,
        Some(n) => n,
    };
    let cursor = query.cursor.map(Cursor);
    let slice = state
        .storage
        .notifications()
        .list_for_user(actor.user_id, cursor, limit)
        .await?;
    let unread = state
        .storage
        .notifications()
        .count_unread(actor.user_id)
        .await?;
    let items = slice.items.into_iter().map(view_of).collect();
    Ok(Json(NotificationListResponse {
        items,
        next_cursor: slice.next.map(|c| c.0),
        unread,
    }))
}

/// `POST /api/v1/notifications/{id}/read` — mark a notification read.
#[utoipa::path(
    post,
    path = "/{id}/read",
    params(("id" = String, Path, description = "Notification id")),
    responses(
        (status = 200, description = "Notification marked read", body = NotificationView),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 404, description = "Notification not found", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "notifications",
)]
pub async fn mark_read<S: AppStorage>(
    State(state): State<AppState<S>>,
    actor: RequireAuth,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<NotificationView>), ApiError> {
    let id = parse_id(&id)?;
    let row = state
        .storage
        .notifications()
        .mark_read(id, actor.user_id, OffsetDateTime::now_utc())
        .await?;
    Ok((StatusCode::OK, Json(view_of(row))))
}

fn parse_id(raw: &str) -> Result<thewiki_core::NotificationId, ApiError> {
    let uuid =
        Uuid::parse_str(raw).map_err(|_| ApiError::InvalidInput("id must be a UUID".into()))?;
    Ok(thewiki_core::NotificationId::from_uuid(uuid))
}
