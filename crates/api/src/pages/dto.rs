//! Request and response payloads for the page CRUD endpoints.
//!
//! Every type here derives [`Serialize`]/[`Deserialize`] for the wire form and
//! [`ToSchema`] so the OpenAPI surface picks it up automatically. The shapes
//! are intentionally narrower than the domain entities — `PageView` for
//! instance flattens the namespace slug onto the response so clients don't
//! need a second round trip just to render a breadcrumb.

use serde::{Deserialize, Serialize};
use thewiki_core::{NamespaceId, PageId, RevisionId};
use time::OffsetDateTime;
use utoipa::ToSchema;

/// Body of `POST /api/v1/pages`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreatePageRequest {
    /// Slug of the namespace this page lives in. The namespace must already
    /// exist; the API does not create namespaces on demand.
    pub namespace_slug: String,
    /// URL-safe slug, unique within `namespace_slug`.
    pub slug: String,
    /// Human-readable title shown in the UI.
    pub title: String,
    /// Initial body for the page. The first revision is committed with this
    /// content.
    pub content: String,
}

/// Body of `PUT /api/v1/pages/{slug}`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdatePageRequest {
    /// New title. Omitting it keeps the existing title.
    #[serde(default)]
    pub title: Option<String>,
    /// New body. Always required — an update commits a new revision.
    pub content: String,
    /// Optional short note describing the edit (think Git commit message).
    #[serde(default)]
    pub edit_summary: Option<String>,
}

/// A single page returned by the read endpoints.
///
/// `content` is the body of the current revision (joined in for convenience).
/// Listing endpoints use the lighter [`PageListItem`] instead so they don't
/// have to ship every page's full body.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PageView {
    /// Stable identifier.
    pub id: PageId,
    /// Namespace this page lives in.
    pub namespace_id: NamespaceId,
    /// Slug of the namespace; joined in so clients don't need a second
    /// round trip just to render a breadcrumb.
    pub namespace_slug: String,
    /// URL slug, unique within the namespace.
    pub slug: String,
    /// Human-readable title.
    pub title: String,
    /// Pointer to the current head revision. `None` only in the transient
    /// state between page creation and the first revision being committed
    /// (today that window is closed inside `POST /api/v1/pages`).
    pub current_revision_id: Option<RevisionId>,
    /// Body of the current revision, or empty string if no revision exists.
    pub content: String,
    /// When the page row was first created.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// When the page row was last touched.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// Lighter representation of a page used inside [`PageListResponse`].
///
/// Lacks `content` and `namespace_id`; clients listing a namespace already
/// know the namespace.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PageListItem {
    /// Stable identifier.
    pub id: PageId,
    /// Slug of the namespace this page lives in.
    pub namespace_slug: String,
    /// URL slug.
    pub slug: String,
    /// Human-readable title.
    pub title: String,
    /// When the page row was last touched.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// Response from `GET /api/v1/pages?cursor=…&limit=…`.
///
/// `next_cursor` is `None` once the listing has been exhausted; otherwise
/// pass it back as `?cursor=…` to fetch the next page.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PageListResponse {
    /// Rows in this batch, ordered `(created_at ASC, id ASC)` per the
    /// storage layer's contract.
    pub items: Vec<PageListItem>,
    /// Token to fetch the next page, or `None` if there are no more pages.
    pub next_cursor: Option<String>,
}

/// Query parameters for `GET /api/v1/pages`.
#[derive(Debug, Clone, Default, Deserialize, utoipa::IntoParams)]
pub struct ListPagesQuery {
    /// Namespace slug to list pages from. Defaults to `Main` if absent.
    /// Namespace prefix routing lands with #28.
    #[serde(default)]
    pub namespace: Option<String>,
    /// Opaque cursor returned by a previous call. Omit to start from the
    /// beginning.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Page size. Clamped to
    /// [`thewiki_storage::repo::MAX_PAGE_SIZE`]. `0`/missing falls back to
    /// the route-level default.
    #[serde(default)]
    pub limit: Option<u32>,
}
