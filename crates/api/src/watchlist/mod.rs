//! Per-user watchlist REST routes (`/api/v1/watchlist*`) — #46.
//!
//! The watchlist is a simple subscription model: an authenticated user can
//! mark any page they can read as "watched", and the Atom feed at
//! `/api/v1/watchlist.atom` (see [`crate::feeds`]) syndicates the most recent
//! revision per watched page so external readers can poll one URL instead of
//! follow-up requests per page.
//!
//! Every endpoint requires a session ([`AuthSession`]). Reads are scoped to
//! the caller's own rows; there is no admin view across users.
//!
//! Audit log entries are written for both `watch` and `unwatch`. The Atom
//! feed read is intentionally **not** audited — feed polls are noisy and the
//! row already exists.
//!
//! [`AuthSession`]: crate::auth::AuthSession

pub mod dto;
pub mod routes;

use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::state::{AppState, AppStorage};

/// Build the watchlist subrouter wrapped in a utoipa [`OpenApiRouter`].
///
/// Mounted by [`crate::app::build_with_state`] and
/// [`crate::app::build_full`] under `/api/v1/watchlist`. The Atom variant is
/// owned by [`crate::feeds`] so the feed router can stay route-shape
/// homogeneous.
pub fn router<S: AppStorage>() -> OpenApiRouter<AppState<S>> {
    OpenApiRouter::new()
        .routes(routes!(routes::list_watchlist, routes::add_to_watchlist))
        .routes(routes!(routes::remove_from_watchlist))
}
