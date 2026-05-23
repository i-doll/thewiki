//! Integration tests for the admin config viewer (#47).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::Value;
use thewiki_api::AppState;
use thewiki_api::app;
use thewiki_api::auth::password::Argon2Hasher;
use thewiki_api::auth::state::AuthState;
use thewiki_api::config::{ApprovalScope, Argon2Config, AuthConfig, Config};
use thewiki_core::{EmailAddress, Permissions, Role, RoleId, RoleName, User, UserId, Username};
use thewiki_storage::repo::{NamespaceRepository, RoleRepository, SessionRepository, UserRepository};
use thewiki_storage::sqlite::{SqliteOptions, SqliteStorage};
use time::OffsetDateTime;
use tower::ServiceExt;

fn test_argon2() -> Argon2Config {
    Argon2Config {
        memory_kib: 19_456,
        iterations: 2,
        parallelism: 1,
    }
}

fn disabled_rate_limit() -> thewiki_api::config::RateLimitConfig {
    let mut cfg = Config::defaults().rate_limit;
    cfg.enabled = false;
    cfg
}

fn auth_cfg() -> AuthConfig {
    let mut cfg = Config::defaults().auth;
    cfg.anonymous_edits = false;
    cfg.approval_required_for = ApprovalScope::None;
    cfg
}

async fn boot_with_config(config: Config) -> (Router, SqliteStorage) {
    let storage = SqliteStorage::new(
        "sqlite::memory:",
        SqliteOptions {
            max_connections: 1,
            acquire_timeout: Duration::from_secs(5),
            foreign_keys: true,
        },
    )
    .await
    .expect("open + migrate sqlite");

    storage
        .namespaces()
        .get_or_create_default()
        .await
        .expect("seed namespace");

    let cfg = auth_cfg();
    let hasher = Arc::new(Argon2Hasher::new(test_argon2()).expect("hasher"));
    let auth_state = AuthState::new(
        storage.clone(),
        hasher,
        Duration::from_secs(60 * 60),
        false,
        cfg.clone(),
    );
    let state = AppState::new(storage.clone(), cfg)
        .with_auth_state(auth_state)
        .with_runtime_config(Arc::new(config));
    let router = app::build_with_state_with_rate_limit(state, disabled_rate_limit());
    (router, storage)
}

async fn boot_without_config() -> (Router, SqliteStorage) {
    let storage = SqliteStorage::new(
        "sqlite::memory:",
        SqliteOptions {
            max_connections: 1,
            acquire_timeout: Duration::from_secs(5),
            foreign_keys: true,
        },
    )
    .await
    .expect("open + migrate sqlite");
    storage
        .namespaces()
        .get_or_create_default()
        .await
        .expect("seed namespace");
    let cfg = auth_cfg();
    let hasher = Arc::new(Argon2Hasher::new(test_argon2()).expect("hasher"));
    let auth_state = AuthState::new(
        storage.clone(),
        hasher,
        Duration::from_secs(60 * 60),
        false,
        cfg.clone(),
    );
    let state = AppState::new(storage.clone(), cfg).with_auth_state(auth_state);
    let router = app::build_with_state_with_rate_limit(state, disabled_rate_limit());
    (router, storage)
}

async fn seed_user(storage: &SqliteStorage, username: &str) -> User {
    let user = User {
        id: UserId::new(),
        username: Username::new(username).expect("valid username"),
        email: Some(EmailAddress::new(format!("{username}@example.com")).expect("valid email")),
        display_name: Some(username.to_string()),
        created_at: OffsetDateTime::now_utc() - time::Duration::days(30),
        last_login_at: None,
    };
    storage
        .users()
        .create(&user, None)
        .await
        .expect("seed user");
    user
}

async fn seed_role_for(
    storage: &SqliteStorage,
    user_id: UserId,
    name: &str,
    permissions: Permissions,
) {
    let role = Role {
        id: RoleId::new(),
        name: RoleName::new(name).expect("role name"),
        display_name: name.to_string(),
        permissions,
    };
    storage.roles().create(&role).await.expect("seed role");
    storage
        .roles()
        .assign_to_user(user_id, role.id)
        .await
        .expect("assign role");
}

