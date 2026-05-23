//! Admin endpoints for the IP / URL blocklists (#42).
//!
//! Authorisation: every endpoint requires [`Permissions::MANAGE_BLOCKLIST`]
//! on the calling session. This bit is new in #42 — operators grant it
//! through a role assignment (see `crates/core/src/permissions.rs`). The
//! `VIEW_AUDIT_LOG` and `MANAGE_BLOCKLIST` permissions are kept separate
//! so a read-only "compliance" role can inspect audit history without
//! gaining the ability to edit the blocklist itself.
//!
//! Each mutation (`POST`, `DELETE`) writes an audit row before it returns,
//! using the standard `(actor_id, actor_username, action, target_kind,
//! target_id, target_label, metadata)` shape. Reads do not emit audit rows.
//!
//! After a successful mutation the in-memory [`BlocklistState`] is
//! refreshed by re-reading both tables — the cheapest way to keep the
//! snapshot consistent without piping individual deltas through the
//! `RwLock` API.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;
use thewiki_core::Permissions;
use thewiki_storage::repo::{
    AuditLogRepository, IpBlocklistEntry, IpBlocklistRepository, NewAuditLogEntry,
    NewIpBlocklistEntry, NewUrlBlocklistEntry, UrlBlocklistEntry, UrlBlocklistRepository,
};
use time::OffsetDateTime;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::auth::AuthSession;
use crate::error::ApiError;
use crate::state::{AppState, AppStorage};

/// IP blocklist row as returned by the API.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct IpBlocklistView {
    /// Stable row identifier (UUIDv7).
    pub id: Uuid,
    /// CIDR in canonical human form (`203.0.113.0/24`, `2001:db8::/32`).
    pub cidr: String,
    /// Free-form reason (may be empty).
    pub reason: String,
    /// User who created the entry.
    pub created_by: Uuid,
    /// RFC3339 timestamp.
    pub created_at: OffsetDateTime,
}

impl From<IpBlocklistEntry> for IpBlocklistView {
    fn from(entry: IpBlocklistEntry) -> Self {
        Self {
            id: entry.id,
            cidr: entry.cidr,
            reason: entry.reason,
            created_by: entry.created_by.into_uuid(),
            created_at: entry.created_at,
        }
    }
}

/// URL blocklist row as returned by the API.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UrlBlocklistView {
    /// Stable row identifier (UUIDv7).
    pub id: Uuid,
    /// Rust `regex` pattern.
    pub pattern: String,
    /// Free-form reason (may be empty).
    pub reason: String,
    /// User who created the entry.
    pub created_by: Uuid,
    /// RFC3339 timestamp.
    pub created_at: OffsetDateTime,
}

impl From<UrlBlocklistEntry> for UrlBlocklistView {
    fn from(entry: UrlBlocklistEntry) -> Self {
        Self {
            id: entry.id,
            pattern: entry.pattern,
            reason: entry.reason,
            created_by: entry.created_by.into_uuid(),
            created_at: entry.created_at,
        }
    }
}

/// Wrapper response for `GET` endpoints.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct IpBlocklistListResponse {
    /// Rows, newest first.
    pub items: Vec<IpBlocklistView>,
}

/// Wrapper response for `GET` endpoints.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UrlBlocklistListResponse {
    /// Rows, newest first.
    pub items: Vec<UrlBlocklistView>,
}

/// Body for `POST /api/v1/admin/blocklist/ip`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateIpEntryRequest {
    /// CIDR in canonical form. The handler parses with `ipnet::IpNet::from_str`
    /// before persisting — invalid input returns 400 without touching the DB.
    pub cidr: String,
    /// Optional reason. Omitting it stores an empty string.
    #[serde(default)]
    pub reason: String,
}

/// Body for `POST /api/v1/admin/blocklist/url`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateUrlEntryRequest {
    /// Rust `regex` pattern. Compiled with `regex::Regex::new` for validation
    /// before persisting.
    pub pattern: String,
    /// Optional reason.
    #[serde(default)]
    pub reason: String,
}

