//! Axum handlers for the page CRUD endpoints.
//!
//! Each handler is generic over the storage facade (`S: AppStorage`) so the
//! route layer stays backend-agnostic. The handler bodies stay small and
//! readable; cross-cutting work — error mapping, default page sizes, the
//! configurable-auth gate — lives in [`crate::error`], [`crate::state`] and
//! [`crate::extractors`].

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use thewiki_core::{ContentFormat, NamespaceSlug, Page, PageId, ProtectionLevel, Revision};
use thewiki_storage::repo::{Cursor, NamespaceRepository, PageRepository, RevisionRepository};
use time::OffsetDateTime;

use crate::config::ApprovalScope;
use crate::error::ApiError;
use crate::extractors::EditorExtractor;
use crate::pages::dto::{
    CreatePageRequest, ListPagesQuery, PageListItem, PageListResponse, PageView, UpdatePageRequest,
};
use crate::state::{AppState, AppStorage};

/// Default namespace slug used when a request doesn't carry one.
///
/// TODO(#28): once namespace prefix routing lands, the namespace will be
/// part of the path. Until then, every request resolves against this slug.
const DEFAULT_NAMESPACE: &str = "Main";

/// Window (in seconds) during which a freshly-registered account is treated
/// as "new" for [`ApprovalScope::NewUsers`] gating. 24h matches the spec in
/// `thewiki.example.toml` and is short enough to throttle bot signups without
/// punishing genuine new editors for longer than necessary.
const NEW_USER_WINDOW_SECS: i64 = 24 * 60 * 60;

/// Decide whether a revision should land in the approval queue rather than
/// going live immediately. Pure function — no I/O, no async — so each branch
/// can be exhaustively unit-tested.
///
/// TODO(#40): when the queue lands, this function will also drive the
/// `revisions.status` column. For now the decision is observed via
/// [`queue_or_publish`] which logs a `tracing::info!` instead of persisting
/// the pending revision row.
fn needs_approval(scope: ApprovalScope, editor: &EditorExtractor) -> bool {
    match scope {
        ApprovalScope::None => false,
        ApprovalScope::Anonymous => editor.is_anonymous,
        ApprovalScope::NewUsers => {
            if editor.is_anonymous {
                return true;
            }
            match editor.user_created_at {
                Some(created_at) => {
                    let age = OffsetDateTime::now_utc() - created_at;
                    age.whole_seconds() < NEW_USER_WINDOW_SECS
                }
                // No `created_at` on an authenticated session is impossible
                // (the User row always has one) but be defensive — treating
                // it as "new" is the safer side.
                None => true,
            }
        }
        ApprovalScope::All => true,
    }
}

/// Commit a revision either to the live history or to the approval queue
/// (M2 — currently a no-op stub).
///
/// Returns `true` when the revision was committed live and the caller should
/// proceed to flip `pages.current_revision_id`; `false` when it landed in
/// the (stubbed) approval queue and the page should keep its existing head.
///
/// TODO(#40): replace the `create_pending` branch with a real queue write —
/// today this only emits a structured `tracing::info!` so operators (and the
/// tests) can verify the gating works without us having to ship the queue
/// schema before its tracking issue.
async fn queue_or_publish<S: AppStorage>(
    state: &AppState<S>,
    revision: &Revision,
    editor: &EditorExtractor,
) -> Result<bool, ApiError> {
    if needs_approval(state.auth_config.approval_required_for, editor) {
        // TODO(#40): persist the revision to a `pending_revisions` table and
        // surface it on the moderator approval queue. For the wiring-only PR
        // we log the decision and return `false` so the caller does not
        // promote the revision to head.
        tracing::info!(
            page_id = %revision.page_id,
            revision_id = %revision.id,
            author_id = %revision.author_id,
            anonymous = editor.is_anonymous,
            approval_scope = ?state.auth_config.approval_required_for,
            "would queue revision for approval (TODO #40)"
        );
        Ok(false)
    } else {
        state.storage.revisions().create(revision).await?;
        Ok(true)
    }
}

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
/// 4. Update the page row to point at the new revision (unless approval is
///    required, in which case the page stays headless until a moderator
///    promotes the pending revision).
///
/// The schema's `pages.current_revision_id` FK is `ON DELETE SET NULL`, so
/// the brief NULL state in step 2 is legitimate even with FK enforcement on.
#[utoipa::path(
    post,
    path = "",
    request_body = CreatePageRequest,
    responses(
        (status = 201, description = "Page created", body = PageView),
        (status = 202, description = "Edit accepted but pending approval", body = PageView),
        (status = 400, description = "Invalid input", body = crate::error::ErrorBody),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 404, description = "Namespace not found", body = crate::error::ErrorBody),
        (status = 409, description = "Slug already taken", body = crate::error::ErrorBody),
    ),
    tag = "pages",
)]
pub async fn create_page<S: AppStorage>(
    State(state): State<AppState<S>>,
    editor: EditorExtractor,
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

    let revision = Revision::new(page.id, None, editor.user_id, req.content, None);
    let live = queue_or_publish(&state, &revision, &editor).await?;

    let status = if live {
        page.current_revision_id = Some(revision.id);
        page.updated_at = OffsetDateTime::now_utc();
        state.storage.pages().update(&page).await?;
        StatusCode::CREATED
    } else {
        // 202 Accepted: the request was understood and queued, but the page
        // doesn't yet reflect the change. The wire body still shows the
        // page row (no current revision) so the client can correlate.
        StatusCode::ACCEPTED
    };

    let view = hydrate_page_view(&state, page, namespace.slug.into_string()).await?;
    Ok((status, Json(view)))
}

