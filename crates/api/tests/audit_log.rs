//! Integration tests for the administrative audit-log endpoints.

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
use thewiki_api::config::{Argon2Config, Config};
use thewiki_core::{
    EmailAddress, Namespace, NamespaceId, NamespaceSlug, Permissions, Role, RoleId, RoleName, User,
    UserId, Username,
};
use thewiki_storage::repo::{
    NamespaceRepository, RoleRepository, SessionRepository, UserRepository,
};
use thewiki_storage::sqlite::{SqliteOptions, SqliteStorage};
use time::OffsetDateTime;
use tower::ServiceExt;

struct TestApp {
    router: Router,
    admin_session: String,
    editor_session: String,
}

async fn fresh_app() -> TestApp {
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

    let admin = seed_user(&storage, "admin").await;
    let editor = seed_user(&storage, "editor").await;
    let role = Role {
        id: RoleId::new(),
        name: RoleName::new("auditor").expect("role name"),
        display_name: "Auditor".to_string(),
        permissions: Permissions::VIEW_AUDIT_LOG,
    };
    storage.roles().create(&role).await.expect("seed role");
    storage
        .roles()
        .assign_to_user(admin.id, role.id)
        .await
        .expect("assign role");

    let auth_cfg = Config::defaults().auth;
    let hasher = Arc::new(
        Argon2Hasher::new(Argon2Config {
            memory_kib: 19_456,
            iterations: 2,
            parallelism: 1,
        })
        .expect("hasher"),
    );
    let auth_state = AuthState::new(
        storage.clone(),
        hasher,
        Duration::from_secs(60 * 60),
        false,
        auth_cfg.clone(),
    );
    let state = AppState::new(storage.clone(), auth_cfg).with_auth_state(auth_state);
    let mut rate_limit = Config::defaults().rate_limit;
    rate_limit.enabled = false;
    let router = app::build_with_state_with_rate_limit(state, rate_limit);

    TestApp {
        router,
        admin_session: seed_session(&storage, admin.id).await,
        editor_session: seed_session(&storage, editor.id).await,
    }
}

