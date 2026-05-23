//! Axum handlers for the approval-queue endpoints (#40).
//!
//! All endpoints require [`Permissions::REVIEW_EDITS`] on the calling
//! session. `MANAGE_USERS` is honoured as a convenience super-power so
//! admins inherit reviewer access without an explicit grant — the spec
//! lets the implementer pick one or the other; we pick "both work".

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde_json::json;
use thewiki_core::notification::kind as notif_kind;
use thewiki_core::{
    NewNotification, Page, Permissions, Revision, RevisionId, UserId, pending_revision,
};
use thewiki_storage::repo::{
    Cursor, NotificationRepository, PageAuditMutation, PageLinkRepository, PageRepository,
    PendingRevisionFilter, PendingRevisionRepository, RevisionRepository,
};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::ApiError;
use crate::extractors::{ANONYMOUS_USERNAME, RequireAuth, anonymous_user_id, ensure_anonymous_user};
use crate::pages::audit::page_event;
use crate::pending_revisions::dto::{
    ListPendingRevisionsQuery, PendingRevisionDetailResponse, PendingRevisionListResponse,
    PendingRevisionView, RejectPendingRevisionRequest,
};
use crate::state::{AppState, AppStorage};

const PENDING_REV_TARGET_KIND: &str = "pending_revision";

/// Verify the calling session can review queued edits.
///
/// `REVIEW_EDITS` is the dedicated bit; `MANAGE_USERS` keeps existing
/// admins eligible without a config dance.
fn require_review(actor: &RequireAuth) -> Result<(), ApiError> {
    if actor
        .permissions
        .intersects(Permissions::REVIEW_EDITS | Permissions::MANAGE_USERS)
    {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

fn parse_status(raw: Option<&str>) -> Result<Option<pending_revision::PendingRevisionStatus>, ApiError> {
    let Some(s) = raw else {
        return Ok(None);
    };
    pending_revision::PendingRevisionStatus::parse(s).map(Some).ok_or_else(|| {
        ApiError::InvalidInput(format!(
            "status must be one of pending, approved, rejected (got {s:?})",
        ))
    })
}

/// Hydrate one pending row into the wire shape, joining in the page +
/// namespace + author labels needed for the reviewer UI.
async fn build_view<S: AppStorage>(
    state: &AppState<S>,
    pending: pending_revision::PendingRevision,
) -> Result<(PendingRevisionView, Page), ApiError> {
    use thewiki_storage::repo::{NamespaceRepository, PageRepository, UserRepository};

    let page = state.storage.pages().get_by_id(pending.page_id).await?;
    let namespace = state
        .storage
        .namespaces()
        .get_by_id(page.namespace_id)
        .await?;
    let author_label = match pending.author_id {
        Some(id) => match state.storage.users().get_by_id(id).await {
            Ok(u) => u.username.as_str().to_owned(),
            Err(_) => pending
                .author_ip
                .clone()
                .unwrap_or_else(|| ANONYMOUS_USERNAME.to_owned()),
        },
        None => pending
            .author_ip
            .clone()
            .unwrap_or_else(|| ANONYMOUS_USERNAME.to_owned()),
    };
    Ok((
        PendingRevisionView {
            id: pending.id,
            page_id: pending.page_id,
            namespace_id: namespace.id,
            namespace_slug: namespace.slug.as_str().to_owned(),
            page_slug: page.slug.clone(),
            page_title: page.title.clone(),
            parent_revision_id: pending.parent_revision_id,
            author_id: pending.author_id,
            author_label,
            comment: pending.comment,
            status: pending.status,
            reviewer_id: pending.reviewer_id,
            decided_at: pending.decided_at,
            rejection_reason: pending.rejection_reason,
            created_at: pending.created_at,
        },
        page,
    ))
}

/// `GET /api/v1/pending-revisions` — reviewer queue list.
#[utoipa::path(
    get,
    path = "",
    params(ListPendingRevisionsQuery),
    responses(
        (status = 200, description = "Pending revisions list", body = PendingRevisionListResponse),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "Caller lacks REVIEW_EDITS / MANAGE_USERS", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "pending-revisions",
)]
pub async fn list_pending<S: AppStorage>(
    State(state): State<AppState<S>>,
    actor: RequireAuth,
    Query(query): Query<ListPendingRevisionsQuery>,
) -> Result<Json<PendingRevisionListResponse>, ApiError> {
    require_review(&actor)?;
    // Default to `pending` so the reviewer landing view shows only the
    // queue, not historical decisions.
    let status = match query.status.as_deref() {
        Some(s) => parse_status(Some(s))?,
        None => Some(pending_revision::PendingRevisionStatus::Pending),
    };
    let filter = PendingRevisionFilter { status };
    let limit = match query.limit {
        Some(0) | None => state.route_config.default_page_size,
        Some(n) => n,
    };
    let cursor = query.cursor.map(Cursor);
    let slice = state
        .storage
        .pending_revisions()
        .list(filter, cursor, limit)
        .await?;
    let total = state.storage.pending_revisions().count(filter).await?;

    let mut items = Vec::with_capacity(slice.items.len());
    for row in slice.items {
        let (view, _page) = build_view(&state, row).await?;
        items.push(view);
    }
    Ok(Json(PendingRevisionListResponse {
        items,
        next_cursor: slice.next.map(|c| c.0),
        total,
    }))
}

/// `GET /api/v1/pending-revisions/{id}` — single row + parent body.
#[utoipa::path(
    get,
    path = "/{id}",
    params(("id" = String, Path, description = "Pending revision id")),
    responses(
        (status = 200, description = "Pending revision detail", body = PendingRevisionDetailResponse),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "Caller lacks REVIEW_EDITS / MANAGE_USERS", body = crate::error::ErrorBody),
        (status = 404, description = "Pending revision not found", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "pending-revisions",
)]
pub async fn get_pending<S: AppStorage>(
    State(state): State<AppState<S>>,
    actor: RequireAuth,
    Path(id): Path<String>,
) -> Result<Json<PendingRevisionDetailResponse>, ApiError> {
    require_review(&actor)?;
    let id = parse_id(&id)?;
    let pending = state.storage.pending_revisions().get_by_id(id).await?;
    let body = pending.body.clone();
    let parent_revision_id = pending.parent_revision_id;
    let (view, _page) = build_view(&state, pending).await?;

    let parent_body = match parent_revision_id {
        Some(rev_id) => state
            .storage
            .revisions()
            .get_by_id(rev_id)
            .await
            .ok()
            .map(|r| r.body),
        None => None,
    };
    Ok(Json(PendingRevisionDetailResponse {
        view,
        body,
        parent_body,
    }))
}