/// `GET /api/v1/pages/{slug}` — fetch a page by slug in the default namespace.
///
/// Read is open by design — anonymous reads are always allowed (the
/// `anonymous_edits` flag only gates mutating endpoints).
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
/// produces a new revision row. When the approval-queue gate matches, the
/// revision lands in the (stubbed) queue and the response is `202 Accepted`
/// with the page row still pointing at the previous head.
#[utoipa::path(
    put,
    path = "/{slug}",
    params(("slug" = String, Path, description = "URL slug within the default namespace")),
    request_body = UpdatePageRequest,
    responses(
        (status = 200, description = "Page updated", body = PageView),
        (status = 202, description = "Edit accepted but pending approval", body = PageView),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 404, description = "Page not found", body = crate::error::ErrorBody),
    ),
    tag = "pages",
)]
pub async fn update_page<S: AppStorage>(
    State(state): State<AppState<S>>,
    Path(slug): Path<String>,
    editor: EditorExtractor,
    Json(req): Json<UpdatePageRequest>,
) -> Result<(StatusCode, Json<PageView>), ApiError> {
    // Validate inputs BEFORE any storage writes — otherwise a bad request
    // would leave a dangling revision row that never becomes the page's
    // current_revision_id.
    let new_title = match req.title {
        Some(title) if title.trim().is_empty() => {
            return Err(ApiError::InvalidInput("title must not be empty".into()));
        }
        other => other,
    };

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
        editor.user_id,
        req.content,
        req.edit_summary,
    );
    let live = queue_or_publish(&state, &revision, &editor).await?;

    let status = if live {
        if let Some(title) = new_title {
            page.title = title;
        }
        page.current_revision_id = Some(revision.id);
        page.updated_at = OffsetDateTime::now_utc();
        state.storage.pages().update(&page).await?;
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };

    let view = hydrate_page_view(&state, page, namespace.slug.into_string()).await?;
    Ok((status, Json(view)))
}

/// `DELETE /api/v1/pages/{slug}` — remove a page and all its revisions.
///
/// The `revisions` table has `ON DELETE CASCADE` on `page_id`, so wiping the
/// page row collapses the history. Deletion is gated by the same configurable
/// extractor as edits — operators who flip `anonymous_edits = true` accept
/// that anonymous callers can delete as well. The eventual role-gated
/// extractor (TODO(#14): `RequireRole(Role::Admin)`) will tighten this.
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
    // extractor — `RequireRole(Role::Admin)` or similar. Today
    // [`EditorExtractor`] covers the 401-vs-anonymous distinction.
    _editor: EditorExtractor,
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

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use thewiki_core::UserId;

    fn editor(is_anonymous: bool, user_age_secs: Option<i64>) -> EditorExtractor {
        EditorExtractor {
            user_id: UserId::new(),
            is_anonymous,
            user_created_at: user_age_secs
                .map(|s| OffsetDateTime::now_utc() - time::Duration::seconds(s)),
        }
    }

    #[test]
    fn needs_approval_scope_none_is_always_false() {
        assert!(!needs_approval(ApprovalScope::None, &editor(true, None)));
        assert!(!needs_approval(
            ApprovalScope::None,
            &editor(false, Some(0))
        ));
        assert!(!needs_approval(
            ApprovalScope::None,
            &editor(false, Some(NEW_USER_WINDOW_SECS * 10))
        ));
    }

    #[test]
    fn needs_approval_scope_anonymous_only_gates_anonymous() {
        assert!(needs_approval(
            ApprovalScope::Anonymous,
            &editor(true, None)
        ));
        assert!(!needs_approval(
            ApprovalScope::Anonymous,
            &editor(false, Some(0))
        ));
    }

    #[test]
    fn needs_approval_scope_new_users_gates_fresh_accounts() {
        // Anonymous → always queued.
        assert!(needs_approval(ApprovalScope::NewUsers, &editor(true, None)));
        // 5-minute-old account → queued.
        assert!(needs_approval(
            ApprovalScope::NewUsers,
            &editor(false, Some(5 * 60))
        ));
        // 48-hour-old account → not queued.
        assert!(!needs_approval(
            ApprovalScope::NewUsers,
            &editor(false, Some(NEW_USER_WINDOW_SECS * 2))
        ));
    }

    #[test]
    fn needs_approval_scope_all_is_always_true() {
        assert!(needs_approval(ApprovalScope::All, &editor(true, None)));
        assert!(needs_approval(
            ApprovalScope::All,
            &editor(false, Some(NEW_USER_WINDOW_SECS * 10))
        ));
    }
}
