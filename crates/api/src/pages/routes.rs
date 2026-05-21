//! Axum handlers for the page CRUD endpoints.
//!
//! Each handler is generic over the storage facade (`S: AppStorage`) so the
//! route layer stays backend-agnostic. The handler bodies stay small and
//! readable; cross-cutting work — error mapping, default page sizes — lives
//! in [`crate::error`] and [`crate::state`].

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use thewiki_core::{ContentFormat, NamespaceSlug, Page, PageId, ProtectionLevel, Revision};
use thewiki_storage::repo::{Cursor, NamespaceRepository, PageRepository, RevisionRepository};
use time::OffsetDateTime;

use crate::error::ApiError;
use crate::extractors::RequireAuth;
use crate::pages::dto::{
    CreatePageRequest, ListPagesQuery, PageListItem, PageListResponse, PageView, UpdatePageRequest,
};
use crate::state::{AppState, AppStorage};

/// Default namespace slug used when a request doesn't carry one.
///
/// TODO(#28): once namespace prefix routing lands, the namespace will be
/// part of the path. Until then, every request resolves against this slug.
const DEFAULT_NAMESPACE: &str = "Main";

/// Parse a caller-supplied namespace slug, falling back to [`DEFAULT_NAMESPACE`].
fn parse_namespace_slug(raw: Option<&str>) -> Result<NamespaceSlug, ApiError> {
    let value = raw.unwrap_or(DEFAULT_NAMESPACE);
    NamespaceSlug::new(value)
        .map_err(|err| ApiError::InvalidInput(format!("namespace_slug: {err}")))
}

/// Look up a namespace, mapping the storage-level "not found" to the API-
/// level 404 unchanged.
async fn resolve_namespace<S: AppStorage>(
    state: &AppState<S>,
    slug: &NamespaceSlug,
) -> Result<thewiki_core::Namespace, ApiError> {
    state
        .storage
        .namespaces()
        .get_by_slug(slug)
        .await
        .map_err(ApiError::from)
}

/// Build a [`PageView`] for a freshly-loaded page, joining in the namespace
/// slug and the current revision's body.
async fn hydrate_page_view<S: AppStorage>(
    state: &AppState<S>,
    page: Page,
    namespace_slug: String,
) -> Result<PageView, ApiError> {
    let content = match page.current_revision_id {
        Some(rev_id) => state
            .storage
            .revisions()
            .get_by_id(rev_id)
            .await
            .map(|r| r.body)
            // A dangling `current_revision_id` shouldn't happen — the
            // schema's FK is `ON DELETE SET NULL` — but if it does we'd
            // rather return an empty body than 500 the client.
            .unwrap_or_default(),
        None => String::new(),
    };
    Ok(PageView {
        id: page.id,
        namespace_id: page.namespace_id,
        namespace_slug,
        slug: page.slug,
        title: page.title,
        current_revision_id: page.current_revision_id,
        content,
        created_at: page.created_at,
        updated_at: page.updated_at,
    })
}

/// `POST /api/v1/pages` — create a page plus its initial revision.
///
/// Steps:
/// 1. Resolve the namespace by slug (404 if missing).
/// 2. Insert the page row with `current_revision_id = NULL`.
/// 3. Insert the initial revision, authored by the caller.
/// 4. Update the page row to point at the new revision.
///
/// The schema's `pages.current_revision_id` FK is `ON DELETE SET NULL`, so
/// the brief NULL state in step 2 is legitimate even with FK enforcement on.
#[utoipa::path(
    post,
    path = "",
    request_body = CreatePageRequest,
    responses(
        (status = 201, description = "Page created", body = PageView),
        (status = 400, description = "Invalid input", body = crate::error::ErrorBody),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 404, description = "Namespace not found", body = crate::error::ErrorBody),
        (status = 409, description = "Slug already taken", body = crate::error::ErrorBody),
    ),
    tag = "pages",
)]
pub async fn create_page<S: AppStorage>(
    State(state): State<AppState<S>>,
    RequireAuth(author_id): RequireAuth,
    Json(req): Json<CreatePageRequest>,
) -> Result<(StatusCode, Json<PageView>), ApiError> {
    if req.slug.trim().is_empty() {
        return Err(ApiError::InvalidInput("slug must not be empty".into()));
    }
    if req.title.trim().is_empty() {
        return Err(ApiError::InvalidInput("title must not be empty".into()));
    }

    let namespace_slug = parse_namespace_slug(Some(&req.namespace_slug))?;
    let namespace = resolve_namespace(&state, &namespace_slug).await?;

    let now = OffsetDateTime::now_utc();
    let mut page = Page {
        id: PageId::new(),
        namespace_id: namespace.id,
        slug: req.slug,
        title: req.title,
        current_revision_id: None,
        content_format: ContentFormat::Markdown,
        protection_level: ProtectionLevel::None,
        created_at: now,
        updated_at: now,
    };

    state.storage.pages().create(&page).await?;

    let revision = Revision::new(page.id, None, author_id, req.content, None);
    state.storage.revisions().create(&revision).await?;

    page.current_revision_id = Some(revision.id);
    page.updated_at = OffsetDateTime::now_utc();
    state.storage.pages().update(&page).await?;

    let view = hydrate_page_view(&state, page, namespace.slug.into_string()).await?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// `GET /api/v1/pages/{slug}` — fetch a page by slug in the default namespace.
///
/// Read is open today (TODO(#13): swap to real auth + permissions checks).
#[utoipa::path(
    get,
    path = "/{slug}",
    params(("slug" = String, Path, description = "URL slug within the default namespace")),
    responses(
        (status = 200, description = "Page", body = PageView),
        (status = 404, description = "Page not found", body = crate::error::ErrorBody),
    ),
    tag = "pages",
)]
pub async fn get_page<S: AppStorage>(
    State(state): State<AppState<S>>,
    Path(slug): Path<String>,
) -> Result<Json<PageView>, ApiError> {
    let namespace_slug = parse_namespace_slug(None)?;
    let namespace = resolve_namespace(&state, &namespace_slug).await?;
    let page = state
        .storage
        .pages()
        .get_by_namespace_and_slug(namespace.id, &slug)
        .await?;
    let view = hydrate_page_view(&state, page, namespace.slug.into_string()).await?;
    Ok(Json(view))
}

