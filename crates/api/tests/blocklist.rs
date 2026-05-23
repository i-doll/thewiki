//! Integration tests for the IP / URL blocklist (#42).
//!
//! Covers:
//!
//! - Admin endpoints reject callers without `MANAGE_BLOCKLIST`.
//! - `POST /admin/blocklist/url` rejects invalid regex with 400.
//! - A blocklisted URL in an edit body returns 400 with the matched URL.
//! - The IP middleware 403s blocked IPs across non-health routes and lets
//!   `/healthz` through.
//!
//! For the snapshot/CIDR matching and X-Forwarded-For logic we lean on the
//! per-module unit tests in `crates/api/src/blocklist/*`. This file focuses
//! on end-to-end wiring through the Axum router.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use thewiki_api::AppState;
use thewiki_api::app;
use thewiki_api::auth::password::Argon2Hasher;
use thewiki_api::auth::state::AuthState;
use thewiki_api::blocklist::BlocklistState;
use thewiki_api::config::{ApprovalScope, Argon2Config, AuthConfig, Config};
use thewiki_core::{
    EmailAddress, Namespace, NamespaceId, NamespaceSlug, Permissions, Role, RoleId, RoleName, User,
    UserId, Username,
};
use thewiki_storage::repo::{
    IpBlocklistRepository, NamespaceRepository, NewIpBlocklistEntry, NewUrlBlocklistEntry,
    RoleRepository, SessionRepository, UrlBlocklistRepository, UserRepository,
};
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

fn auth_cfg(anonymous_edits: bool) -> AuthConfig {
    let mut cfg = Config::defaults().auth;
    cfg.anonymous_edits = anonymous_edits;
    cfg.approval_required_for = ApprovalScope::None;
    cfg
}