fn ensure_manage_blocklist(session: &AuthSession) -> Result<(), ApiError> {
    if session.permissions.contains(Permissions::MANAGE_BLOCKLIST) {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

/// `GET /api/v1/admin/blocklist/ip` — list every IP blocklist row.
#[utoipa::path(
    get,
    path = "",
    responses(
        (status = 200, description = "IP blocklist", body = IpBlocklistListResponse),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "Caller lacks MANAGE_BLOCKLIST", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    security(("SessionCookie" = [])),
    tag = "admin-blocklist",
)]
pub async fn list_ip<S: AppStorage>(
    State(state): State<AppState<S>>,
    session: AuthSession,
) -> Result<Json<IpBlocklistListResponse>, ApiError> {
    ensure_manage_blocklist(&session)?;
    let items = state.storage.ip_blocklist().list_all().await?;
    Ok(Json(IpBlocklistListResponse {
        items: items.into_iter().map(Into::into).collect(),
    }))
}

/// `POST /api/v1/admin/blocklist/ip` — add a new IP blocklist row.
#[utoipa::path(
    post,
    path = "",
    request_body = CreateIpEntryRequest,
    responses(
        (status = 201, description = "Row created", body = IpBlocklistView),
        (status = 400, description = "Invalid CIDR", body = crate::error::ErrorBody),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "Caller lacks MANAGE_BLOCKLIST", body = crate::error::ErrorBody),
        (status = 409, description = "CIDR already in list", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    security(("SessionCookie" = [])),
    tag = "admin-blocklist",
)]
pub async fn create_ip<S: AppStorage>(
    State(state): State<AppState<S>>,
    session: AuthSession,
    Json(req): Json<CreateIpEntryRequest>,
) -> Result<(StatusCode, Json<IpBlocklistView>), ApiError> {
    ensure_manage_blocklist(&session)?;

    // Validate the CIDR before touching storage. We round-trip through the
    // parser + Display to canonicalise the wire form: `203.000.113.0/24` is
    // accepted and stored as `203.0.113.0/24`.
    let parsed: ipnet::IpNet = req
        .cidr
        .parse()
        .map_err(|err: ipnet::AddrParseError| {
            ApiError::InvalidInput(format!("cidr: {err}"))
        })?;
    let canonical = parsed.to_string();

    let stored = state
        .storage
        .ip_blocklist()
        .create(NewIpBlocklistEntry {
            cidr: canonical.clone(),
            reason: req.reason.clone(),
            created_by: session.user.id,
        })
        .await?;

    // Audit + refresh snapshot. The audit row goes first so a successful
    // reply implies a durable record.
    let audit = NewAuditLogEntry {
        actor_id: session.user.id,
        actor_username: session.user.username.as_str().to_owned(),
        action: "blocklist.ip.create".to_owned(),
        target_kind: "ip_blocklist".to_owned(),
        target_id: stored.id,
        target_label: Some(stored.cidr.clone()),
        metadata: json!({ "cidr": stored.cidr, "reason": stored.reason }),
    };
    state.storage.audit_log().create(audit).await?;

    refresh_blocklist_snapshot(&state).await?;

    Ok((StatusCode::CREATED, Json(stored.into())))
}

