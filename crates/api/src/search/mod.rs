//! Search REST routes (`/api/v1/search`).
//!
//! Read-only endpoint that fronts the Tantivy index built by
//! [`thewiki_search`]. The handler lives in [`routes`]; the wire shapes in
//! [`dto`]. The router wires the handler into a utoipa-aware router so the
//! OpenAPI document tracks the deployed surface.

pub mod dto;
pub mod routes;

use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::state::{AppState, AppStorage};

/// Build the search subrouter wrapped in a utoipa
/// [`OpenApiRouter`].
///
/// Mounted by [`crate::app::build_with_state`] and
/// [`crate::app::build_full`] under `/api/v1/search`.
pub fn router<S: AppStorage>() -> OpenApiRouter<AppState<S>> {
    OpenApiRouter::new().routes(routes!(routes::search))
}
