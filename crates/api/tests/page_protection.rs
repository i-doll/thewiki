//! Integration tests for per-page protection enforcement (#34).
//!
//! Each test boots a fresh in-memory SQLite, applies migrations, seeds the
//! default namespace, seeds users with role assignments matching the
//! protection level under test, then drives the router via
//! `tower::ServiceExt::oneshot`.
//!
//! What we cover, mirroring the issue's acceptance matrix:
//!
//! - `None` (the default) — anonymous edits succeed when `anonymous_edits`
//!   is on; 401 when off.
//! - `SemiProtected` — anonymous → 403, authenticated user with no roles
//!   → 200.
//! - `Protected` — authenticated user without `EDIT` → 403; user with
//!   `EDIT` → 200.
//! - `FullyProtected` — user with `EDIT` but not `PROTECT` → 403;
//!   user with `PROTECT` → 200.
//! - `POST /api/v1/pages/{slug}/protect` — non-PROTECT actor → 403;
//!   PROTECT actor → 200 and the audit log captures the transition.

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
use thewiki_core::{
    EmailAddress, Namespace, NamespaceId, NamespaceSlug, Permissions, Role, RoleId, RoleName, User,
    UserId, Username,
};
use thewiki_storage::repo::{
    AuditLogFilter, AuditLogRepository, NamespaceRepository, RoleRepository, SessionRepository,
    UserRepository,
};
use thewiki_storage::sqlite::{SqliteOptions, SqliteStorage};
use time::OffsetDateTime;
use tower::ServiceExt;

// ─── Fixture ──────────────────────────────────────────────────────────────

/// Test-friendly Argon2 parameters at the OWASP floor so test startup stays
/// fast. The hasher is constructed but unused — sessions are seeded directly.
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

