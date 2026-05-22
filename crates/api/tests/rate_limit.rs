//! Integration tests for token-bucket rate limiting (#35).
//!
//! These tests exercise the middleware end-to-end via the auth router (which
//! is the smallest production-shaped surface that already wires the limiter,
//! CSRF, and cookies). The bucket capacities used here are intentionally
//! tiny so a single test request exhausts them — production defaults are
//! validated separately in the config unit tests.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::Value;
use thewiki_api::app;
use thewiki_api::auth::password::Argon2Hasher;
use thewiki_api::auth::session::encode_session_id;
use thewiki_api::auth::state::AuthState;
use thewiki_api::config::{
    Argon2Config, ClientIpHeader, Config, RateLimitBackendConfig, RateLimitBucketConfig,
    RateLimitConfig,
};
use thewiki_core::{EmailAddress, User, UserId, Username};
use thewiki_storage::repo::{SessionRepository, UserRepository};
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

/// Capacity-1 bucket with a 60s refill — easy to exhaust, easy to assert
/// `Retry-After` against.
fn tiny_bucket() -> RateLimitBucketConfig {
    RateLimitBucketConfig {
        capacity: 1,
        refill_tokens: 1,
        refill_interval_secs: 60,
    }
}

/// Capacity-`n` bucket with a 60s refill — used to verify "first N succeed,
/// (N+1)th 429s" semantics with N > 1.
fn bucket_with_capacity(capacity: u32) -> RateLimitBucketConfig {
    RateLimitBucketConfig {
        capacity,
        refill_tokens: capacity,
        refill_interval_secs: 60,
    }
}

fn rate_limit_config() -> RateLimitConfig {
    RateLimitConfig {
        enabled: true,
        read: tiny_bucket(),
        write: tiny_bucket(),
        authenticated_read: None,
        authenticated_write: None,
        client_ip_header: None,
        trusted_proxies: Vec::new(),
        backend: RateLimitBackendConfig::InMemory,
    }
}

async fn setup() -> (AuthState, UserId) {
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

    let user = User {
        id: UserId::new(),
        username: Username::new("alice").expect("uname"),
        email: Some(EmailAddress::new("alice@example.com").expect("email")),
        display_name: Some("Alice".into()),
        created_at: OffsetDateTime::now_utc(),
        last_login_at: None,
    };
    storage
        .users()
        .create(&user, None)
        .await
        .expect("seed user");

    let state = AuthState::new(
        storage,
        Arc::new(Argon2Hasher::new(test_argon2()).expect("hasher")),
        Duration::from_secs(60 * 60),
        false,
        Config::defaults().auth,
    );
    (state, user.id)
}

async fn create_user(state: &AuthState, username: &str, email: &str) -> UserId {
    let user = User {
        id: UserId::new(),
        username: Username::new(username).expect("uname"),
        email: Some(EmailAddress::new(email).expect("email")),
        display_name: Some(username.into()),
        created_at: OffsetDateTime::now_utc(),
        last_login_at: None,
    };
    state
        .storage
        .users()
        .create(&user, None)
        .await
        .expect("seed extra user");
    user.id
}

fn ip(octet: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(203, 0, 113, octet))
}

fn request(method: Method, uri: &str, peer_ip: IpAddr, body: Body) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .body(body)
        .expect("request");
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::new(peer_ip, 12345)));
    request
}

async fn body_json(response: axum::http::Response<Body>) -> Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("body is json")
}

async fn get(app: Router, uri: &str, peer_ip: IpAddr) -> axum::http::Response<Body> {
    app.oneshot(request(Method::GET, uri, peer_ip, Body::empty()))
        .await
        .expect("router")
}

async fn get_with_forwarded_for(
    app: Router,
    uri: &str,
    peer_ip: IpAddr,
    forwarded_for: &str,
) -> axum::http::Response<Body> {
    let mut req = request(Method::GET, uri, peer_ip, Body::empty());
    req.headers_mut().insert(
        "x-forwarded-for",
        forwarded_for.parse().expect("valid forwarded header"),
    );
    app.oneshot(req).await.expect("router")
}

