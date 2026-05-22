//! Axum handlers for the namespace CRUD endpoints (#28).
//!
//! Reads are open; mutations require [`Permissions::MANAGE_NAMESPACES`] on
//! the caller's session. The handlers are deliberately small: validate the
//! slug, dispatch to the storage repo, surface storage errors as the right
//! HTTP code via [`From<StorageError>`](crate::error::ApiError::from).

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::json;
use thewiki_core::{Namespace, NamespaceId, NamespaceSlug, Permissions};
use thewiki_storage::repo::{
    AuditLogRepository, NamespaceRepository, NewAuditLogEntry, PageRepository,
};

use crate::error::ApiError;
use crate::extractors::RequireAuth;
use crate::namespaces::dto::{
    CreateNamespaceRequest, NamespaceListResponse, NamespaceView, UpdateNamespaceRequest,
};
use crate::state::{AppState, AppStorage};

/// Verify the calling session carries [`Permissions::MANAGE_NAMESPACES`].
///
/// Returns [`ApiError::Forbidden`] when the bit is missing so the SPA can
/// render a clean "you can't do that" surface. Authentication itself is
/// handled by the [`RequireAuth`] extractor on each handler.
fn require_manage(actor: &RequireAuth) -> Result<(), ApiError> {
    if actor.permissions.contains(Permissions::MANAGE_NAMESPACES) {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

/// Parse a caller-supplied namespace slug, mapping validation failures to
/// `400 Bad Request`.
fn parse_slug(raw: &str) -> Result<NamespaceSlug, ApiError> {
    NamespaceSlug::new(raw).map_err(|err| ApiError::InvalidInput(format!("slug: {err}")))
}

/// Build one namespace-targeted audit row.
fn namespace_event(
    actor: &RequireAuth,
    action: &str,
    id: NamespaceId,
    label: String,
    metadata: serde_json::Value,
) -> NewAuditLogEntry {
    NewAuditLogEntry {
        actor_id: actor.user_id,
        actor_username: actor.username.clone(),
        action: action.to_owned(),
        target_kind: "namespace".to_owned(),
        target_id: id.into_uuid(),
        target_label: Some(label),
        metadata,
    }
}

/// `GET /api/v1/namespaces` — list every namespace defined on this wiki.
///
/// Open to anonymous callers; the SPA's page list and search results already
/// need this information to render namespace prefixes.
#[utoipa::path(
    get,
    path = "",
    responses(
        (status = 200, description = "Namespace list", body = NamespaceListResponse),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "namespaces",
)]
pub async fn list_namespaces<S: AppStorage>(
    State(state): State<AppState<S>>,
) -> Result<Json<NamespaceListResponse>, ApiError> {
    let rows = state.storage.namespaces().list().await?;
    let items = rows.into_iter().map(NamespaceView::from).collect();
    Ok(Json(NamespaceListResponse { items }))
}

/// `POST /api/v1/namespaces` — create a new namespace.
///
/// Requires [`Permissions::MANAGE_NAMESPACES`]. Returns `201 Created` with
/// the new row on success, `409` if the slug is already in use.
#[utoipa::path(
    post,
    path = "",
    params(
        ("cookie" = String, Header, description = "Session and CSRF cookies: `thewiki_session=...; thewiki_csrf=...`"),
        ("x-csrf-token" = String, Header, description = "Double-submit CSRF token matching the `thewiki_csrf` cookie"),
    ),
    request_body = CreateNamespaceRequest,
    responses(
        (status = 201, description = "Namespace created", body = NamespaceView),
        (status = 400, description = "Invalid input", body = crate::error::ErrorBody),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "Caller lacks MANAGE_NAMESPACES", body = crate::error::ErrorBody),
        (status = 409, description = "Slug already in use", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "namespaces",
)]
pub async fn create_namespace<S: AppStorage>(
    State(state): State<AppState<S>>,
    actor: RequireAuth,
    Json(req): Json<CreateNamespaceRequest>,
) -> Result<(StatusCode, Json<NamespaceView>), ApiError> {
    require_manage(&actor)?;
    if req.display_name.trim().is_empty() {
        return Err(ApiError::InvalidInput(
            "display_name must not be empty".into(),
        ));
    }
    let slug = parse_slug(&req.slug)?;
    let namespace = Namespace {
        id: NamespaceId::new(),
        slug,
        display_name: req.display_name,
    };
    state.storage.namespaces().create(&namespace).await?;

    // Audit row. We write through the audit log directly because there is
    // no namespace-specific PageAuditMutation — namespaces aren't pages and
    // don't carry the same atomic-mutation guarantees.
    let audit = namespace_event(
        &actor,
        "namespace.create",
        namespace.id,
        namespace.slug.as_str().to_owned(),
        json!({
            "slug": namespace.slug.as_str(),
            "display_name": namespace.display_name,
        }),
    );
    state.storage.audit_log().create(audit).await?;

    Ok((StatusCode::CREATED, Json(NamespaceView::from(namespace))))
}

