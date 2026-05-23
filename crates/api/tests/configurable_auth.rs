//! Integration tests for the configurable-auth wiring (#14).
//!
//! Each test boots a fresh in-memory SQLite + the full pages router with a
//! specific `AuthConfig` and drives `POST /api/v1/pages` to verify the
//! anonymous-edit and approval-queue knobs. The four key combinations are:
//!
//! | anonymous_edits | approval_required_for | Expected behaviour                 |
//! |-----------------|-----------------------|------------------------------------|
//! | false           | None                  | anonymous → 401; auth → 201        |
//! | true            | None                  | anonymous → 201, author = Anonymous |
//! | true            | Anonymous             | anonymous → 202, pending logged    |
//! | false           | All                   | auth → 202, pending logged         |
//!
//! Plus a `GET /api/v1/auth/policy` round-trip that asserts the wire shape.
//!
//! Sessions are pre-seeded directly in the `sessions` table (bypassing the
//! login handler) so tests don't need to hash a password per run.

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
use thewiki_api::config::{ApprovalScope, Argon2Config, AuthConfig, Config, RegistrationPolicy};
use thewiki_core::{Namespace, NamespaceId, NamespaceSlug, User, UserId, Username};
use thewiki_storage::repo::{
    NamespaceRepository, PageRepository, RevisionRepository, SessionRepository, UserRepository,
};
use thewiki_storage::sqlite::{SqliteOptions, SqliteStorage};
use time::OffsetDateTime;
use tower::ServiceExt;

// ─── Fixture ──────────────────────────────────────────────────────────────

fn disabled_rate_limit() -> thewiki_api::config::RateLimitConfig {
    let mut cfg = Config::defaults().rate_limit;
    cfg.enabled = false;
    cfg
}

/// Test-friendly Argon2 parameters at the OWASP floor so test startup stays
/// fast. The hasher is instantiated but never actually used (sessions are
/// pre-seeded directly), so the cost is paid only once at construction.
fn test_argon2() -> Argon2Config {
    Argon2Config {
        memory_kib: 19_456,
        iterations: 2,
        parallelism: 1,
    }
}

/// Build a router parameterised by an [`AuthConfig`]. Seeds the `Main`
/// namespace and a `tester` user (no password set; sessions are seeded
/// directly).
async fn app_with_auth(auth_cfg: AuthConfig) -> (Router, UserId, SqliteStorage) {
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

    let namespace = Namespace {
        id: NamespaceId::new(),
        slug: NamespaceSlug::new("Main").expect("valid slug"),
        display_name: "Main".into(),
        is_talk: false,
        paired_namespace_id: None,
    };
    storage
        .namespaces()
        .create(&namespace)
        .await
        .expect("seed Main namespace");

    let user = User {
        id: UserId::new(),
        username: Username::new("tester").expect("valid username"),
        email: None,
        display_name: Some("Tester".into()),
        // Set well in the past so the NewUsers approval scope (24h window)
        // treats this user as established.
        created_at: OffsetDateTime::now_utc() - time::Duration::days(30),
        last_login_at: None,
    };
    storage
        .users()
        .create(&user, None)
        .await
        .expect("seed user");

    let hasher = Arc::new(Argon2Hasher::new(test_argon2()).expect("hasher"));
    let auth_state = AuthState::new(
        storage.clone(),
        hasher,
        Duration::from_secs(60 * 60),
        false,
        auth_cfg.clone(),
    );
    let state = AppState::new(storage.clone(), auth_cfg).with_auth_state(auth_state.clone());
    // We need both the pages router (for the configurable-auth tests) and the
    // auth router (for the /policy endpoint). `build_full` mounts everything
    // behind the production stack, which is what we want here.
    let router = app::build_full(state, auth_state, false, disabled_rate_limit());

    (router, user.id, storage)
}

/// Pre-seed a session for `user_id` and return the cookie value.
async fn seed_session(storage: &SqliteStorage, user_id: UserId) -> String {
    let session = storage
        .sessions()
        .create(user_id, Duration::from_secs(60 * 60), None, None)
        .await
        .expect("seed session");
    session.id.into_uuid().to_string()
}

