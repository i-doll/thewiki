//! Namespace-aware page routes mounted at `/api/v1/wiki/{namespace}` (#28).
//!
//! These handlers are the canonical entry points for page CRUD on a wiki
//! with multiple namespaces. They mirror the legacy `/api/v1/pages/...`
//! routes — which assume the `Main` namespace — and delegate to the same
//! shared handler bodies in [`crate::pages::routes`] so behaviour stays
//! identical.
//!
//! URL shape:
//!
//! - `POST   /api/v1/wiki/{namespace}` — create page in namespace
//! - `GET    /api/v1/wiki/{namespace}/{slug}` — fetch page
//! - `PUT    /api/v1/wiki/{namespace}/{slug}` — update page
//! - `DELETE /api/v1/wiki/{namespace}/{slug}` — delete page
//! - `GET    /api/v1/wiki/{namespace}/{slug}/backlinks`
//! - `GET    /api/v1/wiki/{namespace}/{slug}/revisions`
//! - `GET    /api/v1/wiki/{namespace}/{slug}/diff`
//! - `POST   /api/v1/wiki/{namespace}/{slug}/revert`
//! - `POST   /api/v1/wiki/{namespace}/{slug}/protect`

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use thewiki_core::NamespaceSlug;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::error::ApiError;
use crate::extractors::{EditorExtractor, RequireAuth};
use crate::pages::dto::{
    BacklinkListResponse, CreatePageRequest, ListBacklinksQuery, PageView, UpdatePageRequest,
};
use crate::pages::protect::ProtectRequest;
use crate::pages::revert::RevertRequest;
use crate::pages::revisions::{DiffQuery, DiffResponse, ListRevisionsQuery, RevisionListResponse};
use crate::pages::{protect, revert, revisions, routes};
use crate::state::{AppState, AppStorage};

/// Parse a path-segment namespace slug. `400 Bad Request` on validation
/// failure.
fn parse_path_namespace(raw: &str) -> Result<NamespaceSlug, ApiError> {
    NamespaceSlug::new(raw).map_err(|err| ApiError::InvalidInput(format!("namespace: {err}")))
}

/// `POST /api/v1/wiki/{namespace}` — create a page in a specific namespace.
#[utoipa::path(
    post,
    path = "/{namespace}",
    params(
        ("namespace" = String, Path, description = "Slug of the namespace to create the page in"),
        ("cookie" = Option<String>, Header, description = "Optional session and CSRF cookies"),
        ("x-csrf-token" = Option<String>, Header, description = "Double-submit CSRF token"),
    ),
    request_body = CreatePageRequest,
    responses(
        (status = 201, description = "Page created", body = PageView),
        (status = 202, description = "Edit accepted but pending approval", body = PageView),
        (status = 400, description = "Invalid input", body = crate::error::ErrorBody),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "CSRF token missing or invalid", body = crate::auth::error::AuthErrorBody),
        (status = 404, description = "Namespace not found", body = crate::error::ErrorBody),
        (status = 409, description = "Slug already taken", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "wiki",
)]
pub async fn create<S: AppStorage>(
    State(state): State<AppState<S>>,
    Path(namespace): Path<String>,
    editor: EditorExtractor,
    Json(req): Json<CreatePageRequest>,
) -> Result<(StatusCode, Json<PageView>), ApiError> {
    let ns = parse_path_namespace(&namespace)?;
    routes::create_page_in_namespace(state, editor, ns, req).await
}

/// `GET /api/v1/wiki/{namespace}/{slug}` — fetch a page.
#[utoipa::path(
    get,
    path = "/{namespace}/{slug}",
    params(
        ("namespace" = String, Path, description = "Namespace slug"),
        ("slug" = String, Path, description = "Page slug within the namespace"),
    ),
    responses(
        (status = 200, description = "Page", body = PageView),
        (status = 404, description = "Page or namespace not found", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "wiki",
)]
pub async fn get<S: AppStorage>(
    State(state): State<AppState<S>>,
    Path((namespace, slug)): Path<(String, String)>,
) -> Result<Json<PageView>, ApiError> {
    let ns = parse_path_namespace(&namespace)?;
    routes::get_page_in_namespace(state, ns, slug).await
}

