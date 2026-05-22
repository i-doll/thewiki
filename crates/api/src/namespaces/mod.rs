//! Namespace CRUD routes (`/api/v1/namespaces*`) — added in #28.
//!
//! Namespaces partition the page space (`Main`, `Help`, `User`, …). The
//! `Main` namespace is implicit and seeded at server boot via
//! [`NamespaceRepository::get_or_create_default`]; the routes here let an
//! administrator create additional ones, list them, rename their display
//! label, and delete empty ones.
//!
//! All mutations require [`Permissions::MANAGE_NAMESPACES`]; reads are open
//! by design (the page list / search endpoints already need to know which
//! namespaces exist).

pub mod dto;
pub mod routes;

use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::state::{AppState, AppStorage};

/// Build the namespace CRUD subrouter wrapped in a utoipa
/// [`OpenApiRouter`].
///
/// Mounted by [`crate::app::build_with_state`] and
/// [`crate::app::build_full`] under `/api/v1/namespaces`.
pub fn router<S: AppStorage>() -> OpenApiRouter<AppState<S>> {
    OpenApiRouter::new()
        .routes(routes!(routes::list_namespaces, routes::create_namespace))
        .routes(routes!(routes::update_namespace, routes::delete_namespace))
}
