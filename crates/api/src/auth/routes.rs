//! Auth HTTP handlers: `/login`, `/logout`, `/me`.
//!
//! The routes are mounted under `/api/v1/auth` by [`build_router`] and
//! consume an [`AuthState`]. All three sit *behind* the cookie + CSRF stack
//! configured by [`crate::app::build_with_state`].
//!
//! Wire contract:
//!
//! | Route                  | Method | Body                                  | Success                                                  |
//! |------------------------|--------|---------------------------------------|----------------------------------------------------------|
//! | `/api/v1/auth/login`   | POST   | `{ "username": "...", "password":...}`| 200 + `Set-Cookie: thewiki_session; thewiki_csrf` + body |
//! | `/api/v1/auth/logout`  | POST   | (empty)                               | 204 + `Set-Cookie: ...; Max-Age=0` (both names)          |
//! | `/api/v1/auth/me`      | GET    | (none)                                | 200 + user payload                                       |
//!
//! Failure paths all funnel through [`AuthError::into_response`] so the JSON
//! shape is consistent.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thewiki_core::Username;
use thewiki_core::{Permissions, Role, User, UserId};
use thewiki_storage::StorageError;
use thewiki_storage::repo::{
    AuditLogRepository, NewAuditLogEntry, RoleRepository, SessionRepository, UserRepository,
};
use time::OffsetDateTime;
use tower_cookies::Cookies;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::auth::error::AuthError;
use crate::auth::extractors::AuthSession;
use crate::auth::password::PasswordHasher;
use crate::auth::session::{
    CSRF_COOKIE, SESSION_COOKIE, build_clearing_cookie, build_csrf_cookie, build_session_cookie,
    fresh_csrf_token,
};
use crate::auth::state::AuthState;
use crate::config::RegistrationPolicy;

/// JSON body for [`login`].
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct LoginRequest {
    /// Login handle. Must validate as a [`Username`](thewiki_core::Username);
    /// invalid strings short-circuit to 401 with the generic error to avoid
    /// leaking what's a valid handle.
    pub username: String,
    /// Password in clear. Compared in constant time via Argon2id verify.
    pub password: String,
}

/// JSON body for a successful login / `/me`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UserPayload {
    /// User ID (UUIDv7).
    pub id: uuid::Uuid,
    /// Login handle.
    pub username: String,
    /// Display name (falls back to username at the UI layer).
    pub display_name: Option<String>,
    /// Email, if any.
    pub email: Option<String>,
    /// Role names the user holds. Permission bits are not exposed on the
    /// wire to keep the surface stable across permission-set changes.
    pub roles: Vec<String>,
    /// Effective permission flags, pipe-separated (e.g. `"READ | EDIT"`).
    /// Convenience for the SPA so it doesn't need to re-evaluate the role
    /// table.
    pub permissions: String,
}

fn user_payload(user: &User, roles: &[Role], permissions: Permissions) -> UserPayload {
    UserPayload {
        id: user.id.into_uuid(),
        username: user.username.as_str().to_owned(),
        display_name: user.display_name.clone(),
        email: user.email.as_ref().map(|e| e.as_str().to_owned()),
        roles: roles.iter().map(|r| r.name.as_str().to_owned()).collect(),
        permissions: format_permissions(permissions),
    }
}

/// Format a [`Permissions`] set as a human-readable pipe-separated string
/// (e.g. `"READ | EDIT"`). An empty set is the empty string. Keeps the
/// `bitflags`-Debug wart (`Permissions(0x0)`) off the wire.
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

