//! `GET /api/v1/captcha/config` — publish the public CAPTCHA config so the
//! SPA can render (or skip) the widget on app boot.
//!
//! The body is the JSON form of [`CaptchaFrontendConfig`] when a provider
//! is wired, or `null` otherwise. The endpoint is open by design — its
//! contents are explicitly the *public* surface (site key + provider name);
//! the secret never leaves the server.

use std::sync::Arc;

use axum::extract::FromRef;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use thewiki_core::{CaptchaFrontendConfig, CaptchaProvider};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::state::{AppState, AppStorage};

/// Thin wrapper around the captcha provider so it can be plucked out of
/// `AppState` via `FromRef` without dragging the rest of the state into
/// the handler signature.
#[derive(Clone)]
pub struct CaptchaState {
    /// The wired provider. `Arc<dyn …>` because [`CaptchaProvider`] is
    /// dyn-compatible (uses `async_trait`).
    pub provider: Arc<dyn CaptchaProvider>,
}

impl<S: AppStorage> FromRef<AppState<S>> for CaptchaState {
    fn from_ref(input: &AppState<S>) -> Self {
        Self {
            provider: Arc::clone(&input.captcha),
        }
    }
}

/// `GET /api/v1/captcha/config` — return the provider's frontend config or
/// `null` when no widget should be rendered.
///
/// The response is `application/json`; the body is either a
/// [`CaptchaFrontendConfig`] object or the JSON literal `null`. The SPA
/// branches on the latter to skip mounting the widget.
#[utoipa::path(
    get,
    path = "/config",
    responses(
        (status = 200, description = "Frontend CAPTCHA config (object) or `null` when no widget should be rendered.", body = Option<CaptchaFrontendConfig>),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "captcha",
)]
pub async fn captcha_config(State(state): State<CaptchaState>) -> Response {
    let body: Option<CaptchaFrontendConfig> = state.provider.frontend_config();
    Json(body).into_response()
}

/// Build the captcha router. Mounted under `/api/v1/captcha` by
/// [`crate::app::build_full_with_rate_limit_state`].
pub fn build_router<S: AppStorage>() -> OpenApiRouter<AppState<S>> {
    OpenApiRouter::new().routes(routes!(captcha_config))
}
