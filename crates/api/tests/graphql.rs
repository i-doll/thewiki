//! Integration tests for the GraphQL surface (#37).
//!
//! Drives the full router via `tower::ServiceExt::oneshot` against an
//! in-memory SQLite so the resolvers run through the same storage paths the
//! REST endpoints do. The schema is mounted by `app::build_full_with_rate_limit_state`
//! alongside the REST routes; this test only exercises the `/api/graphql*`
//! paths.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use thewiki_api::app;
use thewiki_api::auth::password::Argon2Hasher;
use thewiki_api::auth::state::AuthState;
use thewiki_api::config::{Argon2Config, Config, GraphQLConfig};
use thewiki_api::rate_limit::RateLimitState;
use thewiki_api::state::AppState;
use thewiki_core::{
    EmailAddress, Namespace, NamespaceId, NamespaceSlug, Permissions, Role, RoleId, RoleName, User,
    UserId, Username,
};
use thewiki_storage::repo::{
    NamespaceRepository, PageAuditMutation, RoleRepository, SessionRepository, UserRepository,
};
use thewiki_storage::sqlite::{SqliteOptions, SqliteStorage};
use time::OffsetDateTime;
use tower::ServiceExt;

use tempfile::TempDir;
use thewiki_search::{PageDoc, SearchIndex, Searcher};

// ─── Fixtures ─────────────────────────────────────────────────────────────

struct TestApp {
    router: Router,
    /// Session id (hyphenated uuid) for `alice`. Always returns 401 when
    /// not supplied; supply with `cookie: thewiki_session=...`.
    alice_session: String,
    /// Session id for `auditor` who holds VIEW_AUDIT_LOG.
    auditor_session: String,
}

async fn build_app() -> TestApp {
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
            slug: NamespaceSlug::new("Main").expect("slug"),
            display_name: "Main".into(),
            is_talk: false,
            paired_namespace_id: None,
        })
        .await
        .expect("seed namespace");

    let hasher = Arc::new(
        Argon2Hasher::new(Argon2Config {
            memory_kib: 19_456,
            iterations: 2,
            parallelism: 1,
        })
        .expect("hasher"),
    );
    let alice = seed_user(&storage, "alice").await;
    let auditor = seed_user(&storage, "auditor").await;
    let audit_role = Role {
        id: RoleId::new(),
        name: RoleName::new("auditor").expect("role"),
        display_name: "Auditor".to_string(),
        permissions: Permissions::VIEW_AUDIT_LOG,
    };
    storage
        .roles()
        .create(&audit_role)
        .await
        .expect("seed role");
    storage
        .roles()
        .assign_to_user(auditor.id, audit_role.id)
        .await
        .expect("assign role");

    let mut auth_cfg = Config::defaults().auth;
    // Need anonymous edits OFF so the `createPage without session => 401`
    // test asserts the right thing.
    auth_cfg.anonymous_edits = false;
    let auth_state = AuthState::new(
        storage.clone(),
        hasher,
        Duration::from_secs(60 * 60),
        false,
        auth_cfg.clone(),
    );
    let app_state = AppState::new(storage.clone(), auth_cfg).with_auth_state(auth_state.clone());

    let mut rate_limit = Config::defaults().rate_limit;
    rate_limit.enabled = false;
    let rate_limit_state = RateLimitState::new(rate_limit, Some(auth_state.clone()));

    let router = app::build_full_with_rate_limit_state(
        app_state,
        auth_state,
        false,
        rate_limit_state,
        GraphQLConfig::default(),
        Config::defaults().security,
    );

    let alice_session = seed_session(&storage, alice.id).await;
    let auditor_session = seed_session(&storage, auditor.id).await;

    // Seed a `home` page so query tests have something to read. We do this
    // through the storage layer directly to avoid coupling these tests to
    // the REST create path (whose own tests already cover it).
    let now = OffsetDateTime::now_utc();
    let ns = storage
        .namespaces()
        .get_by_slug(&NamespaceSlug::new("Main").expect("slug"))
        .await
        .expect("ns");
    let page = thewiki_core::page::Page {
        id: thewiki_core::PageId::new(),
        namespace_id: ns.id,
        slug: "home".to_string(),
        title: "Home".to_string(),
        current_revision_id: None,
        content_format: thewiki_core::ContentFormat::Markdown,
        protection_level: thewiki_core::ProtectionLevel::None,
        created_at: now,
        updated_at: now,
    };
    let mut page_with_rev = page.clone();
    let revision = thewiki_core::revision::Revision::new(
        page.id,
        None,
        alice.id,
        "# Hello\n\nworld".to_string(),
        None,
    );
    page_with_rev.current_revision_id = Some(revision.id);
    storage
        .commit_page_audit(
            PageAuditMutation::CreatePage {
                page: page_with_rev,
                live_revision: Some(revision),
            },
            thewiki_storage::repo::NewAuditLogEntry {
                actor_id: alice.id,
                actor_username: "alice".to_string(),
                action: "page.create".to_string(),
                target_kind: "page".to_string(),
                target_id: page.id.into_uuid(),
                target_label: Some("Main/home".to_string()),
                metadata: serde_json::json!({}),
            },
        )
        .await
        .expect("seed home page");

    TestApp {
        router,
        alice_session,
        auditor_session,
    }
}

