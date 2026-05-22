//! Administrative audit-log routes (`/api/v1/audit-log`).

pub mod routes;

use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::state::{AppState, AppStorage};

/// Build the audit-log subrouter.
pub fn router<S: AppStorage>() -> OpenApiRouter<AppState<S>> {
    OpenApiRouter::new()
        .routes(routes!(routes::list_audit_log))
        .routes(routes!(routes::audit_log_atom))
}