/// A fixed CSRF token used by every authenticated request in this suite.
/// We pass it as both the cookie value and the matching `X-CSRF-Token`
/// header so the double-submit middleware lets the request through.
const TEST_CSRF: &str = "test-csrf-token-fixed-value-32b";

/// Drive a request through the router, returning the status and parsed body.
async fn json_request(
    router: Router,
    method: &str,
    uri: &str,
    session_cookie: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(cookie) = session_cookie {
        // Authenticated mutating requests must satisfy the double-submit
        // CSRF check (`thewiki_csrf` cookie equals the `X-CSRF-Token`
        // header). Pass both to slip past the middleware.
        builder = builder
            .header(
                header::COOKIE,
                format!("thewiki_session={cookie}; thewiki_csrf={TEST_CSRF}"),
            )
            .header("x-csrf-token", TEST_CSRF);
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

/// Common request body for the create-page endpoint.
fn create_body(slug: &str) -> Value {
    json!({
        "namespace_slug": "Main",
        "slug": slug,
        "title": slug,
        "content": format!("body of {slug}"),
    })
}

fn cfg(anonymous: bool, scope: ApprovalScope) -> AuthConfig {
    let mut cfg = Config::defaults().auth;
    cfg.anonymous_edits = anonymous;
    cfg.approval_required_for = scope;
    cfg
}

// ─── Matrix tests ─────────────────────────────────────────────────────────

#[tokio::test]
async fn anonymous_disabled_no_approval_anonymous_post_returns_401() {
    let (router, _, _) = app_with_auth(cfg(false, ApprovalScope::None)).await;
    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/pages",
        None,
        Some(create_body("p1")),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_eq!(body["code"], "unauthenticated");
}

#[tokio::test]
async fn anonymous_disabled_no_approval_auth_post_creates_immediately() {
    let (router, user_id, storage) = app_with_auth(cfg(false, ApprovalScope::None)).await;
    let session = seed_session(&storage, user_id).await;

    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/pages",
        Some(&session),
        Some(create_body("p1")),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert!(body["current_revision_id"].is_string());
    assert_eq!(body["slug"], "p1");
}

#[tokio::test]
async fn anonymous_enabled_no_approval_anonymous_post_creates_as_anonymous() {
    let (router, _, storage) = app_with_auth(cfg(true, ApprovalScope::None)).await;
    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/pages",
        None,
        Some(create_body("p1")),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    // The current revision should be authored by the lazily-provisioned
    // Anonymous user. We assert against storage directly because the wire
    // body doesn't include the author id today.
    let rev_id_str = body["current_revision_id"].as_str().expect("rev id");
    let rev_id =
        thewiki_core::RevisionId::from_uuid(uuid::Uuid::parse_str(rev_id_str).expect("uuid"));
    let rev = storage
        .revisions()
        .get_by_id(rev_id)
        .await
        .expect("fetch revision");
    let author = storage
        .users()
        .get_by_id(rev.author_id)
        .await
        .expect("fetch author");
    assert_eq!(
        author.username.as_str(),
        "Anonymous",
        "anonymous edit should be credited to the singleton Anonymous user"
    );
}

#[tokio::test]
async fn anonymous_enabled_approval_anonymous_post_returns_202_and_stays_pending() {
    let (router, _, storage) = app_with_auth(cfg(true, ApprovalScope::Anonymous)).await;
    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/pages",
        None,
        Some(create_body("p1")),
    )
    .await;
    // 202 Accepted: the request was understood but the page hasn't gone
    // live — the (stubbed) approval queue owns it now.
    assert_eq!(status, StatusCode::ACCEPTED, "body: {body}");
    // The page row exists (so the slug is reserved) but has no current head.
    assert!(
        body["current_revision_id"].is_null(),
        "page should have no head until approval lands; body: {body}"
    );
    // No revisions should have been written to live storage either — the
    // approval-queue stub is a no-op for now (TODO #40).
    let namespace = storage
        .namespaces()
        .get_by_slug(&NamespaceSlug::new("Main").expect("slug"))
        .await
        .expect("namespace");
    let page = storage
        .pages()
        .get_by_namespace_and_slug(namespace.id, "p1")
        .await
        .expect("page");
    let history = storage
        .revisions()
        .list_for_page(page.id, None, 10)
        .await
        .expect("history");
    assert!(
        history.items.is_empty(),
        "approval-queue stub should not persist the revision yet (TODO #40), got: {:?}",
        history.items
    );
}

#[tokio::test]
async fn anonymous_disabled_approval_all_auth_post_returns_202() {
    let (router, user_id, storage) = app_with_auth(cfg(false, ApprovalScope::All)).await;
    let session = seed_session(&storage, user_id).await;

    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/pages",
        Some(&session),
        Some(create_body("p1")),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {body}");
    assert!(
        body["current_revision_id"].is_null(),
        "ApprovalScope::All gates even authenticated edits; head should stay None"
    );
}

// ─── Policy endpoint ──────────────────────────────────────────────────────

#[tokio::test]
async fn policy_endpoint_reports_closed_defaults() {
    let (router, _, _) = app_with_auth(Config::defaults().auth).await;
    let (status, body) = json_request(router, "GET", "/api/v1/auth/policy", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["registration"], "closed");
    assert_eq!(body["anonymous_edits"], false);
    assert_eq!(body["approval_required_for"], "none");
}

#[tokio::test]
async fn policy_endpoint_reports_open_registration() {
    let mut cfg = Config::defaults().auth;
    cfg.registration = RegistrationPolicy::Open;
    cfg.anonymous_edits = true;
    cfg.approval_required_for = ApprovalScope::Anonymous;

    let (router, _, _) = app_with_auth(cfg).await;
    let (status, body) = json_request(router, "GET", "/api/v1/auth/policy", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["registration"], "open");
    assert_eq!(body["anonymous_edits"], true);
    assert_eq!(body["approval_required_for"], "anonymous");
}

#[tokio::test]
async fn policy_endpoint_reports_invite_registration() {
    let mut cfg = Config::defaults().auth;
    cfg.registration = RegistrationPolicy::Invite;
    cfg.approval_required_for = ApprovalScope::NewUsers;

    let (router, _, _) = app_with_auth(cfg).await;
    let (status, body) = json_request(router, "GET", "/api/v1/auth/policy", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["registration"], "invite");
    assert_eq!(body["approval_required_for"], "new-users");
}

#[tokio::test]
async fn policy_endpoint_reports_approval_all() {
    let mut cfg = Config::defaults().auth;
    cfg.approval_required_for = ApprovalScope::All;

    let (router, _, _) = app_with_auth(cfg).await;
    let (status, body) = json_request(router, "GET", "/api/v1/auth/policy", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["approval_required_for"], "all");
}

// ─── Approval scope subtleties ────────────────────────────────────────────

#[tokio::test]
async fn new_users_scope_lets_established_authors_through() {
    // tester is created 30 days ago — outside the 24h NewUsers window.
    let (router, user_id, storage) = app_with_auth(cfg(false, ApprovalScope::NewUsers)).await;
    let session = seed_session(&storage, user_id).await;

    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/pages",
        Some(&session),
        Some(create_body("p1")),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert!(body["current_revision_id"].is_string());
}

#[tokio::test]
async fn new_users_scope_gates_fresh_accounts() {
    let cfg = cfg(false, ApprovalScope::NewUsers);
    let (router, _, storage) = app_with_auth(cfg).await;

    // Seed a brand-new user so they fall inside the 24h window.
    let new_user = User {
        id: UserId::new(),
        username: Username::new("newbie").expect("valid username"),
        email: None,
        display_name: None,
        created_at: OffsetDateTime::now_utc(),
        last_login_at: None,
    };
    storage
        .users()
        .create(&new_user, None)
        .await
        .expect("seed user");
    let session = seed_session(&storage, new_user.id).await;

    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/pages",
        Some(&session),
        Some(create_body("p2")),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {body}");
}
