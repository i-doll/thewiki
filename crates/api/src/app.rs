//! Axum application wiring: routes and middleware stack.
//!
//! - [`build`] — health-only router used by the existing smoke tests.
//! - [`build_with_state`] — page CRUD + OpenAPI mounted on [`AppState`]. From #9.
//! - [`build_auth_app`] — auth routes mounted on [`AuthState`]. From #13. Used
//!   directly by the auth integration tests.
//! - [`build_full`] — production wiring: both [`AppState`] and [`AuthState`]
//!   combined behind the cookie + CSRF + tracing stack. Used by the `serve`
//!   subcommand.
//!
//! Keep handler logic out of this module — it should read as a table of
//! routes plus the middleware layering.
//!
//! TODO(#14): collapse `build_with_state` and `build_auth_app` into the single
//! `build_full` once configurable auth lands and `AppState` carries the
//! auth context directly. The split exists only because #9 and #13 landed in
//! the same batch and avoided the larger refactor.

use axum::{
    Router,
    http::{HeaderName, HeaderValue, Request},
    middleware,
    response::IntoResponse,
    routing::{any, get},
};
use tower::ServiceBuilder;
use tower_cookies::CookieManagerLayer;
use tower_http::{
    catch_panic::CatchPanicLayer,
    request_id::{MakeRequestId, PropagateRequestIdLayer, RequestId, SetRequestIdLayer},
    trace::{DefaultOnResponse, TraceLayer},
};
use tracing::{Level, field};
use utoipa::OpenApi;
use utoipa::openapi::{
    Components, OpenApi as OpenApiDoc,
    path::HttpMethod,
    security::{ApiKey, ApiKeyValue, SecurityRequirement, SecurityScheme},
};
use utoipa_axum::router::OpenApiRouter;
use utoipa_swagger_ui::SwaggerUi;
use uuid::Uuid;

use crate::audit_log;
use crate::auth::{self, AuthState, csrf};
use crate::config::{Config, RateLimitConfig};
use crate::media;
use crate::pages;
use crate::rate_limit::{self, RateLimitState};
use crate::recent_changes;
use crate::state::{AppState, AppStorage};
use crate::static_assets;

/// HTTP header used to carry the per-request correlation ID.
const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

/// Path that serves the generated OpenAPI document as JSON.
pub const OPENAPI_JSON_PATH: &str = "/api/openapi.json";

/// Path that serves the Swagger UI explorer.
pub const SWAGGER_UI_PATH: &str = "/api/docs";

const SESSION_COOKIE_SECURITY: &str = "SessionCookie";
const CSRF_TOKEN_SECURITY: &str = "CsrfToken";

/// Aggregated OpenAPI document.
///
/// `utoipa_axum::router::OpenApiRouter` discovers handlers (via the
/// `routes!(…)` macro) and merges their `#[utoipa::path]` metadata in
/// automatically, but we still need a top-level `OpenApi` derive so the
/// title/version/etc. are populated.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "thewiki API",
        version = env!("CARGO_PKG_VERSION"),
        description = "REST endpoints for thewiki — see https://github.com/i-doll/thewiki",
    ),
    tags(
        (name = "pages", description = "Page CRUD"),
        (name = "revisions", description = "Revision history + diffs"),
        (name = "auth", description = "Sessions, login, /me"),
        (name = "recent-changes", description = "Wiki-wide chronological edit feed"),
        (name = "audit-log", description = "Administrative audit trail"),
        (name = "media", description = "Content-addressed media uploads"),
    )
)]
pub struct ApiDoc;

/// Build the OpenAPI-aware REST router for the app-state backed endpoints.
fn api_router<S: AppStorage>() -> OpenApiRouter<AppState<S>> {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .nest("/api/v1/pages", pages::router::<S>())
        .nest("/api/v1/recent-changes", recent_changes::router::<S>())
        .nest("/api/v1/audit-log", audit_log::router::<S>())
        .nest("/api/v1/media", media::router::<S>())
}

/// Generate the full public REST OpenAPI document.
///
/// The app has two state roots today (`AppState<S>` for wiki data and
/// `AuthState` for session endpoints), so auth routes are documented through a
/// second OpenAPI router and then merged into the main document.
pub fn openapi<S: AppStorage>() -> OpenApiDoc {
    let (_, mut api_doc) = api_router::<S>().split_for_parts();
    let (_, auth_doc) = OpenApiRouter::new()
        .nest("/api/v1/auth", auth::routes::build_router())
        .split_for_parts();
    api_doc.merge(auth_doc);
    finalize_openapi(&mut api_doc);
    api_doc
}

fn finalize_openapi(api_doc: &mut OpenApiDoc) {
    add_security_schemes(api_doc);
    add_operation_security(api_doc);
}

