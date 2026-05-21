//! Request and response payloads for the recent-changes endpoint.
//!
//! The wire shape is intentionally flat — one row per revision, with the
//! page slug, namespace slug, and author username joined in so a client can
//! render the feed without follow-up lookups.

use serde::{Deserialize, Serialize};
use thewiki_core::{NamespaceId, PageId, RevisionId, UserId};
use time::OffsetDateTime;
use utoipa::ToSchema;

/// A single entry in the recent-changes feed.
///
/// Constructed by the handler from a `thewiki_storage::repo::RecentChange`
/// — one revision flattened with its page, namespace, and author context.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RecentChangeView {
    /// Identifier of the revision this entry refers to.
    pub revision_id: RevisionId,
    /// The page that was edited.
    pub page_id: PageId,
    /// URL slug of the edited page.
    pub page_slug: String,
    /// Namespace the edited page lives in.
    pub namespace_id: NamespaceId,
    /// Slug of the namespace.
    pub namespace_slug: String,
    /// User who committed the revision.
    pub author_id: UserId,
    /// Username of the author.
    pub author_username: String,
    /// Optional short note describing the edit.
    pub edit_summary: Option<String>,
    /// When the revision was committed.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// Response from `GET /api/v1/recent-changes`.
///
/// Items are ordered newest first. `next_cursor` is `None` once the feed has
/// been exhausted; otherwise pass it back as `?cursor=…` to fetch the next
/// page. The cursor encodes a fixed `(created_at, id)` boundary so it stays
/// stable even when new edits land between calls.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RecentChangesResponse {
    /// Rows in this batch, ordered `(created_at DESC, id DESC)`.
    pub items: Vec<RecentChangeView>,
    /// Token to fetch the next page, or `None` if the feed has been
    /// exhausted.
    pub next_cursor: Option<String>,
}

/// Query parameters for `GET /api/v1/recent-changes`.
#[derive(Debug, Clone, Default, Deserialize, utoipa::IntoParams)]
pub struct RecentChangesQuery {
    /// RFC 3339 timestamp. Only revisions committed at or after this point
    /// are returned. Omit to include all history.
    #[serde(default)]
    pub since: Option<String>,
    /// Namespace slug to filter on. Omit to include every namespace.
    #[serde(default)]
    pub namespace: Option<String>,
    /// Username to filter on. Only revisions authored by this user are
    /// returned.
    #[serde(default)]
    pub actor: Option<String>,
    /// Opaque cursor returned by a previous call. Omit to start from the
    /// newest entry.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Page size. Clamped to
    /// [`thewiki_storage::repo::MAX_PAGE_SIZE`]. `0`/missing falls back to
    /// the route-level default.
    #[serde(default)]
    pub limit: Option<u32>,
}
