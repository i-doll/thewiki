//! Media upload pipeline (#32).
//!
//! Routes live in [`routes`], wire shapes in [`dto`], and the storage
//! backend abstraction (DB blobs vs S3-compatible) in [`storage`]. The
//! [`router`] function returns the utoipa-aware subrouter that
//! [`crate::app`] mounts at `/api/v1/media`.

pub mod dto;
pub mod routes;
pub mod storage;
pub mod thumbnail;

pub use storage::{
    DbMediaBackend, MediaBackend, MediaBackendError, S3MediaBackend, build_media_backend,
};
pub use thumbnail::{VARIANT_LARGE, VARIANT_MEDIUM, VARIANT_SMALL, VARIANTS};

use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::state::{AppState, AppStorage};

/// Build the media upload subrouter wrapped in a utoipa [`OpenApiRouter`].
///
/// Mounted by [`crate::app`] under `/api/v1/media`.
pub fn router<S: AppStorage>() -> OpenApiRouter<AppState<S>> {
    OpenApiRouter::new()
        .routes(routes!(routes::upload_media))
        .routes(routes!(routes::get_media, routes::delete_media))
        // Cap the multipart request body at a generous ceiling so a
        // malformed envelope doesn't soak memory. The per-field
        // `storage.media.max_upload_bytes` cap is the user-visible limit;
        // see `routes::body_limit_layer`.
        .layer(routes::body_limit_layer())
}
