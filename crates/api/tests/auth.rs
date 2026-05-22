//! Integration tests for the auth scaffold (#13).
//!
//! Spins up the full router via [`thewiki_api::app::build_auth_app`] against
//! an in-memory SQLite. The session token issued by `login` is fed back into
//! subsequent requests as a `Cookie:` header so the flow mirrors what a
//! browser would do.
//!
//! Covered cases:
//!
//! - Login with correct credentials yields 200 + both cookies.
//! - Login with wrong password / unknown username yields 401 with identical
//!   body shape (no username enumeration).
//! - `GET /me` with a valid cookie returns the user; without one returns 401.
//! - Logout clears the cookies, returns 204, and the session no longer
//!   resolves.
//! - An artificially-expired session row resolves to 401 on `GET /me`.
//! - CSRF: a POST without `X-CSRF-Token` returns 403 even with a valid
//!   session cookie; with a matching header it returns 204.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::Value;
use thewiki_api::app;
use thewiki_api::auth::password::{Argon2Hasher, PasswordHasher};
use thewiki_api::auth::state::AuthState;
use thewiki_api::config::Argon2Config;
use thewiki_core::{EmailAddress, User, UserId, Username};
use thewiki_storage::repo::UserRepository;
use thewiki_storage::sqlite::{SqliteOptions, SqliteStorage};
use time::OffsetDateTime;
use tower::ServiceExt;

// ─── Test fixture helpers ─────────────────────────────────────────────────

/// OWASP-floor Argon2 parameters so tests stay fast (~100 ms per hash).
/// Production uses the higher defaults from `Config::defaults`, but the floor
/// is what `Config::validate` accepts and is sufficient to exercise the
/// crypto path.
fn test_argon2() -> Argon2Config {
    Argon2Config {
        memory_kib: 19_456,
        iterations: 2,
        parallelism: 1,
    }
}

/// Spin up fresh storage + a hasher and seed `alice` with `password123`.
async fn setup() -> (AuthState, UserId) {
    let storage = SqliteStorage::new(
        "sqlite::memory:",
        SqliteOptions {
            max_connections: 1,
            acquire_timeout: Duration::from_secs(5),
            foreign_keys: true,
        },
    )
    .await
    .expect("storage");

    let hasher = Arc::new(Argon2Hasher::new(test_argon2()).expect("hasher"));
    let phc = hasher.hash("password123").expect("hash");

    let user = User {
        id: UserId::new(),
        username: Username::new("alice").expect("uname"),
        email: Some(EmailAddress::new("alice@example.com").expect("email")),
        display_name: Some("Alice".into()),
        created_at: OffsetDateTime::now_utc(),
        last_login_at: None,
    };
    storage
        .users()
        .create(&user, Some(&phc))
        .await
        .expect("seed user");

    let state = AuthState::new(
        storage,
        hasher,
        Duration::from_secs(60 * 60), // 1 hour
        false,                        // `Secure` off for tests over plain HTTP
        thewiki_api::config::Config::defaults().auth,
    );
    (state, user.id)
}

/// Pull the `Set-Cookie` headers out of a response in the order the server
/// emitted them.
fn set_cookie_headers(response: &axum::http::Response<Body>) -> Vec<String> {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().expect("ascii cookie").to_owned())
        .collect()
}

/// Extract the `name=value` segment from the first matching `Set-Cookie`
/// header.
fn cookie_value(cookies: &[String], name: &str) -> Option<String> {
    for c in cookies {
        if let Some(eq) = c.find('=')
            && &c[..eq] == name
        {
            let rest = &c[eq + 1..];
            let end = rest.find(';').unwrap_or(rest.len());
            return Some(rest[..end].to_owned());
        }
    }
    None
}

/// Collect a body into a JSON value.
async fn body_json(response: axum::http::Response<Body>) -> Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("body is json")
}

/// Hit the login endpoint and return the raw response.
async fn login(state: AuthState, username: &str, password: &str) -> axum::http::Response<Body> {
    let body = serde_json::json!({ "username": username, "password": password });
    let body_bytes = serde_json::to_vec(&body).expect("encode");
    app::build_auth_app(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(body_bytes))
                .expect("build req"),
        )
        .await
        .expect("router")
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn login_success_returns_user_and_cookies() {
    let (state, _uid) = setup().await;

    let response = login(state.clone(), "alice", "password123").await;
    assert_eq!(response.status(), StatusCode::OK);

    let cookies = set_cookie_headers(&response);
    assert!(
        cookies.iter().any(|c| c.starts_with("thewiki_session=")),
        "session cookie should be set: {cookies:?}",
    );
    assert!(
        cookies.iter().any(|c| c.starts_with("thewiki_csrf=")),
        "csrf cookie should be set: {cookies:?}",
    );

    let json = body_json(response).await;
    assert_eq!(json["username"], "alice");
    assert_eq!(json["display_name"], "Alice");
    assert_eq!(json["email"], "alice@example.com");
    assert!(json["roles"].is_array());
}

#[tokio::test]
async fn login_wrong_password_returns_401_with_generic_body() {
    let (state, _uid) = setup().await;
    let response = login(state, "alice", "wrong").await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let json = body_json(response).await;
    assert_eq!(json["error"], "invalid_credentials");
}

