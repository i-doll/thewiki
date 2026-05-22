//! Authentication helpers for GraphQL resolvers.
//!
//! The HTTP-layer auth machinery (`crate::auth::AuthSession`,
//! `crate::extractors::EditorExtractor`) lives behind Axum's `FromRequestParts`,
//! which a GraphQL resolver cannot reach into directly. Instead the GraphQL
//! handler resolves the session cookie *before* invoking the schema and
//! stores the resulting [`SessionContext`] in `async_graphql::Context::data`.
//! Resolvers pull it back out via [`current_session`] / [`require_session`].
//!
//! This split keeps:
//!
//! - The schema testable with arbitrary callers — set or omit `SessionContext`
//!   to simulate anonymous / authenticated requests.
//! - The cookie machinery in one place (the request handler) rather than in
//!   every resolver.

use async_graphql::{Context, Error, ErrorExtensions};
use thewiki_core::{Permissions, User, UserId};

/// Snapshot of the caller's session, injected into the resolver context.
///
/// Cheap to clone — it carries owned strings (not references into the
/// request body) so it survives `'static` futures.
#[derive(Debug, Clone)]
pub struct SessionContext {
    /// The authenticated user, or `None` when the request was anonymous.
    pub user: Option<User>,
    /// Effective permission set (union of role permissions). `Permissions::empty()`
    /// for anonymous callers.
    pub permissions: Permissions,
}

impl SessionContext {
    /// Build the anonymous (no session) context.
    #[must_use]
    pub fn anonymous() -> Self {
        Self {
            user: None,
            permissions: Permissions::empty(),
        }
    }

    /// Build an authenticated context from the resolved AuthSession parts.
    #[must_use]
    pub fn authenticated(user: User, permissions: Permissions) -> Self {
        Self {
            user: Some(user),
            permissions,
        }
    }
}

/// Borrow the current request's session context, falling back to the
/// "anonymous" sentinel if the GraphQL handler didn't inject one (e.g. unit
/// tests that build the schema directly).
#[must_use]
pub fn current_session<'a>(ctx: &'a Context<'_>) -> &'a SessionContext {
    ctx.data_opt::<SessionContext>()
        .unwrap_or(&ANONYMOUS_SESSION)
}

static ANONYMOUS_SESSION: SessionContext = SessionContext {
    user: None,
    permissions: Permissions::empty(),
};

/// Resolver guard: return the current user, or surface an `UNAUTHENTICATED`
/// GraphQL error. Mutations that mutate page state go through this.
///
/// # Errors
///
/// Returns an `async_graphql::Error` with extension `code = "UNAUTHENTICATED"`
/// when the caller has no resolved session.
pub fn require_session<'a>(ctx: &'a Context<'_>) -> Result<&'a User, Error> {
    let s = current_session(ctx);
    s.user.as_ref().ok_or_else(unauthenticated_error)
}

/// Resolver guard: return the current user id, plus a flag indicating
/// whether the caller is anonymous (per the `AuthConfig::anonymous_edits`
/// policy). Mutations that respect the anonymous-edits flag use this.
///
/// # Errors
///
/// Returns an `UNAUTHENTICATED` GraphQL error when the request was anonymous
/// and `allow_anonymous` is `false`.
pub fn require_user_or_anonymous(
    ctx: &Context<'_>,
    allow_anonymous: bool,
) -> Result<(UserId, String, bool), Error> {
    let s = current_session(ctx);
    if let Some(user) = &s.user {
        return Ok((user.id, user.username.as_str().to_owned(), false));
    }
    if allow_anonymous {
        let anon = crate::extractors::anonymous_user_id();
        Ok((anon, crate::extractors::ANONYMOUS_USERNAME.to_owned(), true))
    } else {
        Err(unauthenticated_error())
    }
}

/// Build the standardised UNAUTHENTICATED error returned by every resolver
/// that requires a session.
pub fn unauthenticated_error() -> Error {
    Error::new("authentication required").extend_with(|_, e| e.set("code", "UNAUTHENTICATED"))
}

/// Build the standardised FORBIDDEN error returned by resolvers that need
/// a specific permission.
pub fn forbidden_error() -> Error {
    Error::new("insufficient permissions").extend_with(|_, e| e.set("code", "FORBIDDEN"))
}

/// Guard: require the caller to hold every bit in `required`.
///
/// # Errors
///
/// - `UNAUTHENTICATED` when the caller is anonymous.
/// - `FORBIDDEN` when the caller is authenticated but missing the bits.
pub fn require_permissions<'a>(
    ctx: &'a Context<'_>,
    required: Permissions,
) -> Result<&'a User, Error> {
    let s = current_session(ctx);
    match &s.user {
        None => Err(unauthenticated_error()),
        Some(user) => {
            if s.permissions.contains(required) {
                Ok(user)
            } else {
                Err(forbidden_error())
            }
        }
    }
}
