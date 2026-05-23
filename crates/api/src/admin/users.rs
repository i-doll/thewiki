//! Admin endpoints for user management (#47).
//!
//! Surface:
//!
//! * `GET    /api/v1/admin/users` — paginated list with `?search=` and `?role_id=` filters.
//! * `GET    /api/v1/admin/users/{id}` — single user + their roles.
//! * `POST   /api/v1/admin/users/{id}/roles` — assign one or more roles (body `{ role_ids: [...] }`).
//! * `DELETE /api/v1/admin/users/{id}/roles/{role_id}` — revoke a role.
//!
//! Every mutation is gated by [`Permissions::MANAGE_USERS`] and writes an
//! audit row with the standard `(actor_id, actor_username, action, target_kind,
//! target_id, target_label, metadata)` shape. Reads also require the
//! permission — listing the user table is a privileged operation.
//!
//! Bulk assign is built on the singular endpoint by accepting a list in the
//! body: the SPA calls one POST per user it wants to mutate (the list of
//! `role_ids` makes per-user assigns atomic, but cross-user atomicity
//! belongs in the client because partial failure is a useful signal — see
//! the "/admin/users" bulk-role-assign panel in the SPA).

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;
use thewiki_core::{Permissions, RoleId, UserId};
use thewiki_storage::repo::{
    AuditLogRepository, Cursor, NewAuditLogEntry, PageSlice, RoleRepository, UserListFilter,
    UserRepository,
};
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::error::ApiError;
use crate::extractors::RequireAuth;
use crate::state::{AppState, AppStorage};

/// A role attached to a user, returned by the user list and detail endpoints.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UserRoleView {
    /// Role identifier.
    pub id: Uuid,
    /// Machine name (URL-safe, immutable).
    pub name: String,
    /// Human-readable label.
    pub display_name: String,
    /// Pipe-separated permission flag string, e.g. `"READ | EDIT"`.
    pub permissions: String,
}

/// One user as the admin UI sees them.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AdminUserView {
    /// User identifier.
    pub id: Uuid,
    /// Login handle.
    pub username: String,
    /// Optional email.
    pub email: Option<String>,
    /// Optional display name.
    pub display_name: Option<String>,
    /// When the row was created.
    pub created_at: OffsetDateTime,
    /// Last login timestamp, if any.
    pub last_login_at: Option<OffsetDateTime>,
    /// Roles attached to this user.
    pub roles: Vec<UserRoleView>,
}

/// Paginated response from `GET /api/v1/admin/users`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AdminUserListResponse {
    /// Rows in this batch, ordered by `(created_at ASC, id ASC)`.
    pub items: Vec<AdminUserView>,
    /// Cursor to pass back for the next page; `None` when exhausted.
    pub next_cursor: Option<String>,
}

/// Query parameters for `GET /api/v1/admin/users`.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AdminUserListQuery {
    /// Substring match against `username` or `email` (case-insensitive).
    pub search: Option<String>,
    /// Filter to users who currently hold this role.
    pub role_id: Option<Uuid>,
    /// Page size. Clamped by the storage default (`50`) / hard cap (`500`).
    pub limit: Option<u32>,
    /// Opaque cursor returned by the previous response.
    pub cursor: Option<String>,
}

/// Body for `POST /api/v1/admin/users/{id}/roles`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct AssignRolesRequest {
    /// Role ids to grant. Already-held roles are no-ops.
    pub role_ids: Vec<Uuid>,
}