/// `POST /api/v1/auth/login` — verify credentials and issue a session cookie.
///
/// Constant-time guarantee on the failure path: when the username is unknown,
/// we still run an Argon2 verify against a pre-computed PHC string. Wall-clock
/// time per attempt is therefore dominated by the configured Argon2 cost
/// regardless of which arm fires.
///
/// Rate limiting is applied by the router middleware before this handler runs.
#[utoipa::path(
    post,
    path = "/login",
    params(
        ("cookie" = Option<String>, Header, description = "Optional existing session and CSRF cookies: `thewiki_session=...; thewiki_csrf=...`. Required only when a session cookie is present."),
        ("x-csrf-token" = Option<String>, Header, description = "Double-submit CSRF token matching the `thewiki_csrf` cookie. Required only when a session cookie is present."),
    ),
    request_body = LoginRequest,
    responses(
        (status = 200, description = "Authenticated user and session cookies", body = UserPayload),
        (status = 401, description = "Invalid credentials", body = crate::auth::error::AuthErrorBody),
        (status = 403, description = "CSRF token missing or invalid when a session cookie is present", body = crate::auth::error::AuthErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
        (status = 500, description = "Authentication storage or hashing failure", body = crate::auth::error::AuthErrorBody),
    ),
    tag = "auth",
)]
pub async fn login(
    State(state): State<AuthState>,
    cookies: Cookies,
    Json(req): Json<LoginRequest>,
) -> Result<Response, AuthError> {
    // Always do an Argon2 verify, even on the "user doesn't exist" branch, so
    // response time doesn't leak username existence.
    let dummy_phc = state.hasher.dummy_hash_for_timing()?;

    let username_parsed = Username::new(req.username.clone());
    let user_lookup = match username_parsed {
        Ok(u) => state.storage.users().get_by_username(&u).await,
        // Invalid username: fall through to a verify against the dummy so we
        // burn the same wall-clock time as the real path.
        Err(_) => Err(StorageError::NotFound),
    };

    let (user, password_hash) = match user_lookup {
        Ok(user) => {
            let hash = fetch_password_hash(&state, user.id).await?;
            (Some(user), hash)
        }
        Err(StorageError::NotFound) => {
            // Match the wall-clock + DB-roundtrip shape of the found-user
            // path: we burn the same `fetch_password_hash` round-trip against
            // a throwaway UUIDv7 (which won't match anything; result is None).
            // Without this, "user not found" returns one DB RTT faster than
            // "user found, wrong password", which is enough to enumerate
            // usernames over a high-latency link.
            let _ = fetch_password_hash(&state, UserId::new()).await?;
            (None, None)
        }
        Err(e) => return Err(AuthError::Storage(e)),
    };

    // Pick the hash to compare against: real one if known, dummy otherwise.
    let hash_to_check = password_hash.as_deref().unwrap_or(&dummy_phc);
    let ok = state.hasher.verify(&req.password, hash_to_check)?;

    let Some(user) = user else {
        return Err(AuthError::InvalidCredentials);
    };
    if !ok {
        return Err(AuthError::InvalidCredentials);
    }
    // password_hash being None means the account has no password set — treat
    // as no credentials match.
    if password_hash.is_none() {
        return Err(AuthError::InvalidCredentials);
    }

    // Issue a session row.
    let session = state
        .storage
        .sessions()
        .create(user.id, state.session_ttl, None, None)
        .await?;

    // Generate a CSRF token and store it on the cookie. We deliberately do
    // *not* persist this token server-side — the double-submit pattern only
    // needs the client to echo it back; treating both copies as derived from
    // the same source-of-truth (the cookie) is the whole point.
    let csrf = fresh_csrf_token();
    cookies.add(build_session_cookie(
        session.id,
        session.expires_at,
        state.secure_cookies,
    ));
    cookies.add(build_csrf_cookie(
        &csrf,
        session.expires_at,
        state.secure_cookies,
    ));

    // Update last_login_at on the user row. Failure here is non-fatal — the
    // login still succeeded; we surface it in logs.
    let mut updated = user.clone();
    updated.last_login_at = Some(OffsetDateTime::now_utc());
    if let Err(e) = state.storage.users().update(&updated).await {
        tracing::warn!(error = %e, "failed to bump last_login_at");
    }

    // Audit-log the login. Best-effort: a failure to persist the audit row
    // must not invalidate the freshly issued session, so we log and continue.
    record_auth_event(
        &state,
        user.id,
        user.username.as_str(),
        "auth.login",
        json!({ "session_id": session.id.into_uuid() }),
    )
    .await;

    let roles = state
        .storage
        .roles()
        .list_for_user(user.id)
        .await
        .map_err(AuthError::Storage)?;
    let permissions = roles
        .iter()
        .fold(Permissions::empty(), |acc, r| acc | r.permissions);

    let payload = user_payload(&user, &roles, permissions);
    Ok((StatusCode::OK, Json(payload)).into_response())
}