/// Build a router plus a storage handle for assertions. Anonymous edits and
/// approval scope are passed in so each test pins the wiki-wide policy.
async fn boot(anonymous_edits: bool) -> (Router, SqliteStorage) {
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
        // Set well in the past so the NewUsers approval scope (24h window)
        // — which we don't currently exercise but might add later — treats
        // every seed user as established.
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

/// Seed a role with the supplied permission bits and grant it to `user_id`.
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
    // Note: build_with_state_with_rate_limit does not add the CSRF layer,
    // so tests can just present the session cookie without an X-CSRF-Token
    // header. The production build_full DOES enforce CSRF on these routes;
    // we accept the gap because exercising the integration of CSRF + the
    // page-CRUD router is the job of configurable_auth.rs.
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

async fn create_home(router: Router, session: &str) -> Value {
    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/pages",
        Some(session),
        Some(json!({
            "namespace_slug": "Main",
            "slug": "home",
            "title": "Home",
            "content": "v1",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create body: {body}");
    body
}

async fn set_protection(router: Router, session: &str, level: &str) -> (StatusCode, Value) {
    json_request(
        router,
        "POST",
        "/api/v1/pages/home/protect",
        Some(session),
        Some(json!({ "protection_level": level })),
    )
    .await
}

async fn attempt_update(router: Router, session: Option<&str>) -> (StatusCode, Value) {
    json_request(
        router,
        "PUT",
        "/api/v1/pages/home",
        session,
        Some(json!({ "content": "v2" })),
    )
    .await
}

// ─── None: anonymous-edits matrix ─────────────────────────────────────────

#[tokio::test]
async fn default_protection_allows_anonymous_when_anonymous_edits_enabled() {
    let (router, _storage) = boot(true).await;

    // Create the page anonymously (allowed because anonymous_edits=true).
    let (status, body) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        None,
        Some(json!({
            "namespace_slug": "Main",
            "slug": "home",
            "title": "Home",
            "content": "v1",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create body: {body}");
    assert_eq!(body["protection_level"], "none");

    // Anonymous edit also succeeds.
    let (status, body) = attempt_update(router, None).await;
    assert_eq!(status, StatusCode::OK, "update body: {body}");
}

#[tokio::test]
async fn default_protection_rejects_anonymous_when_anonymous_edits_disabled() {
    // With anonymous_edits=false, we can't create the page anonymously; seed
    // it via an authenticated user, then verify anonymous PUT is 401.
    let (router, storage) = boot(false).await;
    let admin = seed_user(&storage, "seeder").await;
    seed_role_for(&storage, admin.id, "seeder", Permissions::EDIT).await;
    let session = seed_session(&storage, admin.id).await;
    let _ = create_home(router.clone(), &session).await;

    let (status, body) = attempt_update(router, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_eq!(body["code"], "unauthenticated");
}

// ─── SemiProtected ────────────────────────────────────────────────────────

#[tokio::test]
async fn semi_protected_blocks_anonymous_allows_logged_in_no_roles() {
    let (router, storage) = boot(true).await;
    let admin = seed_user(&storage, "admin").await;
    seed_role_for(
        &storage,
        admin.id,
        "admin",
        Permissions::EDIT | Permissions::PROTECT,
    )
    .await;
    let admin_session = seed_session(&storage, admin.id).await;
    let _ = create_home(router.clone(), &admin_session).await;

    // Raise to semi-protected.
    let (status, body) = set_protection(router.clone(), &admin_session, "semi_protected").await;
    assert_eq!(status, StatusCode::OK, "protect body: {body}");
    assert_eq!(body["protection_level"], "semi_protected");

    // Anonymous edit → 403 with page_protected.
    let (status, body) = attempt_update(router.clone(), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["code"], "page_protected");
    assert!(
        body["message"].as_str().unwrap().contains("semi_protected"),
        "message: {body}"
    );

    // A plain logged-in user with no roles is still allowed (semi just
    // requires "any session").
    let plain = seed_user(&storage, "plain").await;
    let plain_session = seed_session(&storage, plain.id).await;
    let (status, body) = attempt_update(router, Some(&plain_session)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
}

// ─── Protected ────────────────────────────────────────────────────────────

#[tokio::test]
async fn protected_blocks_users_without_edit_permission() {
    let (router, storage) = boot(true).await;
    let admin = seed_user(&storage, "admin").await;
    seed_role_for(
        &storage,
        admin.id,
        "admin",
        Permissions::EDIT | Permissions::PROTECT,
    )
    .await;
    let admin_session = seed_session(&storage, admin.id).await;
    let _ = create_home(router.clone(), &admin_session).await;

    let (status, _) = set_protection(router.clone(), &admin_session, "protected").await;
    assert_eq!(status, StatusCode::OK);

    // Plain user (no roles) → 403.
    let plain = seed_user(&storage, "plain").await;
    let plain_session = seed_session(&storage, plain.id).await;
    let (status, body) = attempt_update(router.clone(), Some(&plain_session)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["code"], "page_protected");
    assert!(
        body["message"].as_str().unwrap().contains("EDIT"),
        "message: {body}"
    );

    // User with the EDIT bit succeeds.
    let editor = seed_user(&storage, "editor").await;
    seed_role_for(&storage, editor.id, "editor", Permissions::EDIT).await;
    let editor_session = seed_session(&storage, editor.id).await;
    let (status, body) = attempt_update(router, Some(&editor_session)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
}

// ─── FullyProtected ───────────────────────────────────────────────────────

#[tokio::test]
async fn fully_protected_requires_protect_bit() {
    let (router, storage) = boot(true).await;
    let admin = seed_user(&storage, "admin").await;
    seed_role_for(
        &storage,
        admin.id,
        "admin",
        Permissions::EDIT | Permissions::PROTECT,
    )
    .await;
    let admin_session = seed_session(&storage, admin.id).await;
    let _ = create_home(router.clone(), &admin_session).await;

    let (status, _) = set_protection(router.clone(), &admin_session, "fully_protected").await;
    assert_eq!(status, StatusCode::OK);

    // Editor (EDIT but no PROTECT) → 403.
    let editor = seed_user(&storage, "editor").await;
    seed_role_for(&storage, editor.id, "editor", Permissions::EDIT).await;
    let editor_session = seed_session(&storage, editor.id).await;
    let (status, body) = attempt_update(router.clone(), Some(&editor_session)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["code"], "page_protected");
    assert!(
        body["message"].as_str().unwrap().contains("PROTECT"),
        "message: {body}"
    );

    // Admin (PROTECT) succeeds.
    let (status, body) = attempt_update(router, Some(&admin_session)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
}

// ─── Protect endpoint authorisation ───────────────────────────────────────

#[tokio::test]
async fn protect_endpoint_rejects_caller_without_protect_permission() {
    let (router, storage) = boot(true).await;
    let admin = seed_user(&storage, "admin").await;
    seed_role_for(
        &storage,
        admin.id,
        "admin",
        Permissions::EDIT | Permissions::PROTECT,
    )
    .await;
    let admin_session = seed_session(&storage, admin.id).await;
    let _ = create_home(router.clone(), &admin_session).await;

    let editor = seed_user(&storage, "editor").await;
    seed_role_for(&storage, editor.id, "editor", Permissions::EDIT).await;
    let editor_session = seed_session(&storage, editor.id).await;

    let (status, body) = set_protection(router, &editor_session, "fully_protected").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["code"], "page_protected");
}

#[tokio::test]
async fn protect_endpoint_writes_audit_log_entry() {
    let (router, storage) = boot(true).await;
    let admin = seed_user(&storage, "admin").await;
    seed_role_for(
        &storage,
        admin.id,
        "admin",
        Permissions::EDIT | Permissions::PROTECT,
    )
    .await;
    let admin_session = seed_session(&storage, admin.id).await;
    let _ = create_home(router.clone(), &admin_session).await;

    let (status, _) = set_protection(router, &admin_session, "fully_protected").await;
    assert_eq!(status, StatusCode::OK);

    // Inspect the audit log directly. The append-only contract from #36
    // means the protect event must be present whenever the protect call
    // succeeded.
    let entries = storage
        .audit_log()
        .list(
            AuditLogFilter {
                actor_username: None,
                action: Some("page.protect".to_string()),
                since: None,
                until: None,
            },
            None,
            10,
        )
        .await
        .expect("list audit log");
    assert_eq!(
        entries.items.len(),
        1,
        "expected one page.protect audit row, got {:?}",
        entries.items
    );
    let row = &entries.items[0];
    assert_eq!(row.action, "page.protect");
    assert_eq!(row.actor_username, "admin");
    assert_eq!(row.metadata["from"], "none");
    assert_eq!(row.metadata["to"], "fully_protected");
}

// ─── PageView serialisation ───────────────────────────────────────────────

#[tokio::test]
async fn get_page_includes_protection_level() {
    let (router, storage) = boot(true).await;
    let admin = seed_user(&storage, "admin").await;
    seed_role_for(
        &storage,
        admin.id,
        "admin",
        Permissions::EDIT | Permissions::PROTECT,
    )
    .await;
    let admin_session = seed_session(&storage, admin.id).await;
    let created = create_home(router.clone(), &admin_session).await;
    assert_eq!(created["protection_level"], "none");

    let (status, _) = set_protection(router.clone(), &admin_session, "protected").await;
    assert_eq!(status, StatusCode::OK);

    let (status, fetched) = json_request(router, "GET", "/api/v1/pages/home", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {fetched}");
    assert_eq!(fetched["protection_level"], "protected");
}