/// `PATCH /api/v1/namespaces/{slug}` — rename the display name of a
/// namespace.
///
/// Slug renames are intentionally out of scope (URL breakage); only the
/// human-readable label can be changed here.
#[utoipa::path(
    patch,
    path = "/{slug}",
    params(
        ("slug" = String, Path, description = "Slug of the namespace to update"),
        ("cookie" = String, Header, description = "Session and CSRF cookies: `thewiki_session=...; thewiki_csrf=...`"),
        ("x-csrf-token" = String, Header, description = "Double-submit CSRF token matching the `thewiki_csrf` cookie"),
    ),
    request_body = UpdateNamespaceRequest,
    responses(
        (status = 200, description = "Display name updated", body = NamespaceView),
        (status = 400, description = "Invalid input", body = crate::error::ErrorBody),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "Caller lacks MANAGE_NAMESPACES", body = crate::error::ErrorBody),
        (status = 404, description = "Namespace not found", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "namespaces",
)]
pub async fn update_namespace<S: AppStorage>(
    State(state): State<AppState<S>>,
    Path(slug): Path<String>,
    actor: RequireAuth,
    Json(req): Json<UpdateNamespaceRequest>,
) -> Result<Json<NamespaceView>, ApiError> {
    require_manage(&actor)?;
    if req.display_name.trim().is_empty() {
        return Err(ApiError::InvalidInput(
            "display_name must not be empty".into(),
        ));
    }
    let ns_slug = parse_slug(&slug)?;
    let namespace = state.storage.namespaces().get_by_slug(&ns_slug).await?;
    let previous_display = namespace.display_name.clone();
    state
        .storage
        .namespaces()
        .update_display_name(namespace.id, &req.display_name)
        .await?;

    let audit = namespace_event(
        &actor,
        "namespace.update",
        namespace.id,
        namespace.slug.as_str().to_owned(),
        json!({
            "slug": namespace.slug.as_str(),
            "from": previous_display,
            "to": req.display_name,
        }),
    );
    state.storage.audit_log().create(audit).await?;

    Ok(Json(NamespaceView {
        id: namespace.id,
        slug: namespace.slug.into_string(),
        display_name: req.display_name,
    }))
}

/// `DELETE /api/v1/namespaces/{slug}` — remove a namespace.
///
/// Refuses (409) when any pages still live in the namespace; the admin must
/// move them first. The schema's FK enforces this on the storage side too,
/// but we surface the 409 with a clear message rather than letting the
/// database error bubble up generic.
#[utoipa::path(
    delete,
    path = "/{slug}",
    params(
        ("slug" = String, Path, description = "Slug of the namespace to delete"),
        ("cookie" = String, Header, description = "Session and CSRF cookies: `thewiki_session=...; thewiki_csrf=...`"),
        ("x-csrf-token" = String, Header, description = "Double-submit CSRF token matching the `thewiki_csrf` cookie"),
    ),
    responses(
        (status = 204, description = "Namespace deleted"),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "Caller lacks MANAGE_NAMESPACES", body = crate::error::ErrorBody),
        (status = 404, description = "Namespace not found", body = crate::error::ErrorBody),
        (status = 409, description = "Namespace still contains pages", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "namespaces",
)]
pub async fn delete_namespace<S: AppStorage>(
    State(state): State<AppState<S>>,
    Path(slug): Path<String>,
    actor: RequireAuth,
) -> Result<StatusCode, ApiError> {
    require_manage(&actor)?;
    let ns_slug = parse_slug(&slug)?;
    let namespace = state.storage.namespaces().get_by_slug(&ns_slug).await?;

    // Pre-flight check: list one page and refuse if any exist. This is the
    // operator-friendly side of the FK constraint — the storage layer
    // returns the same 409 if a page slips in between this check and the
    // delete, but in the common case the message names the constraint
    // explicitly.
    let slice = state
        .storage
        .pages()
        .list_in_namespace(namespace.id, None, 1)
        .await?;
    if !slice.items.is_empty() {
        return Err(ApiError::Conflict(
            "namespace still contains pages; move them before deleting".into(),
        ));
    }

    state.storage.namespaces().delete(namespace.id).await?;

    let audit = namespace_event(
        &actor,
        "namespace.delete",
        namespace.id,
        namespace.slug.as_str().to_owned(),
        json!({
            "slug": namespace.slug.as_str(),
            "display_name": namespace.display_name,
        }),
    );
    state.storage.audit_log().create(audit).await?;

    Ok(StatusCode::NO_CONTENT)
}
