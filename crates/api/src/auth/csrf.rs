//! CSRF protection (double-submit cookie pattern).
//!
//! `SameSite=Strict` on `thewiki_session` already prevents cross-origin
//! requests from carrying the session cookie, so the only realistic CSRF
//! vector is same-origin scripts (XSS). This middleware adds defence in
//! depth:
//!
//! - On login we issue a `thewiki_csrf` cookie that is **not** `HttpOnly` so
//!   the SPA can read it from JS.
//! - On every mutating request (POST/PUT/PATCH/DELETE) we compare the cookie
//!   value against the [`CSRF_HEADER`] header the SPA echoes back. A miss is
//!   a 403.
//!
//! Comparison is constant-time via [`subtle::ConstantTimeEq`] so an attacker
//! can't time their way to the right token byte-by-byte.

use axum::extract::Request;
use axum::http::Method;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use subtle::ConstantTimeEq;
use tower_cookies::Cookies;

#[cfg(test)]
use axum::http::StatusCode;

use crate::auth::error::AuthError;
use crate::auth::session::{CSRF_COOKIE, CSRF_HEADER, SESSION_COOKIE};

/// Middleware: enforce the double-submit cookie on mutating methods.
///
/// Safe methods (GET, HEAD, OPTIONS) pass through unchanged.
///
/// Requests **without** a `thewiki_session` cookie also pass through — they
/// can't be acting on a logged-in user's behalf, so CSRF would only block
/// `POST /api/v1/auth/login` (which has no session yet to protect). Once a
/// session cookie is present, every mutation requires both:
///
/// 1. A `thewiki_csrf` cookie on the request.
/// 2. An `X-CSRF-Token` header whose value equals (1) in constant time.
///
/// A missing cookie *or* a missing/mismatched header both surface as
/// [`AuthError::CsrfFailed`] (HTTP 403).
pub async fn csrf_layer(cookies: Cookies, req: Request, next: Next) -> Response {
    let is_safe = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if is_safe {
        return next.run(req).await;
    }

    // No session cookie → request isn't authenticated. Let it through; the
    // downstream handler will reject it as a 401 (and login is allowed
    // through unconditionally since the session is what it's trying to mint).
    if cookies.get(SESSION_COOKIE).is_none() {
        return next.run(req).await;
    }

    let cookie_value = cookies.get(CSRF_COOKIE).map(|c| c.value().to_owned());
    let header_value = req
        .headers()
        .get(CSRF_HEADER)
        .and_then(|h| h.to_str().ok())
        .map(str::to_owned);

    let Some(cookie_token) = cookie_value else {
        return AuthError::CsrfFailed.into_response();
    };
    let Some(header_token) = header_value else {
        return AuthError::CsrfFailed.into_response();
    };

    // Constant-time comparison. `ct_eq` requires equal-length inputs to be
    // meaningful; for unequal lengths we short-circuit fail before the
    // comparison so we don't leak the length difference. (Lengths leak
    // anyway via the Content-Length of the response, but defence-in-depth.)
    if cookie_token.len() != header_token.len() {
        return AuthError::CsrfFailed.into_response();
    }
    if cookie_token
        .as_bytes()
        .ct_eq(header_token.as_bytes())
        .into()
    {
        next.run(req).await
    } else {
        AuthError::CsrfFailed.into_response()
    }
}

/// Predicate: does this status code mean "we rejected before invoking the
/// handler"?
///
/// Exposed only for the test suite, which needs to assert that a CSRF
/// rejection happens *before* any handler side-effect.
#[must_use]
#[cfg(test)]
pub fn is_csrf_rejection(status: StatusCode) -> bool {
    status == StatusCode::FORBIDDEN
}