async fn seed_user(storage: &SqliteStorage, username: &str) -> User {
    let user = User {
        id: UserId::new(),
        username: Username::new(username).expect("valid username"),
        email: Some(EmailAddress::new(format!("{username}@example.com")).expect("valid email")),
        display_name: Some(username.to_string()),
        created_at: OffsetDateTime::now_utc(),
        last_login_at: None,
    };
    storage
        .users()
        .create(&user, None)
        .await
        .expect("seed user");
    user
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

async fn request(
    router: Router,
    method: &str,
    uri: &str,
    session: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(session) = session {
        builder = builder.header(header::COOKIE, format!("thewiki_session={session}"));
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
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("read body")
        .to_bytes()
        .to_vec();
    (status, headers, bytes)
}

async fn json_request(
    router: Router,
    method: &str,
    uri: &str,
    session: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (status, _, bytes) = request(router, method, uri, session, body).await;
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("json response")
    };
    (status, value)
}

async fn create_page(router: Router, session: &str, slug: &str) -> Value {
    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/pages",
        Some(session),
        Some(json!({
            "namespace_slug": "Main",
            "slug": slug,
            "title": slug,
            "content": "body",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    body
}

async fn update_page(router: Router, session: &str, slug: &str) -> Value {
    let (status, body) = json_request(
        router,
        "PUT",
        &format!("/api/v1/pages/{slug}"),
        Some(session),
        Some(json!({
            "title": format!("{slug} updated"),
            "content": "updated body",
            "edit_summary": "test update",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    body
}

async fn delete_page(router: Router, session: &str, slug: &str) {
    let (status, body) = json_request(
        router,
        "DELETE",
        &format!("/api/v1/pages/{slug}"),
        Some(session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(body, Value::Null);
}

#[tokio::test]
async fn page_mutations_write_audit_rows_and_admin_can_list_them() {
    let app = fresh_app().await;
    create_page(app.router.clone(), &app.editor_session, "home").await;
    update_page(app.router.clone(), &app.editor_session, "home").await;
    delete_page(app.router.clone(), &app.editor_session, "home").await;

    let (status, body) = json_request(
        app.router,
        "GET",
        "/api/v1/audit-log?actor=editor",
        Some(&app.admin_session),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["action"], "page.delete");
    assert_eq!(items[1]["action"], "page.update");
    assert_eq!(items[2]["action"], "page.create");
    assert_eq!(body["items"][0]["actor_username"], "editor");
    assert_eq!(body["items"][0]["target_kind"], "page");
    assert_eq!(body["items"][0]["target_label"], "Main/home");
    assert_eq!(body["items"][0]["metadata"]["slug"], "home");
    assert_eq!(body["items"][1]["metadata"]["title_changed"], true);
}

#[tokio::test]
async fn update_audit_title_changed_reflects_persisted_delta() {
    let app = fresh_app().await;
    create_page(app.router.clone(), &app.editor_session, "home").await;

    let (status, _) = json_request(
        app.router.clone(),
        "PUT",
        "/api/v1/pages/home",
        Some(&app.editor_session),
        Some(json!({
            "title": "home",
            "content": "updated body",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = json_request(
        app.router,
        "GET",
        "/api/v1/audit-log?actor=editor&action=page.update",
        Some(&app.admin_session),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["items"].as_array().expect("items").len(), 1);
    assert_eq!(body["items"][0]["metadata"]["title_changed"], false);
}

#[tokio::test]
async fn audit_log_requires_view_permission() {
    let app = fresh_app().await;
    create_page(app.router.clone(), &app.editor_session, "home").await;

    let (status, body) = json_request(
        app.router.clone(),
        "GET",
        "/api/v1/audit-log",
        Some(&app.editor_session),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "forbidden");

    let (status, body) = json_request(
        app.router,
        "GET",
        "/api/v1/audit-log/atom",
        Some(&app.editor_session),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "forbidden");
}

#[tokio::test]
async fn audit_log_rejects_inverted_time_window() {
    let app = fresh_app().await;

    let (status, body) = json_request(
        app.router,
        "GET",
        "/api/v1/audit-log?since=2026-01-02T00:00:00Z&until=2026-01-01T00:00:00Z",
        Some(&app.admin_session),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_input");
}

#[tokio::test]
async fn audit_log_atom_feed_contains_entries() {
    let app = fresh_app().await;
    create_page(app.router.clone(), &app.editor_session, "home").await;

    let (status, headers, bytes) = request(
        app.router,
        "GET",
        "/api/v1/audit-log/atom",
        Some(&app.admin_session),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/atom+xml; charset=utf-8")
    );
    let body = String::from_utf8(bytes).expect("utf8");
    let document = roxmltree::Document::parse(&body).expect("well-formed XML");
    let feed = document.root_element();
    assert_eq!(feed.tag_name().name(), "feed");
    assert_eq!(
        feed.tag_name().namespace(),
        Some("http://www.w3.org/2005/Atom")
    );
    let entry = feed
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "entry")
        .expect("entry element");
    let title = entry
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "title")
        .and_then(|node| node.text())
        .expect("entry title");
    let author_name = entry
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "author")
        .and_then(|author| {
            author
                .children()
                .find(|node| node.is_element() && node.tag_name().name() == "name")
        })
        .and_then(|node| node.text())
        .expect("entry author name");
    assert_eq!(title, "page.create Main/home");
    assert_eq!(author_name, "editor");
}

#[tokio::test]
async fn audit_log_rejects_anonymous_callers_with_401() {
    let app = fresh_app().await;
    let (status, body) =
        json_request(app.router.clone(), "GET", "/api/v1/audit-log", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // AuthError wire shape uses `error`, not `code`.
    assert_eq!(body["error"], "invalid_credentials");

    let (status, _, _) = request(app.router, "GET", "/api/v1/audit-log/atom", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn audit_log_filters_by_since_and_until_window() {
    let app = fresh_app().await;
    create_page(app.router.clone(), &app.editor_session, "alpha").await;
    create_page(app.router.clone(), &app.editor_session, "beta").await;

    // Window covering both creations.
    let (status, body) = json_request(
        app.router.clone(),
        "GET",
        "/api/v1/audit-log?since=2024-01-01T00:00:00Z",
        Some(&app.admin_session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["items"].as_array().expect("items").len(), 2);

    // Until in the deep past wipes the window.
    let (status, body) = json_request(
        app.router,
        "GET",
        "/api/v1/audit-log?until=2024-01-01T00:00:00Z",
        Some(&app.admin_session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["items"].as_array().expect("items").is_empty());
}

#[tokio::test]
async fn audit_log_cursor_walks_pages() {
    let app = fresh_app().await;
    for slug in ["one", "two", "three"] {
        create_page(app.router.clone(), &app.editor_session, slug).await;
    }

    // First page (limit=2) returns the two newest entries plus a cursor.
    let (status, first) = json_request(
        app.router.clone(),
        "GET",
        "/api/v1/audit-log?limit=2",
        Some(&app.admin_session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["items"].as_array().expect("items").len(), 2);
    let cursor = first["next_cursor"]
        .as_str()
        .expect("next_cursor on first page")
        .to_string();

    // Second page yields the final entry; cursor empties out.
    let encoded = urlencoding(&cursor);
    let (status, second) = json_request(
        app.router,
        "GET",
        &format!("/api/v1/audit-log?limit=2&cursor={encoded}"),
        Some(&app.admin_session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second["items"].as_array().expect("items").len(), 1);
    assert!(second["next_cursor"].is_null());

    // The two pages cover three distinct IDs in total.
    let mut ids: Vec<String> = first["items"]
        .as_array()
        .expect("first items")
        .iter()
        .chain(second["items"].as_array().expect("second items").iter())
        .map(|entry| {
            entry["id"]
                .as_str()
                .expect("entry id is a string")
                .to_string()
        })
        .collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 3);
}

fn urlencoding(raw: &str) -> String {
    // The audit-log cursor is `<rfc3339>|<hex>` — the `|` and `:` characters
    // need percent-encoding for query strings. Keep this minimal rather than
    // pulling in a urlencoding dep.
    let mut out = String::with_capacity(raw.len());
    for b in raw.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[tokio::test]
async fn revert_writes_audit_row() {
    let app = fresh_app().await;
    let created = create_page(app.router.clone(), &app.editor_session, "home").await;
    let original_revision = created["current_revision_id"]
        .as_str()
        .expect("created revision")
        .to_string();
    update_page(app.router.clone(), &app.editor_session, "home").await;

    let (status, _) = json_request(
        app.router.clone(),
        "POST",
        "/api/v1/pages/home/revert",
        Some(&app.editor_session),
        Some(json!({
            "from_revision": original_revision,
            "message": "restore first revision",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = json_request(
        app.router,
        "GET",
        "/api/v1/audit-log?actor=editor&action=page.revert",
        Some(&app.admin_session),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["items"].as_array().expect("items").len(), 1);
    assert_eq!(body["items"][0]["action"], "page.revert");
    assert_eq!(
        body["items"][0]["metadata"]["from_revision_id"],
        original_revision
    );
}