/// `POST /api/v1/pending-revisions/{id}/approve` — promote to a real
/// revision against the target page.
#[utoipa::path(
    post,
    path = "/{id}/approve",
    params(("id" = String, Path, description = "Pending revision id")),
    responses(
        (status = 200, description = "Pending revision approved", body = PendingRevisionView),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "Caller lacks REVIEW_EDITS / MANAGE_USERS", body = crate::error::ErrorBody),
        (status = 404, description = "Pending revision not found", body = crate::error::ErrorBody),
        (status = 409, description = "Pending revision already decided", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "pending-revisions",
)]
pub async fn approve_pending<S: AppStorage>(
    State(state): State<AppState<S>>,
    actor: RequireAuth,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<PendingRevisionView>), ApiError> {
    require_review(&actor)?;
    let id = parse_id(&id)?;
    let pending = state.storage.pending_revisions().get_by_id(id).await?;
    if !matches!(
        pending.status,
        pending_revision::PendingRevisionStatus::Pending
    ) {
        return Err(ApiError::Conflict(format!(
            "pending revision already {:?}",
            pending.status,
        )));
    }

    // Resolve the author id used on the promoted revision. For anonymous
    // edits we route the credit to the singleton anonymous user — same
    // identity the live anonymous path uses, so page history stays
    // consistent.
    let author_id = match pending.author_id {
        Some(id) => id,
        None => ensure_anonymous_user(state.storage.as_ref()).await?,
    };

    let mut page = state.storage.pages().get_by_id(pending.page_id).await?;
    let namespace = {
        use thewiki_storage::repo::NamespaceRepository;
        state
            .storage
            .namespaces()
            .get_by_id(page.namespace_id)
            .await?
    };

    let revision = Revision {
        id: RevisionId::new(),
        page_id: page.id,
        parent_id: page.current_revision_id,
        author_id,
        body: pending.body.clone(),
        edit_summary: if pending.comment.is_empty() {
            None
        } else {
            Some(pending.comment.clone())
        },
        created_at: OffsetDateTime::now_utc(),
    };
    page.current_revision_id = Some(revision.id);
    page.updated_at = revision.created_at;

    let metadata = json!({
        "namespace": namespace.slug.as_str(),
        "slug": page.slug.as_str(),
        "live": true,
        "from_pending": pending.id.into_uuid(),
        "revision_id": revision.id.into_uuid(),
        "reviewer": actor.username,
    });
    let audit = page_event(
        actor.user_id,
        &actor.username,
        "pending_revision.approve",
        page.id,
        format!("{}/{}", namespace.slug.as_str(), page.slug),
        metadata,
    );

    let revision_body = revision.body.clone();
    let namespace_slug = namespace.slug.as_str().to_owned();
    state
        .storage
        .commit_page_audit(
            PageAuditMutation::CommitRevision {
                page: page.clone(),
                revision,
            },
            audit,
        )
        .await?;

    // Refresh outbound wikilinks for the freshly promoted body.
    let renderer = thewiki_render::MarkdownRenderer::new();
    let links = thewiki_core::render::Renderer::extract_links(&renderer, &revision_body);
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let rows: Vec<thewiki_storage::repo::PageLink> = links
        .into_iter()
        .filter(|l| !l.target.trim().is_empty())
        .filter_map(|l| {
            let key = (namespace_slug.clone(), l.target.clone());
            if seen.insert(key) {
                Some(thewiki_storage::repo::PageLink {
                    source_page_id: page.id,
                    target_namespace_slug: namespace_slug.clone(),
                    target_page_slug: l.target,
                })
            } else {
                None
            }
        })
        .collect();
    state
        .storage
        .page_links()
        .replace_for_source(page.id, &rows)
        .await?;

    // Flip the pending row to approved. The repo guards against a racing
    // double-approve by checking `status = pending` inside the transaction.
    let updated = state
        .storage
        .pending_revisions()
        .approve(id, actor.user_id, OffsetDateTime::now_utc())
        .await?;

    // Send the in-app notification to the original author, if any.
    if let Some(author) = pending.author_id {
        let payload = json!({
            "pending_revision_id": pending.id.into_uuid(),
            "page_id": page.id.into_uuid(),
            "namespace_slug": namespace_slug,
            "page_slug": page.slug,
            "page_title": page.title,
        });
        let _ = state
            .storage
            .notifications()
            .create(NewNotification {
                user_id: author,
                kind: notif_kind::PENDING_REVISION_APPROVED.to_owned(),
                payload: Some(payload),
            })
            .await;
    }

    let (view, _) = build_view(&state, updated).await?;
    Ok((StatusCode::OK, Json(view)))
}