#[tokio::test]
async fn login_unknown_user_returns_401_same_shape() {
    let (state, _uid) = setup().await;
    let response = login(state, "ghost", "whatever").await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let json = body_json(response).await;
    // Identical shape to the wrong-password case: a probe can't tell which arm fired.
    assert_eq!(json["error"], "invalid_credentials");
}

#[tokio::test]
async fn login_invalid_username_format_also_401() {
    // An invalid username (e.g. one containing '@') should not 400 — that
    // would distinguish "real user exists" from "this string can't be a user".
    let (state, _uid) = setup().await;
    let response = login(state, "alice@example.com", "password123").await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn me_with_valid_cookie_returns_user() {
    let (state, _uid) = setup().await;
    let login_resp = login(state.clone(), "alice", "password123").await;
    let cookies = set_cookie_headers(&login_resp);
    let session = cookie_value(&cookies, "thewiki_session").expect("session cookie");

    let response = app::build_auth_app(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/auth/me")
                .header("cookie", format!("thewiki_session={session}"))
                .body(Body::empty())
                .expect("build req"),
        )
        .await
        .expect("router");

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["username"], "alice");
}

#[tokio::test]
async fn me_without_cookie_returns_401() {
    let (state, _uid) = setup().await;
    let response = app::build_auth_app(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/auth/me")
                .body(Body::empty())
                .expect("build req"),
        )
        .await
        .expect("router");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn me_with_bogus_cookie_returns_401() {
    let (state, _uid) = setup().await;
    let response = app::build_auth_app(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/auth/me")
                .header("cookie", "thewiki_session=not-a-uuid")
                .body(Body::empty())
                .expect("build req"),
        )
        .await
        .expect("router");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn logout_with_csrf_returns_204_and_clears_cookies() {
    let (state, _uid) = setup().await;
    let login_resp = login(state.clone(), "alice", "password123").await;
    let cookies = set_cookie_headers(&login_resp);
    let session = cookie_value(&cookies, "thewiki_session").expect("session cookie");
    let csrf = cookie_value(&cookies, "thewiki_csrf").expect("csrf cookie");

    let response = app::build_auth_app(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/logout")
                .header(
                    "cookie",
                    format!("thewiki_session={session}; thewiki_csrf={csrf}"),
                )
                .header("x-csrf-token", csrf.clone())
                .body(Body::empty())
                .expect("build req"),
        )
        .await
        .expect("router");

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let cookies = set_cookie_headers(&response);
    // Both clearing cookies should be present with Max-Age=0.
    assert!(
        cookies
            .iter()
            .any(|c| c.starts_with("thewiki_session=") && c.contains("Max-Age=0")),
        "session clearing cookie missing: {cookies:?}",
    );
    assert!(
        cookies
            .iter()
            .any(|c| c.starts_with("thewiki_csrf=") && c.contains("Max-Age=0")),
        "csrf clearing cookie missing: {cookies:?}",
    );

    // The next /me with the (now-revoked) session cookie should 401.
    let response = app::build_auth_app(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/auth/me")
                .header("cookie", format!("thewiki_session={session}"))
                .body(Body::empty())
                .expect("build req"),
        )
        .await
        .expect("router");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn logout_without_csrf_is_rejected_403() {
    let (state, _uid) = setup().await;
    let login_resp = login(state.clone(), "alice", "password123").await;
    let cookies = set_cookie_headers(&login_resp);
    let session = cookie_value(&cookies, "thewiki_session").expect("session cookie");
    let csrf = cookie_value(&cookies, "thewiki_csrf").expect("csrf cookie");

    // POST without the X-CSRF-Token header — should 403 *before* the
    // logout handler runs.
    let response = app::build_auth_app(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/logout")
                .header(
                    "cookie",
                    format!("thewiki_session={session}; thewiki_csrf={csrf}"),
                )
                .body(Body::empty())
                .expect("build req"),
        )
        .await
        .expect("router");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // And with a wrong header value.
    let response = app::build_auth_app(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/logout")
                .header(
                    "cookie",
                    format!("thewiki_session={session}; thewiki_csrf={csrf}"),
                )
                .header("x-csrf-token", "not-the-token")
                .body(Body::empty())
                .expect("build req"),
        )
        .await
        .expect("router");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn expired_session_returns_401() {
    let (state, _uid) = setup().await;
    let login_resp = login(state.clone(), "alice", "password123").await;
    let cookies = set_cookie_headers(&login_resp);
    let session = cookie_value(&cookies, "thewiki_session").expect("session cookie");

    // Backdate the session in storage so the next lookup sees it as expired.
    // We rewrite `expires_at` to 1970-01-01 directly via the pool.
    sqlx::query("UPDATE sessions SET expires_at = ?1")
        .bind("1970-01-01T00:00:00Z")
        .execute(state.storage.pool())
        .await
        .expect("backdate session");

    let response = app::build_auth_app(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/auth/me")
                .header("cookie", format!("thewiki_session={session}"))
                .body(Body::empty())
                .expect("build req"),
        )
        .await
        .expect("router");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
