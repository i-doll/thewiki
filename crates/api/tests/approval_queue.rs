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
    AuditLogFilter, AuditLogRepository, NamespaceRepository, PendingRevisionFilter,
    PendingRevisionRepository, RevisionRepository, RoleRepository, SessionRepository,
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

// ─── Self-review guard ────────────────────────────────────────────────────

#[tokio::test]
async fn reviewer_cannot_approve_own_edit() {
    let (router, storage) = boot_with(ApprovalScope::All, false).await;

    // One user who is both author and reviewer (REVIEW_EDITS + EDIT).
    let dual = seed_user(&storage, "dual").await;
    seed_role_for(
        &storage,
        dual.id,
        "dual",
        Permissions::EDIT | Permissions::REVIEW_EDITS,
    )
    .await;
    let session = seed_session(&storage, dual.id).await;

    // Queue an edit as `dual`.
    let (status, body) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        Some(&session),
        Some(json!({
            "slug": "selfie",
            "title": "Selfie",
            "content": "draft",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {body}");
    let pending_id = body["pending_revision_id"]
        .as_str()
        .expect("pending_revision_id")
        .to_string();

    // Try to approve own row → 403.
    let (status, body) = json_request(
        router,
        "POST",
        &format!("/api/v1/pending-revisions/{pending_id}/approve"),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["code"], "forbidden");
}

#[tokio::test]
async fn reviewer_cannot_reject_own_edit() {
    let (router, storage) = boot_with(ApprovalScope::All, false).await;

    let dual = seed_user(&storage, "dual").await;
    seed_role_for(
        &storage,
        dual.id,
        "dual",
        Permissions::EDIT | Permissions::REVIEW_EDITS,
    )
    .await;
    let session = seed_session(&storage, dual.id).await;

    let (status, body) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        Some(&session),
        Some(json!({
            "slug": "selfie",
            "title": "Selfie",
            "content": "draft",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {body}");
    let pending_id = body["pending_revision_id"]
        .as_str()
        .expect("pending_revision_id")
        .to_string();

    let (status, body) = json_request(
        router,
        "POST",
        &format!("/api/v1/pending-revisions/{pending_id}/reject"),
        Some(&session),
        Some(json!({"reason": "nope"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["code"], "forbidden");
}

// ─── Partial-failure / retry safety ───────────────────────────────────────

#[tokio::test]
async fn approve_flips_status_first_so_retries_cannot_duplicate_revisions() {
    // Regression for the duplicate-revision race: if the live revision
    // were committed before the pending row is flipped, a transient
    // failure on the flip would let a retry create a SECOND revision.
    //
    // We verify the ordering indirectly: after a successful approve, the
    // pending row is `approved` AND only one revision exists for the
    // page. We then simulate the "transient failure leaves an approved
    // row" recoverable state by manually flipping a fresh row via the
    // repo and confirming a retry hits 409 (no duplicate revision can
    // be created).
    let (router, storage) = boot_with(ApprovalScope::Anonymous, true).await;

    // 1. Queue + approve as normal — should land exactly one revision.
    let (_, body) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        None,
        Some(json!({
            "slug": "once",
            "title": "Once",
            "content": "first",
        })),
    )
    .await;
    let pending_id_str = body["pending_revision_id"]
        .as_str()
        .expect("pending_revision_id")
        .to_string();

    let reviewer = seed_user(&storage, "rev").await;
    seed_role_for(&storage, reviewer.id, "rev", Permissions::REVIEW_EDITS).await;
    let session = seed_session(&storage, reviewer.id).await;

    let (status, body) = json_request(
        router.clone(),
        "POST",
        &format!("/api/v1/pending-revisions/{pending_id_str}/approve"),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let page_id_str = body["page_id"].as_str().expect("page_id").to_string();
    let page_id = thewiki_core::PageId::from_uuid(
        uuid::Uuid::parse_str(&page_id_str).expect("page id"),
    );

    let slice = storage
        .revisions()
        .list_for_page(page_id, None, 50)
        .await
        .expect("list revisions");
    assert_eq!(
        slice.items.len(),
        1,
        "approve should produce exactly one live revision, got {}",
        slice.items.len()
    );

    // 2. A retry on the approved row hits the conflict guard — no extra
    //    revision is created.
    let (status, _) = json_request(
        router.clone(),
        "POST",
        &format!("/api/v1/pending-revisions/{pending_id_str}/approve"),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    let slice = storage
        .revisions()
        .list_for_page(page_id, None, 50)
        .await
        .expect("list revisions");
    assert_eq!(
        slice.items.len(),
        1,
        "retry must not duplicate the revision"
    );

    // 3. Simulate the partial-failure recoverable state directly: queue a
    //    second edit, flip its pending row to `approved` via the repo
    //    (mimicking "CAS succeeded, commit_page_audit then failed"), and
    //    confirm a retry through the HTTP layer can't manufacture a
    //    duplicate revision — it returns 409.
    let (_, body) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        None,
        Some(json!({
            "slug": "twice",
            "title": "Twice",
            "content": "draft",
        })),
    )
    .await;
    let pending_id_str2 = body["pending_revision_id"]
        .as_str()
        .expect("pending_revision_id")
        .to_string();
    let pending_id2 = thewiki_core::PendingRevisionId::from_uuid(
        uuid::Uuid::parse_str(&pending_id_str2).expect("uuid"),
    );

    // Flip directly via the repo to mimic the "approved row, no
    // revision committed yet" recoverable state.
    let _ = storage
        .pending_revisions()
        .approve(pending_id2, reviewer.id, OffsetDateTime::now_utc())
        .await
        .expect("manual flip");

    // The HTTP retry now hits the conflict guard immediately — the live
    // revision count for the `twice` page remains 0 (no duplicate).
    let (status, _) = json_request(
        router.clone(),
        "POST",
        &format!("/api/v1/pending-revisions/{pending_id_str2}/approve"),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // No revision should exist for the second page (proves the retry
    // didn't silently manufacture one).
    let pending_row = storage
        .pending_revisions()
        .get_by_id(pending_id2)
        .await
        .expect("read row");
    let target_page_id = pending_row.page_id;
    let slice = storage
        .revisions()
        .list_for_page(target_page_id, None, 50)
        .await
        .expect("list revisions");
    assert!(
        slice.items.is_empty(),
        "no revisions should have been created for the partially-failed approval"
    );

    // Total pending count remains zero (both rows are decided).
    let pending_total = storage
        .pending_revisions()
        .count(PendingRevisionFilter {
            status: Some(thewiki_core::pending_revision::PendingRevisionStatus::Pending),
        })
        .await
        .expect("count");
    assert_eq!(pending_total, 0);
}

// ─── Head drift surfacing ─────────────────────────────────────────────────

#[tokio::test]
async fn detail_flags_head_moved_when_another_edit_lands_after_queue() {
    let (router, storage) = boot_with(ApprovalScope::Anonymous, true).await;

    // 1. Authenticated editor lands an initial revision on the page so
    //    a queued anonymous edit has a parent. The page is created live
    //    because the author is authenticated and the scope is Anonymous.
    let editor = seed_user(&storage, "editor").await;
    seed_role_for(&storage, editor.id, "editor", Permissions::EDIT).await;
    let editor_session = seed_session(&storage, editor.id).await;
    let (status, body) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        Some(&editor_session),
        Some(json!({
            "slug": "drift",
            "title": "Drift",
            "content": "v1",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    // 2. An anonymous edit lands in the queue against v1.
    let (status, body) = json_request(
        router.clone(),
        "PUT",
        "/api/v1/pages/drift",
        None,
        Some(json!({
            "title": "Drift",
            "content": "v2-proposed",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {body}");
    let pending_id = body["pending_revision_id"]
        .as_str()
        .expect("pending_revision_id")
        .to_string();

    // 3. The editor pushes a second live revision — head moves from v1
    //    to v3 while the proposal still references v1 as its parent.
    let (status, _) = json_request(
        router.clone(),
        "PUT",
        "/api/v1/pages/drift",
        Some(&editor_session),
        Some(json!({
            "title": "Drift",
            "content": "v3-live",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 4. Reviewer fetches the detail — head_moved_since_proposal is true
    //    and head_body is the latest live body.
    let reviewer = seed_user(&storage, "rev").await;
    seed_role_for(&storage, reviewer.id, "rev", Permissions::REVIEW_EDITS).await;
    let session = seed_session(&storage, reviewer.id).await;
    let (status, body) = json_request(
        router,
        "GET",
        &format!("/api/v1/pending-revisions/{pending_id}"),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["head_moved_since_proposal"], true, "body: {body}");
    assert_eq!(body["head_body"], "v3-live", "body: {body}");
    assert_eq!(body["parent_body"], "v1", "body: {body}");
}

#[tokio::test]
async fn detail_head_not_moved_when_no_concurrent_edit() {
    let (router, storage) = boot_with(ApprovalScope::Anonymous, true).await;

    // Authenticated editor seeds v1.
    let editor = seed_user(&storage, "editor").await;
    seed_role_for(&storage, editor.id, "editor", Permissions::EDIT).await;
    let editor_session = seed_session(&storage, editor.id).await;
    let (status, _) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        Some(&editor_session),
        Some(json!({
            "slug": "stable",
            "title": "Stable",
            "content": "v1",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // Anonymous queues v2 — no further edits before the reviewer looks.
    let (status, body) = json_request(
        router.clone(),
        "PUT",
        "/api/v1/pages/stable",
        None,
        Some(json!({
            "title": "Stable",
            "content": "v2-proposed",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let pending_id = body["pending_revision_id"]
        .as_str()
        .expect("pending_revision_id")
        .to_string();

    let reviewer = seed_user(&storage, "rev").await;
    seed_role_for(&storage, reviewer.id, "rev", Permissions::REVIEW_EDITS).await;
    let session = seed_session(&storage, reviewer.id).await;
    let (status, body) = json_request(
        router,
        "GET",
        &format!("/api/v1/pending-revisions/{pending_id}"),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["head_moved_since_proposal"], false, "body: {body}");
    assert_eq!(body["head_body"], "v1", "body: {body}");
}

// ─── Approve notification payload parity ──────────────────────────────────

#[tokio::test]
async fn approve_notification_payload_includes_reviewer_and_new_revision_id() {
    let (router, storage) = boot_with(ApprovalScope::All, false).await;

    let author = seed_user(&storage, "author").await;
    seed_role_for(&storage, author.id, "editor", Permissions::EDIT).await;
    let author_session = seed_session(&storage, author.id).await;

    let (_, body) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        Some(&author_session),
        Some(json!({
            "slug": "notif",
            "title": "Notif",
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

    let (_, body) = json_request(
        router,
        "GET",
        "/api/v1/notifications",
        Some(&author_session),
        None,
    )
    .await;
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 1, "{body}");
    assert_eq!(items[0]["kind"], "pending_revision_approved");
    assert_eq!(items[0]["payload"]["reviewer_username"], "rev", "{body}");
    assert!(
        items[0]["payload"]["new_revision_id"].is_string(),
        "new_revision_id should be a string, body: {body}"
    );
}
