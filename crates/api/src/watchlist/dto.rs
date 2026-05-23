//! Request and response payloads for the watchlist endpoints.

use serde::{Deserialize, Serialize};
use thewiki_core::PageId;
use time::OffsetDateTime;
use utoipa::ToSchema;

/// A single row on the user's watchlist.
///
/// Constructed from a `thewiki_storage::repo::WatchedPage` — the row already
/// joins in namespace slug and page title so the SPA can render the list
/// without follow-up lookups.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WatchedPageView {
    /// Watched page's stable identifier.
    pub page_id: PageId,
    /// Namespace slug the page lives in.
    pub namespace: String,
    /// URL slug of the page within its namespace.
    pub slug: String,
    /// Human-readable title.
    pub title: String,
    /// When the user added the page to their watchlist.
    #[serde(with = "time::serde::rfc3339")]
    pub watched_at: OffsetDateTime,
}

/// Response from `GET /api/v1/watchlist`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WatchlistResponse {
    /// Pages on the caller's watchlist, newest first.
    pub items: Vec<WatchedPageView>,
}

/// Body for `POST /api/v1/watchlist`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct AddWatchRequest {
    /// Identifier of the page to add to the watchlist.
    pub page_id: PageId,
}

/// Response from `POST /api/v1/watchlist` and the toggle path on the SPA.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WatchStatus {
    /// Whether the page is currently on the caller's watchlist after the
    /// operation.
    pub watched: bool,
}