async fn get_with_real_ip(
    app: Router,
    uri: &str,
    peer_ip: IpAddr,
    real_ip: IpAddr,
) -> axum::http::Response<Body> {
    let mut req = request(Method::GET, uri, peer_ip, Body::empty());
    req.headers_mut()
        .insert("x-real-ip", real_ip.to_string().parse().expect("valid IP"));
    app.oneshot(req).await.expect("router")
}

// ---------------------------------------------------------------------------
// Spec coverage: burst N+1, refill, key separation, GET vs POST.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn burst_first_n_succeed_then_429_with_retry_after() {
    const N: u32 = 4;
    let (state, _uid) = setup().await;
    let mut config = rate_limit_config();
    config.read = bucket_with_capacity(N);
    let app = app::build_auth_app_with_rate_limit(state, config);

    // First N reads must succeed.
    for i in 0..N {
        let response = get(app.clone(), "/api/v1/auth/policy", ip(40)).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "request {i} should be allowed within capacity {N}"
        );
    }

    // N+1th must 429 with a Retry-After header.
    let response = get(app, "/api/v1/auth/policy", ip(40)).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let retry_after = response
        .headers()
        .get(header::RETRY_AFTER)
        .expect("Retry-After present on 429");
    let secs: u64 = retry_after
        .to_str()
        .expect("ascii header value")
        .parse()
        .expect("Retry-After is an integer seconds value");
    assert!(
        secs >= 1,
        "Retry-After must be at least 1 second, got {secs}"
    );
    assert_eq!(body_json(response).await["error"], "rate_limited");
}

#[tokio::test]
async fn token_refill_unblocks_next_request() {
    // Bucket: capacity 1, refill 1 per 1 second. Sleep long enough to be sure
    // the token is back without making the test painfully slow.
    let (state, _uid) = setup().await;
    let mut config = rate_limit_config();
    config.read = RateLimitBucketConfig {
        capacity: 1,
        refill_tokens: 1,
        refill_interval_secs: 1,
    };
    let app = app::build_auth_app_with_rate_limit(state, config);

    let first = get(app.clone(), "/api/v1/auth/policy", ip(41)).await;
    assert_eq!(first.status(), StatusCode::OK);

    let denied = get(app.clone(), "/api/v1/auth/policy", ip(41)).await;
    assert_eq!(denied.status(), StatusCode::TOO_MANY_REQUESTS);

    // Wait for the token to refill, then try again. Allow a small safety
    // margin for the busy CI runner.
    tokio::time::sleep(Duration::from_millis(1_100)).await;

    let after = get(app, "/api/v1/auth/policy", ip(41)).await;
    assert_eq!(after.status(), StatusCode::OK);
}

