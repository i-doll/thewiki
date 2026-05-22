//! `POST /api/v1/pages/{slug}/protect` — change a page's protection level (#34).
//!
//! Authorisation: requires [`Permissions::PROTECT`] on the caller (the bit
//! that already gates "raise / lower the protection level" in the role
//! catalogue). Anonymous + under-privileged callers see a 403 with the
//! `page_protected` machine code — same shape the edit handlers return — so
//! a single SPA branch handles both "you can't edit this" and "you can't
//! protect this".
//!
//! Persistence: the new level is written via the same atomic mutation +
//! audit-row commit used by every other privileged page action
//! ([`PageAuditMutation::UpdatePage`]). The audit entry has `action =
//! "page.protect"` and metadata `{ from, to }`. Audit rows are append-only
//! per #36; the storage layer commits the page update and the audit insert
//! in a single transaction so an operator can never observe a level change
//! without a matching audit record.

use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thewiki_core::{Permissions, ProtectionLevel};
use thewiki_storage::repo::{PageAuditMutation, PageRepository};
use time::OffsetDateTime;
use utoipa::ToSchema;

use crate::error::ApiError;
use crate::extractors::RequireAuth;
use crate::pages::audit::page_event;
use crate::pages::dto::PageView;
use crate::pages::routes::{hydrate_page_view, parse_default_namespace_slug, resolve_namespace};
use crate::state::{AppState, AppStorage};

/// Body of `POST /api/v1/pages/{slug}/protect`.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct ProtectRequest {
    /// New protection level for the page. Accepted wire forms mirror the
    /// snake-case serde representation of [`ProtectionLevel`]:
    /// `"none"`, `"semi_protected"`, `"protected"`, `"fully_protected"`.
    pub protection_level: ProtectionLevel,
}

/// `POST /api/v1/pages/{slug}/protect` — change a page's protection level.
#[utoipa::path(
    post,
    path = "/{slug}/protect",
    params(
        ("slug" = String, Path, description = "URL slug within the default namespace"),
        ("cookie" = String, Header, description = "Session and CSRF cookies: `thewiki_session=...; thewiki_csrf=...`"),
        ("x-csrf-token" = String, Header, description = "Double-submit CSRF token matching the `thewiki_csrf` cookie"),
    ),
    request_body = ProtectRequest,
    responses(
        (status = 200, description = "Protection level updated", body = PageView),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "Caller lacks PROTECT permission", body = crate::error::ErrorBody),
        (status = 404, description = "Page not found", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "pages",
)]
pub async fn protect_page<S: AppStorage>(
    State(state): State<AppState<S>>,
    Path(slug): Path<String>,
    actor: RequireAuth,
    Json(req): Json<ProtectRequest>,
) -> Result<Json<PageView>, ApiError> {
    // Gate on PROTECT. Same machine code the edit handlers use, so the SPA
    // can render a single "you don't have permission to change this" surface.
    if !actor.permissions.contains(Permissions::PROTECT) {
        return Err(ApiError::PageProtected {
            level: req.protection_level.as_str(),
            required: "PROTECT",
        });
    }

    let namespace_slug = parse_default_namespace_slug()?;
    let namespace = resolve_namespace(&state, &namespace_slug).await?;
    let namespace_label = namespace.slug.as_str().to_owned();

    let mut page = state
        .storage
        .pages()
        .get_by_namespace_and_slug(namespace.id, &slug)
        .await?;

    let previous = page.protection_level;
    // Idempotent: setting to the current level is a no-op so admins can
    // hit the endpoint without worrying about audit-log spam. We still
    // return the current page view so the SPA sees the latest state.
    if previous == req.protection_level {
        let view = hydrate_page_view(&state, page, namespace.slug.into_string()).await?;
        return Ok(Json(view));
    }

    page.protection_level = req.protection_level;
    page.updated_at = OffsetDateTime::now_utc();

    let audit = page_event(
        actor.user_id,
        &actor.username,
        "page.protect",
        page.id,
        format!("{namespace_label}/{}", page.slug),
        json!({
            "namespace": namespace_label,
            "slug": page.slug.as_str(),
            "from": previous.as_str(),
            "to": req.protection_level.as_str(),
        }),
    );

    state
        .storage
        .commit_page_audit(PageAuditMutation::UpdatePage { page: page.clone() }, audit)
        .await?;

    let view = hydrate_page_view(&state, page, namespace.slug.into_string()).await?;
    Ok(Json(view))
}
