//! Request extractors used by the route handlers.
//!
//! Two extractors live here:
//!
//! - [`EditorExtractor`] — the configurable-auth gate for mutating page
//!   endpoints (#14). Resolves a session cookie via [`AuthSession`] when one
//!   is present; otherwise consults [`AuthConfig::anonymous_edits`] to decide
//!   whether to fall back to the anonymous editor identity (200 OK) or short-
//!   circuit with 401.
//! - [`RequireAuth`] — strict "authenticated only" extractor for routes that
//!   must never accept anonymous callers regardless of configuration (e.g.
//!   the future admin endpoints). Today this delegates to [`AuthSession`] and
//!   maps the rejection to [`ApiError::Unauthenticated`] so handler signatures
//!   stay simple.
//!
//! Anonymous edits are charged to a singleton "Anonymous" user row that the
//! [`EditorExtractor`] lazily provisions in the `users` table the first time
//! it's needed. The row's id is a stable namespace UUID (see
//! [`ANONYMOUS_USER_UUID`]) so the same identity persists across restarts —
//! page history shows "Anonymous" as the author rather than a churn of
//! per-request placeholder rows. The lazy `INSERT … ON CONFLICT DO NOTHING`
//! is cheap (single SQL roundtrip) and inherently idempotent across racing
//! requests, so we don't bother caching the lookup result in-process.

use axum::extract::{FromRef, FromRequestParts};
use axum::http::request::Parts;
use thewiki_core::{EmailAddress, User, UserId, Username};
use thewiki_storage::StorageError;
use thewiki_storage::repo::UserRepository;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::AuthSession;
use crate::auth::error::AuthError;
use crate::config::AuthConfig;
use crate::error::ApiError;
use crate::state::{AppState, AppStorage};

/// Stable identifier for the singleton anonymous editor row.
///
/// A version-4-like UUID with all-zero fields except a deliberate non-zero
/// nibble so it parses cleanly and round-trips through every storage backend.
/// We intentionally do *not* use the nil UUID (`00000000-…`) so an accidental
/// "default UserId" elsewhere in the codebase can't collide with this identity.
pub const ANONYMOUS_USER_UUID: Uuid = Uuid::from_bytes([
    0xA0, 0x07, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
]);

/// Username used for the singleton anonymous editor.
///
/// Matches [`Username`]'s allowed character set (ASCII alphanumerics, `_`,
/// `-`). Capitalised so it stands out in the recent-changes feed.
pub const ANONYMOUS_USERNAME: &str = "Anonymous";

/// `UserId` of the singleton anonymous editor.
#[must_use]
pub fn anonymous_user_id() -> UserId {
    UserId::from_uuid(ANONYMOUS_USER_UUID)
}

/// Ensure the anonymous editor row exists in `users`, creating it idempotently
/// on first use. The row is keyed by [`ANONYMOUS_USER_UUID`] so we never end
/// up with multiple "Anonymous" identities even across racing requests.
///
/// Why not a migration? A migration would impose the row on every deploy
/// regardless of whether anonymous edits are enabled, and the row would
/// always be present even after the operator flips `anonymous_edits = false`.
/// Lazy provisioning keeps the schema unconditional and lets the auth
/// config own the lifecycle.
///
/// # Errors
///
/// Propagates the storage layer's [`ApiError`] variants — typically only
/// reachable if the database is unavailable.
pub async fn ensure_anonymous_user<S: AppStorage>(storage: &S) -> Result<UserId, ApiError> {
    let uid = anonymous_user_id();
    let users = storage.users();
    match users.get_by_id(uid).await {
        Ok(_) => Ok(uid),
        Err(StorageError::NotFound) => {
            let user = User {
                id: uid,
                #[allow(
                    clippy::expect_used,
                    reason = "ANONYMOUS_USERNAME is a compile-time constant that passes \
                              Username validation; failure here is a programmer error"
                )]
                username: Username::new(ANONYMOUS_USERNAME)
                    .expect("ANONYMOUS_USERNAME satisfies Username validation"),
                email: None::<EmailAddress>,
                display_name: Some("Anonymous".into()),
                created_at: OffsetDateTime::now_utc(),
                last_login_at: None,
            };
            match users.create(&user, None).await {
                Ok(()) => Ok(uid),
                // A racing request beat us to the insert — the row is there,
                // job done.
                Err(StorageError::Conflict(_)) => Ok(uid),
                Err(e) => Err(ApiError::from(e)),
            }
        }
        Err(e) => Err(ApiError::from(e)),
    }
}

