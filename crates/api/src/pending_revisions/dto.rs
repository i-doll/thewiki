//! Request and response payloads for the approval queue endpoints (#40).

use serde::{Deserialize, Serialize};
use thewiki_core::{
    NamespaceId, PageId, PendingRevisionId, PendingRevisionStatus, RevisionId, UserId,
};
use time::OffsetDateTime;
use utoipa::ToSchema;

/// One row in the reviewer-facing pending list.
///
/// `body` is intentionally omitted from the listing shape: the queue is
/// usually long and the SPA fetches the full row through
/// [`PendingRevisionDetailResponse`] when the reviewer opens an entry. The
/// page slug + author label is joined in so the list can render without a
/// second round trip.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PendingRevisionView {
    /// Stable identifier.
    pub id: PendingRevisionId,
    /// Page the edit targets.
    pub page_id: PageId,
    /// Namespace the target page lives in (joined in).
    pub namespace_id: NamespaceId,
    /// Slug of the namespace (joined in).
    pub namespace_slug: String,
    /// Slug of the page (joined in).
    pub page_slug: String,
    /// Title of the page at queue time (joined in).
    pub page_title: String,
    /// Parent revision id the edit was based on.
    pub parent_revision_id: Option<RevisionId>,
    /// Authenticated author id, or `None` for anonymous edits.
    pub author_id: Option<UserId>,
    /// Username snapshot, joined in for anonymous-friendly display.
    pub author_label: String,
    /// Operator-visible comment / edit summary attached to the edit.
    pub comment: String,
    /// Lifecycle state.
    pub status: PendingRevisionStatus,
    /// Reviewer who decided the row, or `None` while pending.
    pub reviewer_id: Option<UserId>,
    /// When the reviewer acted, or `None` while pending.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub decided_at: Option<OffsetDateTime>,
    /// Operator-visible note attached to a rejection.
    pub rejection_reason: Option<String>,
    /// When the row was queued.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// Response from `GET /api/v1/pending-revisions`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PendingRevisionListResponse {
    /// Rows in this batch, newest first.
    pub items: Vec<PendingRevisionView>,
    /// Token to fetch the next page, or `None` when the listing is
    /// exhausted.
    pub next_cursor: Option<String>,
    /// Total count of pending rows (matches the filter when `status` is
    /// provided, otherwise counts everything). Useful for the reviewer
    /// UI's "N pending" badge.
    pub total: u64,
}

/// Query parameters for `GET /api/v1/pending-revisions`.
#[derive(Debug, Clone, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListPendingRevisionsQuery {
    /// Restrict to one lifecycle status — `pending`, `approved`, or
    /// `rejected`. Defaults to `pending` so the reviewer queue stays
    /// uncluttered.
    #[serde(default)]
    pub status: Option<String>,
    /// Opaque cursor returned by a previous call.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Page size. Clamped to
    /// [`thewiki_storage::repo::MAX_PAGE_SIZE`].
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Detail shape returned by `GET /api/v1/pending-revisions/{id}`.
///
/// Carries the full proposed body so the reviewer's diff view can render
/// without a second round trip.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PendingRevisionDetailResponse {
    /// List-shape metadata (id, status, page coordinates, …).
    #[serde(flatten)]
    pub view: PendingRevisionView,
    /// Full proposed Markdown body.
    pub body: String,
    /// Body of the parent revision the edit was based on, joined in so the
    /// SPA can render a diff against it. `None` when the page didn't have
    /// a head at queue time (the row proposes the initial revision).
    pub parent_body: Option<String>,
}

/// Body of `POST /api/v1/pending-revisions/{id}/reject`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct RejectPendingRevisionRequest {
    /// Operator-visible reason. Stored verbatim and echoed back to the
    /// (authenticated) author through the in-app inbox.
    pub reason: String,
}
