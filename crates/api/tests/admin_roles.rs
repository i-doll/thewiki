//! Integration tests for the admin role endpoints (#47).

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

#[tokio::test]
async fn list_roles_requires_manage_roles() {
    let (router, storage) = boot().await;
    let viewer = seed_user(&storage, "viewer").await;
    let session = seed_session(&storage, viewer.id).await;
    let (status, _body) =
        json_request(router, "GET", "/api/v1/admin/roles", Some(&session), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn list_roles_returns_existing_with_assigned_count() {
    let (router, storage) = boot().await;
    let admin = seed_user(&storage, "admin").await;
    let admin_role = seed_role(&storage, "admin", Permissions::MANAGE_ROLES).await;
    assign_role(&storage, admin.id, admin_role.id).await;
    let editor_role = seed_role(&storage, "editor", Permissions::EDIT).await;
    let alice = seed_user(&storage, "alice").await;
    assign_role(&storage, alice.id, editor_role.id).await;
    let session = seed_session(&storage, admin.id).await;

    let (status, body) =
        json_request(router, "GET", "/api/v1/admin/roles", Some(&session), None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let items = body["items"].as_array().expect("items");
    let editor = items
        .iter()
        .find(|r| r["name"] == "editor")
        .expect("editor role visible");
    assert_eq!(editor["assigned_users"].as_u64(), Some(1));
    assert!(
        editor["permission_flags"]
            .as_array()
            .expect("flags")
            .iter()
            .any(|f| f == "EDIT")
    );
}

#[tokio::test]
async fn create_role_persists_and_audits() {
    let (router, storage) = boot().await;
    let admin = seed_user(&storage, "admin").await;
    let admin_role = seed_role(&storage, "admin", Permissions::MANAGE_ROLES).await;
    assign_role(&storage, admin.id, admin_role.id).await;
    let session = seed_session(&storage, admin.id).await;

    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/admin/roles",
        Some(&session),
        Some(json!({
            "name": "moderator",
            "display_name": "Moderator",
            "permissions": ["READ", "EDIT", "REVIEW_EDITS"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(body["name"], "moderator");
    let flags: Vec<&str> = body["permission_flags"]
        .as_array()
        .expect("flags array")
        .iter()
        .map(|v| v.as_str().expect("str"))
        .collect();
    assert!(flags.contains(&"READ"));
    assert!(flags.contains(&"REVIEW_EDITS"));

    // Audit row.
    let log = storage
        .audit_log()
        .list(AuditLogFilter::default(), None, 10)
        .await
        .expect("audit log");
    assert!(
        log.items.iter().any(|e| e.action == "role.create"),
        "role.create audit missing"
    );
}

#[tokio::test]
async fn create_role_rejects_unknown_permission_flag() {
    let (router, storage) = boot().await;
    let admin = seed_user(&storage, "admin").await;
    let admin_role = seed_role(&storage, "admin", Permissions::MANAGE_ROLES).await;
    assign_role(&storage, admin.id, admin_role.id).await;
    let session = seed_session(&storage, admin.id).await;

    let (status, _body) = json_request(
        router,
        "POST",
        "/api/v1/admin/roles",
        Some(&session),
        Some(json!({
            "name": "weird",
            "display_name": "Weird",
            "permissions": ["NOT_A_REAL_FLAG"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_role_rejects_duplicate_name() {
    let (router, storage) = boot().await;
    let admin = seed_user(&storage, "admin").await;
    let admin_role = seed_role(&storage, "admin", Permissions::MANAGE_ROLES).await;
    assign_role(&storage, admin.id, admin_role.id).await;
    seed_role(&storage, "moderator", Permissions::EDIT).await;
    let session = seed_session(&storage, admin.id).await;

    let (status, _body) = json_request(
        router,
        "POST",
        "/api/v1/admin/roles",
        Some(&session),
        Some(json!({
            "name": "moderator",
            "display_name": "Mod2",
            "permissions": [],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn update_role_replaces_permissions() {
    let (router, storage) = boot().await;
    let admin = seed_user(&storage, "admin").await;
    let admin_role = seed_role(&storage, "admin", Permissions::MANAGE_ROLES).await;
    assign_role(&storage, admin.id, admin_role.id).await;
    let editor_role = seed_role(&storage, "editor", Permissions::EDIT).await;
    let session = seed_session(&storage, admin.id).await;

    let (status, body) = json_request(
        router,
        "PUT",
        &format!("/api/v1/admin/roles/{}", editor_role.id.into_uuid()),
        Some(&session),
        Some(json!({
            "display_name": "Wiki Editor",
            "permissions": ["READ", "EDIT", "CREATE"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["display_name"], "Wiki Editor");
    let flags: Vec<&str> = body["permission_flags"]
        .as_array()
        .expect("flags")
        .iter()
        .map(|v| v.as_str().expect("str"))
        .collect();
    assert!(flags.contains(&"CREATE"));
}

#[tokio::test]
async fn delete_role_rejects_when_assigned() {
    let (router, storage) = boot().await;
    let admin = seed_user(&storage, "admin").await;
    let admin_role = seed_role(&storage, "admin", Permissions::MANAGE_ROLES).await;
    assign_role(&storage, admin.id, admin_role.id).await;
    let editor_role = seed_role(&storage, "editor", Permissions::EDIT).await;
    let alice = seed_user(&storage, "alice").await;
    assign_role(&storage, alice.id, editor_role.id).await;
    let session = seed_session(&storage, admin.id).await;

    let (status, body) = json_request(
        router,
        "DELETE",
        &format!("/api/v1/admin/roles/{}", editor_role.id.into_uuid()),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
}

#[tokio::test]
async fn delete_role_removes_unassigned() {
    let (router, storage) = boot().await;
    let admin = seed_user(&storage, "admin").await;
    let admin_role = seed_role(&storage, "admin", Permissions::MANAGE_ROLES).await;
    assign_role(&storage, admin.id, admin_role.id).await;
    let stale_role = seed_role(&storage, "stale", Permissions::READ).await;
    let session = seed_session(&storage, admin.id).await;

    let (status, _body) = json_request(
        router,
        "DELETE",
        &format!("/api/v1/admin/roles/{}", stale_role.id.into_uuid()),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    assert!(
        storage
            .roles()
            .get_by_id(stale_role.id)
            .await
            .is_err(),
        "role row should be gone"
    );

    let log = storage
        .audit_log()
        .list(AuditLogFilter::default(), None, 10)
        .await
        .expect("audit log");
    assert!(
        log.items.iter().any(|e| e.action == "role.delete"),
        "role.delete audit missing"
    );
}