/// Verify the calling session carries [`Permissions::MANAGE_USERS`].
fn ensure_manage_users(actor: &RequireAuth) -> Result<(), ApiError> {
    if actor.permissions.contains(Permissions::MANAGE_USERS) {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

/// Build the per-user role view from the domain `Role`.
fn role_view(role: &thewiki_core::Role) -> UserRoleView {
    UserRoleView {
        id: role.id.into_uuid(),
        name: role.name.as_str().to_owned(),
        display_name: role.display_name.clone(),
        permissions: format_permissions(role.permissions),
    }
}

/// Format a permission bitset as the same `"READ | EDIT"` wire form the
/// auth `/me` endpoint emits. Duplicated here rather than reaching across
/// modules so the user list isn't coupled to the auth router.
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

/// Convert a domain `User` + their roles into the admin DTO.
fn admin_user_view(user: thewiki_core::User, roles: Vec<thewiki_core::Role>) -> AdminUserView {
    AdminUserView {
        id: user.id.into_uuid(),
        username: user.username.into_string(),
        email: user.email.map(|e| e.into_string()),
        display_name: user.display_name,
        created_at: user.created_at,
        last_login_at: user.last_login_at,
        roles: roles.iter().map(role_view).collect(),
    }
}

/// `GET /api/v1/admin/users` — paginated user list.
#[utoipa::path(
    get,
    path = "",
    params(AdminUserListQuery),
    responses(
        (status = 200, description = "User list", body = AdminUserListResponse),
        (status = 400, description = "Malformed query", body = crate::error::ErrorBody),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "Caller lacks MANAGE_USERS", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    security(("SessionCookie" = [])),
    tag = "admin-users",
)]
pub async fn list_users<S: AppStorage>(
    State(state): State<AppState<S>>,
    actor: RequireAuth,
    Query(query): Query<AdminUserListQuery>,
) -> Result<Json<AdminUserListResponse>, ApiError> {
    ensure_manage_users(&actor)?;

    let filter = UserListFilter {
        search: query.search,
        role_id: query.role_id.map(RoleId::from_uuid),
    };
    // Honour the operator's per-deployment override
    // (`state.route_config.default_page_size`) rather than the
    // module-level constant — matches every other paginated endpoint.
    let limit = query
        .limit
        .unwrap_or(state.route_config.default_page_size);
    let cursor = query.cursor.map(Cursor);

    let PageSlice { items, next } = state.storage.users().list(filter, cursor, limit).await?;

    // Hydrate each row with its assigned roles in a single batched query
    // instead of fanning out one `list_for_user` per row. A maxed-out
    // page is 500 users; the old loop fired 500 sequential queries
    // (plus the initial list), turning into a noticeable stall on
    // anything beyond a single-machine deploy.
    let user_ids: Vec<_> = items.iter().map(|u| u.id).collect();
    let mut roles_by_user = state.storage.roles().list_roles_for_users(&user_ids).await?;
    let views: Vec<_> = items
        .into_iter()
        .map(|user| {
            let roles = roles_by_user.remove(&user.id).unwrap_or_default();
            admin_user_view(user, roles)
        })
        .collect();
    Ok(Json(AdminUserListResponse {
        items: views,
        next_cursor: next.map(|c| c.0),
    }))
}

/// `GET /api/v1/admin/users/{id}` — single user detail.
#[utoipa::path(
    get,
    path = "/{id}",
    params(
        ("id" = Uuid, Path, description = "User identifier"),
    ),
    responses(
        (status = 200, description = "User detail", body = AdminUserView),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "Caller lacks MANAGE_USERS", body = crate::error::ErrorBody),
        (status = 404, description = "User not found", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    security(("SessionCookie" = [])),
    tag = "admin-users",
)]
pub async fn get_user<S: AppStorage>(
    State(state): State<AppState<S>>,
    actor: RequireAuth,
    Path(id): Path<Uuid>,
) -> Result<Json<AdminUserView>, ApiError> {
    ensure_manage_users(&actor)?;
    let user = state.storage.users().get_by_id(UserId::from_uuid(id)).await?;
    let roles = state.storage.roles().list_for_user(user.id).await?;
    Ok(Json(admin_user_view(user, roles)))
}

