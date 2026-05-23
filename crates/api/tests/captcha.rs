//! Integration tests for the CAPTCHA wiring (#41).
//!
//! Covers the wire surface end-to-end:
//!
//! 1. `GET /api/v1/captcha/config` returns `null` when the noop provider is
//!    wired and a populated object when an hCaptcha provider is.
//! 2. `POST /api/v1/auth/register` requires a `captcha_response` when the
//!    operator opted into `apply_to_registration` — missing returns 400 with
//!    a stable `captcha_failed` machine code.
//! 3. With `RegistrationPolicy::Closed` (the default) the register endpoint
//!    surfaces `registration_closed` (403) regardless of CAPTCHA state.
//!
//! The HCaptcha-flavoured config test instantiates the provider against a
//! `wiremock` server so we never touch the real upstream.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use thewiki_api::app;
use thewiki_api::auth::password::Argon2Hasher;
use thewiki_api::auth::state::AuthState;
use thewiki_api::captcha::hcaptcha::HCaptcha;
use thewiki_api::config::{Argon2Config, CaptchaConfig, CaptchaProviderKind, RegistrationPolicy};
use thewiki_core::NoopCaptcha;
use thewiki_storage::sqlite::{SqliteOptions, SqliteStorage};
use tower::ServiceExt;

fn test_argon2() -> Argon2Config {
    Argon2Config {
        memory_kib: 19_456,
        iterations: 2,
        parallelism: 1,
    }
}

async fn storage() -> SqliteStorage {
    SqliteStorage::new(
        "sqlite::memory:",
        SqliteOptions {
            max_connections: 1,
            acquire_timeout: Duration::from_secs(5),
            foreign_keys: true,
        },
    )
    .await
    .expect("storage")
}

/// Build an `AuthState` parameterised by the registration policy and
/// captcha config the test wants to exercise.
async fn auth_state(
    registration: RegistrationPolicy,
    captcha_config: CaptchaConfig,
    provider: Arc<dyn thewiki_core::CaptchaProvider>,
) -> AuthState {
    let storage = storage().await;
    let hasher = Arc::new(Argon2Hasher::new(test_argon2()).expect("hasher"));
    let mut auth_cfg = thewiki_api::config::Config::defaults().auth;
    auth_cfg.registration = registration;

    AuthState::new(
        storage,
        hasher,
        Duration::from_secs(60 * 60),
        false,
        auth_cfg,
    )
    .with_captcha(captcha_config, provider)
}

async fn body_json(response: axum::http::Response<Body>) -> Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("body is json")
}

#[tokio::test]
async fn captcha_config_returns_null_for_noop_provider() {
    // The captcha config endpoint lives on `AppState`, but the auth router
    // doesn't mount it. We exercise the handler directly via the auth
    // build_full path is heavy; instead spin up the captcha route on its
    // own `AppState` for the test.
    let storage = storage().await;
    let app_state = thewiki_api::state::AppState::new(
        storage,
        thewiki_api::config::Config::defaults().auth,
    );
    // Default state ships with a NoopCaptcha — no `with_captcha` needed.

    let router = app::build_with_state(app_state);
    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/v1/captcha/config")
                .body(Body::empty())
                .expect("build req"),
        )
        .await
        .expect("router");
    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response).await;
    assert!(
        body.is_null(),
        "noop captcha should publish null config; got {body}",
    );
}

#[tokio::test]
async fn captcha_config_returns_provider_for_hcaptcha() {
    let storage = storage().await;
    // Build a real HCaptcha; the endpoint override doesn't matter because
    // this test never calls `verify`.
    let provider = Arc::new(
        HCaptcha::new("test-site-key".to_string(), "test-secret".to_string())
            .expect("provider builds"),
    ) as Arc<dyn thewiki_core::CaptchaProvider>;
    let captcha_cfg = CaptchaConfig {
        provider: CaptchaProviderKind::Hcaptcha,
        site_key: "test-site-key".to_string(),
        secret_key: "test-secret".to_string(),
        apply_to_registration: true,
        apply_to_anonymous_edits: false,
    };
    let app_state = thewiki_api::state::AppState::new(
        storage,
        thewiki_api::config::Config::defaults().auth,
    )
    .with_captcha(captcha_cfg, provider);

    let router = app::build_with_state(app_state);
    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/v1/captcha/config")
                .body(Body::empty())
                .expect("build req"),
        )
        .await
        .expect("router");
    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response).await;
    assert_eq!(body["provider"], "hcaptcha");
    assert_eq!(body["site_key"], "test-site-key");
}