/// `POST /api/v1/pending-revisions/{id}/reject` — record a rejection.
#[utoipa::path(
    post,
    path = "/{id}/reject",
    params(("id" = String, Path, description = "Pending revision id")),
    request_body = RejectPendingRevisionRequest,
    responses(
        (status = 200, description = "Pending revision rejected", body = PendingRevisionView),
        (status = 400, description = "Invalid input", body = crate::error::ErrorBody),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "Caller lacks REVIEW_EDITS / MANAGE_USERS", body = crate::error::ErrorBody),
        (status = 404, description = "Pending revision not found", body = crate::error::ErrorBody),
        (status = 409, description = "Pending revision already decided", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "pending-revisions",
)]
pub async fn reject_pending<S: AppStorage>(
    State(state): State<AppState<S>>,
    actor: RequireAuth,
    Path(id): Path<String>,
    Json(req): Json<RejectPendingRevisionRequest>,
) -> Result<(StatusCode, Json<PendingRevisionView>), ApiError> {
    require_review(&actor)?;
    let id = parse_id(&id)?;
    let reason = req.reason.trim();
    if reason.is_empty() {
        return Err(ApiError::InvalidInput(
            "reason must not be empty".into(),
        ));
    }
    let pending = state.storage.pending_revisions().get_by_id(id).await?;
    if !matches!(
        pending.status,
        pending_revision::PendingRevisionStatus::Pending
    ) {
        return Err(ApiError::Conflict(format!(
            "pending revision already {:?}",
            pending.status,
        )));
    }

    let updated = state
        .storage
        .pending_revisions()
        .reject(id, actor.user_id, reason, OffsetDateTime::now_utc())
        .await?;

    // Audit row (no page mutation here, so we write through the audit log
    // directly).
    let page = state.storage.pages().get_by_id(pending.page_id).await?;
    let namespace = {
        use thewiki_storage::repo::NamespaceRepository;
        state
            .storage
            .namespaces()
            .get_by_id(page.namespace_id)
            .await?
    };
    let target_label = format!("{}/{}", namespace.slug.as_str(), page.slug);
    let audit = thewiki_storage::repo::NewAuditLogEntry {
        actor_id: actor.user_id,
        actor_username: actor.username.clone(),
        action: "pending_revision.reject".to_owned(),
        target_kind: PENDING_REV_TARGET_KIND.to_owned(),
        target_id: pending.id.into_uuid(),
        target_label: Some(target_label),
        metadata: json!({
            "page_id": page.id.into_uuid(),
            "namespace_slug": namespace.slug.as_str(),
            "page_slug": page.slug,
            "reason": reason,
        }),
    };
    use thewiki_storage::repo::AuditLogRepository;
    state.storage.audit_log().create(audit).await?;

    if let Some(author) = pending.author_id {
        let payload = json!({
            "pending_revision_id": pending.id.into_uuid(),
            "page_id": page.id.into_uuid(),
            "namespace_slug": namespace.slug.as_str(),
            "page_slug": page.slug,
            "page_title": page.title,
            "reason": reason,
        });
        let _ = state
            .storage
            .notifications()
            .create(NewNotification {
                user_id: author,
                kind: notif_kind::PENDING_REVISION_REJECTED.to_owned(),
                payload: Some(payload),
            })
            .await;
    }

    let (view, _) = build_view(&state, updated).await?;
    Ok((StatusCode::OK, Json(view)))
}

fn parse_id(raw: &str) -> Result<thewiki_core::PendingRevisionId, ApiError> {
    let uuid =
        Uuid::parse_str(raw).map_err(|_| ApiError::InvalidInput("id must be a UUID".into()))?;
    Ok(thewiki_core::PendingRevisionId::from_uuid(uuid))
}

// Silence the "anonymous UserId helper is unused in this module" lint when
// the anonymous-create path isn't hit at runtime — we still want the
// helper imported because the docs reference it.
#[allow(dead_code)]
fn _anon_id_for_doc() -> UserId {
    anonymous_user_id()
}
