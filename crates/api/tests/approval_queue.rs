//! Integration tests for the edit approval queue + in-app inbox (#40).
//!
//! Coverage:
//!
//! - Anonymous edit under `anonymous` approval scope queues a row that
//!   shows up on `GET /api/v1/pending-revisions`.
//! - Reviewer approving the row promotes it to a real revision against
//!   the page, drops the queue count, writes an audit row, and (for
//!   authenticated authors) delivers an inbox notification.
//! - Reviewer rejecting records the reason, the audit row, and the
//!   inbox notification. Anonymous authors get nothing.
//! - Non-reviewer callers get 403; unauthenticated callers get 401.
//! - Double-decide on an already-decided row returns 409.
//! - Inbox `POST /{id}/read` flips `read_at` and the unread counter.
//!
//! Each test boots a fresh in-memory SQLite, seeds `Main`, and stands
//! up the auth stack so session cookies resolve.

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
use thewiki_api::config::{
    ApprovalRequirement, ApprovalScope, Argon2Config, AuthConfig, Config, ModerationConfig,
};
use thewiki_core::{EmailAddress, Permissions, Role, RoleId, RoleName, User, UserId, Username};
use thewiki_storage::repo::{
    AuditLogFilter, AuditLogRepository, NamespaceRepository, RoleRepository, SessionRepository,
    UserRepository,
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

fn auth_cfg(scope: ApprovalScope, anon: bool) -> AuthConfig {
    let mut cfg = Config::defaults().auth;
    cfg.anonymous_edits = anon;
    cfg.approval_required_for = scope;
    cfg
}

async fn boot_with(
    scope: ApprovalScope,
    anonymous_edits: bool,
) -> (Router, SqliteStorage) {
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
        .expect("seed default namespace");

    let cfg = auth_cfg(scope, anonymous_edits);
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
        .with_moderation_config(ModerationConfig::default());
    let router = app::build_with_state_with_rate_limit(state, disabled_rate_limit());

    (router, storage)
}

async fn seed_user(storage: &SqliteStorage, username: &str) -> User {
    let user = User {
        id: UserId::new(),
        username: Username::new(username).expect("valid username"),
        email: Some(
            EmailAddress::new(format!("{username}@example.com")).expect("valid email"),
        ),
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

// ─── Tests ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn anonymous_post_under_approval_scope_creates_pending_row() {
    let (router, _storage) = boot_with(ApprovalScope::Anonymous, true).await;

    // Anonymous create — no session cookie.
    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/pages",
        None,
        Some(json!({
            "slug": "queued",
            "title": "Queued",
            "content": "draft body",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {body}");
    assert_eq!(body["queued"], true, "body: {body}");
    assert!(body["pending_revision_id"].is_string(), "body: {body}");
    assert_eq!(body["queue_position"], 1, "body: {body}");
}

#[tokio::test]
async fn reviewer_can_list_pending_and_approve() {
    let (router, storage) = boot_with(ApprovalScope::Anonymous, true).await;

    // 1. Anonymous edit lands in the queue.
    let (status, body) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        None,
        Some(json!({
            "slug": "p1",
            "title": "P1",
            "content": "hello world",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {body}");
    let pending_id = body["pending_revision_id"]
        .as_str()
        .expect("pending_revision_id")
        .to_string();

    // 2. Reviewer (REVIEW_EDITS) lists the queue.
    let reviewer = seed_user(&storage, "rev").await;
    seed_role_for(&storage, reviewer.id, "rev", Permissions::REVIEW_EDITS).await;
    let session = seed_session(&storage, reviewer.id).await;
    let (status, body) = json_request(
        router.clone(),
        "GET",
        "/api/v1/pending-revisions",
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 1, "expected one pending row: {body}");
    assert_eq!(items[0]["id"], pending_id);
    assert_eq!(items[0]["status"], "pending");
    assert_eq!(items[0]["page_slug"], "p1");
    assert_eq!(body["total"], 1);

    // 3. Reviewer fetches the detail view (with parent_body = null).
    let (status, body) = json_request(
        router.clone(),
        "GET",
        &format!("/api/v1/pending-revisions/{pending_id}"),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["body"], "hello world");
    assert!(body["parent_body"].is_null(), "body: {body}");

    // 4. Approve. The page row now has a current revision and the queue
    //    drops to zero.
    let (status, body) = json_request(
        router.clone(),
        "POST",
        &format!("/api/v1/pending-revisions/{pending_id}/approve"),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["status"], "approved");

    let (status, body) = json_request(
        router.clone(),
        "GET",
        "/api/v1/pending-revisions",
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["total"], 0);

    // 5. Page now has a head revision.
    let (status, body) = json_request(router.clone(), "GET", "/api/v1/pages/p1", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body["current_revision_id"].is_string(), "body: {body}");
    assert_eq!(body["content"], "hello world");

    // 6. Audit log carries a pending_revision.approve row.
    let audit = storage
        .audit_log()
        .list(AuditLogFilter::default(), None, 50)
        .await
        .expect("audit list");
    let approve_row = audit
        .items
        .iter()
        .find(|e| e.action == "pending_revision.approve");
    assert!(
        approve_row.is_some(),
        "expected an approve audit row, got: {:?}",
        audit.items.iter().map(|e| &e.action).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn reviewer_can_reject_and_authenticated_author_gets_inbox_row() {
    let (router, storage) = boot_with(ApprovalScope::All, false).await;

    // Authenticated author with EDIT but no REVIEW_EDITS.
    let author = seed_user(&storage, "author").await;
    seed_role_for(&storage, author.id, "editor", Permissions::EDIT).await;
    let author_session = seed_session(&storage, author.id).await;

    // The author creates a page — ApprovalScope::All gates it.
    let (status, body) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        Some(&author_session),
        Some(json!({
            "slug": "needs-review",
            "title": "Needs review",
            "content": "first draft",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {body}");
    let pending_id = body["pending_revision_id"]
        .as_str()
        .expect("pending_revision_id")
        .to_string();

    // Reviewer rejects with a reason.
    let reviewer = seed_user(&storage, "rev").await;
    seed_role_for(&storage, reviewer.id, "rev", Permissions::REVIEW_EDITS).await;
    let reviewer_session = seed_session(&storage, reviewer.id).await;
    let (status, body) = json_request(
        router.clone(),
        "POST",
        &format!("/api/v1/pending-revisions/{pending_id}/reject"),
        Some(&reviewer_session),
        Some(json!({"reason": "spam"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["status"], "rejected");
    assert_eq!(body["rejection_reason"], "spam");

    // Author's inbox shows the rejection.
    let (status, body) = json_request(
        router.clone(),
        "GET",
        "/api/v1/notifications",
        Some(&author_session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["unread"], 1);
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 1, "{body}");
    assert_eq!(items[0]["kind"], "pending_revision_rejected");
    assert_eq!(items[0]["payload"]["reason"], "spam");

    // Audit row carries the reject action.
    let audit = storage
        .audit_log()
        .list(AuditLogFilter::default(), None, 50)
        .await
        .expect("audit list");
    let reject_row = audit
        .items
        .iter()
        .find(|e| e.action == "pending_revision.reject");
    assert!(
        reject_row.is_some(),
        "expected a reject audit row, got: {:?}",
        audit.items.iter().map(|e| &e.action).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn non_reviewer_gets_403() {
    let (router, storage) = boot_with(ApprovalScope::None, true).await;
    let plain = seed_user(&storage, "plain").await;
    let session = seed_session(&storage, plain.id).await;
    let (status, body) = json_request(
        router,
        "GET",
        "/api/v1/pending-revisions",
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["code"], "forbidden");
}

#[tokio::test]
async fn anonymous_caller_gets_401_on_review_endpoints() {
    let (router, _) = boot_with(ApprovalScope::None, true).await;
    let (status, body) =
        json_request(router, "GET", "/api/v1/pending-revisions", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_eq!(body["code"], "unauthenticated");
}

#[tokio::test]
async fn approving_already_decided_row_returns_409() {
    let (router, storage) = boot_with(ApprovalScope::Anonymous, true).await;
    let (_, body) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        None,
        Some(json!({
            "slug": "p1",
            "title": "P1",
            "content": "body",
        })),
    )
    .await;
    let pending_id = body["pending_revision_id"]
        .as_str()
        .expect("pending_revision_id")
        .to_string();

    let reviewer = seed_user(&storage, "rev").await;
    seed_role_for(&storage, reviewer.id, "rev", Permissions::REVIEW_EDITS).await;
    let session = seed_session(&storage, reviewer.id).await;

    // First approve succeeds.
    let (status, _) = json_request(
        router.clone(),
        "POST",
        &format!("/api/v1/pending-revisions/{pending_id}/approve"),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Second approve hits the conflict guard.
    let (status, body) = json_request(
        router,
        "POST",
        &format!("/api/v1/pending-revisions/{pending_id}/approve"),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
}

#[tokio::test]
async fn reject_requires_non_empty_reason() {
    let (router, storage) = boot_with(ApprovalScope::Anonymous, true).await;
    let (_, body) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        None,
        Some(json!({
            "slug": "p1",
            "title": "P1",
            "content": "body",
        })),
    )
    .await;
    let pending_id = body["pending_revision_id"]
        .as_str()
        .expect("pending_revision_id")
        .to_string();

    let reviewer = seed_user(&storage, "rev").await;
    seed_role_for(&storage, reviewer.id, "rev", Permissions::REVIEW_EDITS).await;
    let session = seed_session(&storage, reviewer.id).await;

    let (status, body) = json_request(
        router,
        "POST",
        &format!("/api/v1/pending-revisions/{pending_id}/reject"),
        Some(&session),
        Some(json!({"reason": "   "})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
}

#[tokio::test]
async fn mark_notification_read_clears_unread_counter() {
    let (router, storage) = boot_with(ApprovalScope::All, false).await;

    let author = seed_user(&storage, "author").await;
    seed_role_for(&storage, author.id, "editor", Permissions::EDIT).await;
    let author_session = seed_session(&storage, author.id).await;

    // Author submits, ApprovalScope::All queues it.
    let (_, body) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        Some(&author_session),
        Some(json!({
            "slug": "p1",
            "title": "P1",
            "content": "hi",
        })),
    )
    .await;
    let pending_id = body["pending_revision_id"]
        .as_str()
        .expect("pending_revision_id")
        .to_string();

    // Reviewer approves — triggers the notification.
    let reviewer = seed_user(&storage, "rev").await;
    seed_role_for(&storage, reviewer.id, "rev", Permissions::REVIEW_EDITS).await;
    let reviewer_session = seed_session(&storage, reviewer.id).await;
    let (status, _) = json_request(
        router.clone(),
        "POST",
        &format!("/api/v1/pending-revisions/{pending_id}/approve"),
        Some(&reviewer_session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // List should show 1 unread.
    let (_, body) = json_request(
        router.clone(),
        "GET",
        "/api/v1/notifications",
        Some(&author_session),
        None,
    )
    .await;
    assert_eq!(body["unread"], 1);
    let notif_id = body["items"][0]["id"]
        .as_str()
        .expect("notif id")
        .to_string();

    // Mark read.
    let (status, body) = json_request(
        router.clone(),
        "POST",
        &format!("/api/v1/notifications/{notif_id}/read"),
        Some(&author_session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body["read_at"].is_string(), "body: {body}");

    // Counter drops to 0.
    let (_, body) = json_request(
        router,
        "GET",
        "/api/v1/notifications",
        Some(&author_session),
        None,
    )
    .await;
    assert_eq!(body["unread"], 0);
}

#[tokio::test]
async fn moderation_config_takes_precedence_over_legacy_field() {
    // Build with the legacy field set to None, the modern moderation
    // section set to Anon — the resulting effective scope must queue
    // anonymous edits.
    let storage = SqliteStorage::new(
        "sqlite::memory:",
        SqliteOptions {
            max_connections: 1,
            acquire_timeout: Duration::from_secs(5),
            foreign_keys: true,
        },
    )
    .await
    .expect("sqlite");
    storage
        .namespaces()
        .get_or_create_default()
        .await
        .expect("seed");

    let auth = auth_cfg(ApprovalScope::None, true);
    let hasher = Arc::new(Argon2Hasher::new(test_argon2()).expect("hasher"));
    let auth_state = AuthState::new(
        storage.clone(),
        hasher,
        Duration::from_secs(60 * 60),
        false,
        auth.clone(),
    );
    let moderation = ModerationConfig {
        approval: thewiki_api::config::ApprovalConfig {
            require_approval_for: ApprovalRequirement::Anon,
            new_user_threshold_days: 7,
        },
    };
    let state = AppState::new(storage.clone(), auth)
        .with_auth_state(auth_state)
        .with_moderation_config(moderation);
    let router = app::build_with_state_with_rate_limit(state, disabled_rate_limit());

    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/pages",
        None,
        Some(json!({
            "slug": "p1",
            "title": "P1",
            "content": "body",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {body}");
    assert_eq!(body["queued"], true);
}