#[tokio::test]
async fn register_missing_captcha_token_returns_400_when_required() {
    // Operator wired the noop provider but flipped `apply_to_registration`.
    // The handler still requires a non-empty token (the gate is independent
    // of provider choice) — when it's missing we expect 400/captcha_failed
    // *before* the noop is consulted.
    let captcha_cfg = CaptchaConfig {
        provider: CaptchaProviderKind::Noop,
        site_key: String::new(),
        secret_key: String::new(),
        apply_to_registration: true,
        apply_to_anonymous_edits: false,
    };
    let state = auth_state(RegistrationPolicy::Open, captcha_cfg, Arc::new(NoopCaptcha)).await;

    let body = serde_json::json!({
        "username": "alice",
        "password": "password123",
        // `captcha_response` deliberately omitted.
    });
    let body_bytes = serde_json::to_vec(&body).expect("encode");
    let response = app::build_auth_app(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/register")
                .header("content-type", "application/json")
                .body(Body::from(body_bytes))
                .expect("build req"),
        )
        .await
        .expect("router");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(body["error"], "captcha_failed");
}

#[tokio::test]
async fn register_succeeds_when_captcha_disabled_and_open() {
    // No apply_to_registration: tokens are not consulted. Open
    // registration: the handler proceeds to create the user.
    let captcha_cfg = CaptchaConfig {
        provider: CaptchaProviderKind::Noop,
        site_key: String::new(),
        secret_key: String::new(),
        apply_to_registration: false,
        apply_to_anonymous_edits: false,
    };
    let state = auth_state(RegistrationPolicy::Open, captcha_cfg, Arc::new(NoopCaptcha)).await;

    let body = serde_json::json!({
        "username": "alice",
        "password": "password123",
    });
    let body_bytes = serde_json::to_vec(&body).expect("encode");
    let response = app::build_auth_app(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/register")
                .header("content-type", "application/json")
                .body(Body::from(body_bytes))
                .expect("build req"),
        )
        .await
        .expect("router");
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = body_json(response).await;
    assert_eq!(body["username"], "alice");
}

#[tokio::test]
async fn register_closed_returns_403_even_with_captcha_token() {
    // Registration is closed: the policy gate fires before the captcha
    // gate, returning `registration_closed` regardless of token presence.
    let captcha_cfg = CaptchaConfig {
        provider: CaptchaProviderKind::Noop,
        site_key: String::new(),
        secret_key: String::new(),
        apply_to_registration: true,
        apply_to_anonymous_edits: false,
    };
    let state = auth_state(
        RegistrationPolicy::Closed,
        captcha_cfg,
        Arc::new(NoopCaptcha),
    )
    .await;

    let body = serde_json::json!({
        "username": "alice",
        "password": "password123",
        "captcha_response": "valid-token",
    });
    let body_bytes = serde_json::to_vec(&body).expect("encode");
    let response = app::build_auth_app(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/register")
                .header("content-type", "application/json")
                .body(Body::from(body_bytes))
                .expect("build req"),
        )
        .await
        .expect("router");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = body_json(response).await;
    assert_eq!(body["error"], "registration_closed");
}

#[tokio::test]
async fn register_accepts_when_captcha_token_valid() {
    // Noop provider accepts any non-empty token. With
    // apply_to_registration=true + Open, the handler verifies and creates
    // the user.
    let captcha_cfg = CaptchaConfig {
        provider: CaptchaProviderKind::Noop,
        site_key: String::new(),
        secret_key: String::new(),
        apply_to_registration: true,
        apply_to_anonymous_edits: false,
    };
    let state = auth_state(RegistrationPolicy::Open, captcha_cfg, Arc::new(NoopCaptcha)).await;

    let body = serde_json::json!({
        "username": "bob",
        "password": "password123",
        "captcha_response": "anything-non-empty",
    });
    let body_bytes = serde_json::to_vec(&body).expect("encode");
    let response = app::build_auth_app(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/register")
                .header("content-type", "application/json")
                .body(Body::from(body_bytes))
                .expect("build req"),
        )
        .await
        .expect("router");
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = body_json(response).await;
    assert_eq!(body["username"], "bob");
}