/// `PUT /api/v1/wiki/{namespace}/{slug}` — commit a new revision.
#[utoipa::path(
    put,
    path = "/{namespace}/{slug}",
    params(
        ("namespace" = String, Path, description = "Namespace slug"),
        ("slug" = String, Path, description = "Page slug within the namespace"),
        ("cookie" = Option<String>, Header, description = "Optional session and CSRF cookies"),
        ("x-csrf-token" = Option<String>, Header, description = "Double-submit CSRF token"),
    ),
    request_body = UpdatePageRequest,
    responses(
        (status = 200, description = "Page updated", body = PageView),
        (status = 202, description = "Edit accepted but pending approval", body = PageView),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "CSRF token missing or invalid", body = crate::auth::error::AuthErrorBody),
        (status = 404, description = "Page or namespace not found", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "wiki",
)]
pub async fn update<S: AppStorage>(
    State(state): State<AppState<S>>,
    Path((namespace, slug)): Path<(String, String)>,
    editor: EditorExtractor,
    Json(req): Json<UpdatePageRequest>,
) -> Result<(StatusCode, Json<PageView>), ApiError> {
    let ns = parse_path_namespace(&namespace)?;
    routes::update_page_in_namespace(state, ns, slug, editor, req).await
}

/// `DELETE /api/v1/wiki/{namespace}/{slug}` — remove a page and its
/// revisions.
#[utoipa::path(
    delete,
    path = "/{namespace}/{slug}",
    params(
        ("namespace" = String, Path, description = "Namespace slug"),
        ("slug" = String, Path, description = "Page slug within the namespace"),
        ("cookie" = Option<String>, Header, description = "Optional session and CSRF cookies"),
        ("x-csrf-token" = Option<String>, Header, description = "Double-submit CSRF token"),
    ),
    responses(
        (status = 204, description = "Page deleted"),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "CSRF token missing or invalid", body = crate::auth::error::AuthErrorBody),
        (status = 404, description = "Page or namespace not found", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "wiki",
)]
pub async fn delete<S: AppStorage>(
    State(state): State<AppState<S>>,
    Path((namespace, slug)): Path<(String, String)>,
    editor: EditorExtractor,
) -> Result<StatusCode, ApiError> {
    let ns = parse_path_namespace(&namespace)?;
    routes::delete_page_in_namespace(state, ns, slug, editor).await
}

/// `GET /api/v1/wiki/{namespace}/{slug}/backlinks`.
#[utoipa::path(
    get,
    path = "/{namespace}/{slug}/backlinks",
    params(
        ("namespace" = String, Path, description = "Namespace slug"),
        ("slug" = String, Path, description = "Page slug within the namespace"),
        ListBacklinksQuery,
    ),
    responses(
        (status = 200, description = "Backlinks list", body = BacklinkListResponse),
        (status = 404, description = "Namespace not found", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "wiki",
)]
pub async fn list_backlinks<S: AppStorage>(
    State(state): State<AppState<S>>,
    Path((namespace, slug)): Path<(String, String)>,
    Query(query): Query<ListBacklinksQuery>,
) -> Result<Json<BacklinkListResponse>, ApiError> {
    let ns = parse_path_namespace(&namespace)?;
    routes::list_backlinks_in_namespace(state, ns, slug, query).await
}

/// `GET /api/v1/wiki/{namespace}/{slug}/revisions`.
#[utoipa::path(
    get,
    path = "/{namespace}/{slug}/revisions",
    params(
        ("namespace" = String, Path, description = "Namespace slug"),
        ("slug" = String, Path, description = "Page slug within the namespace"),
        ListRevisionsQuery,
    ),
    responses(
        (status = 200, description = "Revision history", body = RevisionListResponse),
        (status = 404, description = "Page or namespace not found", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "wiki",
)]
pub async fn list_revisions<S: AppStorage>(
    State(state): State<AppState<S>>,
    Path((namespace, slug)): Path<(String, String)>,
    Query(query): Query<ListRevisionsQuery>,
) -> Result<Json<RevisionListResponse>, ApiError> {
    let ns = parse_path_namespace(&namespace)?;
    revisions::list_revisions_in_namespace(state, ns, slug, query).await
}