async fn seed_user(storage: &SqliteStorage, username: &str) -> User {
    let user = User {
        id: UserId::new(),
        username: Username::new(username).expect("uname"),
        email: Some(EmailAddress::new(format!("{username}@example.com")).expect("email")),
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

async fn gql_request(router: Router, body: Value, session: Option<&str>) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/graphql")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(s) = session {
        builder = builder.header(header::COOKIE, format!("thewiki_session={s}"));
    }
    let req = builder
        .body(Body::from(body.to_string()))
        .expect("build req");
    let response = router.oneshot(req).await.expect("router");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let value: Value = serde_json::from_slice(&bytes).expect("json");
    (status, value)
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn introspection_works() {
    let app = build_app().await;
    let (status, body) = gql_request(
        app.router,
        json!({ "query": "{ __schema { types { name } } }" }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let types = body["data"]["__schema"]["types"]
        .as_array()
        .expect("types array");
    let names: Vec<&str> = types.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(names.contains(&"Page"), "schema is missing Page: {names:?}");
    assert!(names.contains(&"Revision"), "schema is missing Revision");
    assert!(
        names.contains(&"Query"),
        "schema is missing Query root: {names:?}"
    );
}

#[tokio::test]
async fn page_query_returns_the_page() {
    let app = build_app().await;
    let (status, body) = gql_request(
        app.router,
        json!({
            "query": "{ page(slug: \"home\") { slug title content } }"
        }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.get("errors").is_none(), "unexpected errors: {body}");
    let page = &body["data"]["page"];
    assert_eq!(page["slug"], "home");
    assert_eq!(page["title"], "Home");
    assert!(page["content"].as_str().expect("content").contains("Hello"));
}

#[tokio::test]
async fn create_page_requires_session() {
    let app = build_app().await;
    let (status, body) = gql_request(
        app.router,
        json!({
            "query": "mutation { createPage(namespaceSlug: \"Main\", slug: \"new\", title: \"New\", content: \"x\") { slug } }"
        }),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "transport-level status should be 200 for GraphQL errors"
    );
    let errors = body["errors"].as_array().expect("errors array");
    assert!(!errors.is_empty(), "expected an error: {body}");
    let code = errors[0]["extensions"]["code"]
        .as_str()
        .expect("error code");
    assert_eq!(code, "UNAUTHENTICATED", "got body: {body}");
}

#[tokio::test]
async fn create_page_with_session_succeeds() {
    let app = build_app().await;
    let (status, body) = gql_request(
        app.router,
        json!({
            "query": "mutation { createPage(namespaceSlug: \"Main\", slug: \"new-page\", title: \"New Page\", content: \"hello\") { slug title content } }"
        }),
        Some(&app.alice_session),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.get("errors").is_none(), "unexpected errors: {body}");
    let page = &body["data"]["createPage"];
    assert_eq!(page["slug"], "new-page");
    assert_eq!(page["title"], "New Page");
}

#[tokio::test]
async fn pages_list_paginates() {
    let app = build_app().await;
    let (status, body) = gql_request(
        app.router,
        json!({
            "query": "{ pages(limit: 2) { items { slug } pageInfo { hasNextPage endCursor } } }"
        }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let items = body["data"]["pages"]["items"]
        .as_array()
        .expect("items array");
    // Only `home` was seeded — so 1 item, no next.
    assert_eq!(items.len(), 1);
    assert_eq!(body["data"]["pages"]["pageInfo"]["hasNextPage"], false);
    assert!(body["data"]["pages"]["pageInfo"]["endCursor"].is_null());
}

#[tokio::test]
async fn search_query_returns_results_shape() {
    // The search worker isn't spun up in this test fixture so the resolver
    // returns the disabled-handle path: an empty result set rather than an
    // error. This still proves the type shape is wired correctly.
    let app = build_app().await;
    let (status, body) = gql_request(
        app.router,
        json!({
            "query": "{ search(query: \"hello\") { hits { slug title score } totalEstimate } }"
        }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.get("errors").is_none(), "got errors: {body}");
    let hits = body["data"]["search"]["hits"]
        .as_array()
        .expect("hits array");
    assert!(
        hits.is_empty(),
        "fixture has no search index, expected empty"
    );
    assert_eq!(body["data"]["search"]["totalEstimate"], 0);
}

/// Build a router with a real Tantivy index seeded with two pages so we can
/// assert that the GraphQL `search` resolver returns the same shape as REST.
async fn build_app_with_seeded_search() -> (Router, TempDir) {
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
            slug: NamespaceSlug::new("Main").expect("slug"),
            display_name: "Main".into(),
            is_talk: false,
            paired_namespace_id: None,
        })
        .await
        .expect("seed namespace");

    let hasher = Arc::new(
        Argon2Hasher::new(Argon2Config {
            memory_kib: 19_456,
            iterations: 2,
            parallelism: 1,
        })
        .expect("hasher"),
    );
    let auth_cfg = Config::defaults().auth;
    let auth_state = AuthState::new(
        storage.clone(),
        hasher,
        Duration::from_secs(60 * 60),
        false,
        auth_cfg.clone(),
    );

    let dir = TempDir::new().expect("tmpdir");
    let index = SearchIndex::open(dir.path()).expect("open");
    let mut writer = index.new_writer().expect("writer");
    let ns_id = NamespaceId::new();
    for (title, slug, body) in [
        (
            "Apollo Program",
            "apollo-program",
            "The Apollo program landed the first humans on the Moon.",
        ),
        (
            "Voyager Probes",
            "voyager-probes",
            "Voyager 1 and 2 explored the outer planets and beyond.",
        ),
    ] {
        index
            .upsert_on(
                &writer,
                &PageDoc {
                    page_id: thewiki_core::PageId::new(),
                    namespace_id: ns_id,
                    namespace_slug: "Main".to_string(),
                    slug: slug.to_string(),
                    title: title.to_string(),
                    body: body.to_string(),
                    tags: Vec::new(),
                    updated_at: OffsetDateTime::now_utc(),
                    is_talk: false,
                },
            )
            .expect("upsert");
    }
    writer.commit().expect("commit");
    index.write_last_indexed_marker().expect("marker");
    let index = Arc::new(SearchIndex::open(dir.path()).expect("reopen"));
    let searcher = Searcher::new(Arc::clone(&index));

    let app_state = AppState::new(storage, auth_cfg)
        .with_auth_state(auth_state.clone())
        .with_searcher(searcher);
    let mut rate_limit = Config::defaults().rate_limit;
    rate_limit.enabled = false;
    let rate_limit_state = RateLimitState::new(rate_limit, Some(auth_state.clone()));
    let router = app::build_full_with_rate_limit_state(
        app_state,
        auth_state,
        false,
        rate_limit_state,
        GraphQLConfig::default(),
        Config::defaults().security,
    );
    (router, dir)
}

#[tokio::test]
async fn search_query_returns_real_hits_when_index_is_seeded() {
    let (router, _dir) = build_app_with_seeded_search().await;
    let (status, body) = gql_request(
        router,
        json!({
            "query": "{ search(query: \"Apollo\") { hits { title slug score snippet } totalEstimate } }"
        }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.get("errors").is_none(), "got errors: {body}");
    let hits = body["data"]["search"]["hits"]
        .as_array()
        .expect("hits array");
    assert!(!hits.is_empty(), "expected at least one hit; body: {body}");
    let top = &hits[0];
    assert_eq!(top["title"], "Apollo Program");
    // The body field contains "Apollo" so the snippet should carry highlight
    // markers.
    let snippet = top["snippet"].as_str().expect("snippet");
    assert!(
        snippet.contains("<mark>") && snippet.contains("</mark>"),
        "snippet should carry <mark>; got: {snippet}"
    );
}

#[tokio::test]
async fn audit_log_requires_view_audit_log_permission() {
    let app = build_app().await;

    // Alice has no roles → FORBIDDEN.
    let (status, body) = gql_request(
        app.router.clone(),
        json!({
            "query": "{ auditLog(limit: 5) { items { action } pageInfo { hasNextPage } } }"
        }),
        Some(&app.alice_session),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let code = body["errors"][0]["extensions"]["code"]
        .as_str()
        .expect("code");
    assert_eq!(code, "FORBIDDEN", "body: {body}");

    // Auditor has VIEW_AUDIT_LOG → succeeds.
    let (status, body) = gql_request(
        app.router,
        json!({
            "query": "{ auditLog(limit: 5) { items { action actorUsername } pageInfo { hasNextPage } } }"
        }),
        Some(&app.auditor_session),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.get("errors").is_none(), "unexpected errors: {body}");
    assert!(
        body["data"]["auditLog"]["items"].is_array(),
        "items should be an array: {body}"
    );
}

#[tokio::test]
async fn playground_returns_html() {
    let app = build_app().await;
    let response = app
        .router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/graphql/playground")
                .body(Body::empty())
                .expect("build req"),
        )
        .await
        .expect("router");
    assert_eq!(response.status(), StatusCode::OK);
    let ct = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        ct.starts_with("text/html"),
        "expected html content-type, got {ct:?}"
    );
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let html = std::str::from_utf8(&bytes).expect("utf8");
    assert!(
        html.contains("graphiql") || html.contains("GraphiQL"),
        "html: {html}"
    );
}

#[tokio::test]
async fn schema_endpoint_returns_sdl() {
    let app = build_app().await;
    let response = app
        .router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/graphql/schema")
                .body(Body::empty())
                .expect("build req"),
        )
        .await
        .expect("router");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let sdl = std::str::from_utf8(&bytes).expect("utf8");
    assert!(
        sdl.contains("type Page") || sdl.contains("type Query"),
        "expected schema SDL contents: first 400 bytes: {}",
        &sdl[..sdl.len().min(400)]
    );
}

#[tokio::test]
async fn me_returns_null_for_anonymous() {
    let app = build_app().await;
    let (status, body) =
        gql_request(app.router, json!({ "query": "{ me { username } }" }), None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body["data"]["me"].is_null(), "expected null me: {body}");
}

#[tokio::test]
async fn me_returns_user_for_authenticated() {
    let app = build_app().await;
    let (status, body) = gql_request(
        app.router,
        json!({ "query": "{ me { username permissions } }" }),
        Some(&app.alice_session),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.get("errors").is_none(), "errors: {body}");
    assert_eq!(body["data"]["me"]["username"], "alice");
}
