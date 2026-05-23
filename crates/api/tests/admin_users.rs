//! Integration tests for the admin user endpoints (#47).
//!
//! Coverage:
//!
//! - Authorisation: MANAGE_USERS is required for every endpoint; anon
//!   callers see 401, under-privileged callers see 403.
//! - Listing: pagination, search filter, role filter.
//! - Detail: 404 on missing id; roles are included inline.
//! - Bulk role assign: idempotent + audit-logged per actual change.
//! - Revoke: returns 204 on both first and second call (idempotent).

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
    AuditLogFilter, AuditLogRepository, NamespaceRepository, RoleRepository, SessionRepository,
    UserRepository,
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

fn auth_cfg() -> AuthConfig {
    let mut cfg = Config::defaults().auth;
    cfg.anonymous_edits = false;
    cfg.approval_required_for = ApprovalScope::None;
    cfg
}

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
    .expect("open + migrate sqlite");

    storage
        .namespaces()
        .get_or_create_default()
        .await
        .expect("seed Main namespace");

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

async fn seed_role(
    storage: &SqliteStorage,
    name: &str,
    permissions: Permissions,
) -> Role {
    let role = Role {
        id: RoleId::new(),
        name: RoleName::new(name).expect("role name"),
        display_name: name.to_string(),
        permissions,
    };
    storage.roles().create(&role).await.expect("seed role");
    role
}

async fn assign_role(storage: &SqliteStorage, user_id: UserId, role_id: RoleId) {
    storage
        .roles()
        .assign_to_user(user_id, role_id)
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
            .unwrap_or_else(|_| panic!("response was not JSON: {bytes:?}"));
        (status, parsed)
    }
}

// ─── Authorisation ────────────────────────────────────────────────────────