/// `GET /api/v1/wiki/{namespace}/{slug}/diff`.
#[utoipa::path(
    get,
    path = "/{namespace}/{slug}/diff",
    params(
        ("namespace" = String, Path, description = "Namespace slug"),
        ("slug" = String, Path, description = "Page slug within the namespace"),
        DiffQuery,
    ),
    responses(
        (status = 200, description = "Diff between two revisions", body = DiffResponse),
        (status = 404, description = "Page or revision not found", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "wiki",
)]
pub async fn diff_revisions<S: AppStorage>(
    State(state): State<AppState<S>>,
    Path((namespace, slug)): Path<(String, String)>,
    Query(query): Query<DiffQuery>,
) -> Result<Json<DiffResponse>, ApiError> {
    let ns = parse_path_namespace(&namespace)?;
    revisions::diff_revisions_in_namespace(state, ns, slug, query).await
}

/// `POST /api/v1/wiki/{namespace}/{slug}/revert`.
#[utoipa::path(
    post,
    path = "/{namespace}/{slug}/revert",
    params(
        ("namespace" = String, Path, description = "Namespace slug"),
        ("slug" = String, Path, description = "Page slug within the namespace"),
        ("cookie" = String, Header, description = "Session and CSRF cookies"),
        ("x-csrf-token" = String, Header, description = "Double-submit CSRF token"),
    ),
    request_body = RevertRequest,
    responses(
        (status = 200, description = "Page reverted; new revision committed", body = PageView),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "CSRF token missing or invalid", body = crate::auth::error::AuthErrorBody),
        (status = 404, description = "Page or revision not found", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "wiki",
)]
pub async fn revert<S: AppStorage>(
    State(state): State<AppState<S>>,
    Path((namespace, slug)): Path<(String, String)>,
    author: RequireAuth,
    Json(req): Json<RevertRequest>,
) -> Result<Json<PageView>, ApiError> {
    let ns = parse_path_namespace(&namespace)?;
    revert::revert_page_in_namespace(state, ns, slug, author, req).await
}

/// `POST /api/v1/wiki/{namespace}/{slug}/protect`.
#[utoipa::path(
    post,
    path = "/{namespace}/{slug}/protect",
    params(
        ("namespace" = String, Path, description = "Namespace slug"),
        ("slug" = String, Path, description = "Page slug within the namespace"),
        ("cookie" = String, Header, description = "Session and CSRF cookies"),
        ("x-csrf-token" = String, Header, description = "Double-submit CSRF token"),
    ),
    request_body = ProtectRequest,
    responses(
        (status = 200, description = "Protection level updated", body = PageView),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "Caller lacks PROTECT permission", body = crate::error::ErrorBody),
        (status = 404, description = "Page not found", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "wiki",
)]
pub async fn protect<S: AppStorage>(
    State(state): State<AppState<S>>,
    Path((namespace, slug)): Path<(String, String)>,
    actor: RequireAuth,
    Json(req): Json<ProtectRequest>,
) -> Result<Json<PageView>, ApiError> {
    let ns = parse_path_namespace(&namespace)?;
    protect::protect_page_in_namespace(state, ns, slug, actor, req).await
}

/// Build the namespace-aware wiki subrouter.
///
/// Mounted under `/api/v1/wiki` so the full URL of each handler reads as
/// `/api/v1/wiki/{namespace}/{slug}/...`.
pub fn router<S: AppStorage>() -> OpenApiRouter<AppState<S>> {
    OpenApiRouter::new()
        .routes(routes!(create))
        .routes(routes!(get, update, delete))
        .routes(routes!(list_backlinks))
        .routes(routes!(list_revisions))
        .routes(routes!(diff_revisions))
        .routes(routes!(revert))
        .routes(routes!(protect))
}
