//! Integration tests for talk pages (#43).
//!
//! Coverage:
//!
//! - The default `Main` namespace boots with a paired `Talk_Main` partner.
//! - Creating a custom namespace auto-creates its `Talk_<slug>` partner and
//!   wires `paired_namespace_id` on both sides.
//! - `GET /api/v1/wiki/{ns}/{slug}/talk` returns 404 before the talk page
//!   exists, then 200 once it does.
//! - The page-read response embeds `_links.talk` for subject pages and not
//!   for talk-namespace pages.
//! - `~~~~` is expanded to `[[User:<name>]] <RFC 3339 stamp>` on talk pages
//!   but left as-is on subject pages.

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

#[tokio::test]
async fn default_main_namespace_has_paired_talk_partner() {
    let (router, _) = boot().await;
    let (status, body) = json_request(router, "GET", "/api/v1/namespaces", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let items = body["items"].as_array().expect("items");
    let slugs: Vec<&str> = items.iter().filter_map(|i| i["slug"].as_str()).collect();
    assert!(slugs.contains(&"Main"));
    assert!(slugs.contains(&"Talk_Main"));

    let main = items
        .iter()
        .find(|i| i["slug"] == "Main")
        .expect("Main namespace");
    let talk = items
        .iter()
        .find(|i| i["slug"] == "Talk_Main")
        .expect("Talk_Main namespace");
    assert_eq!(main["is_talk"], false);
    assert_eq!(talk["is_talk"], true);
    assert_eq!(main["paired_namespace_id"], talk["id"]);
    assert_eq!(talk["paired_namespace_id"], main["id"]);
}

#[tokio::test]
async fn create_namespace_auto_creates_talk_partner() {
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
        router.clone(),
        "POST",
        "/api/v1/namespaces",
        Some(&session),
        Some(json!({"slug": "Help", "display_name": "Help"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(body["slug"], "Help");
    assert_eq!(body["is_talk"], false);
    let help_id = body["id"].as_str().expect("Help id").to_owned();
    let talk_id = body["paired_namespace_id"]
        .as_str()
        .expect("Help paired id should be set after create")
        .to_owned();
    assert_ne!(help_id, talk_id);

    // The talk partner should be visible in the namespace list and point
    // back at the subject namespace.
    let (_, list) = json_request(router, "GET", "/api/v1/namespaces", None, None).await;
    let items = list["items"].as_array().expect("items");
    let talk_help = items
        .iter()
        .find(|i| i["slug"] == "Talk_Help")
        .expect("Talk_Help namespace");
    assert_eq!(talk_help["is_talk"], true);
    assert_eq!(talk_help["id"], talk_id);
    assert_eq!(talk_help["paired_namespace_id"], help_id);
    assert_eq!(talk_help["display_name"], "Talk: Help");
}

#[tokio::test]
async fn page_read_response_includes_talk_link_for_subject_pages() {
    let (router, _) = boot().await;
    let (status, _) = json_request(
        router.clone(),
        "POST",
        "/api/v1/wiki/Main",
        None,
        Some(json!({
            "slug": "foo",
            "title": "Foo",
            "content": "Hello",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = json_request(router, "GET", "/api/v1/wiki/Main/foo", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["is_talk"], false);
    assert_eq!(body["_links"]["talk"], "/api/v1/wiki/Main/foo/talk");
    assert_eq!(body["signature_convention"]["marker"], "~~~~");
}

#[tokio::test]
async fn talk_endpoint_404s_before_talk_page_created() {
    let (router, _) = boot().await;
    let (status, _) = json_request(
        router.clone(),
        "POST",
        "/api/v1/wiki/Main",
        None,
        Some(json!({
            "slug": "foo",
            "title": "Foo",
            "content": "Hello",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) =
        json_request(router, "GET", "/api/v1/wiki/Main/foo/talk", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
}

#[tokio::test]
async fn talk_endpoint_returns_talk_page_when_present() {
    let (router, storage) = boot().await;
    let user = seed_user(&storage, "discusser").await;
    let session = seed_session(&storage, user.id).await;

    // Subject page.
    let (status, _) = json_request(
        router.clone(),
        "POST",
        "/api/v1/wiki/Main",
        Some(&session),
        Some(json!({
            "slug": "physics",
            "title": "Physics",
            "content": "Physics is the study of matter and energy.",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // Talk page — same slug, talk namespace.
    let (status, _) = json_request(
        router.clone(),
        "POST",
        "/api/v1/wiki/Talk_Main",
        Some(&session),
        Some(json!({
            "slug": "physics",
            "title": "Talk: Physics",
            "content": "## Initial thread\n\nWhat about quantum?\n\n~~~~",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // The talk endpoint should now resolve.
    let (status, body) = json_request(
        router.clone(),
        "GET",
        "/api/v1/wiki/Main/physics/talk",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["namespace_slug"], "Talk_Main");
    assert_eq!(body["slug"], "physics");
    assert_eq!(body["is_talk"], true);
    // Signature was expanded on save — the literal `~~~~` should not appear.
    let content = body["content"].as_str().expect("content");
    assert!(
        !content.contains("~~~~"),
        "signature not expanded: {content}"
    );
    assert!(
        content.contains("[[User:discusser]]"),
        "user wikilink missing from expanded signature: {content}"
    );

    // Talk pages themselves have no `_links.talk` (no "talk of a talk").
    assert!(body["_links"]["talk"].is_null());
}

#[tokio::test]
async fn talk_endpoint_rejects_talk_namespace_input() {
    // `/api/v1/wiki/Talk_Main/foo/talk` is a malformed request — talk
    // pages don't have their own talk page. The handler returns 400.
    let (router, _) = boot().await;
    let (status, _) = json_request(
        router.clone(),
        "POST",
        "/api/v1/wiki/Talk_Main",
        None,
        Some(json!({
            "slug": "stub",
            "title": "Stub",
            "content": "x",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = json_request(
        router,
        "GET",
        "/api/v1/wiki/Talk_Main/stub/talk",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
}

#[tokio::test]
async fn signature_marker_is_not_expanded_on_subject_pages() {
    let (router, storage) = boot().await;
    let user = seed_user(&storage, "writer").await;
    let session = seed_session(&storage, user.id).await;
    let (status, _) = json_request(
        router.clone(),
        "POST",
        "/api/v1/wiki/Main",
        Some(&session),
        Some(json!({
            "slug": "signature-doc",
            "title": "Signature",
            "content": "Sign your edits with ~~~~",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (_, body) =
        json_request(router, "GET", "/api/v1/wiki/Main/signature-doc", None, None).await;
    let content = body["content"].as_str().expect("content");
    assert!(
        content.contains("~~~~"),
        "subject page should keep marker literal: {content}"
    );
}