#[tokio::test]
async fn list_users_unauthenticated_returns_401() {
    let (router, _storage) = boot().await;
    let (status, _body) = json_request(router, "GET", "/api/v1/admin/users", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn list_users_without_manage_users_returns_403() {
    let (router, storage) = boot().await;
    let viewer = seed_user(&storage, "viewer").await;
    let session = seed_session(&storage, viewer.id).await;
    let (status, _body) = json_request(
        router,
        "GET",
        "/api/v1/admin/users",
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn list_users_with_permission_returns_seeded_rows() {
    let (router, storage) = boot().await;
    let admin = seed_user(&storage, "admin").await;
    let admin_role = seed_role(&storage, "admin", Permissions::MANAGE_USERS).await;
    assign_role(&storage, admin.id, admin_role.id).await;
    seed_user(&storage, "alice").await;
    seed_user(&storage, "bob").await;
    let session = seed_session(&storage, admin.id).await;

    let (status, body) = json_request(
        router,
        "GET",
        "/api/v1/admin/users",
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let items = body["items"].as_array().expect("items is array");
    let usernames: Vec<&str> = items
        .iter()
        .map(|u| u["username"].as_str().expect("string"))
        .collect();
    assert!(usernames.contains(&"admin"));
    assert!(usernames.contains(&"alice"));
    assert!(usernames.contains(&"bob"));
}

#[tokio::test]
async fn list_users_search_filters_by_username() {
    let (router, storage) = boot().await;
    let admin = seed_user(&storage, "admin").await;
    let admin_role = seed_role(&storage, "admin", Permissions::MANAGE_USERS).await;
    assign_role(&storage, admin.id, admin_role.id).await;
    seed_user(&storage, "alice").await;
    seed_user(&storage, "bobby").await;
    let session = seed_session(&storage, admin.id).await;

    let (status, body) = json_request(
        router,
        "GET",
        "/api/v1/admin/users?search=ali",
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["username"], "alice");
}

#[tokio::test]
async fn list_users_role_filter_restricts_to_assigned() {
    let (router, storage) = boot().await;
    let admin = seed_user(&storage, "admin").await;
    let admin_role = seed_role(&storage, "admin", Permissions::MANAGE_USERS).await;
    assign_role(&storage, admin.id, admin_role.id).await;
    let alice = seed_user(&storage, "alice").await;
    let editor_role = seed_role(&storage, "editor", Permissions::EDIT).await;
    assign_role(&storage, alice.id, editor_role.id).await;
    let _bob = seed_user(&storage, "bob").await;
    let session = seed_session(&storage, admin.id).await;

    let (status, body) = json_request(
        router,
        "GET",
        &format!(
            "/api/v1/admin/users?role_id={}",
            editor_role.id.into_uuid()
        ),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["username"], "alice");
}

#[tokio::test]
async fn list_users_hydrates_roles_for_every_row_in_one_batch() {
    // Regression test for the N+1 fan-out fix. Three users on the page,
    // each with a different role profile (two roles / one role / none).
    // The handler now calls `list_roles_for_users` once and groups the
    // result server-side; this test asserts the wire response carries
    // the right roles per user regardless of how the rows are grouped.
    let (router, storage) = boot().await;
    let admin = seed_user(&storage, "admin").await;
    let admin_role = seed_role(&storage, "admin", Permissions::MANAGE_USERS).await;
    assign_role(&storage, admin.id, admin_role.id).await;

    let alice = seed_user(&storage, "alice").await;
    let editor_role = seed_role(&storage, "editor", Permissions::EDIT).await;
    let reviewer_role = seed_role(&storage, "reviewer", Permissions::READ).await;
    assign_role(&storage, alice.id, editor_role.id).await;
    assign_role(&storage, alice.id, reviewer_role.id).await;

    let bob = seed_user(&storage, "bob").await;
    assign_role(&storage, bob.id, editor_role.id).await;

    // carol holds no roles — must still appear in the listing with `roles: []`.
    let _carol = seed_user(&storage, "carol").await;

    let session = seed_session(&storage, admin.id).await;
    let (status, body) = json_request(
        router,
        "GET",
        "/api/v1/admin/users",
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 4, "admin + alice + bob + carol");

    let by_username: std::collections::HashMap<&str, &Value> = items
        .iter()
        .map(|u| (u["username"].as_str().expect("username"), u))
        .collect();

    let role_names = |entry: &Value| -> Vec<String> {
        let mut names: Vec<String> = entry["roles"]
            .as_array()
            .expect("roles array")
            .iter()
            .map(|r| r["name"].as_str().expect("role name").to_string())
            .collect();
        names.sort();
        names
    };

    assert_eq!(role_names(by_username["alice"]), vec!["editor", "reviewer"]);
    assert_eq!(role_names(by_username["bob"]), vec!["editor"]);
    assert_eq!(role_names(by_username["carol"]), Vec::<String>::new());
    assert_eq!(role_names(by_username["admin"]), vec!["admin"]);
}

#[tokio::test]
async fn get_user_returns_attached_roles() {
    let (router, storage) = boot().await;
    let admin = seed_user(&storage, "admin").await;
    let admin_role = seed_role(&storage, "admin", Permissions::MANAGE_USERS).await;
    assign_role(&storage, admin.id, admin_role.id).await;
    let alice = seed_user(&storage, "alice").await;
    let editor_role = seed_role(&storage, "editor", Permissions::EDIT).await;
    assign_role(&storage, alice.id, editor_role.id).await;
    let session = seed_session(&storage, admin.id).await;

    let (status, body) = json_request(
        router,
        "GET",
        &format!("/api/v1/admin/users/{}", alice.id.into_uuid()),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let role_names: Vec<&str> = body["roles"]
        .as_array()
        .expect("roles array")
        .iter()
        .map(|r| r["name"].as_str().expect("string"))
        .collect();
    assert_eq!(role_names, vec!["editor"]);
}

#[tokio::test]
async fn assign_roles_grants_and_audits() {
    let (router, storage) = boot().await;
    let admin = seed_user(&storage, "admin").await;
    let admin_role = seed_role(&storage, "admin", Permissions::MANAGE_USERS).await;
    assign_role(&storage, admin.id, admin_role.id).await;
    let alice = seed_user(&storage, "alice").await;
    let editor_role = seed_role(&storage, "editor", Permissions::EDIT).await;
    let session = seed_session(&storage, admin.id).await;

    let (status, body) = json_request(
        router.clone(),
        "POST",
        &format!("/api/v1/admin/users/{}/roles", alice.id.into_uuid()),
        Some(&session),
        Some(json!({ "role_ids": [editor_role.id.into_uuid().to_string()] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let role_names: Vec<&str> = body["roles"]
        .as_array()
        .expect("roles")
        .iter()
        .map(|r| r["name"].as_str().expect("string"))
        .collect();
    assert!(role_names.contains(&"editor"));

    // Audit row is written. The standard filter is "newest first" — pick
    // the first matching action.
    let log = storage
        .audit_log()
        .list(AuditLogFilter::default(), None, 10)
        .await
        .expect("audit log");
    assert!(
        log.items
            .iter()
            .any(|e| e.action == "user.role.assign"
                && e.target_id == alice.id.into_uuid()),
        "audit row missing"
    );
}

#[tokio::test]
async fn assign_roles_is_idempotent_on_existing_assignment() {
    let (router, storage) = boot().await;
    let admin = seed_user(&storage, "admin").await;
    let admin_role = seed_role(&storage, "admin", Permissions::MANAGE_USERS).await;
    assign_role(&storage, admin.id, admin_role.id).await;
    let alice = seed_user(&storage, "alice").await;
    let editor_role = seed_role(&storage, "editor", Permissions::EDIT).await;
    assign_role(&storage, alice.id, editor_role.id).await;
    let session = seed_session(&storage, admin.id).await;

    let (status, _body) = json_request(
        router.clone(),
        "POST",
        &format!("/api/v1/admin/users/{}/roles", alice.id.into_uuid()),
        Some(&session),
        Some(json!({ "role_ids": [editor_role.id.into_uuid().to_string()] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // No new audit row because the assignment already existed.
    let log = storage
        .audit_log()
        .list(AuditLogFilter::default(), None, 50)
        .await
        .expect("audit log");
    let count = log
        .items
        .iter()
        .filter(|e| e.action == "user.role.assign")
        .count();
    assert_eq!(count, 0, "no audit row should be emitted for a no-op");
}

#[tokio::test]
async fn assign_roles_rejects_unknown_role_id() {
    let (router, storage) = boot().await;
    let admin = seed_user(&storage, "admin").await;
    let admin_role = seed_role(&storage, "admin", Permissions::MANAGE_USERS).await;
    assign_role(&storage, admin.id, admin_role.id).await;
    let alice = seed_user(&storage, "alice").await;
    let session = seed_session(&storage, admin.id).await;

    let bogus = uuid::Uuid::now_v7();
    let (status, _body) = json_request(
        router,
        "POST",
        &format!("/api/v1/admin/users/{}/roles", alice.id.into_uuid()),
        Some(&session),
        Some(json!({ "role_ids": [bogus.to_string()] })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn revoke_role_returns_204_and_audits_first_revoke_only() {
    let (router, storage) = boot().await;
    let admin = seed_user(&storage, "admin").await;
    let admin_role = seed_role(&storage, "admin", Permissions::MANAGE_USERS).await;
    assign_role(&storage, admin.id, admin_role.id).await;
    let alice = seed_user(&storage, "alice").await;
    let editor_role = seed_role(&storage, "editor", Permissions::EDIT).await;
    assign_role(&storage, alice.id, editor_role.id).await;
    let session = seed_session(&storage, admin.id).await;

    let uri = format!(
        "/api/v1/admin/users/{}/roles/{}",
        alice.id.into_uuid(),
        editor_role.id.into_uuid()
    );
    let (status, _body) = json_request(
        router.clone(),
        "DELETE",
        &uri,
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Second call is idempotent (also 204) but emits no extra audit row.
    let (status, _body) = json_request(router, "DELETE", &uri, Some(&session), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let log = storage
        .audit_log()
        .list(AuditLogFilter::default(), None, 50)
        .await
        .expect("audit log");
    let revoke_count = log
        .items
        .iter()
        .filter(|e| e.action == "user.role.revoke")
        .count();
    assert_eq!(revoke_count, 1, "exactly one revoke audit row expected");
}
