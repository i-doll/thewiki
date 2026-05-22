//! Handlers for the administrative audit log.

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thewiki_core::Permissions;
use thewiki_storage::repo::{
    AuditLogEntry, AuditLogFilter, AuditLogRepository, Cursor, DEFAULT_PAGE_SIZE, PageSlice,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use utoipa::{IntoParams, ToSchema};

use crate::auth::AuthSession;
use crate::error::ApiError;
use crate::state::{AppState, AppStorage};

const ATOM_SELF_PATH: &str = "/api/v1/audit-log/atom";

/// Query parameters accepted by audit-log endpoints.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AuditLogQuery {
    /// Filter by actor username.
    pub actor: Option<String>,
    /// Filter by stable action code, e.g. `page.create`.
    pub action: Option<String>,
    /// Include entries at or after this RFC 3339 timestamp.
    #[param(format = DateTime)]
    pub since: Option<String>,
    /// Include entries at or before this RFC 3339 timestamp.
    #[param(format = DateTime)]
    pub until: Option<String>,
    /// Page size. Clamped by storage defaults.
    pub limit: Option<u32>,
    /// Opaque cursor returned by a previous response.
    pub cursor: Option<String>,
}

/// JSON representation of one audit-log entry.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AuditLogEntryView {
    /// Audit entry ID.
    pub id: uuid::Uuid,
    /// Actor user ID.
    pub actor_id: uuid::Uuid,
    /// Actor username snapshot.
    pub actor_username: String,
    /// Stable machine action.
    pub action: String,
    /// Target kind, e.g. `page`.
    pub target_kind: String,
    /// Target ID.
    pub target_id: uuid::Uuid,
    /// Human target label at event time.
    pub target_label: Option<String>,
    /// Small structured metadata payload.
    #[schema(schema_with = audit_metadata_schema)]
    pub metadata: Value,
    /// Event timestamp.
    pub created_at: OffsetDateTime,
}

fn audit_metadata_schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
    use utoipa::openapi::schema::{AdditionalProperties, ObjectBuilder, Type};

    ObjectBuilder::new()
        .schema_type(Type::Object)
        .description(Some("Small structured metadata payload."))
        .additional_properties(Some(AdditionalProperties::FreeForm(true)))
        .into()
}

/// Paginated audit-log response.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AuditLogListResponse {
    /// Entries ordered newest first.
    pub items: Vec<AuditLogEntryView>,
    /// Cursor for the next page, if any.
    pub next_cursor: Option<String>,
}

impl From<AuditLogEntry> for AuditLogEntryView {
    fn from(entry: AuditLogEntry) -> Self {
        Self {
            id: entry.id.into_uuid(),
            actor_id: entry.actor_id.into_uuid(),
            actor_username: entry.actor_username,
            action: entry.action,
            target_kind: entry.target_kind,
            target_id: entry.target_id,
            target_label: entry.target_label,
            metadata: entry.metadata,
            created_at: entry.created_at,
        }
    }
}

/// `GET /api/v1/audit-log` — list administrative audit events.
#[utoipa::path(
    get,
    path = "",
    params(AuditLogQuery),
    responses(
        (status = 200, description = "Audit log entries", body = AuditLogListResponse),
        (status = 400, description = "Malformed query", body = crate::error::ErrorBody),
        (status = 401, description = "Missing or expired session", body = crate::auth::error::AuthErrorBody),
        (status = 403, description = "Caller lacks VIEW_AUDIT_LOG", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    security(("SessionCookie" = [])),
    tag = "audit-log",
)]
pub async fn list_audit_log<S: AppStorage>(
    State(state): State<AppState<S>>,
    session: AuthSession,
    Query(query): Query<AuditLogQuery>,
) -> Result<Json<AuditLogListResponse>, ApiError> {
    ensure_can_view_audit_log(&session)?;
    let page = load_entries(&state, query).await?;
    Ok(Json(AuditLogListResponse {
        items: page.items.into_iter().map(Into::into).collect(),
        next_cursor: page.next.map(|c| c.0),
    }))
}

