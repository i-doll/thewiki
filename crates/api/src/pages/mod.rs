//! Page CRUD routes (`/api/v1/pages*`).
//!
//! The handlers live in [`mod@routes`]; DTOs in [`dto`]. [`router`] wires
//! them into an [`axum::Router`] and returns the matching utoipa
//! [`utoipa_axum::router::OpenApiRouter`] so the OpenAPI spec stays in sync
//! with what is actually mounted.

pub mod dto;
pub mod routes;

use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::state::{AppState, AppStorage};

/// Build the page CRUD subrouter wrapped in a utoipa
/// [`OpenApiRouter`].
///
/// Mounted by [`crate::app::build`] under `/api/v1/pages`.
pub fn router<S: AppStorage>() -> OpenApiRouter<AppState<S>> {
    // Each `routes!(…)` group bundles handlers that share a URL path (axum
    // panics if two distinct method routers register the same path). Generic
    // handlers are turbofish-free here because the `routes!` macro expects a
    // bare `$handler:path` — axum infers `S` from the `OpenApiRouter`'s
    // state type.
    OpenApiRouter::new()
        .routes(routes!(routes::create_page, routes::list_pages))
        .routes(routes!(
            routes::get_page,
            routes::update_page,
            routes::delete_page,
        ))
}
