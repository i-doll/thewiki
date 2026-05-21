//! Axum application wiring: routes and middleware stack.
//!
//! The router exposed by [`build_with_state`] is the production wiring used
//! by both the binary and the page-CRUD integration tests. [`build`] is
//! a smaller health-only constructor kept for the existing smoke tests that
//! don't need a storage handle.
//!
//! Keep handler logic out of this module — it should read as a table of
//! routes plus the middleware layering.

use axum::{
    Router,
    http::{HeaderName, HeaderValue, Request},
    response::IntoResponse,
    routing::get,
};
use tower::ServiceBuilder;
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

use crate::pages;
use crate::state::{AppState, AppStorage};

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

/// Build the full application router mounted on the supplied [`AppState`].
///
/// Routes:
///
/// - `/api/v1/pages*` — page CRUD (see [`crate::pages`]).
/// - [`OPENAPI_JSON_PATH`] — generated OpenAPI document.
/// - [`SWAGGER_UI_PATH`] — Swagger UI explorer.
/// - `/healthz`, `/readyz` — liveness / readiness probes.
pub fn build_with_state<S: AppStorage>(state: AppState<S>) -> Router {
    // Build the API subrouter with utoipa so its handler set populates the
    // OpenAPI document automatically.
    let api_router: OpenApiRouter<AppState<S>> =
        OpenApiRouter::with_openapi(ApiDoc::openapi()).nest("/api/v1/pages", pages::router::<S>());

    let (api_router, api_doc) = api_router.split_for_parts();

    // `SwaggerUi::new(...).url(...)` both serves the Swagger UI assets at
    // `/api/docs` and exposes the OpenAPI JSON at `/api/openapi.json` — the
    // url() call registers the JSON route under the hood so we don't add a
    // second explicit handler for it (axum panics on overlapping routes).
    let swagger = SwaggerUi::new(SWAGGER_UI_PATH).url(OPENAPI_JSON_PATH, api_doc);

    let stateful = api_router.with_state(state);

    let router = Router::new()
        .merge(stateful)
        .merge(swagger)
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz));

    with_middleware(router)
}

/// Liveness probe. Returns 200 as soon as the process is accepting requests.
async fn healthz() -> impl IntoResponse {
    "ok"
}

/// Readiness probe.
///
/// TODO(#7): once #4 lands the storage layer, this should check that the DB
/// pool is live and that all migrations have been applied. For the skeleton
/// PR we always return 200 so the route shape is stable.
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
