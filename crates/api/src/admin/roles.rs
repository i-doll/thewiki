//! Admin endpoints for role management (#47).
//!
//! Surface:
//!
//! * `GET    /api/v1/admin/roles` — list every defined role.
//! * `POST   /api/v1/admin/roles` — create a role.
//! * `PUT    /api/v1/admin/roles/{id}` — update display name + permissions.
//! * `DELETE /api/v1/admin/roles/{id}` — delete; 409 when still assigned.
//!
//! Authorisation: every endpoint is gated by [`Permissions::MANAGE_ROLES`].
//! Each mutation writes an audit row capturing the before/after permission
//! set so the change history is recoverable.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;
use thewiki_core::{Permissions, Role, RoleId, RoleName};
use thewiki_storage::repo::{AuditLogRepository, NewAuditLogEntry, RoleRepository};
use thewiki_storage::StorageError;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::error::ApiError;
use crate::extractors::RequireAuth;
use crate::state::{AppState, AppStorage};

/// JSON representation of a role.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AdminRoleView {
    /// Stable identifier.
    pub id: Uuid,
    /// Machine name (URL-safe, immutable).
    pub name: String,
    /// Human-readable label.
    pub display_name: String,
    /// Permission set as the pipe-separated wire form (`"READ | EDIT"`).
    pub permissions: String,
    /// Permission flags as machine names (e.g. `["READ", "EDIT"]`). Handed
    /// back alongside the textual form because the admin UI binds against
    /// individual flags, not the joined string.
    pub permission_flags: Vec<String>,
    /// Number of users currently assigned to this role.
    pub assigned_users: u64,
}

/// Response from `GET /api/v1/admin/roles`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AdminRoleListResponse {
    /// Roles ordered by `name ASC`.
    pub items: Vec<AdminRoleView>,
}

/// Body for `POST /api/v1/admin/roles`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateRoleRequest {
    /// Machine name (immutable). Must satisfy [`RoleName`]'s validation.
    pub name: String,
    /// Display label.
    pub display_name: String,
    /// Permission flag names (e.g. `["READ", "EDIT", "CREATE"]`). The
    /// pipe-separated string the wire emits is also accepted for ergonomic
    /// parity with the GraphQL clients.
    pub permissions: Vec<String>,
}

/// Body for `PUT /api/v1/admin/roles/{id}`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateRoleRequest {
    /// New display label. Omit to leave unchanged.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Replacement permission flag names. Omit to leave unchanged.
    #[serde(default)]
    pub permissions: Option<Vec<String>>,
}

