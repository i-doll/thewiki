//! Axum middleware that 403s blocklisted client IPs (#42).
//!
//! The layer is intentionally tiny: pull the cached snapshot off the
//! extension, resolve the effective client IP, and short-circuit with a
//! structured 403 if the IP matches a CIDR. Static-asset / SPA-fallback
//! paths are *not* skipped — the operator intent for an IP block is "this
//! actor sees nothing from us", which includes HTML shells. Health probes
//! (`/healthz`, `/readyz`) are exempt so an internal monitoring system
//! never gets caught in a blocklist sweep.
//!
//! Wired in [`app::build_full_with_rate_limit_state`] before the cookie /
//! tracing / CSRF stack so the cheapest layer denies a known-bad IP without
//! even a session cookie parse.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Json;
use axum::extract::{ConnectInfo, Extension};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;

use crate::blocklist::peer_ip::{SecurityRuntime, effective_client_ip};
use crate::blocklist::state::BlocklistState;

/// Paths the blocklist layer never blocks.
///
/// Health probes are exempt so a misconfigured firewall doesn't take down
/// monitoring — the operator runs them locally on a path that's already
/// behind their own access control.
const HEALTH_PATHS: &[&str] = &["/healthz", "/readyz"];

/// Wire form returned alongside the 403 status when a request is blocked.
///
/// Kept distinct from the generic [`ErrorBody`](crate::error::ErrorBody) so
/// the SPA can branch on this specific structured response (and so the
/// machine-readable `code` is locked in).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BlocklistedErrorBody {
    /// Stable machine code.
    pub code: &'static str,
    /// Human-readable message. Never includes the matched CIDR — operators
    /// who want to know which entry caught the request go to the admin UI;
    /// returning it to the request would leak internal policy.
    pub message: &'static str,
}

/// Variant we use internally so handlers can `?`-bubble through it. Today
/// only the middleware itself produces this; exposed so future call sites
/// (e.g. a GraphQL resolver that re-checks for stitched-up sub-resources)
/// don't have to re-invent the body shape.
#[derive(Debug)]
pub struct BlocklistedError;

impl IntoResponse for BlocklistedError {
    fn into_response(self) -> Response {
        (
            StatusCode::FORBIDDEN,
            Json(BlocklistedErrorBody {
                code: "blocklisted_ip",
                message: "your IP is on this site's blocklist",
            }),
        )
            .into_response()
    }
}

/// Tower-style async middleware. Wired with `middleware::from_fn`.
pub async fn blocklist_layer(
    Extension(state): Extension<BlocklistState>,
    Extension(runtime): Extension<Arc<SecurityRuntime>>,
    connect_info: Option<axum::extract::Extension<ConnectInfo<SocketAddr>>>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if HEALTH_PATHS.contains(&path) {
        return next.run(request).await;
    }

    let connect_info_ref = connect_info.as_ref().map(|Extension(ci)| ci);
    let ip = effective_client_ip(connect_info_ref, request.headers(), runtime.as_ref());
    let snapshot = state.snapshot().await;
    if snapshot.contains_ip(ip) {
        tracing::info!(
            client_ip = %ip,
            path = %path,
            "request blocked by IP blocklist"
        );
        return BlocklistedError.into_response();
    }
    next.run(request).await
}
