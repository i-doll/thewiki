//! Administrative endpoints under `/api/v1/admin/*`.
//!
//! Grouped here so the broader admin UI surface (#47) can hang new
//! sub-routers off the same prefix without further refactoring. Each
//! admin endpoint is gated by the appropriate [`Permissions`] bit on the
//! resolved [`AuthSession`]; the gate lives inside the handler rather than
//! at the router level so each endpoint can pick the right bit and surface
//! a consistent `403 forbidden` body.
//!
//! [`Permissions`]: thewiki_core::Permissions
//! [`AuthSession`]: crate::auth::AuthSession

pub mod blocklist;

use utoipa_axum::router::OpenApiRouter;

use crate::state::{AppState, AppStorage};

/// Build the admin subrouter mounted at `/api/v1/admin`.
pub fn router<S: AppStorage>() -> OpenApiRouter<AppState<S>> {
    OpenApiRouter::new()
        .nest("/blocklist/ip", blocklist::ip_router::<S>())
        .nest("/blocklist/url", blocklist::url_router::<S>())
}