/// Boot a router with an empty blocklist state wired up.
async fn boot(anonymous_edits: bool) -> (Router, SqliteStorage, BlocklistState) {
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
        .create(&Namespace {
            id: NamespaceId::new(),
            slug: NamespaceSlug::new("Main").expect("valid slug"),
            display_name: "Main".into(),
            is_talk: false,
            paired_namespace_id: None,
        })
        .await
        .expect("seed Main namespace");

    let cfg = auth_cfg(anonymous_edits);
    let hasher = Arc::new(Argon2Hasher::new(test_argon2()).expect("hasher"));
    let auth_state = AuthState::new(
        storage.clone(),
        hasher,
        Duration::from_secs(60 * 60),
        false,
        cfg.clone(),
    );
    let blocklist = BlocklistState::empty();
    let state = AppState::new(storage.clone(), cfg)
        .with_auth_state(auth_state)
        .with_blocklist(blocklist.clone());
    let router = app::build_with_state_with_rate_limit(state, disabled_rate_limit());
    (router, storage, blocklist)
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

// ─── Admin endpoints: authorisation ────────────────────────────────────────

#[tokio::test]
async fn admin_list_requires_manage_blocklist() {
    let (router, storage, _bl) = boot(false).await;
    let user = seed_user(&storage, "viewer").await;
    // No permissions granted.
    let session = seed_session(&storage, user.id).await;

    let (status, body) = json_request(
        router,
        "GET",
        "/api/v1/admin/blocklist/ip",
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
}

#[tokio::test]
async fn admin_list_unauthenticated_returns_401() {
    let (router, _storage, _bl) = boot(false).await;
    let (status, _body) = json_request(
        router,
        "GET",
        "/api/v1/admin/blocklist/ip",
        None,
        None,
    )
    .await;
    // The unauthenticated request goes through the AuthSession extractor
    // (no session cookie) which renders as 401 with the auth error body.
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_create_ip_persists_and_refreshes_snapshot() {
    let (router, storage, blocklist) = boot(false).await;
    let admin = seed_user(&storage, "admin").await;
    seed_role_for(
        &storage,
        admin.id,
        "admin",
        Permissions::MANAGE_BLOCKLIST,
    )
    .await;
    let session = seed_session(&storage, admin.id).await;

    let (status, body) = json_request(
        router.clone(),
        "POST",
        "/api/v1/admin/blocklist/ip",
        Some(&session),
        Some(json!({ "cidr": "203.0.113.0/24", "reason": "spam" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(body["cidr"], "203.0.113.0/24");
    assert_eq!(body["reason"], "spam");

    // The middleware snapshot is hydrated on every mutation; confirm the IP
    // now matches.
    let snap = blocklist.snapshot().await;
    assert!(snap.contains_ip("203.0.113.42".parse().unwrap()));
}

#[tokio::test]
async fn admin_create_ip_rejects_invalid_cidr() {
    let (router, storage, _bl) = boot(false).await;
    let admin = seed_user(&storage, "admin").await;
    seed_role_for(&storage, admin.id, "admin", Permissions::MANAGE_BLOCKLIST).await;
    let session = seed_session(&storage, admin.id).await;

    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/admin/blocklist/ip",
        Some(&session),
        Some(json!({ "cidr": "not-a-cidr", "reason": "" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["code"], "invalid_input");
}

#[tokio::test]
async fn admin_create_url_rejects_invalid_regex() {
    let (router, storage, _bl) = boot(false).await;
    let admin = seed_user(&storage, "admin").await;
    seed_role_for(&storage, admin.id, "admin", Permissions::MANAGE_BLOCKLIST).await;
    let session = seed_session(&storage, admin.id).await;

    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/admin/blocklist/url",
        Some(&session),
        // `(` is an unclosed group — `regex::Regex::new` returns a parse
        // error and the handler rejects before persisting.
        Some(json!({ "pattern": "(unclosed", "reason": "bad" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["code"], "invalid_input");
}

#[tokio::test]
async fn admin_delete_ip_returns_204_and_refreshes() {
    let (router, storage, blocklist) = boot(false).await;
    let admin = seed_user(&storage, "admin").await;
    seed_role_for(&storage, admin.id, "admin", Permissions::MANAGE_BLOCKLIST).await;
    let session = seed_session(&storage, admin.id).await;

    let (create_status, created) = json_request(
        router.clone(),
        "POST",
        "/api/v1/admin/blocklist/ip",
        Some(&session),
        Some(json!({ "cidr": "203.0.113.0/24" })),
    )
    .await;
    assert_eq!(create_status, StatusCode::CREATED, "create: {created}");
    let id = created["id"].as_str().expect("id is string").to_owned();

    let (status, body) = json_request(
        router.clone(),
        "DELETE",
        &format!("/api/v1/admin/blocklist/ip/{id}"),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body: {body}");

    let snap = blocklist.snapshot().await;
    assert!(!snap.contains_ip("203.0.113.42".parse().unwrap()));
}

#[tokio::test]
async fn admin_delete_ip_missing_returns_404() {
    let (router, storage, _bl) = boot(false).await;
    let admin = seed_user(&storage, "admin").await;
    seed_role_for(&storage, admin.id, "admin", Permissions::MANAGE_BLOCKLIST).await;
    let session = seed_session(&storage, admin.id).await;

    let missing = uuid::Uuid::now_v7();
    let (status, _body) = json_request(
        router,
        "DELETE",
        &format!("/api/v1/admin/blocklist/ip/{missing}"),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ─── URL blocklist enforcement on edits ────────────────────────────────────

#[tokio::test]
async fn url_in_edit_body_rejected_with_400() {
    let (router, storage, blocklist) = boot(true).await;
    // Seed via direct storage write + refresh so this test is independent of
    // the admin endpoint path.
    let admin = seed_user(&storage, "admin").await;
    storage
        .url_blocklist()
        .create(NewUrlBlocklistEntry {
            pattern: r"evil\.example".to_string(),
            reason: "spam".to_string(),
            created_by: admin.id,
        })
        .await
        .expect("seed url row");
    blocklist
        .refresh_from(&storage.ip_blocklist(), &storage.url_blocklist())
        .await
        .expect("refresh snapshot");

    // Anonymous create with a body that references the bad URL.
    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/pages",
        None,
        Some(json!({
            "namespace_slug": "Main",
            "slug": "home",
            "title": "Home",
            "content": "follow https://evil.example/page",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["code"], "invalid_input");
    let message = body["message"].as_str().expect("message");
    assert!(message.contains("evil.example"), "message: {message}");
}

#[tokio::test]
async fn benign_url_in_edit_body_passes() {
    let (router, storage, blocklist) = boot(true).await;
    let admin = seed_user(&storage, "admin").await;
    storage
        .url_blocklist()
        .create(NewUrlBlocklistEntry {
            pattern: r"evil\.example".to_string(),
            reason: String::new(),
            created_by: admin.id,
        })
        .await
        .expect("seed url row");
    blocklist
        .refresh_from(&storage.ip_blocklist(), &storage.url_blocklist())
        .await
        .expect("refresh snapshot");

    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/pages",
        None,
        Some(json!({
            "namespace_slug": "Main",
            "slug": "home",
            "title": "Home",
            "content": "see https://good.example/page",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
}

// ─── IP middleware enforcement ─────────────────────────────────────────────

/// Run a request through the full production router with the supplied
/// ConnectInfo extension, mimicking what
/// `into_make_service_with_connect_info` does at the listener boundary.
async fn run_with_peer(
    router: Router,
    method: &str,
    uri: &str,
    peer: std::net::SocketAddr,
) -> StatusCode {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .expect("build request");
    request.extensions_mut().insert(ConnectInfo(peer));
    let response = router.oneshot(request).await.expect("router responded");
    response.status()
}

#[tokio::test]
async fn ip_middleware_blocks_listed_ipv4_and_passes_others() {
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
    storage
        .namespaces()
        .create(&Namespace {
            id: NamespaceId::new(),
            slug: NamespaceSlug::new("Main").expect("valid slug"),
            display_name: "Main".into(),
            is_talk: false,
            paired_namespace_id: None,
        })
        .await
        .expect("seed namespace");

    // Seed an admin user we can charge the IP row to.
    let admin = seed_user(&storage, "admin").await;
    storage
        .ip_blocklist()
        .create(NewIpBlocklistEntry {
            cidr: "203.0.113.0/24".to_string(),
            reason: String::new(),
            created_by: admin.id,
        })
        .await
        .expect("seed ip row");

    // Build the same wiring as `build_full` but with a disabled rate
    // limiter so we can exercise the blocklist without the rate-limit gate.
    let hasher = Arc::new(Argon2Hasher::new(test_argon2()).expect("hasher"));
    let cfg = auth_cfg(true);
    let auth_state = AuthState::new(
        storage.clone(),
        hasher,
        Duration::from_secs(60 * 60),
        false,
        cfg.clone(),
    );
    let blocklist = BlocklistState::empty();
    blocklist
        .refresh_from(&storage.ip_blocklist(), &storage.url_blocklist())
        .await
        .expect("refresh");
    let app_state = AppState::new(storage.clone(), cfg)
        .with_auth_state(auth_state.clone())
        .with_blocklist(blocklist);
    let rate_limit_state =
        thewiki_api::rate_limit::RateLimitState::new(disabled_rate_limit(), Some(auth_state.clone()));
    let router = app::build_full_with_rate_limit_state(
        app_state,
        auth_state,
        false,
        rate_limit_state,
        Config::defaults().graphql,
        Config::defaults().security,
    );

    // Blocked peer → 403 even on a public read path.
    let status = run_with_peer(
        router.clone(),
        "GET",
        "/api/v1/pages",
        "203.0.113.42:1024".parse().unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "blocked IP should 403");

    // Health checks are always allowed.
    let status = run_with_peer(
        router.clone(),
        "GET",
        "/healthz",
        "203.0.113.42:1024".parse().unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "/healthz must not be blocked");

    // Readiness checks are also exempt — same contract as /healthz.
    let status = run_with_peer(
        router.clone(),
        "GET",
        "/readyz",
        "203.0.113.42:1024".parse().unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "/readyz must not be blocked");

    // A different IP still gets through to the API.
    let status = run_with_peer(
        router,
        "GET",
        "/api/v1/pages",
        "198.51.100.7:1024".parse().unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unblocked IP should reach the page list",
    );
}
