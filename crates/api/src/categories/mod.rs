//! Categories + tags REST routes (`/api/v1/categories*`, `/api/v1/tags*`) — #29.

pub mod dto;
pub mod routes;

use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::state::{AppState, AppStorage};

/// Build the categories subrouter wrapped in a utoipa
/// [`OpenApiRouter`].
pub fn categories_router<S: AppStorage>() -> OpenApiRouter<AppState<S>> {
    OpenApiRouter::new()
        .routes(routes!(routes::list_categories, routes::create_category))
        .routes(routes!(routes::get_category))
}

/// Build the tags subrouter wrapped in a utoipa
/// [`OpenApiRouter`].
pub fn tags_router<S: AppStorage>() -> OpenApiRouter<AppState<S>> {
    OpenApiRouter::new()
        .routes(routes!(routes::list_tags))
        .routes(routes!(routes::get_tag))
}
