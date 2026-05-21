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
use utoipa_axum::router::OpenApiRouter;
use utoipa_swagger_ui::SwaggerUi;
use uuid::Uuid;

use crate::auth::{self, AuthState, csrf};
use crate::pages;
use crate::recent_changes;
use crate::state::{AppState, AppStorage};
use crate::static_assets;

/// HTTP header used to carry the per-request correlation ID.
const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

/// Path that serves the generated OpenAPI document as JSON.
pub const OPENAPI_JSON_PATH: &str = "/api/openapi.json";

/// Path that serves the Swagger UI explorer.
pub const SWAGGER_UI_PATH: &str = "/api/docs";

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
    )
)]
pub struct ApiDoc;

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
/// callers want [`build_full`] which adds auth + CSRF on top.
pub fn build_with_state<S: AppStorage>(state: AppState<S>) -> Router {
    let api_router: OpenApiRouter<AppState<S>> = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .nest("/api/v1/pages", pages::router::<S>())
        .nest("/api/v1/recent-changes", recent_changes::router::<S>());

    let (api_router, api_doc) = api_router.split_for_parts();
    let swagger = SwaggerUi::new(SWAGGER_UI_PATH).url(OPENAPI_JSON_PATH, api_doc);
    let stateful = api_router.with_state(state);

    let router = Router::new()
        .merge(stateful)
        .merge(swagger)
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz));

    with_middleware(router)
}

/// Build the auth-only application router mounted on the supplied [`AuthState`].
///
/// Used by the auth integration tests (`tests/auth.rs`). Production callers
/// want [`build_full`] which combines pages + auth.
pub fn build_auth_app(state: AuthState) -> Router {
    let auth_router = auth::routes::build_router();

    let router = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .nest("/api/v1/auth", auth_router)
        .layer(axum::middleware::from_fn(csrf::csrf_layer))
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
) -> Router {
    // Page CRUD + recent-changes + OpenAPI subrouter.
    let api_router: OpenApiRouter<AppState<S>> = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .nest("/api/v1/pages", pages::router::<S>())
        .nest("/api/v1/recent-changes", recent_changes::router::<S>());
    let (api_router, api_doc) = api_router.split_for_parts();
    let swagger = SwaggerUi::new(SWAGGER_UI_PATH).url(OPENAPI_JSON_PATH, api_doc);
    let stateful_api = api_router.with_state(app_state);

    // Auth subrouter.
    let auth_router = auth::routes::build_router().with_state(auth_state);

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

    let router = router
        .layer(axum::middleware::from_fn(csrf::csrf_layer))
        .layer(CookieManagerLayer::new());

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
