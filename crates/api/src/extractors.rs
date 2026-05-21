//! Request extractors used by the route handlers.
//!
//! Today this module only carries the [`RequireAuth`] placeholder — see the
//! `TODO(#13)` below. The proper session-auth scaffold lands with issue #13;
//! once that's in, this extractor swaps over to looking up a real session
//! from a cookie or bearer token instead of trusting an `X-User-Id` header.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use thewiki_core::UserId;
use uuid::Uuid;

use crate::error::ApiError;

/// HTTP header carrying the placeholder authenticated user id.
///
/// TODO(#13): drop this once real session auth lands; the extractor will then
/// resolve the session from a cookie / bearer token.
pub const USER_ID_HEADER: &str = "x-user-id";

/// Extractor that requires an authenticated caller.
///
/// Today this is a **placeholder**: it reads the [`USER_ID_HEADER`] and parses
/// it as a UUID. A missing or malformed header returns
/// [`ApiError::Unauthenticated`] (401). Real session-auth scaffolding is
/// tracked by [#13]. The shape of this extractor will not change when #13
/// lands — handlers can already write `RequireAuth(uid): RequireAuth`.
///
/// [#13]: https://github.com/i-doll/thewiki/issues/13
#[derive(Debug, Clone, Copy)]
pub struct RequireAuth(pub UserId);

impl<S> FromRequestParts<S> for RequireAuth
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        // TODO(#13): replace with real session lookup. For now the header is
        // the entire trust model; that's fine for development and the
        // integration tests, and it must never ship to a production deploy
        // without #13 being merged first.
        let raw = parts
            .headers
            .get(USER_ID_HEADER)
            .ok_or(ApiError::Unauthenticated)?
            .to_str()
            .map_err(|_| ApiError::Unauthenticated)?;
        let uuid = Uuid::parse_str(raw).map_err(|_| ApiError::Unauthenticated)?;
        Ok(Self(UserId::from_uuid(uuid)))
    }
}