/// `POST /api/v1/auth/logout` — revoke the session and clear both cookies.
///
/// Requires a valid session. The CSRF layer also requires the matching
/// `X-CSRF-Token` header because logout is a state-mutating call.
#[utoipa::path(
    post,
    path = "/logout",
    params(
        ("cookie" = String, Header, description = "Session and CSRF cookies: `thewiki_session=...; thewiki_csrf=...`"),
        ("x-csrf-token" = String, Header, description = "Double-submit CSRF token matching the `thewiki_csrf` cookie"),
    ),
    responses(
        (status = 204, description = "Session revoked and cookies cleared"),
        (status = 401, description = "Missing or expired session", body = crate::auth::error::AuthErrorBody),
        (status = 403, description = "CSRF token missing or invalid", body = crate::auth::error::AuthErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
        (status = 500, description = "Authentication storage failure", body = crate::auth::error::AuthErrorBody),
    ),
    tag = "auth",
)]
pub async fn logout(
    State(state): State<AuthState>,
    cookies: Cookies,
    auth: AuthSession,
) -> Result<Response, AuthError> {
    // Best-effort delete: if it's already gone (race with a parallel logout)
    // we still want to clear the client-side cookies.
    match state.storage.sessions().delete(auth.session_id).await {
        Ok(()) | Err(StorageError::NotFound) => {}
        Err(e) => return Err(AuthError::Storage(e)),
    }
    cookies.add(build_clearing_cookie(SESSION_COOKIE, state.secure_cookies));
    cookies.add(build_clearing_cookie(CSRF_COOKIE, state.secure_cookies));

    // Audit-log the logout. Best-effort — same rationale as login.
    record_auth_event(
        &state,
        auth.user.id,
        auth.user.username.as_str(),
        "auth.logout",
        json!({ "session_id": auth.session_id.into_uuid() }),
    )
    .await;

    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Persist one auth-related audit row. The actor's own user row is also the
/// audit target (kind=`user`), since the action targets the actor's session.
///
/// Best-effort by design: handlers that call this have already committed the
/// authoritative state (the session row, or its deletion). Surfacing a
/// storage error here would force the client into a confusing retry loop
/// where the session is gone but the response says it failed. We log the
/// failure and move on so operators see it without harming the live request.
async fn record_auth_event(
    state: &AuthState,
    user_id: UserId,
    username: &str,
    action: &str,
    metadata: Value,
) {
    let entry = NewAuditLogEntry {
        actor_id: user_id,
        actor_username: username.to_owned(),
        action: action.to_owned(),
        target_kind: "user".to_owned(),
        target_id: user_id.into_uuid(),
        target_label: Some(username.to_owned()),
        metadata,
    };
    if let Err(err) = state.storage.audit_log().create(entry).await {
        tracing::warn!(error = %err, action, "failed to write auth audit row");
    }
}

/// `GET /api/v1/auth/me` — return the authenticated user payload.
#[utoipa::path(
    get,
    path = "/me",
    params(("cookie" = String, Header, description = "`thewiki_session` cookie")),
    responses(
        (status = 200, description = "Authenticated user", body = UserPayload),
        (status = 401, description = "Missing or expired session", body = crate::auth::error::AuthErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
        (status = 500, description = "Authentication storage failure", body = crate::auth::error::AuthErrorBody),
    ),
    tag = "auth",
)]
pub async fn me(
    State(state): State<AuthState>,
    auth: AuthSession,
) -> Result<Json<UserPayload>, AuthError> {
    let roles = state
        .storage
        .roles()
        .list_for_user(auth.user.id)
        .await
        .map_err(AuthError::Storage)?;
    Ok(Json(user_payload(&auth.user, &roles, auth.permissions)))
}

/// Wire shape of `GET /api/v1/auth/policy`.
///
/// The SPA reads this on boot to decide whether to render the "Sign up" CTA
/// (open / invite) and whether to surface an "Edit anonymously" affordance.
/// Kept narrow on purpose — operators tune fifteen knobs in `thewiki.toml`,
/// but only these two affect what the SPA shows the user *before* they have
/// a session.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AuthPolicyPayload {
    /// Account registration policy: `"open"`, `"invite"`, or `"closed"`.
    pub registration: String,
    /// Whether anonymous (logged-out) callers can submit edits.
    pub anonymous_edits: bool,
    /// Whether edits land in a moderator approval queue. The exact scope
    /// (`"none"`, `"anonymous"`, `"new-users"`, `"all"`) is exposed so the
    /// SPA can show a "your edit will be reviewed" hint before the user
    /// clicks save.
    pub approval_required_for: String,
}

/// `GET /api/v1/auth/policy` — publish the operator-configured auth shape
/// so the SPA can render the right affordances.
///
/// Always available (no auth required) — by design, since the answer is what
/// the UI needs *before* the user has a session.
#[utoipa::path(
    get,
    path = "/policy",
    responses(
        (status = 200, description = "Auth policy", body = AuthPolicyPayload),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "auth",
)]
pub async fn policy(State(state): State<AuthState>) -> Json<AuthPolicyPayload> {
    let registration = match state.config.registration {
        RegistrationPolicy::Open => "open",
        RegistrationPolicy::Invite => "invite",
        RegistrationPolicy::Closed => "closed",
    };
    let approval = match state.config.approval_required_for {
        crate::config::ApprovalScope::None => "none",
        crate::config::ApprovalScope::Anonymous => "anonymous",
        crate::config::ApprovalScope::NewUsers => "new-users",
        crate::config::ApprovalScope::All => "all",
    };
    Json(AuthPolicyPayload {
        registration: registration.to_string(),
        anonymous_edits: state.config.anonymous_edits,
        approval_required_for: approval.to_string(),
    })
}

/// Build the auth router. Mounted under `/api/v1/auth` by [`crate::app::build_with_state`].
pub fn build_router() -> OpenApiRouter<AuthState> {
    OpenApiRouter::new()
        .routes(routes!(login))
        .routes(routes!(logout))
        .routes(routes!(me))
        .routes(routes!(policy))
}

/// Look up a user's stored PHC password hash. `None` means the user exists
/// but has no password set (an externally-authenticated account, say).
async fn fetch_password_hash(
    state: &AuthState,
    user_id: thewiki_core::UserId,
) -> Result<Option<String>, AuthError> {
    let id_bytes = *user_id.as_uuid().as_bytes();
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT password_hash FROM users WHERE id = ?1")
            .bind(id_bytes.as_slice())
            .fetch_optional(state.storage.pool())
            .await
            .map_err(|e| AuthError::Storage(StorageError::Database(e)))?;
    Ok(row.and_then(|(h,)| h))
}