fn add_security_schemes(api_doc: &mut OpenApiDoc) {
    let components = api_doc.components.get_or_insert_with(Components::new);
    components.add_security_scheme(
        SESSION_COOKIE_SECURITY,
        SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
            auth::session::SESSION_COOKIE,
            "Opaque session cookie issued by POST /api/v1/auth/login.",
        ))),
    );
    components.add_security_scheme(
        CSRF_TOKEN_SECURITY,
        SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::with_description(
            auth::session::CSRF_HEADER,
            "Double-submit CSRF token matching the thewiki_csrf cookie.",
        ))),
    );
}

fn add_operation_security(api_doc: &mut OpenApiDoc) {
    set_operation_security(
        api_doc,
        "/api/v1/auth/login",
        HttpMethod::Post,
        optional_session_and_csrf_requirement(),
    );
    set_operation_security(
        api_doc,
        "/api/v1/auth/logout",
        HttpMethod::Post,
        vec![session_and_csrf_requirement()],
    );
    set_operation_security(
        api_doc,
        "/api/v1/auth/me",
        HttpMethod::Get,
        vec![session_requirement()],
    );
    set_operation_security(
        api_doc,
        "/api/v1/audit-log",
        HttpMethod::Get,
        vec![session_requirement()],
    );
    set_operation_security(
        api_doc,
        "/api/v1/audit-log/atom",
        HttpMethod::Get,
        vec![session_requirement()],
    );
    set_operation_security(
        api_doc,
        "/api/v1/pages",
        HttpMethod::Post,
        optional_session_and_csrf_requirement(),
    );
    set_operation_security(
        api_doc,
        "/api/v1/pages/{slug}",
        HttpMethod::Put,
        optional_session_and_csrf_requirement(),
    );
    set_operation_security(
        api_doc,
        "/api/v1/pages/{slug}",
        HttpMethod::Delete,
        optional_session_and_csrf_requirement(),
    );
    set_operation_security(
        api_doc,
        "/api/v1/pages/{slug}/revert",
        HttpMethod::Post,
        vec![session_and_csrf_requirement()],
    );
    set_operation_security(
        api_doc,
        "/api/v1/media",
        HttpMethod::Post,
        vec![session_and_csrf_requirement()],
    );
    set_operation_security(
        api_doc,
        "/api/v1/media/{id}",
        HttpMethod::Delete,
        vec![session_and_csrf_requirement()],
    );
}

fn session_requirement() -> SecurityRequirement {
    SecurityRequirement::new(SESSION_COOKIE_SECURITY, Vec::<String>::new())
}

fn session_and_csrf_requirement() -> SecurityRequirement {
    session_requirement().add(CSRF_TOKEN_SECURITY, Vec::<String>::new())
}

fn optional_session_and_csrf_requirement() -> Vec<SecurityRequirement> {
    vec![
        SecurityRequirement::default(),
        session_and_csrf_requirement(),
    ]
}

fn set_operation_security(
    api_doc: &mut OpenApiDoc,
    path: &str,
    method: HttpMethod,
    security: Vec<SecurityRequirement>,
) {
    let method_name = match &method {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
        HttpMethod::Put => "PUT",
        HttpMethod::Delete => "DELETE",
        HttpMethod::Options => "OPTIONS",
        HttpMethod::Head => "HEAD",
        HttpMethod::Patch => "PATCH",
        HttpMethod::Trace => "TRACE",
    };
    let operation = api_doc
        .paths
        .paths
        .get_mut(path)
        .and_then(|item| match method {
            HttpMethod::Get => item.get.as_mut(),
            HttpMethod::Post => item.post.as_mut(),
            HttpMethod::Put => item.put.as_mut(),
            HttpMethod::Delete => item.delete.as_mut(),
            HttpMethod::Options => item.options.as_mut(),
            HttpMethod::Head => item.head.as_mut(),
            HttpMethod::Patch => item.patch.as_mut(),
            HttpMethod::Trace => item.trace.as_mut(),
        });

    if let Some(operation) = operation {
        operation.security = Some(security);
    } else {
        tracing::warn!(
            path,
            method = method_name,
            "OpenAPI operation not found for security annotation"
        );
    }
}

/// Span factory used by [`TraceLayer`].
///
/// Pulled out of the closure into a `fn` item so both router constructors
/// share the same callback type and we don't fight inference at the call
/// site.
fn request_span(request: &Request<axum::body::Body>) -> tracing::Span {
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .and_then(|id| id.header_value().to_str().ok())
        .unwrap_or("-");
    tracing::info_span!(
        "request",
        method = %request.method(),
        uri = %request.uri(),
        version = ?request.version(),
        request_id = %request_id,
        status = field::Empty,
    )
}

