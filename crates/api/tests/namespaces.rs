//! Integration tests for the namespace-aware URL routing and the namespace
//! CRUD endpoints (#28).
//!
//! Coverage:
//!
//! - `POST   /api/v1/namespaces` — admin creates `Help` (201); non-admin → 403.
//! - `GET    /api/v1/namespaces` — list returns `Main` + `Help`.
//! - `POST   /api/v1/wiki/Help`  — create page in `Help` (201).
//! - `GET    /api/v1/wiki/Help/foo` — fetches the page from `Help`.
//! - `GET    /api/v1/wiki/foo` — fetches the page from `Main` (default).
//! - `GET    /api/v1/pages/foo` — legacy back-compat still works.
//!
//! Each test boots a fresh in-memory SQLite, seeds `Main` via
//! `get_or_create_default()` (mirroring boot wiring), and seeds a user with
//! the requested permission set so the CSRF + session double-submit pair is
//! the only ceremony the test has to manage.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use thewiki_api::AppState;
use thewiki_api::app;
use thewiki_api::auth::password::Argon2Hasher;
use thewiki_api::auth::state::AuthState;
use thewiki_api::config::{ApprovalScope, Argon2Config, AuthConfig, Config};
use thewiki_core::{EmailAddress, Permissions, Role, RoleId, RoleName, User, UserId, Username};
use thewiki_storage::repo::{
    NamespaceRepository, RoleRepository, SessionRepository, UserRepository,
};
use thewiki_storage::sqlite::{SqliteOptions, SqliteStorage};
use time::OffsetDateTime;
use tower::ServiceExt;

// ─── Fixture ──────────────────────────────────────────────────────────────

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
    cfg.anonymous_edits = true;
    cfg.approval_required_for = ApprovalScope::None;
    cfg
}

