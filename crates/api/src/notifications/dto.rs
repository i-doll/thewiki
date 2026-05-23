//! Request and response payloads for the in-app inbox endpoints (#40).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thewiki_core::{NotificationId, UserId};
use time::OffsetDateTime;
use utoipa::ToSchema;

/// One row in `GET /api/v1/notifications`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct NotificationView {
    /// Stable identifier.
    pub id: NotificationId,
    /// Recipient.
    pub user_id: UserId,
    /// Stable kind string (see [`thewiki_core::notification::kind`]).
    pub kind: String,
    /// Arbitrary structured payload, identical to what the producer
    /// attached.
    pub payload: Option<Value>,
    /// `None` while unread; the RFC3339 timestamp once the user opened
    /// the row.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub read_at: Option<OffsetDateTime>,
    /// When the notification was produced.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// Response from `GET /api/v1/notifications`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct NotificationListResponse {
    /// Rows in this batch, newest first.
    pub items: Vec<NotificationView>,
    /// Token to fetch the next page, or `None` when the listing is
    /// exhausted.
    pub next_cursor: Option<String>,
    /// Count of unread rows in the current user's inbox.
    pub unread: u64,
}

/// Query parameters for `GET /api/v1/notifications`.
#[derive(Debug, Clone, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListNotificationsQuery {
    /// Opaque cursor returned by a previous call.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Page size. Clamped to
    /// [`thewiki_storage::repo::MAX_PAGE_SIZE`].
    #[serde(default)]
    pub limit: Option<u32>,
}