fn ensure_manage_roles(actor: &RequireAuth) -> Result<(), ApiError> {
    if actor.permissions.contains(Permissions::MANAGE_ROLES) {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

/// Format a permission bitset as pipe-separated flag names.
fn format_permissions(p: Permissions) -> String {
    use bitflags::Flags;
    let mut parts = Vec::new();
    for flag in Permissions::FLAGS {
        if p.contains(*flag.value()) {
            parts.push(flag.name());
        }
    }
    parts.join(" | ")
}

/// Decompose a permission bitset into individual flag names.
fn permission_flags(p: Permissions) -> Vec<String> {
    use bitflags::Flags;
    Permissions::FLAGS
        .iter()
        .filter(|flag| p.contains(*flag.value()))
        .map(|flag| flag.name().to_owned())
        .collect()
}

/// Parse caller-supplied permission flag names into a [`Permissions`] set.
///
/// Accepts either a list of flag names (`["READ", "EDIT"]`) or a single
/// pipe-separated string (`["READ | EDIT"]`). Empty list / empty string
/// resolves to [`Permissions::empty`]. Unknown names return 400.
fn parse_permissions(input: &[String]) -> Result<Permissions, ApiError> {
    use bitflags::Flags;
    let mut perms = Permissions::empty();
    for raw in input {
        for token in raw.split('|') {
            let trimmed = token.trim();
            if trimmed.is_empty() {
                continue;
            }
            let flag = Permissions::FLAGS
                .iter()
                .find(|f| f.name().eq_ignore_ascii_case(trimmed))
                .ok_or_else(|| {
                    ApiError::InvalidInput(format!("unknown permission flag: {trimmed}"))
                })?;
            perms |= *flag.value();
        }
    }
    Ok(perms)
}

async fn build_view<S: AppStorage>(
    state: &AppState<S>,
    role: Role,
) -> Result<AdminRoleView, ApiError> {
    let assigned_users = state.storage.roles().count_users(role.id).await?;
    Ok(AdminRoleView {
        id: role.id.into_uuid(),
        name: role.name.into_string(),
        display_name: role.display_name,
        permissions: format_permissions(role.permissions),
        permission_flags: permission_flags(role.permissions),
        assigned_users,
    })
}

/// `GET /api/v1/admin/roles` — list every role.
#[utoipa::path(
    get,
    path = "",
    responses(
        (status = 200, description = "Role list", body = AdminRoleListResponse),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "Caller lacks MANAGE_ROLES", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    security(("SessionCookie" = [])),
    tag = "admin-roles",
)]
pub async fn list_roles<S: AppStorage>(
    State(state): State<AppState<S>>,
    actor: RequireAuth,
) -> Result<Json<AdminRoleListResponse>, ApiError> {
    ensure_manage_roles(&actor)?;
    let roles = state.storage.roles().list().await?;
    let mut items = Vec::with_capacity(roles.len());
    for role in roles {
        items.push(build_view(&state, role).await?);
    }
    Ok(Json(AdminRoleListResponse { items }))
}

/// `POST /api/v1/admin/roles` — create a role.
#[utoipa::path(
    post,
    path = "",
    request_body = CreateRoleRequest,
    responses(
        (status = 201, description = "Role created", body = AdminRoleView),
        (status = 400, description = "Invalid input", body = crate::error::ErrorBody),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "Caller lacks MANAGE_ROLES", body = crate::error::ErrorBody),
        (status = 409, description = "Role name already in use", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    security(("SessionCookie" = [])),
    tag = "admin-roles",
)]
pub async fn create_role<S: AppStorage>(
    State(state): State<AppState<S>>,
    actor: RequireAuth,
    Json(req): Json<CreateRoleRequest>,
) -> Result<(StatusCode, Json<AdminRoleView>), ApiError> {
    ensure_manage_roles(&actor)?;
    if req.display_name.trim().is_empty() {
        return Err(ApiError::InvalidInput(
            "display_name must not be empty".into(),
        ));
    }
    let name = RoleName::new(&req.name)
        .map_err(|err| ApiError::InvalidInput(format!("name: {err}")))?;
    let permissions = parse_permissions(&req.permissions)?;

    let role = Role {
        id: RoleId::new(),
        name,
        display_name: req.display_name.clone(),
        permissions,
    };
    state.storage.roles().create(&role).await?;

    let audit = NewAuditLogEntry {
        actor_id: actor.user_id,
        actor_username: actor.username.clone(),
        action: "role.create".to_owned(),
        target_kind: "role".to_owned(),
        target_id: role.id.into_uuid(),
        target_label: Some(role.name.as_str().to_owned()),
        metadata: json!({
            "name": role.name.as_str(),
            "display_name": role.display_name,
            "permissions": format_permissions(role.permissions),
        }),
    };
    state.storage.audit_log().create(audit).await?;

    let view = build_view(&state, role).await?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// `PUT /api/v1/admin/roles/{id}` — update display name + permissions.
#[utoipa::path(
    put,
    path = "/{id}",
    params(
        ("id" = Uuid, Path, description = "Role identifier"),
    ),
    request_body = UpdateRoleRequest,
    responses(
        (status = 200, description = "Role updated", body = AdminRoleView),
        (status = 400, description = "Invalid input", body = crate::error::ErrorBody),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "Caller lacks MANAGE_ROLES", body = crate::error::ErrorBody),
        (status = 404, description = "Role not found", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    security(("SessionCookie" = [])),
    tag = "admin-roles",
)]
pub async fn update_role<S: AppStorage>(
    State(state): State<AppState<S>>,
    actor: RequireAuth,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateRoleRequest>,
) -> Result<Json<AdminRoleView>, ApiError> {
    ensure_manage_roles(&actor)?;
    let role_id = RoleId::from_uuid(id);
    let mut role = state.storage.roles().get_by_id(role_id).await?;
    let previous_display = role.display_name.clone();
    let previous_perms = role.permissions;

    let mut changed = false;
    if let Some(display_name) = req.display_name {
        if display_name.trim().is_empty() {
            return Err(ApiError::InvalidInput(
                "display_name must not be empty".into(),
            ));
        }
        if display_name != role.display_name {
            role.display_name = display_name;
            changed = true;
        }
    }
    if let Some(perms_input) = req.permissions {
        let new_perms = parse_permissions(&perms_input)?;
        if new_perms != role.permissions {
            role.permissions = new_perms;
            changed = true;
        }
    }

    if changed {
        state.storage.roles().update(&role).await?;
        let audit = NewAuditLogEntry {
            actor_id: actor.user_id,
            actor_username: actor.username.clone(),
            action: "role.update".to_owned(),
            target_kind: "role".to_owned(),
            target_id: role.id.into_uuid(),
            target_label: Some(role.name.as_str().to_owned()),
            metadata: json!({
                "name": role.name.as_str(),
                "from_display_name": previous_display,
                "to_display_name": role.display_name,
                "from_permissions": format_permissions(previous_perms),
                "to_permissions": format_permissions(role.permissions),
            }),
        };
        state.storage.audit_log().create(audit).await?;
    }

    let view = build_view(&state, role).await?;
    Ok(Json(view))
}

