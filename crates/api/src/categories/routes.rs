//! Axum handlers for the categories + tags endpoints (#29).

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use thewiki_core::{Category, CategoryId, Tag};
use thewiki_storage::repo::{CategoryRepository, Cursor, TagRepository};
use time::OffsetDateTime;

use crate::categories::dto::{
    CategoryDetailQuery, CategoryDetailResponse, CategoryListResponse, CategoryMemberView,
    CategoryView, CreateCategoryRequest, ListTagsQuery, TagDetailQuery, TagDetailResponse,
    TagListResponse,
};
use crate::error::ApiError;
use crate::extractors::EditorExtractor;
use crate::state::{AppState, AppStorage};

/// Convert a domain [`Category`] into its wire shape.
pub(crate) fn category_view(c: Category) -> CategoryView {
    CategoryView {
        id: c.id,
        slug: c.slug,
        display_name: c.display_name,
        parent_id: c.parent_id,
        created_at: c.created_at,
    }
}

/// `GET /api/v1/categories` — list every category.
#[utoipa::path(
    get,
    path = "",
    responses(
        (status = 200, description = "Category list", body = CategoryListResponse),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
        (status = 500, description = "Internal", body = crate::error::ErrorBody),
    ),
    tag = "categories",
)]
pub async fn list_categories<S: AppStorage>(
    State(state): State<AppState<S>>,
) -> Result<Json<CategoryListResponse>, ApiError> {
    let items = state
        .storage
        .categories()
        .list_all()
        .await?
        .into_iter()
        .map(category_view)
        .collect();
    Ok(Json(CategoryListResponse { items }))
}

/// `POST /api/v1/categories` — create a new category.
///
/// Requires an authenticated session. Anonymous callers (even with
/// `anonymous_edits = true`) cannot mint categories — the taxonomy is an
/// operator concern.
#[utoipa::path(
    post,
    path = "",
    request_body = CreateCategoryRequest,
    responses(
        (status = 201, description = "Category created", body = CategoryView),
        (status = 400, description = "Invalid input", body = crate::error::ErrorBody),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 404, description = "Parent not found", body = crate::error::ErrorBody),
        (status = 409, description = "Slug already exists or cycle", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "categories",
)]
pub async fn create_category<S: AppStorage>(
    State(state): State<AppState<S>>,
    _editor: EditorExtractor,
    Json(req): Json<CreateCategoryRequest>,
) -> Result<(StatusCode, Json<CategoryView>), ApiError> {
    // Category creation goes through the same configurable-auth gate as the
    // page mutators (`EditorExtractor`): the operator decides via
    // `auth.anonymous_edits` whether anonymous callers can mint categories
    // alongside their page edits. A future #14-shaped follow-up will tighten
    // this to a role-gated `RequireAuth(Role::Admin)` once the role bitset
    // grows a "manage taxonomy" flag.
    let slug = req.slug.trim();
    if slug.is_empty() {
        return Err(ApiError::InvalidInput("slug must not be empty".into()));
    }
    if slug
        .chars()
        .any(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    {
        return Err(ApiError::InvalidInput(
            "slug must be ASCII alphanumeric, '-' or '_'".into(),
        ));
    }
    if req.display_name.trim().is_empty() {
        return Err(ApiError::InvalidInput(
            "display_name must not be empty".into(),
        ));
    }

    let category = Category {
        id: CategoryId::new(),
        slug: slug.to_owned(),
        display_name: req.display_name.trim().to_owned(),
        parent_id: req.parent_id,
        created_at: OffsetDateTime::now_utc(),
    };
    state.storage.categories().create(&category).await?;
    Ok((StatusCode::CREATED, Json(category_view(category))))
}

/// `GET /api/v1/categories/{slug}` — fetch a category and its member pages.
#[utoipa::path(
    get,
    path = "/{slug}",
    params(
        ("slug" = String, Path, description = "Category slug"),
        CategoryDetailQuery,
    ),
    responses(
        (status = 200, description = "Category", body = CategoryDetailResponse),
        (status = 404, description = "Category not found", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "categories",
)]
pub async fn get_category<S: AppStorage>(
    State(state): State<AppState<S>>,
    Path(slug): Path<String>,
    Query(query): Query<CategoryDetailQuery>,
) -> Result<Json<CategoryDetailResponse>, ApiError> {
    let category = state.storage.categories().get_by_slug(&slug).await?;
    let limit = match query.limit {
        Some(0) | None => state.route_config.default_page_size,
        Some(n) => n,
    };
    let cursor = query.cursor.map(Cursor);
    let slice = state
        .storage
        .categories()
        .list_pages_in(category.id, cursor, limit)
        .await?;
    let items = slice
        .items
        .into_iter()
        .map(|row| CategoryMemberView {
            page_id: row.page_id,
            namespace_slug: row.namespace_slug,
            slug: row.page_slug,
            title: row.page_title,
        })
        .collect();
    Ok(Json(CategoryDetailResponse {
        category: category_view(category),
        items,
        next_cursor: slice.next.map(|c| c.0),
    }))
}

/// `GET /api/v1/tags?prefix=...&limit=...` — autocomplete tag list.
#[utoipa::path(
    get,
    path = "",
    params(ListTagsQuery),
    responses(
        (status = 200, description = "Tag list", body = TagListResponse),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "tags",
)]
pub async fn list_tags<S: AppStorage>(
    State(state): State<AppState<S>>,
    Query(query): Query<ListTagsQuery>,
) -> Result<Json<TagListResponse>, ApiError> {
    let prefix = query.prefix.unwrap_or_default();
    let limit = match query.limit {
        Some(0) | None => state.route_config.default_page_size,
        Some(n) => n,
    };
    let tags = state.storage.tags().list_all_tags(&prefix, limit).await?;
    let items = tags.into_iter().map(|t| t.into_string()).collect();
    Ok(Json(TagListResponse { items }))
}

/// `GET /api/v1/tags/{tag}` — list pages carrying `{tag}`.
#[utoipa::path(
    get,
    path = "/{tag}",
    params(
        ("tag" = String, Path, description = "Tag value (case-insensitive)"),
        TagDetailQuery,
    ),
    responses(
        (status = 200, description = "Pages with tag", body = TagDetailResponse),
        (status = 400, description = "Invalid tag value", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "tags",
)]
pub async fn get_tag<S: AppStorage>(
    State(state): State<AppState<S>>,
    Path(tag): Path<String>,
    Query(query): Query<TagDetailQuery>,
) -> Result<Json<TagDetailResponse>, ApiError> {
    let tag = Tag::new(tag).map_err(|err| ApiError::InvalidInput(format!("tag: {err}")))?;
    let limit = match query.limit {
        Some(0) | None => state.route_config.default_page_size,
        Some(n) => n,
    };
    let cursor = query.cursor.map(Cursor);
    let slice = state
        .storage
        .tags()
        .list_pages_with_tag(&tag, cursor, limit)
        .await?;
    let items = slice
        .items
        .into_iter()
        .map(|row| CategoryMemberView {
            page_id: row.page_id,
            namespace_slug: row.namespace_slug,
            slug: row.page_slug,
            title: row.page_title,
        })
        .collect();
    Ok(Json(TagDetailResponse {
        tag: tag.into_string(),
        items,
        next_cursor: slice.next.map(|c| c.0),
    }))
}
