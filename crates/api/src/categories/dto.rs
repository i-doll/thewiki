//! Wire shapes for the categories endpoints (#29).

use serde::{Deserialize, Serialize};
use thewiki_core::{CategoryId, PageId};
use time::OffsetDateTime;
use utoipa::ToSchema;

/// A category, flattened for the wire form.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[non_exhaustive]
pub struct CategoryView {
    /// Stable identifier.
    pub id: CategoryId,
    /// URL slug, unique across all categories.
    pub slug: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Optional parent category id.
    pub parent_id: Option<CategoryId>,
    /// When the row was created.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// Body for `POST /api/v1/categories`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateCategoryRequest {
    /// URL slug. Must be unique across all categories.
    pub slug: String,
    /// Human-readable display label.
    pub display_name: String,
    /// Optional parent category id. Setting `None` makes this a top-level
    /// category. Mutually checked against the storage layer's cycle check.
    #[serde(default)]
    pub parent_id: Option<CategoryId>,
}

/// A page entry surfaced by the "members of this category / tag" lists.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CategoryMemberView {
    /// Stable identifier of the member page.
    pub page_id: PageId,
    /// Namespace slug the page lives in.
    pub namespace_slug: String,
    /// URL slug of the page.
    pub slug: String,
    /// Human-readable title.
    pub title: String,
}

/// Response from `GET /api/v1/categories/{slug}`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CategoryDetailResponse {
    /// The category itself.
    pub category: CategoryView,
    /// Member pages in this category, paginated.
    pub items: Vec<CategoryMemberView>,
    /// Next-page cursor, or `None` if exhausted.
    pub next_cursor: Option<String>,
}

/// Response from `GET /api/v1/categories`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CategoryListResponse {
    /// Every defined category, ordered by slug.
    pub items: Vec<CategoryView>,
}

/// Query parameters for `GET /api/v1/categories/{slug}`.
#[derive(Debug, Clone, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct CategoryDetailQuery {
    /// Opaque cursor returned by a previous call.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Page size. `0`/missing falls back to the route-level default.
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Query parameters for `GET /api/v1/tags?prefix=...`.
#[derive(Debug, Clone, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListTagsQuery {
    /// Optional prefix to match. An empty / missing prefix lists every
    /// tag, clamped by `limit`.
    #[serde(default)]
    pub prefix: Option<String>,
    /// Max number of tags to return. `0`/missing falls back to the
    /// route-level default.
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Response from `GET /api/v1/tags?prefix=...`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TagListResponse {
    /// Matching tags, ordered lexicographically.
    pub items: Vec<String>,
}

/// Response from `GET /api/v1/tags/{tag}`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TagDetailResponse {
    /// The tag value as it would appear on `page_tags.tag`.
    pub tag: String,
    /// Pages carrying this tag.
    pub items: Vec<CategoryMemberView>,
    /// Next-page cursor, or `None` if exhausted.
    pub next_cursor: Option<String>,
}

/// Query parameters for `GET /api/v1/tags/{tag}`.
#[derive(Debug, Clone, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TagDetailQuery {
    /// Opaque cursor returned by a previous call.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Page size.
    #[serde(default)]
    pub limit: Option<u32>,
}