/// `DELETE /api/v1/admin/roles/{id}` — delete a role.
#[utoipa::path(
    delete,
    path = "/{id}",
    params(
        ("id" = Uuid, Path, description = "Role identifier"),
    ),
    responses(
        (status = 204, description = "Role deleted"),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "Caller lacks MANAGE_ROLES", body = crate::error::ErrorBody),
        (status = 404, description = "Role not found", body = crate::error::ErrorBody),
        (status = 409, description = "Role still assigned to users", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    security(("SessionCookie" = [])),
    tag = "admin-roles",
)]
pub async fn delete_role<S: AppStorage>(
    State(state): State<AppState<S>>,
    actor: RequireAuth,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    ensure_manage_roles(&actor)?;
    let role_id = RoleId::from_uuid(id);
    let role = state.storage.roles().get_by_id(role_id).await?;

    // Refuse explicitly when the role is still assigned so the operator
    // sees the cleanest possible 409. The storage impl enforces the same
    // invariant for defence-in-depth — see `SqliteRoleRepository::delete`.
    let assigned = state.storage.roles().count_users(role_id).await?;
    if assigned > 0 {
        return Err(ApiError::Conflict(format!(
            "role is still assigned to {assigned} user(s); revoke first"
        )));
    }

    match state.storage.roles().delete(role_id).await {
        Ok(()) => {}
        Err(StorageError::Conflict(msg)) => return Err(ApiError::Conflict(msg)),
        Err(other) => return Err(other.into()),
    }

    let audit = NewAuditLogEntry {
        actor_id: actor.user_id,
        actor_username: actor.username.clone(),
        action: "role.delete".to_owned(),
        target_kind: "role".to_owned(),
        target_id: role.id.into_uuid(),
        target_label: Some(role.name.as_str().to_owned()),
        metadata: json!({
            "name": role.name.as_str(),
            "display_name": role.display_name,
            "permissions": format_permissions(role.permissions),
        }),
    };
    state.storage.audit_log().create(audit).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Build the role-admin subrouter (`/api/v1/admin/roles`).
pub fn router<S: AppStorage>() -> OpenApiRouter<AppState<S>> {
    OpenApiRouter::new()
        .routes(routes!(list_roles, create_role))
        .routes(routes!(update_role, delete_role))
}