/// `GET /api/v1/audit-log/atom` — syndicate audit events as Atom.
#[utoipa::path(
    get,
    path = "/atom",
    params(AuditLogQuery),
    responses(
        (status = 200, description = "Atom feed", content_type = "application/atom+xml", body = String),
        (status = 400, description = "Malformed query", body = crate::error::ErrorBody),
        (status = 401, description = "Missing or expired session", body = crate::auth::error::AuthErrorBody),
        (status = 403, description = "Caller lacks VIEW_AUDIT_LOG", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    security(("SessionCookie" = [])),
    tag = "audit-log",
)]
pub async fn audit_log_atom<S: AppStorage>(
    State(state): State<AppState<S>>,
    session: AuthSession,
    Query(query): Query<AuditLogQuery>,
) -> Result<Response, ApiError> {
    ensure_can_view_audit_log(&session)?;
    let page = load_entries(&state, query).await?;
    let feed = render_atom(&page.items)?;
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/atom+xml; charset=utf-8")],
        feed,
    )
        .into_response())
}

fn ensure_can_view_audit_log(session: &AuthSession) -> Result<(), ApiError> {
    if session.permissions.contains(Permissions::VIEW_AUDIT_LOG) {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

async fn load_entries<S: AppStorage>(
    state: &AppState<S>,
    query: AuditLogQuery,
) -> Result<PageSlice<AuditLogEntry>, ApiError> {
    let since = query.since.as_deref().map(parse_rfc3339).transpose()?;
    let until = query.until.as_deref().map(parse_rfc3339).transpose()?;
    if matches!((since, until), (Some(since), Some(until)) if since > until) {
        return Err(ApiError::InvalidInput(
            "since must be at or before until".to_string(),
        ));
    }

    let filter = AuditLogFilter {
        actor_username: query.actor,
        action: query.action,
        since,
        until,
    };
    let limit = query.limit.unwrap_or(DEFAULT_PAGE_SIZE);
    let cursor = query.cursor.map(Cursor);
    Ok(state
        .storage
        .audit_log()
        .list(filter, cursor, limit)
        .await?)
}

fn parse_rfc3339(raw: &str) -> Result<OffsetDateTime, ApiError> {
    OffsetDateTime::parse(raw, &Rfc3339)
        .map_err(|err| ApiError::InvalidInput(format!("timestamp must be RFC 3339: {err}")))
}

fn format_rfc3339(ts: OffsetDateTime) -> Result<String, ApiError> {
    ts.format(&Rfc3339)
        .map_err(|err| ApiError::Internal(format!("format audit timestamp: {err}")))
}

fn render_atom(entries: &[AuditLogEntry]) -> Result<String, ApiError> {
    let updated = entries
        .first()
        .map(|entry| entry.created_at)
        .unwrap_or_else(OffsetDateTime::now_utc);
    let mut out = String::from(r#"<?xml version="1.0" encoding="utf-8"?>"#);
    out.push_str(r#"<feed xmlns="http://www.w3.org/2005/Atom">"#);
    out.push_str("<title>thewiki audit log</title>");
    out.push_str("<id>urn:thewiki:audit-log</id>");
    out.push_str("<link rel=\"self\" href=\"");
    out.push_str(ATOM_SELF_PATH);
    out.push_str("\"/>");
    out.push_str("<updated>");
    out.push_str(&format_rfc3339(updated)?);
    out.push_str("</updated>");

    for entry in entries {
        out.push_str("<entry>");
        out.push_str("<id>urn:uuid:");
        out.push_str(&entry.id.to_string());
        out.push_str("</id>");
        out.push_str("<title>");
        out.push_str(&xml_escape(&format!(
            "{} {}",
            entry.action,
            entry.target_label.as_deref().unwrap_or(&entry.target_kind)
        )));
        out.push_str("</title>");
        out.push_str("<updated>");
        out.push_str(&format_rfc3339(entry.created_at)?);
        out.push_str("</updated>");
        out.push_str("<author><name>");
        out.push_str(&xml_escape(&entry.actor_username));
        out.push_str("</name></author>");
        out.push_str("<summary type=\"text\">");
        out.push_str(&xml_escape(&format!(
            "{} {}",
            entry.action,
            entry.target_label.as_deref().unwrap_or(&entry.target_kind)
        )));
        out.push_str("</summary>");
        out.push_str("<content type=\"application/json\">");
        out.push_str(&xml_escape(&entry.metadata.to_string()));
        out.push_str("</content>");
        out.push_str("</entry>");
    }

    out.push_str("</feed>");
    Ok(out)
}

fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