/// Apply the shared middleware stack to `router`.
///
/// Layers, outermost first (i.e. first to see the request, last to see the
/// response):
///
/// 1. [`CatchPanicLayer`] — turns panics in downstream services into `500`s so
///    the worker thread isn't torn down.
/// 2. [`SetRequestIdLayer`] — trusts an incoming `x-request-id` if present,
///    otherwise generates a UUIDv7.
/// 3. [`TraceLayer`] — emits a span per request with method/uri/status/latency
///    and the request ID (which is on the request extensions by this point).
/// 4. [`PropagateRequestIdLayer`] — copies the request ID onto the outgoing
///    response so clients can correlate.
fn with_middleware(router: Router) -> Router {
    let trace_layer = TraceLayer::new_for_http()
        .make_span_with(request_span as fn(&Request<axum::body::Body>) -> tracing::Span)
        .on_response(
            DefaultOnResponse::new()
                .level(Level::INFO)
                .latency_unit(tower_http::LatencyUnit::Millis),
        );

    let stack = ServiceBuilder::new()
        .layer(CatchPanicLayer::new())
        .layer(SetRequestIdLayer::new(
            REQUEST_ID_HEADER.clone(),
            MakeUuidV7RequestId,
        ))
        .layer(trace_layer)
        .layer(PropagateRequestIdLayer::new(REQUEST_ID_HEADER.clone()));

    router.layer(stack)
}

/// Build the health-only application router.
///
/// Convenience for callers (the existing `cargo test -p thewiki-api --test
/// health` suite, smoke tests that don't need a storage handle).
pub fn build() -> Router {
    with_middleware(
        Router::new()
            .route("/healthz", get(healthz))
            .route("/readyz", get(readyz)),
    )
}

/// Build the page-CRUD application router mounted on the supplied [`AppState`].
///
/// Used by the page-CRUD integration tests (`tests/pages.rs`). Production
/// callers want [`build_full`] which adds the CSRF layer on top. The cookie
/// manager layer is mounted here unconditionally so the configurable-auth
/// extractor (#14) can resolve session cookies in tests that wire up
/// [`AppState::with_auth_state`].
pub fn build_with_state<S: AppStorage>(state: AppState<S>) -> Router {
    build_with_state_with_rate_limit(state, Config::defaults().rate_limit)
}

/// Build the page-CRUD app with a caller-supplied rate-limit config.
///
/// Integration tests that are not specifically exercising rate limits should
/// pass a disabled config so they do not inherit production defaults.
pub fn build_with_state_with_rate_limit<S: AppStorage>(
    state: AppState<S>,
    rate_limit_config: RateLimitConfig,
) -> Router {
    let rate_limit_state = RateLimitState::new(rate_limit_config, state.auth_state.clone());
    let api_router = api_router::<S>()
        .layer(middleware::from_fn(rate_limit::rate_limit_layer))
        .layer(axum::Extension(rate_limit_state));

    let (api_router, api_doc) = api_router.split_for_parts();
    let swagger = SwaggerUi::new(SWAGGER_UI_PATH).url(OPENAPI_JSON_PATH, api_doc);
    let stateful = api_router.with_state(state);

    let router = Router::new()
        .merge(stateful)
        .merge(swagger)
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .layer(CookieManagerLayer::new());

    with_middleware(router)
}

/// Build the auth-only application router mounted on the supplied [`AuthState`].
///
/// Used by the auth integration tests (`tests/auth.rs`). Production callers
/// want [`build_full`] which combines pages + auth.
pub fn build_auth_app(state: AuthState) -> Router {
    build_auth_app_with_rate_limit(state, Config::defaults().rate_limit)
}

/// Build the auth-only app with a caller-supplied rate-limit config.
///
/// This is primarily for integration tests that need tiny buckets. Production
/// callers should use [`build_full`], which receives the parsed runtime config.
pub fn build_auth_app_with_rate_limit(
    state: AuthState,
    rate_limit_config: RateLimitConfig,
) -> Router {
    let rate_limit_state = RateLimitState::new(rate_limit_config, Some(state.clone()));
    let auth_router: Router<AuthState> = auth::routes::build_router()
        .layer(axum::middleware::from_fn(csrf::csrf_layer))
        .layer(middleware::from_fn(rate_limit::rate_limit_layer))
        .layer(axum::Extension(rate_limit_state))
        .into();

    let router = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .nest("/api/v1/auth", auth_router)
        .layer(CookieManagerLayer::new())
        .with_state(state);

    with_middleware(router)
}

