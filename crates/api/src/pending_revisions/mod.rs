//! Edit approval queue routes (`/api/v1/pending-revisions*`) — #40.
//!
//! Reviewers (callers with [`thewiki_core::Permissions::REVIEW_EDITS`] or
//! [`thewiki_core::Permissions::MANAGE_USERS`]) list queued edits, fetch a
//! single row with its body + the parent revision body for a diff view,
//! and act on rows via the approve / reject endpoints.

pub mod dto;
pub mod routes;

use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::state::{AppState, AppStorage};

/// Build the pending-revisions subrouter.
///
/// Mounted by [`crate::app::api_router`] under
/// `/api/v1/pending-revisions`.
pub fn router<S: AppStorage>() -> OpenApiRouter<AppState<S>> {
    OpenApiRouter::new()
        .routes(routes!(routes::list_pending))
        .routes(routes!(routes::get_pending))
        .routes(routes!(routes::approve_pending))
        .routes(routes!(routes::reject_pending))
}
