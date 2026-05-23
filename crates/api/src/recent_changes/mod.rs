//! Recent-changes feed routes (`/api/v1/recent-changes`).
//!
//! A read-only chronological view of every edit across the wiki, newest first.
//! See [`mod@routes`] for the handler and [`dto`] for the wire shapes.
//! [`router`] wires the handler into an
//! [`utoipa_axum::router::OpenApiRouter`] so the OpenAPI document stays in
//! sync with what is mounted.
//!
//! The Atom syndication variant of this feed lives in [`crate::feeds`]
//! alongside the per-namespace and watchlist feeds (#46).

pub mod dto;
pub mod routes;

use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::state::{AppState, AppStorage};

/// Build the recent-changes subrouter wrapped in a utoipa
/// [`OpenApiRouter`].
///
/// Mounted by [`crate::app::build_with_state`] and
/// [`crate::app::build_full`] under `/api/v1/recent-changes`.
pub fn router<S: AppStorage>() -> OpenApiRouter<AppState<S>> {
    OpenApiRouter::new().routes(routes!(routes::list_recent_changes))
}