#[tokio::test]
async fn distinct_anonymous_ips_have_independent_buckets() {
    let (state, _uid) = setup().await;
    let app = app::build_auth_app_with_rate_limit(state, rate_limit_config());

    assert_eq!(
        get(app.clone(), "/api/v1/auth/policy", ip(50))
            .await
            .status(),
        StatusCode::OK
    );
    // ip(50) exhausted, but ip(51) is fresh.
    assert_eq!(
        get(app.clone(), "/api/v1/auth/policy", ip(51))
            .await
            .status(),
        StatusCode::OK
    );
    // ip(50) is still 429.
    assert_eq!(
        get(app, "/api/v1/auth/policy", ip(50)).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn distinct_users_have_independent_buckets() {
    let (state, alice_id) = setup().await;
    let bob_id = create_user(&state, "bob", "bob@example.com").await;
    let alice_session = state
        .storage
        .sessions()
        .create(alice_id, Duration::from_secs(60 * 60), None, None)
        .await
        .expect("alice session");
    let bob_session = state
        .storage
        .sessions()
        .create(bob_id, Duration::from_secs(60 * 60), None, None)
        .await
        .expect("bob session");
    let alice_cookie = format!("thewiki_session={}", encode_session_id(alice_session.id));
    let bob_cookie = format!("thewiki_session={}", encode_session_id(bob_session.id));
    let app = app::build_auth_app_with_rate_limit(state, rate_limit_config());

    let mut alice_req = request(Method::GET, "/api/v1/auth/me", ip(60), Body::empty());
    alice_req
        .headers_mut()
        .insert(header::COOKIE, alice_cookie.parse().unwrap());
    assert_eq!(
        app.clone()
            .oneshot(alice_req)
            .await
            .expect("router")
            .status(),
        StatusCode::OK
    );

    // Bob — same shared IP — must not share Alice's bucket.
    let mut bob_req = request(Method::GET, "/api/v1/auth/me", ip(60), Body::empty());
    bob_req
        .headers_mut()
        .insert(header::COOKIE, bob_cookie.parse().unwrap());
    assert_eq!(
        app.oneshot(bob_req).await.expect("router").status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn get_and_post_use_separate_buckets() {
    let (state, _uid) = setup().await;
    let app = app::build_auth_app_with_rate_limit(state, rate_limit_config());

    // Burn the read bucket.
    assert_eq!(
        get(app.clone(), "/api/v1/auth/policy", ip(70))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        get(app.clone(), "/api/v1/auth/policy", ip(70))
            .await
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );

    // The write bucket is still untouched. POST /login is the cheapest
    // mutating endpoint to exercise — the body is invalid so it'll come back
    // 401, which is exactly what we need to confirm the rate limiter let it
    // through to the handler.
    let mut post = request(
        Method::POST,
        "/api/v1/auth/login",
        ip(70),
        Body::from(r#"{"username":"ghost","password":"wrong"}"#),
    );
    post.headers_mut()
        .insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    let response = app.oneshot(post).await.expect("router");
    assert!(
        response.status() != StatusCode::TOO_MANY_REQUESTS,
        "POST should not be charged the read bucket; got {}",
        response.status()
    );
}

#[tokio::test]
async fn authenticated_users_get_their_own_larger_bucket() {
    // Anonymous bucket is capacity-1 (exhausts after one request). The
    // authenticated bucket is capacity-3, so a signed-in user can make 3
    // requests from an IP that already exhausted its anonymous limit.
    let (state, uid) = setup().await;
    let mut config = rate_limit_config();
    config.authenticated_read = Some(bucket_with_capacity(3));
    let session = state
        .storage
        .sessions()
        .create(uid, Duration::from_secs(60 * 60), None, None)
        .await
        .expect("session");
    let cookie = format!("thewiki_session={}", encode_session_id(session.id));
    let app = app::build_auth_app_with_rate_limit(state, config);

    for i in 0..3 {
        let mut req = request(Method::GET, "/api/v1/auth/me", ip(80), Body::empty());
        req.headers_mut()
            .insert(header::COOKIE, cookie.parse().unwrap());
        let response = app.clone().oneshot(req).await.expect("router");
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "authenticated request {i} should fit within the user bucket"
        );
    }

    // 4th request hits the user-bucket cap.
    let mut req = request(Method::GET, "/api/v1/auth/me", ip(80), Body::empty());
    req.headers_mut()
        .insert(header::COOKIE, cookie.parse().unwrap());
    let response = app.oneshot(req).await.expect("router");
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
}

// ---------------------------------------------------------------------------
// Regression coverage carried over from the original implementation.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_bucket_exhaustion_returns_429_with_retry_after() {
    let (state, _uid) = setup().await;
    let app = app::build_auth_app_with_rate_limit(state, rate_limit_config());

    let first = get(app.clone(), "/api/v1/auth/policy", ip(10)).await;
    assert_eq!(first.status(), StatusCode::OK);

    let second = get(app, "/api/v1/auth/policy", ip(10)).await;
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        second.headers().get(header::RETRY_AFTER).unwrap(),
        "60",
        "retry-after should match the next read token"
    );
    assert_eq!(body_json(second).await["error"], "rate_limited");
}

#[tokio::test]
async fn write_bucket_exhaustion_is_independent_from_reads() {
    let (state, _uid) = setup().await;
    let app = app::build_auth_app_with_rate_limit(state, rate_limit_config());
    let login_body = || Body::from(r#"{"username":"ghost","password":"wrong"}"#);

    let mut first = request(Method::POST, "/api/v1/auth/login", ip(11), login_body());
    first
        .headers_mut()
        .insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    let first = app.clone().oneshot(first).await.expect("router");
    assert_eq!(first.status(), StatusCode::UNAUTHORIZED);

    let mut second = request(Method::POST, "/api/v1/auth/login", ip(11), login_body());
    second
        .headers_mut()
        .insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    let second = app.clone().oneshot(second).await.expect("router");
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);

    let read = get(app, "/api/v1/auth/policy", ip(11)).await;
    assert_eq!(read.status(), StatusCode::OK);
}

#[tokio::test]
async fn anonymous_requests_are_keyed_by_peer_ip() {
    let (state, _uid) = setup().await;
    let app = app::build_auth_app_with_rate_limit(state, rate_limit_config());

    assert_eq!(
        get(app.clone(), "/api/v1/auth/policy", ip(12))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        get(app.clone(), "/api/v1/auth/policy", ip(12))
            .await
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        get(app, "/api/v1/auth/policy", ip(13)).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn authenticated_requests_are_keyed_by_user_across_ips() {
    let (state, uid) = setup().await;
    let session = state
        .storage
        .sessions()
        .create(uid, Duration::from_secs(60 * 60), None, None)
        .await
        .expect("session");
    let cookie = format!("thewiki_session={}", encode_session_id(session.id));
    let app = app::build_auth_app_with_rate_limit(state, rate_limit_config());

    let mut first = request(Method::GET, "/api/v1/auth/me", ip(14), Body::empty());
    first
        .headers_mut()
        .insert(header::COOKIE, cookie.parse().unwrap());
    let first = app.clone().oneshot(first).await.expect("router");
    assert_eq!(first.status(), StatusCode::OK);

    let mut second = request(Method::GET, "/api/v1/auth/me", ip(15), Body::empty());
    second
        .headers_mut()
        .insert(header::COOKIE, cookie.parse().unwrap());
    let second = app.oneshot(second).await.expect("router");
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn authenticated_requests_are_keyed_by_user_on_same_ip() {
    let (state, alice_id) = setup().await;
    let bob_id = create_user(&state, "bob", "bob@example.com").await;
    let alice_session = state
        .storage
        .sessions()
        .create(alice_id, Duration::from_secs(60 * 60), None, None)
        .await
        .expect("alice session");
    let bob_session = state
        .storage
        .sessions()
        .create(bob_id, Duration::from_secs(60 * 60), None, None)
        .await
        .expect("bob session");
    let alice_cookie = format!("thewiki_session={}", encode_session_id(alice_session.id));
    let bob_cookie = format!("thewiki_session={}", encode_session_id(bob_session.id));
    let app = app::build_auth_app_with_rate_limit(state, rate_limit_config());

    let mut first = request(Method::GET, "/api/v1/auth/me", ip(14), Body::empty());
    first
        .headers_mut()
        .insert(header::COOKIE, alice_cookie.parse().unwrap());
    let first = app.clone().oneshot(first).await.expect("router");
    assert_eq!(first.status(), StatusCode::OK);

    let mut second = request(Method::GET, "/api/v1/auth/me", ip(14), Body::empty());
    second
        .headers_mut()
        .insert(header::COOKIE, bob_cookie.parse().unwrap());
    let second = app.oneshot(second).await.expect("router");
    assert_eq!(second.status(), StatusCode::OK);
}

#[tokio::test]
async fn trusted_proxy_header_keys_anonymous_requests_by_forwarded_ip() {
    let (state, _uid) = setup().await;
    let mut config = rate_limit_config();
    config.client_ip_header = Some(ClientIpHeader::XForwardedFor);
    config.trusted_proxies = vec![IpAddr::V4(Ipv4Addr::LOCALHOST), ip(250)];
    let app = app::build_auth_app_with_rate_limit(state, config);
    let proxy_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);

    assert_eq!(
        get_with_forwarded_for(
            app.clone(),
            "/api/v1/auth/policy",
            proxy_ip,
            "198.51.100.99, 203.0.113.20, 203.0.113.250",
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert_eq!(
        get_with_forwarded_for(
            app.clone(),
            "/api/v1/auth/policy",
            proxy_ip,
            "198.51.100.100, 203.0.113.20, 203.0.113.250",
        )
        .await
        .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        get_with_forwarded_for(
            app,
            "/api/v1/auth/policy",
            proxy_ip,
            "203.0.113.21, 203.0.113.250",
        )
        .await
        .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn trusted_proxy_header_keys_anonymous_requests_by_x_real_ip() {
    let (state, _uid) = setup().await;
    let mut config = rate_limit_config();
    config.client_ip_header = Some(ClientIpHeader::XRealIp);
    config.trusted_proxies = vec![IpAddr::V4(Ipv4Addr::LOCALHOST)];
    let app = app::build_auth_app_with_rate_limit(state, config);
    let proxy_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);

    assert_eq!(
        get_with_real_ip(app.clone(), "/api/v1/auth/policy", proxy_ip, ip(30))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        get_with_real_ip(app.clone(), "/api/v1/auth/policy", proxy_ip, ip(30))
            .await
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        get_with_real_ip(app, "/api/v1/auth/policy", proxy_ip, ip(31))
            .await
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn untrusted_proxy_header_is_ignored() {
    let (state, _uid) = setup().await;
    let mut config = rate_limit_config();
    config.client_ip_header = Some(ClientIpHeader::XForwardedFor);
    config.trusted_proxies = vec![IpAddr::V4(Ipv4Addr::LOCALHOST)];
    let app = app::build_auth_app_with_rate_limit(state, config);

    assert_eq!(
        get_with_forwarded_for(app.clone(), "/api/v1/auth/policy", ip(21), "203.0.113.30")
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        get_with_forwarded_for(app, "/api/v1/auth/policy", ip(21), "203.0.113.31")
            .await
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn csrf_rejections_consume_write_tokens() {
    let (state, uid) = setup().await;
    let session = state
        .storage
        .sessions()
        .create(uid, Duration::from_secs(60 * 60), None, None)
        .await
        .expect("session");
    let cookie = format!("thewiki_session={}", encode_session_id(session.id));
    let app = app::build_auth_app_with_rate_limit(state, rate_limit_config());

    let mut invalid_csrf_logout =
        request(Method::POST, "/api/v1/auth/logout", ip(22), Body::empty());
    invalid_csrf_logout
        .headers_mut()
        .insert(header::COOKIE, cookie.parse().unwrap());
    let invalid_csrf_logout = app
        .clone()
        .oneshot(invalid_csrf_logout)
        .await
        .expect("router");
    assert_eq!(invalid_csrf_logout.status(), StatusCode::FORBIDDEN);

    let mut second_invalid_csrf_logout =
        request(Method::POST, "/api/v1/auth/logout", ip(22), Body::empty());
    second_invalid_csrf_logout
        .headers_mut()
        .insert(header::COOKIE, cookie.parse().unwrap());
    let second_invalid_csrf_logout = app
        .oneshot(second_invalid_csrf_logout)
        .await
        .expect("router");
    assert_eq!(
        second_invalid_csrf_logout.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn health_routes_are_not_limited() {
    let (state, _uid) = setup().await;
    let app = app::build_auth_app_with_rate_limit(state, rate_limit_config());

    assert_eq!(
        get(app.clone(), "/api/v1/auth/policy", ip(16))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        get(app.clone(), "/api/v1/auth/policy", ip(16))
            .await
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(get(app, "/healthz", ip(16)).await.status(), StatusCode::OK);
}