/// `POST /api/v1/admin/users/{id}/roles` — assign one or more roles.
#[utoipa::path(
    post,
    path = "/{id}/roles",
    params(
        ("id" = Uuid, Path, description = "User identifier"),
    ),
    request_body = AssignRolesRequest,
    responses(
        (status = 200, description = "User after the assignment", body = AdminUserView),
        (status = 400, description = "Empty or duplicate role list", body = crate::error::ErrorBody),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "Caller lacks MANAGE_USERS", body = crate::error::ErrorBody),
        (status = 404, description = "User or role not found", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    security(("SessionCookie" = [])),
    tag = "admin-users",
)]
pub async fn assign_roles<S: AppStorage>(
    State(state): State<AppState<S>>,
    actor: RequireAuth,
    Path(id): Path<Uuid>,
    Json(req): Json<AssignRolesRequest>,
) -> Result<Json<AdminUserView>, ApiError> {
    ensure_manage_users(&actor)?;
    if req.role_ids.is_empty() {
        return Err(ApiError::InvalidInput("role_ids must not be empty".into()));
    }
    // Reject duplicate ids up front. The assign call itself is
    // idempotent, but a duplicate slips through the "skip already-held
    // roles" filter on the *first* iteration and would otherwise emit
    // two `user.role.assign` audit rows for the same logical
    // assignment.
    let mut seen = std::collections::HashSet::with_capacity(req.role_ids.len());
    for rid in &req.role_ids {
        if !seen.insert(*rid) {
            return Err(ApiError::InvalidInput(
                "role_ids must not contain duplicates".into(),
            ));
        }
    }
    let user_id = UserId::from_uuid(id);
    let user = state.storage.users().get_by_id(user_id).await?;

    // Resolve each role up-front so a missing id returns a clean 404
    // *before* we mutate anything. The assign call itself is idempotent;
    // we still emit one audit row per assign (skipping no-ops) so the
    // history reflects operator intent.
    let mut resolved = Vec::with_capacity(req.role_ids.len());
    for rid in &req.role_ids {
        let role = state.storage.roles().get_by_id(RoleId::from_uuid(*rid)).await?;
        resolved.push(role);
    }
    let existing = state.storage.roles().list_for_user(user_id).await?;
    let existing_ids: std::collections::HashSet<RoleId> = existing.iter().map(|r| r.id).collect();

    for role in &resolved {
        if existing_ids.contains(&role.id) {
            continue;
        }
        state
            .storage
            .roles()
            .assign_to_user(user_id, role.id)
            .await?;
        let audit = NewAuditLogEntry {
            actor_id: actor.user_id,
            actor_username: actor.username.clone(),
            action: "user.role.assign".to_owned(),
            target_kind: "user".to_owned(),
            target_id: user.id.into_uuid(),
            target_label: Some(user.username.as_str().to_owned()),
            metadata: json!({
                "role_id": role.id.into_uuid(),
                "role_name": role.name.as_str(),
            }),
        };
        state.storage.audit_log().create(audit).await?;
    }

    let fresh_roles = state.storage.roles().list_for_user(user_id).await?;
    Ok(Json(admin_user_view(user, fresh_roles)))
}

/// `DELETE /api/v1/admin/users/{id}/roles/{role_id}` — revoke a role.
#[utoipa::path(
    delete,
    path = "/{id}/roles/{role_id}",
    params(
        ("id" = Uuid, Path, description = "User identifier"),
        ("role_id" = Uuid, Path, description = "Role identifier"),
    ),
    responses(
        (status = 204, description = "Role revoked (idempotent)"),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "Caller lacks MANAGE_USERS", body = crate::error::ErrorBody),
        (status = 404, description = "User or role not found", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    security(("SessionCookie" = [])),
    tag = "admin-users",
)]
pub async fn revoke_role<S: AppStorage>(
    State(state): State<AppState<S>>,
    actor: RequireAuth,
    Path((id, role_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    ensure_manage_users(&actor)?;
    let user_id = UserId::from_uuid(id);
    let user = state.storage.users().get_by_id(user_id).await?;
    let role = state.storage.roles().get_by_id(RoleId::from_uuid(role_id)).await?;

    // Only emit an audit row when the user actually held the role. The
    // revoke call itself is idempotent so calling DELETE twice is a no-op;
    // the audit log should match real state transitions.
    let held = state.storage.roles().list_for_user(user_id).await?;
    let was_assigned = held.iter().any(|r| r.id == role.id);
    state
        .storage
        .roles()
        .revoke_from_user(user_id, role.id)
        .await?;
    if was_assigned {
        let audit = NewAuditLogEntry {
            actor_id: actor.user_id,
            actor_username: actor.username.clone(),
            action: "user.role.revoke".to_owned(),
            target_kind: "user".to_owned(),
            target_id: user.id.into_uuid(),
            target_label: Some(user.username.as_str().to_owned()),
            metadata: json!({
                "role_id": role.id.into_uuid(),
                "role_name": role.name.as_str(),
            }),
        };
        state.storage.audit_log().create(audit).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Build the user-admin subrouter (`/api/v1/admin/users`).
pub fn router<S: AppStorage>() -> OpenApiRouter<AppState<S>> {
    OpenApiRouter::new()
        .routes(routes!(list_users))
        .routes(routes!(get_user))
        .routes(routes!(assign_roles))
        .routes(routes!(revoke_role))
}