/// `PUT /api/v1/pages/{slug}` — commit a new revision.
///
/// Title is optional (keeps the existing title when omitted); content always
/// produces a new revision row.
#[utoipa::path(
    put,
    path = "/{slug}",
    params(("slug" = String, Path, description = "URL slug within the default namespace")),
    request_body = UpdatePageRequest,
    responses(
        (status = 200, description = "Page updated", body = PageView),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 404, description = "Page not found", body = crate::error::ErrorBody),
    ),
    tag = "pages",
)]
pub async fn update_page<S: AppStorage>(
    State(state): State<AppState<S>>,
    Path(slug): Path<String>,
    RequireAuth(author_id): RequireAuth,
    Json(req): Json<UpdatePageRequest>,
) -> Result<Json<PageView>, ApiError> {
    let namespace_slug = parse_namespace_slug(None)?;
    let namespace = resolve_namespace(&state, &namespace_slug).await?;
    let mut page = state
        .storage
        .pages()
        .get_by_namespace_and_slug(namespace.id, &slug)
        .await?;

    let revision = Revision::new(
        page.id,
        page.current_revision_id,
        author_id,
        req.content,
        req.edit_summary,
    );
    state.storage.revisions().create(&revision).await?;

    if let Some(title) = req.title {
        if title.trim().is_empty() {
            return Err(ApiError::InvalidInput("title must not be empty".into()));
        }
        page.title = title;
    }
    page.current_revision_id = Some(revision.id);
    page.updated_at = OffsetDateTime::now_utc();
    state.storage.pages().update(&page).await?;

    let view = hydrate_page_view(&state, page, namespace.slug.into_string()).await?;
    Ok(Json(view))
}

/// `DELETE /api/v1/pages/{slug}` — remove a page and all its revisions.
///
/// The `revisions` table has `ON DELETE CASCADE` on `page_id`, so wiping the
/// page row collapses the history. Today any authenticated user can delete a
/// page (TODO(#14): require the `admin` role).
#[utoipa::path(
    delete,
    path = "/{slug}",
    params(("slug" = String, Path, description = "URL slug within the default namespace")),
    responses(
        (status = 204, description = "Page deleted"),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "Forbidden", body = crate::error::ErrorBody),
        (status = 404, description = "Page not found", body = crate::error::ErrorBody),
    ),
    tag = "pages",
)]
pub async fn delete_page<S: AppStorage>(
    State(state): State<AppState<S>>,
    Path(slug): Path<String>,
    // TODO(#14): replace this placeholder check with a real role-gated
    // extractor — `RequireRole(Role::Admin)` or similar. For now any
    // authenticated caller may delete; the bare `RequireAuth` covers the
    // 401-vs-403 distinction.
    RequireAuth(_): RequireAuth,
) -> Result<StatusCode, ApiError> {
    let namespace_slug = parse_namespace_slug(None)?;
    let namespace = resolve_namespace(&state, &namespace_slug).await?;
    let page = state
        .storage
        .pages()
        .get_by_namespace_and_slug(namespace.id, &slug)
        .await?;
    state.storage.pages().delete(page.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/v1/pages` — list pages, cursor-paginated.
#[utoipa::path(
    get,
    path = "",
    params(ListPagesQuery),
    responses(
        (status = 200, description = "Page list", body = PageListResponse),
        (status = 404, description = "Namespace not found", body = crate::error::ErrorBody),
    ),
    tag = "pages",
)]
pub async fn list_pages<S: AppStorage>(
    State(state): State<AppState<S>>,
    Query(query): Query<ListPagesQuery>,
) -> Result<Json<PageListResponse>, ApiError> {
    let namespace_slug = parse_namespace_slug(query.namespace.as_deref())?;
    let namespace = resolve_namespace(&state, &namespace_slug).await?;

    let limit = match query.limit {
        Some(0) | None => state.route_config.default_page_size,
        Some(n) => n,
    };

    let cursor = query.cursor.map(Cursor);
    let slice = state
        .storage
        .pages()
        .list_in_namespace(namespace.id, cursor, limit)
        .await?;

    let namespace_slug_str = namespace.slug.into_string();
    let items = slice
        .items
        .into_iter()
        .map(|p| PageListItem {
            id: p.id,
            namespace_slug: namespace_slug_str.clone(),
            slug: p.slug,
            title: p.title,
            updated_at: p.updated_at,
        })
        .collect();

    Ok(Json(PageListResponse {
        items,
        next_cursor: slice.next.map(|c| c.0),
    }))
}