/// Outcome of [`EditorExtractor::from_request_parts`].
///
/// `is_anonymous` is preserved alongside the resolved `UserId` so downstream
/// approval-queue gating can match on the original caller class without
/// re-reading the cookie or the auth config.
#[derive(Debug, Clone, Copy)]
pub struct EditorExtractor {
    /// The user id to credit the edit to. Either the authenticated user or
    /// the lazily-provisioned anonymous user (see [`anonymous_user_id`]).
    pub user_id: UserId,
    /// `true` when the caller had no valid session cookie and the edit is
    /// being credited to the anonymous editor under
    /// [`AuthConfig::anonymous_edits`] = true.
    pub is_anonymous: bool,
    /// `Some(created_at)` when the caller is authenticated; `None` for the
    /// anonymous path. Used by the approval-queue wiring to decide whether
    /// the user counts as "new".
    pub user_created_at: Option<OffsetDateTime>,
}

impl<S: AppStorage> FromRequestParts<AppState<S>> for EditorExtractor {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState<S>,
    ) -> Result<Self, Self::Rejection> {
        // Try to resolve a session cookie. The `AuthSession` extractor returns
        // a typed `AuthError` for each "this isn't authenticated" arm; we only
        // care about the success/missing distinction here. Anything else (a
        // storage failure mid-lookup) must surface as 500.
        let auth_outcome = if state.auth_state.is_some() {
            AuthSession::from_request_parts(parts, state).await
        } else {
            // No auth stack wired — every request is treated as anonymous
            // (the only path test fixtures take when they don't seed the
            // auth router). The configurable-auth logic below then decides
            // whether that's a 401 or an anonymous edit.
            Err(AuthError::MissingSession)
        };

        match auth_outcome {
            Ok(session) => Ok(Self {
                user_id: session.user.id,
                is_anonymous: false,
                user_created_at: Some(session.user.created_at),
            }),
            Err(AuthError::MissingSession | AuthError::ExpiredSession) => {
                if state.auth_config.anonymous_edits {
                    let uid = ensure_anonymous_user(state.storage.as_ref()).await?;
                    Ok(Self {
                        user_id: uid,
                        is_anonymous: true,
                        user_created_at: None,
                    })
                } else {
                    Err(ApiError::Unauthenticated)
                }
            }
            // Storage / hash failures inside the auth extractor are real
            // 500s; propagate them as internal errors so logs see the chain.
            Err(other) => {
                tracing::error!(error = %other, "auth resolution failed in EditorExtractor");
                Err(ApiError::Internal(format!("auth: {other}")))
            }
        }
    }
}

/// Strict authenticated-only extractor.
///
/// Always 401 on a missing/invalid session, independent of
/// [`AuthConfig::anonymous_edits`]. Use for endpoints that must never accept
/// anonymous callers (admin tools, account management). Page CRUD goes
/// through [`EditorExtractor`] instead.
#[derive(Debug, Clone, Copy)]
pub struct RequireAuth(pub UserId);

impl<S: AppStorage> FromRequestParts<AppState<S>> for RequireAuth {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState<S>,
    ) -> Result<Self, Self::Rejection> {
        if state.auth_state.is_none() {
            return Err(ApiError::Unauthenticated);
        }
        match AuthSession::from_request_parts(parts, state).await {
            Ok(session) => Ok(Self(session.user.id)),
            Err(AuthError::MissingSession | AuthError::ExpiredSession) => {
                Err(ApiError::Unauthenticated)
            }
            Err(other) => {
                tracing::error!(error = %other, "auth resolution failed in RequireAuth");
                Err(ApiError::Internal(format!("auth: {other}")))
            }
        }
    }
}

/// Expose the AppState's [`AuthConfig`] snapshot for axum's `State<AuthConfig>`.
///
/// Carved out as a `FromRef` impl so handlers can take
/// `State<AuthConfig>` directly without dragging the rest of the AppState
/// shape into their signature.
impl<S: AppStorage> FromRef<AppState<S>> for AuthConfig {
    fn from_ref(input: &AppState<S>) -> Self {
        input.auth_config.clone()
    }
}
