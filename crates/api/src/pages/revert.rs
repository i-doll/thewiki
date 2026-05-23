//! `POST /api/v1/pages/{slug}/revert` — revert a page to a prior revision.
//!
//! The handler never destroys history. It creates a **new** revision whose
//! body equals the targeted historical one, parented on the page's current
//! head. The page's `current_revision_id` is then advanced to the new
//! revision. From the storage layer's perspective this is indistinguishable
//! from a regular `PUT` edit — which is exactly the point: every change to a
//! page produces an append-only entry in its history.
//!
//! Authorisation:
//! - Today the route is gated on [`RequireAuth`] only.
//! - TODO(#14): swap in a real role-gated extractor (`editor` minimum, with
//!   the threshold configurable). The placeholder `RequireAuth` already
//!   distinguishes 401 (no auth) from the role check that will return 403.

use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thewiki_core::{NamespaceSlug, Revision, RevisionId};
use thewiki_search::PageDoc;
use thewiki_storage::repo::{PageAuditMutation, PageRepository, RevisionRepository};
use time::OffsetDateTime;
use utoipa::ToSchema;

use crate::error::ApiError;
use crate::extractors::RequireAuth;
use crate::pages::audit::page_event;
use crate::pages::dto::PageView;
use crate::pages::protection::{EditorContext, check_protection};
use crate::pages::routes::{hydrate_page_view, parse_default_namespace_slug, resolve_namespace};
use crate::state::{AppState, AppStorage};

/// Body of `POST /api/v1/pages/{slug}/revert`.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct RevertRequest {
    /// Revision to revert *to*. Must belong to the page named in the path.
    pub from_revision: RevisionId,
    /// Optional short note describing the revert (think Git commit message).
    /// Falls back to `"Reverted to <revision id>"` when omitted.
    #[serde(default)]
    pub message: Option<String>,
}

/// `POST /api/v1/pages/{slug}/revert` — revert a page to a historical revision.
///
/// Steps:
/// 1. Resolve the page by slug in the default namespace (404 if missing).
/// 2. Load the historical revision by id (404 if missing).
/// 3. Verify the revision belongs to this page; if not, **also 404**. We
///    deliberately do not surface a 403 here — leaking the existence of
///    revision ids belonging to other pages would be a cross-page oracle.
/// 4. Commit a new revision with the historical body. Its `parent_id` is the
///    page's *current* head (not the revision being reverted to) so the
///    history graph stays linear and the audit trail captures both endpoints.
/// 5. Advance `page.current_revision_id` to the new revision.
/// 6. Write a persistent audit-log row for operators.
///
/// Returns the now-current [`PageView`].
#[utoipa::path(
    post,
    path = "/{slug}/revert",
    params(
        ("slug" = String, Path, description = "URL slug within the default namespace"),
        ("cookie" = String, Header, description = "Session and CSRF cookies: `thewiki_session=...; thewiki_csrf=...`"),
        ("x-csrf-token" = String, Header, description = "Double-submit CSRF token matching the `thewiki_csrf` cookie"),
    ),
    request_body = RevertRequest,
    responses(
        (status = 200, description = "Page reverted; new revision committed", body = PageView),
        (status = 400, description = "Invalid input", body = crate::error::ErrorBody),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "CSRF token missing or invalid", body = crate::auth::error::AuthErrorBody),
        (status = 404, description = "Page or revision not found", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "revisions",
)]
pub async fn revert_page<S: AppStorage>(
    State(state): State<AppState<S>>,
    Path(slug): Path<String>,
    // TODO(#14): replace `RequireAuth` with a role-gated extractor —
    // `RequireRole(Role::Editor)` or `RequirePermission(Permissions::EDIT)` —
    // once configurable auth lands. For now any authenticated caller may
    // revert; the bare `RequireAuth` covers the 401-vs-403 distinction.
    author: RequireAuth,
    Json(req): Json<RevertRequest>,
) -> Result<Json<PageView>, ApiError> {
    let namespace_slug = parse_default_namespace_slug()?;
    revert_page_in_namespace(state, namespace_slug, slug, author, req).await
}

/// Shared body for `POST /api/v1/pages/{slug}/revert` and
/// `POST /api/v1/wiki/{namespace}/{slug}/revert`.
pub(crate) async fn revert_page_in_namespace<S: AppStorage>(
    state: AppState<S>,
    namespace_slug: NamespaceSlug,
    slug: String,
    author: RequireAuth,
    req: RevertRequest,
) -> Result<Json<PageView>, ApiError> {
    let namespace = resolve_namespace(&state, &namespace_slug).await?;
    let namespace_label = namespace.slug.as_str().to_owned();
    let mut page = state
        .storage
        .pages()
        .get_by_namespace_and_slug(namespace.id, &slug)
        .await?;

    // Per-page protection check (#34). A revert mutates the page just like
    // any other edit, so the same gate applies. `RequireAuth` already
    // covered the 401 case; this layer adds the 403 for under-privileged
    // sessions.
    check_protection(
        page.protection_level,
        EditorContext {
            is_anonymous: false,
            permissions: author.permissions,
        },
    )?;

    // Load the historical revision. Storage's `NotFound` maps to 404 via
    // `From<StorageError>` in `error.rs`.
    let historical = state
        .storage
        .revisions()
        .get_by_id(req.from_revision)
        .await?;

    // Same-page guard. A mismatch is mapped to 404 — *not* 403 — so we don't
    // confirm the id exists on some other page.
    if historical.page_id != page.id {
        return Err(ApiError::NotFound);
    }

    let edit_summary = req
        .message
        .filter(|m| !m.trim().is_empty())
        .or_else(|| Some(format!("Reverted to {}", historical.id)));

    // Parent is the *current* head, not the revision being reverted to. This
    // keeps the history graph linear and makes the revert auditable as a
    // discrete event between two specific revisions.
    let new_revision = Revision::new(
        page.id,
        page.current_revision_id,
        author.user_id,
        historical.body.clone(),
        edit_summary,
    );
    page.current_revision_id = Some(new_revision.id);
    page.updated_at = OffsetDateTime::now_utc();

    let audit = page_event(
        author.user_id,
        &author.username,
        "page.revert",
        page.id,
        format!("{namespace_label}/{}", page.slug),
        json!({
            "namespace": namespace_label,
            "slug": page.slug.as_str(),
            "from_revision_id": historical.id.into_uuid(),
            "new_revision_id": new_revision.id.into_uuid(),
        }),
    );
    let indexed_body = new_revision.body.clone();
    state
        .storage
        .commit_page_audit(
            PageAuditMutation::CommitRevision {
                page: page.clone(),
                revision: new_revision,
            },
            audit,
        )
        .await?;

    // Re-index the page with the reverted body. From the search layer's
    // perspective a revert is just another upsert — the schema doesn't
    // care that the content matches an older revision.
    state.search.upsert(PageDoc {
        page_id: page.id,
        namespace_id: page.namespace_id,
        namespace_slug: namespace_label,
        slug: page.slug.clone(),
        title: page.title.clone(),
        body: indexed_body,
        tags: Vec::new(),
        updated_at: page.updated_at,
        is_talk: namespace.is_talk,
    });

    let view = hydrate_page_view(&state, page, namespace.slug.into_string()).await?;
    Ok(Json(view))
}
