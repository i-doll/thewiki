//! Request and response payloads for the namespace CRUD endpoints (#28).

use serde::{Deserialize, Serialize};
use thewiki_core::{Namespace, NamespaceId};
use utoipa::ToSchema;

/// A single namespace, returned by both the list and create endpoints.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct NamespaceView {
    /// Stable identifier.
    pub id: NamespaceId,
    /// URL-safe slug. Forms the `<namespace>` segment of `/wiki/<namespace>/<slug>`.
    pub slug: String,
    /// Human-readable display label.
    pub display_name: String,
}

impl From<Namespace> for NamespaceView {
    fn from(ns: Namespace) -> Self {
        Self {
            id: ns.id,
            slug: ns.slug.into_string(),
            display_name: ns.display_name,
        }
    }
}

/// Response from `GET /api/v1/namespaces`.
///
/// No pagination — operators rarely define more than a handful of
/// namespaces, so a single batch is plenty.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct NamespaceListResponse {
    /// Every namespace defined on this wiki, in creation order.
    pub items: Vec<NamespaceView>,
}

/// Body of `POST /api/v1/namespaces`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateNamespaceRequest {
    /// URL slug. Must satisfy
    /// [`thewiki_core::NamespaceSlug`](thewiki_core::NamespaceSlug)'s
    /// validation rules: ASCII alphanumerics, `_`, `-` (no `:`).
    pub slug: String,
    /// Human-readable display name.
    pub display_name: String,
}

/// Body of `PATCH /api/v1/namespaces/{slug}`.
///
/// Slug renames are deliberately not supported — they invalidate URLs and
/// have cascading effects on the link graph, search index, and audit log.
/// Operators who really need to rename a namespace can create a new one,
/// move pages across via the admin tools (future work), and delete the
/// empty original.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateNamespaceRequest {
    /// New display name.
    pub display_name: String,
}