/// Boot a fresh router. The default `Main` namespace is seeded via the
/// same `get_or_create_default()` path the production binary uses, so the
/// tests exercise the boot-seed code as a side effect.
async fn boot() -> (Router, SqliteStorage) {
    let storage = SqliteStorage::new(
        "sqlite::memory:",
        SqliteOptions {
            max_connections: 1,
            acquire_timeout: Duration::from_secs(5),
            foreign_keys: true,
        },
    )
    .await
    .expect("open + migrate in-memory sqlite");

    storage
        .namespaces()
        .get_or_create_default()
        .await
        .expect("seed default namespace at boot");

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
    role_name: &str,
    permissions: Permissions,
) {
    let role = Role {
        id: RoleId::new(),
        name: RoleName::new(role_name).expect("role name"),
        display_name: role_name.to_string(),
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

/// Drive a request through the router. The session cookie shape mirrors
/// what `build_with_state_with_rate_limit` accepts — no CSRF middleware is
/// mounted in that build, so the session cookie alone is sufficient.
async fn json_request(
    router: Router,
    method: &str,
    uri: &str,
    session_cookie: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(cookie) = session_cookie {
        builder = builder.header(header::COOKIE, format!("thewiki_session={cookie}"));
    }
    let request = if let Some(body) = body {
        builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("build request")
    } else {
        builder.body(Body::empty()).expect("build request")
    };

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
            .unwrap_or_else(|_| panic!("response wasn't JSON: {:?}", &bytes));
        (status, parsed)
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn admin_can_create_namespace() {
    let (router, storage) = boot().await;
    let admin = seed_user(&storage, "admin").await;
    seed_role_for(
        &storage,
        admin.id,
        "ns-admin",
        Permissions::MANAGE_NAMESPACES,
    )
    .await;
    let session = seed_session(&storage, admin.id).await;

    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/namespaces",
        Some(&session),
        Some(json!({"slug": "Help", "display_name": "Help"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(body["slug"], "Help");
    assert_eq!(body["display_name"], "Help");
    assert!(body["id"].is_string());
}

#[tokio::test]
async fn create_namespace_without_permission_returns_403() {
    let (router, storage) = boot().await;
    let plain = seed_user(&storage, "plain").await;
    // Deliberately no MANAGE_NAMESPACES grant.
    let session = seed_session(&storage, plain.id).await;

    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/namespaces",
        Some(&session),
        Some(json!({"slug": "Help", "display_name": "Help"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["code"], "forbidden");
}

#[tokio::test]
async fn anonymous_create_namespace_returns_401() {
    let (router, _) = boot().await;
    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/namespaces",
        None,
        Some(json!({"slug": "Help", "display_name": "Help"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_eq!(body["code"], "unauthenticated");
}

#[tokio::test]
async fn list_namespaces_returns_main_and_help() {
    let (router, storage) = boot().await;
    let admin = seed_user(&storage, "admin").await;
    seed_role_for(
        &storage,
        admin.id,
        "ns-admin",
        Permissions::MANAGE_NAMESPACES,
    )
    .await;
    let session = seed_session(&storage, admin.id).await;

    let (status, _) = json_request(
        router.clone(),
        "POST",
        "/api/v1/namespaces",
        Some(&session),
        Some(json!({"slug": "Help", "display_name": "Help"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = json_request(router, "GET", "/api/v1/namespaces", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let items = body["items"].as_array().expect("items array");
    let slugs: Vec<&str> = items.iter().filter_map(|i| i["slug"].as_str()).collect();
    assert!(slugs.contains(&"Main"), "missing Main: {slugs:?}");
    assert!(slugs.contains(&"Help"), "missing Help: {slugs:?}");
}

#[tokio::test]
async fn create_page_via_wiki_namespace_route() {
    let (router, storage) = boot().await;
    let admin = seed_user(&storage, "admin").await;
    seed_role_for(
        &storage,
        admin.id,
        "ns-admin",
        Permissions::MANAGE_NAMESPACES,
    )
    .await;
    let session = seed_session(&storage, admin.id).await;

    // Create the Help namespace.
    let (status, _) = json_request(
        router.clone(),
        "POST",
        "/api/v1/namespaces",
        Some(&session),
        Some(json!({"slug": "Help", "display_name": "Help"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // Create a page in Help via the namespace-aware route.
    let (status, body) = json_request(
        router.clone(),
        "POST",
        "/api/v1/wiki/Help",
        Some(&session),
        Some(json!({
            "slug": "foo",
            "title": "Foo",
            "content": "Hello from Help",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(body["slug"], "foo");
    assert_eq!(body["namespace_slug"], "Help");

    // GET via the namespace-aware path.
    let (status, body) = json_request(router, "GET", "/api/v1/wiki/Help/foo", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["slug"], "foo");
    assert_eq!(body["namespace_slug"], "Help");
    assert_eq!(body["content"], "Hello from Help");
}

#[tokio::test]
async fn wiki_default_namespace_falls_back_to_main() {
    // `/api/v1/wiki/foo` (no namespace segment) currently isn't a separate
    // route — the back-compat path is `/api/v1/pages/foo`, and the
    // namespace-aware route always requires the namespace segment. We
    // exercise both shapes so future regressions stand out:
    //
    // 1. `/api/v1/wiki/Main/foo` — explicit `Main`.
    // 2. `/api/v1/pages/foo`      — legacy back-compat default to `Main`.
    let (router, storage) = boot().await;
    let admin = seed_user(&storage, "admin").await;
    seed_role_for(
        &storage,
        admin.id,
        "ns-admin",
        Permissions::MANAGE_NAMESPACES,
    )
    .await;
    let session = seed_session(&storage, admin.id).await;

    // Create a page in Main via the legacy route.
    let (status, _) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        Some(&session),
        Some(json!({
            "namespace_slug": "Main",
            "slug": "foo",
            "title": "Foo",
            "content": "Hello from Main",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) =
        json_request(router.clone(), "GET", "/api/v1/wiki/Main/foo", None, None).await;
    assert_eq!(status, StatusCode::OK, "explicit Main body: {body}");
    assert_eq!(body["content"], "Hello from Main");

    // Legacy back-compat: /api/v1/pages/foo continues to work.
    let (status, body) = json_request(router, "GET", "/api/v1/pages/foo", None, None).await;
    assert_eq!(status, StatusCode::OK, "legacy body: {body}");
    assert_eq!(body["content"], "Hello from Main");
}

#[tokio::test]
async fn legacy_pages_route_remains_back_compat() {
    let (router, _) = boot().await;
    let (status, body) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        None,
        Some(json!({
            "namespace_slug": "Main",
            "slug": "legacy",
            "title": "Legacy",
            "content": "Legacy body",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(body["namespace_slug"], "Main");

    let (status, body) = json_request(router, "GET", "/api/v1/pages/legacy", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["slug"], "legacy");
}

#[tokio::test]
async fn create_page_in_unknown_namespace_returns_404() {
    let (router, _) = boot().await;
    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/wiki/Nowhere",
        None,
        Some(json!({
            "slug": "foo",
            "title": "Foo",
            "content": ".",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
    assert_eq!(body["code"], "not_found");
}

#[tokio::test]
async fn delete_namespace_with_pages_returns_409() {
    let (router, storage) = boot().await;
    let admin = seed_user(&storage, "admin").await;
    seed_role_for(
        &storage,
        admin.id,
        "ns-admin",
        Permissions::MANAGE_NAMESPACES,
    )
    .await;
    let session = seed_session(&storage, admin.id).await;

    // Create Help and a page inside it.
    let (status, _) = json_request(
        router.clone(),
        "POST",
        "/api/v1/namespaces",
        Some(&session),
        Some(json!({"slug": "Help", "display_name": "Help"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = json_request(
        router.clone(),
        "POST",
        "/api/v1/wiki/Help",
        Some(&session),
        Some(json!({
            "slug": "stuck",
            "title": "Stuck",
            "content": ".",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // Refuse to drop a non-empty namespace.
    let (status, body) = json_request(
        router,
        "DELETE",
        "/api/v1/namespaces/Help",
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert_eq!(body["code"], "conflict");
}

#[tokio::test]
async fn update_namespace_renames_display_name() {
    let (router, storage) = boot().await;
    let admin = seed_user(&storage, "admin").await;
    seed_role_for(
        &storage,
        admin.id,
        "ns-admin",
        Permissions::MANAGE_NAMESPACES,
    )
    .await;
    let session = seed_session(&storage, admin.id).await;

    let (status, _) = json_request(
        router.clone(),
        "POST",
        "/api/v1/namespaces",
        Some(&session),
        Some(json!({"slug": "Help", "display_name": "Help"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = json_request(
        router,
        "PATCH",
        "/api/v1/namespaces/Help",
        Some(&session),
        Some(json!({"display_name": "Help Center"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["slug"], "Help");
    assert_eq!(body["display_name"], "Help Center");
}