async fn seed_session(storage: &SqliteStorage, user_id: UserId) -> String {
    storage
        .sessions()
        .create(user_id, Duration::from_secs(60 * 60), None, None)
        .await
        .expect("seed session")
        .id
        .into_uuid()
        .to_string()
}

async fn json_request(
    router: Router,
    method: &str,
    uri: &str,
    session_cookie: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(cookie) = session_cookie {
        builder = builder.header(header::COOKIE, format!("thewiki_session={cookie}"));
    }
    let request = builder.body(Body::empty()).expect("build request");
    let response = router.oneshot(request).await.expect("router responded");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    if bytes.is_empty() {
        (status, Value::Null)
    } else {
        let parsed: Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| panic!("response was not JSON: {bytes:?}"));
        (status, parsed)
    }
}

#[tokio::test]
async fn config_requires_manage_users() {
    let (router, storage) = boot_with_config(Config::defaults()).await;
    let viewer = seed_user(&storage, "viewer").await;
    let session = seed_session(&storage, viewer.id).await;
    let (status, _body) =
        json_request(router, "GET", "/api/v1/admin/config", Some(&session)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn config_returns_redacted_secret_when_available() {
    let mut cfg = Config::defaults();
    cfg.captcha.secret_key = "supersecret".into();
    let (router, storage) = boot_with_config(cfg).await;
    let admin = seed_user(&storage, "admin").await;
    seed_role_for(&storage, admin.id, "admin", Permissions::MANAGE_USERS).await;
    let session = seed_session(&storage, admin.id).await;

    let (status, body) =
        json_request(router, "GET", "/api/v1/admin/config", Some(&session)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["available"], true);
    assert_eq!(
        body.pointer("/config/captcha/secret_key").and_then(|v| v.as_str()),
        Some("<redacted>"),
    );
    // Sanity: a non-secret field is still visible.
    assert!(
        body.pointer("/config/server/bind").is_some(),
        "server.bind should be visible"
    );
}

#[tokio::test]
async fn config_response_never_leaks_database_url_credentials() {
    // The /admin/config viewer is gated by MANAGE_USERS. A Postgres deploy
    // configures `database.url = "postgres://user:pass@host/db"`; if the
    // redaction step misses that field the admin can read the database
    // password without shell access. Regression guard: assert the
    // credentials never appear in the wire response.
    let mut cfg = Config::defaults();
    cfg.database.url =
        "postgres://wiki_app:hunter2-do-not-leak@db.internal:5432/wiki".into();
    let (router, storage) = boot_with_config(cfg).await;
    let admin = seed_user(&storage, "admin").await;
    seed_role_for(&storage, admin.id, "admin", Permissions::MANAGE_USERS).await;
    let session = seed_session(&storage, admin.id).await;

    let (status, body) =
        json_request(router, "GET", "/api/v1/admin/config", Some(&session)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    // The leaf itself is redacted.
    assert_eq!(
        body.pointer("/config/database/url").and_then(|v| v.as_str()),
        Some("<redacted>"),
    );

    // Defence in depth: serialise the whole response and grep for the
    // password / URL-shape strings. If a future schema change introduces
    // another leaf that surfaces the URL verbatim, this fails loud.
    let serialised = serde_json::to_string(&body).expect("serialise body");
    assert!(
        !serialised.contains("hunter2-do-not-leak"),
        "db password leaked in response: {serialised}"
    );
    assert!(
        !serialised.contains("postgres://"),
        "raw db URL leaked in response: {serialised}"
    );
    assert!(
        !serialised.contains("wiki_app:"),
        "db username leaked in response: {serialised}"
    );
}

#[tokio::test]
async fn config_returns_null_when_no_runtime_config_wired() {
    let (router, storage) = boot_without_config().await;
    let admin = seed_user(&storage, "admin").await;
    seed_role_for(&storage, admin.id, "admin", Permissions::MANAGE_USERS).await;
    let session = seed_session(&storage, admin.id).await;
    let (status, body) =
        json_request(router, "GET", "/api/v1/admin/config", Some(&session)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["available"], false);
    assert!(body["config"].is_null());
}