/// `DELETE /api/v1/admin/blocklist/ip/{id}` — remove a blocklist row.
#[utoipa::path(
    delete,
    path = "/{id}",
    params(("id" = Uuid, Path, description = "Row identifier")),
    responses(
        (status = 204, description = "Row removed"),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "Caller lacks MANAGE_BLOCKLIST", body = crate::error::ErrorBody),
        (status = 404, description = "Row not found", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    security(("SessionCookie" = [])),
    tag = "admin-blocklist",
)]
pub async fn delete_ip<S: AppStorage>(
    State(state): State<AppState<S>>,
    session: AuthSession,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    ensure_manage_blocklist(&session)?;
    state.storage.ip_blocklist().delete(id).await?;

    let audit = NewAuditLogEntry {
        actor_id: session.user.id,
        actor_username: session.user.username.as_str().to_owned(),
        action: "blocklist.ip.delete".to_owned(),
        target_kind: "ip_blocklist".to_owned(),
        target_id: id,
        target_label: None,
        metadata: json!({ "id": id }),
    };
    state.storage.audit_log().create(audit).await?;

    refresh_blocklist_snapshot(&state).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/v1/admin/blocklist/url` — list every URL blocklist row.
#[utoipa::path(
    get,
    path = "",
    responses(
        (status = 200, description = "URL blocklist", body = UrlBlocklistListResponse),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "Caller lacks MANAGE_BLOCKLIST", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    security(("SessionCookie" = [])),
    tag = "admin-blocklist",
)]
pub async fn list_url<S: AppStorage>(
    State(state): State<AppState<S>>,
    session: AuthSession,
) -> Result<Json<UrlBlocklistListResponse>, ApiError> {
    ensure_manage_blocklist(&session)?;
    let items = state.storage.url_blocklist().list_all().await?;
    Ok(Json(UrlBlocklistListResponse {
        items: items.into_iter().map(Into::into).collect(),
    }))
}

/// `POST /api/v1/admin/blocklist/url` — add a new URL blocklist row.
#[utoipa::path(
    post,
    path = "",
    request_body = CreateUrlEntryRequest,
    responses(
        (status = 201, description = "Row created", body = UrlBlocklistView),
        (status = 400, description = "Invalid regex", body = crate::error::ErrorBody),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "Caller lacks MANAGE_BLOCKLIST", body = crate::error::ErrorBody),
        (status = 409, description = "Pattern already in list", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    security(("SessionCookie" = [])),
    tag = "admin-blocklist",
)]
pub async fn create_url<S: AppStorage>(
    State(state): State<AppState<S>>,
    session: AuthSession,
    Json(req): Json<CreateUrlEntryRequest>,
) -> Result<(StatusCode, Json<UrlBlocklistView>), ApiError> {
    ensure_manage_blocklist(&session)?;
    if req.pattern.trim().is_empty() {
        return Err(ApiError::InvalidInput("pattern must not be empty".into()));
    }
    // Compile the regex up front so a malformed pattern can't be persisted.
    regex::Regex::new(&req.pattern)
        .map_err(|err| ApiError::InvalidInput(format!("pattern: {err}")))?;

    let stored = state
        .storage
        .url_blocklist()
        .create(NewUrlBlocklistEntry {
            pattern: req.pattern.clone(),
            reason: req.reason.clone(),
            created_by: session.user.id,
        })
        .await?;

    let audit = NewAuditLogEntry {
        actor_id: session.user.id,
        actor_username: session.user.username.as_str().to_owned(),
        action: "blocklist.url.create".to_owned(),
        target_kind: "url_blocklist".to_owned(),
        target_id: stored.id,
        target_label: Some(stored.pattern.clone()),
        metadata: json!({ "pattern": stored.pattern, "reason": stored.reason }),
    };
    state.storage.audit_log().create(audit).await?;

    refresh_blocklist_snapshot(&state).await?;

    Ok((StatusCode::CREATED, Json(stored.into())))
}

/// `DELETE /api/v1/admin/blocklist/url/{id}` — remove a blocklist row.
#[utoipa::path(
    delete,
    path = "/{id}",
    params(("id" = Uuid, Path, description = "Row identifier")),
    responses(
        (status = 204, description = "Row removed"),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "Caller lacks MANAGE_BLOCKLIST", body = crate::error::ErrorBody),
        (status = 404, description = "Row not found", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    security(("SessionCookie" = [])),
    tag = "admin-blocklist",
)]
pub async fn delete_url<S: AppStorage>(
    State(state): State<AppState<S>>,
    session: AuthSession,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    ensure_manage_blocklist(&session)?;
    state.storage.url_blocklist().delete(id).await?;

    let audit = NewAuditLogEntry {
        actor_id: session.user.id,
        actor_username: session.user.username.as_str().to_owned(),
        action: "blocklist.url.delete".to_owned(),
        target_kind: "url_blocklist".to_owned(),
        target_id: id,
        target_label: None,
        metadata: json!({ "id": id }),
    };
    state.storage.audit_log().create(audit).await?;

    refresh_blocklist_snapshot(&state).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Refresh the in-memory snapshot the middleware reads from. Called from
/// every mutation. The state is optional on [`AppState`] so the test
/// fixtures that don't wire blocklist plumbing still work.
async fn refresh_blocklist_snapshot<S: AppStorage>(state: &AppState<S>) -> Result<(), ApiError> {
    let Some(blocklist) = state.blocklist.as_ref() else {
        return Ok(());
    };
    blocklist
        .refresh_from(&state.storage.ip_blocklist(), &state.storage.url_blocklist())
        .await
        .map_err(ApiError::from)
}

/// Build the IP blocklist subrouter (`/api/v1/admin/blocklist/ip`).
pub fn ip_router<S: AppStorage>() -> OpenApiRouter<AppState<S>> {
    OpenApiRouter::new()
        .routes(routes!(list_ip, create_ip))
        .routes(routes!(delete_ip))
}

/// Build the URL blocklist subrouter (`/api/v1/admin/blocklist/url`).
pub fn url_router<S: AppStorage>() -> OpenApiRouter<AppState<S>> {
    OpenApiRouter::new()
        .routes(routes!(list_url, create_url))
        .routes(routes!(delete_url))
}
