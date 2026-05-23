//! In-app inbox routes (`/api/v1/notifications*`) — #40.

pub mod dto;
pub mod routes;

use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::state::{AppState, AppStorage};

/// Build the notifications subrouter.
///
/// Mounted by [`crate::app::api_router`] under
/// `/api/v1/notifications`.
pub fn router<S: AppStorage>() -> OpenApiRouter<AppState<S>> {
    OpenApiRouter::new()
        .routes(routes!(routes::list_notifications))
        .routes(routes!(routes::mark_read))
}