/// Build the full production router: pages + OpenAPI + auth + CSRF + cookies.
///
/// This is what the `serve` subcommand calls.
///
/// When `serve_frontend` is `true`, mounts the embedded SPA bundle as the
/// fallback service so unmatched routes serve `index.html` (SPA history
/// routing). Any `/api/...` request that doesn't match a real API route is
/// caught by the catch-all `/api/{*rest}` route below so the SPA fallback
/// never eats an API miss — clients see a clean `404`.
pub fn build_full<S: AppStorage>(
    app_state: AppState<S>,
    auth_state: AuthState,
    serve_frontend: bool,
    rate_limit_config: RateLimitConfig,
) -> Router {
    let rate_limit_state = RateLimitState::new(
        rate_limit_config,
        app_state.auth_state.clone().or(Some(auth_state.clone())),
    );
    build_full_with_rate_limit_state(app_state, auth_state, serve_frontend, rate_limit_state)
}

/// Variant of [`build_full`] that takes a caller-built [`RateLimitState`].
///
/// The `serve` subcommand uses this when the operator selected the Redis
/// backend — connecting to Redis is async and fallible, so the state has to
/// be built up front (via [`RateLimitState::connect`]) rather than inline in
/// the router constructor.
pub fn build_full_with_rate_limit_state<S: AppStorage>(
    app_state: AppState<S>,
    auth_state: AuthState,
    serve_frontend: bool,
    rate_limit_state: RateLimitState,
) -> Router {
    // Page CRUD + recent-changes + OpenAPI subrouter.
    let api_router = api_router::<S>()
        .layer(middleware::from_fn(csrf::csrf_layer))
        .layer(middleware::from_fn(rate_limit::rate_limit_layer))
        .layer(axum::Extension(rate_limit_state.clone()));
    let (api_router, _) = api_router.split_for_parts();
    let api_doc = openapi::<S>();
    let swagger = SwaggerUi::new(SWAGGER_UI_PATH).url(OPENAPI_JSON_PATH, api_doc);
    let stateful_api = api_router.with_state(app_state);

    // Auth subrouter.
    let auth_router: Router = auth::routes::build_router()
        .layer(middleware::from_fn(csrf::csrf_layer))
        .layer(middleware::from_fn(rate_limit::rate_limit_layer))
        .layer(axum::Extension(rate_limit_state))
        .with_state(auth_state)
        .into();

    let mut router = Router::new()
        .merge(stateful_api)
        .merge(swagger)
        .nest("/api/v1/auth", auth_router)
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz));

    if serve_frontend {
        // Explicit guard: any `/api/...` path that didn't match a real API
        // route returns 404 instead of being swallowed by the SPA fallback.
        // Without this, `GET /api/v1/pages/does-not-exist` would render the
        // React shell with status 200, which is a confusing API surface.
        router = router
            .route("/api/{*rest}", any(api_not_found))
            .fallback_service(static_assets::static_routes());
    }

    let router = router.layer(CookieManagerLayer::new());

    with_middleware(router)
}

/// Catch-all for unmatched `/api/...` paths. Returns a plain 404 so the SPA
/// fallback service never sees an API miss.
async fn api_not_found() -> impl IntoResponse {
    (axum::http::StatusCode::NOT_FOUND, "not found")
}

/// Liveness probe. Returns 200 as soon as the process is accepting requests.
async fn healthz() -> impl IntoResponse {
    "ok"
}

/// Readiness probe.
///
/// TODO(#7): once the storage layer is wired, check the DB pool is live and
/// migrations are applied. For the skeleton PR we always return 200 so the
/// route shape is stable.
async fn readyz() -> impl IntoResponse {
    "ok"
}

/// `MakeRequestId` impl that mints a UUIDv7 per request.
///
/// UUIDv7 sorts lexicographically by creation time, which makes logs grouped
/// by request ID easy to follow.
#[derive(Clone, Copy, Default)]
pub(crate) struct MakeUuidV7RequestId;

impl MakeRequestId for MakeUuidV7RequestId {
    fn make_request_id<B>(&mut self, _request: &Request<B>) -> Option<RequestId> {
        let id = Uuid::now_v7();
        // UUID hyphenated form is always ASCII; `HeaderValue::from_str` cannot
        // fail. We use `expect` here so a future regression surfaces loudly
        // instead of silently dropping the request ID.
        #[allow(
            clippy::expect_used,
            reason = "UUIDv7 hyphenated form is provably valid for HeaderValue"
        )]
        let value = HeaderValue::from_str(&id.to_string())
            .expect("UUIDv7 hyphenated form is always valid ASCII for a header value");
        Some(RequestId::new(value))
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn healthz_returns_ok() {
        let app = build();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .expect("build healthz request"),
            )
            .await
            .expect("router responded");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("read body")
            .to_bytes();
        assert_eq!(&body[..], b"ok");
    }

    #[tokio::test]
    async fn readyz_returns_ok() {
        let app = build();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/readyz")
                    .body(Body::empty())
                    .expect("build readyz request"),
            )
            .await
            .expect("router responded");

        assert_eq!(response.status(), StatusCode::OK);
    }
}
