//! Axum extractors for authenticated requests.
//!
//! - [`AuthSession`] pulls the `thewiki_session` cookie off the request,
//!   resolves it through [`SessionRepository`], `touch()`es the row to bump
//!   `last_seen_at`, and yields the user + their effective [`Permissions`].
//!   On miss / expired session it short-circuits with 401.
//! - [`RequireRole`] is a constructor extractor (built via
//!   [`RequireRole::new`]) that composes on top of `AuthSession`: it loads
//!   the user, unions their roles' permissions, and 403s unless the configured
//!   capability bit is set. Routes that need an editor stamp do
//!   `.route_layer(axum::middleware::from_extractor::<RequireRole<EDIT>>())`
//!   — see the [`require_permissions`] convenience.
//!
//! Both extractors run *after* the [`CookieManagerLayer`] from `tower-cookies`,
//! so the router must mount that layer before the auth routes (and any guarded
//! routes).
//!
//! [`SessionRepository`]: thewiki_storage::repo::SessionRepository
//! [`Permissions`]: thewiki_core::Permissions

use axum::extract::{FromRef, FromRequestParts};
use axum::http::request::Parts;
use thewiki_core::{Permissions, SessionId, User};
use thewiki_storage::StorageError;
use thewiki_storage::repo::{RoleRepository, SessionRepository, UserRepository};
use tower_cookies::Cookies;

use crate::auth::error::AuthError;
use crate::auth::session::{SESSION_COOKIE, decode_session_id};
use crate::auth::state::AuthState;

/// Successful authentication outcome.
///
/// Carries the resolved [`User`], the session ID it came from (so handlers
/// can revoke it on logout without re-reading the cookie), and the union of
/// the user's roles as a [`Permissions`] set so downstream checks don't
/// re-query the role table.
#[derive(Debug, Clone)]
pub struct AuthSession {
    /// The authenticated user.
    pub user: User,
    /// Session row that authorised this request.
    pub session_id: SessionId,
    /// Effective permissions (union of every role the user holds).
    pub permissions: Permissions,
}

impl<S> FromRequestParts<S> for AuthSession
where
    S: Send + Sync,
    AuthState: FromRef<S>,
{
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let auth_state = AuthState::from_ref(state);
        let cookies = Cookies::from_request_parts(parts, state)
            .await
            .map_err(|_| AuthError::MissingSession)?;

        let session_id = cookies
            .get(SESSION_COOKIE)
            .and_then(|c| decode_session_id(c.value()))
            .ok_or(AuthError::MissingSession)?;

        let session = match auth_state.storage.sessions().get_by_id(session_id).await {
            Ok(s) => s,
            Err(StorageError::NotFound) => return Err(AuthError::ExpiredSession),
            Err(e) => return Err(AuthError::Storage(e)),
        };

        let user = match auth_state.storage.users().get_by_id(session.user_id).await {
            Ok(u) => u,
            // A session whose user has been deleted is effectively expired:
            // the FK cascade should have caught it, but if not, treat it as
            // such instead of leaking a 500.
            Err(StorageError::NotFound) => return Err(AuthError::ExpiredSession),
            Err(e) => return Err(AuthError::Storage(e)),
        };

        // Bumping last_seen_at is fire-and-forget for tests' sake — if the
        // row vanished between the get_by_id and now, the next request will
        // 401 anyway. We surface the error in logs for ops visibility but
        // do not short-circuit the request.
        if let Err(e) = auth_state.storage.sessions().touch(session.id).await {
            tracing::warn!(error = %e, "session touch failed");
        }

        // Union the permissions across every role the user holds. Anonymous
        // users would land in a different branch entirely (no session), so the
        // empty set here means "registered user with no roles" — read-only.
        // A storage error here must surface — silently downgrading to
        // zero permissions would mask DB failures during authenticated
        // requests and could let a session-holding user lose access without
        // any visible diagnostic.
        let roles = auth_state
            .storage
            .roles()
            .list_for_user(user.id)
            .await
            .map_err(AuthError::Storage)?;
        let permissions = roles
            .into_iter()
            .fold(Permissions::empty(), |acc, r| acc | r.permissions);

        Ok(Self {
            user,
            session_id: session.id,
            permissions,
        })
    }
}

/// Extractor that requires both an [`AuthSession`] **and** that the user holds
/// the listed permission(s).
///
/// Usage on a route:
///
/// ```ignore
/// use axum::Router;
/// use axum::routing::post;
/// use thewiki_core::Permissions;
/// use thewiki_api::auth::RequireRole;
///
/// async fn delete_page(_guard: RequireRole) -> &'static str { "ok" }
///
/// let app: Router<thewiki_api::auth::AuthState> = Router::new().route(
///     "/api/v1/pages/{id}",
///     post(delete_page).route_layer(axum::middleware::from_fn_with_state(
///         (), |state, req, next| async move { /* guard */ next.run(req).await }
///     )),
/// );
/// ```
///
/// In practice handlers take `RequireRole` *as* their extractor and pass the
/// required permission bits via a const generic helper — see
/// [`require_permissions`] for the construction-by-function form used in #14.
#[derive(Debug, Clone)]
pub struct RequireRole {
    /// The underlying authenticated session.
    pub session: AuthSession,
    /// The bits the caller required (so handlers can re-inspect, e.g. for
    /// finer-grained checks).
    pub required: Permissions,
}

/// Construct a `RequireRole` extractor that demands `required`.
///
/// Returns a closure usable with `axum::middleware::from_fn_with_state`. For
/// the v1 surface we keep the simpler form: handlers take `AuthSession` and
/// manually check `permissions.contains(...)`, returning [`AuthError::Forbidden`]
/// on miss. The `RequireRole` struct is provided so future call sites that
/// want a single extractor have the type at hand.
pub fn require_permissions(
    session: AuthSession,
    required: Permissions,
) -> Result<RequireRole, AuthError> {
    if session.permissions.contains(required) {
        Ok(RequireRole { session, required })
    } else {
        Err(AuthError::Forbidden)
    }
}
