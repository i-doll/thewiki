//! Axum handlers for the media upload endpoints (#32).
//!
//! Three endpoints:
//!
//! - `POST /api/v1/media` — multipart upload. Validates the content type
//!   against the operator allowlist, enforces the size cap, computes the
//!   SHA-256 hash of the (possibly sanitised) bytes, and either inserts a
//!   new row or returns the existing row for the same hash (dedup).
//! - `GET /api/v1/media/{id}` — fetches the metadata, then streams the
//!   blob from the configured backend with an immutable cache header. The
//!   route is open like the other read endpoints.
//! - `DELETE /api/v1/media/{id}` — auth-gated (TODO #14 for role checks).
//!   Removes both the metadata row and the blob.
//!
//! SVG handling: an `image/svg+xml` upload runs through `ammonia` with a
//! restrictive allowlist (no `<script>`, no `on*` handlers) **before**
//! hashing. So two uploads of "the same" malicious SVG that scrub down to
//! identical sanitised bytes will dedupe; this is intentional.

use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, body::Bytes as AxumBytes};
use bytes::Bytes;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thewiki_core::{Media, MediaId};
use time::OffsetDateTime;
use utoipa::IntoParams;
use uuid::Uuid;

use crate::error::ApiError;
use crate::extractors::RequireAuth;
use crate::media::dto::MediaView;
use crate::media::thumbnail;
use crate::state::{AppState, AppStorage};
use thewiki_storage::repo::{MediaRepository, MediaVariantRepository};

/// Form field that carries the upload payload. Anything else in the
/// multipart request is ignored — clients are free to also pass an
/// `edit_summary` or similar in the future.
const FORM_FIELD: &str = "file";

/// `POST /api/v1/media` — accept a multipart upload, dedup by content hash,
/// store the blob in the configured backend.
///
/// Returns the existing `MediaView` (with status `200 OK`) when a row with
/// the same hash already exists; otherwise inserts a row and returns `200`.
/// We deliberately use `200` for both paths so the client doesn't have to
/// branch on "did the server actually persist this?" — the `id` and `url`
/// are stable either way.
#[utoipa::path(
    post,
    path = "",
    request_body(
        content_type = "multipart/form-data",
        description = "Single `file` field carrying the upload bytes.",
    ),
    params(
        ("cookie" = Option<String>, Header, description = "Session and CSRF cookies."),
        ("x-csrf-token" = Option<String>, Header, description = "Double-submit CSRF token."),
    ),
    responses(
        (status = 200, description = "Upload accepted (or deduped against an existing row)", body = MediaView),
        (status = 400, description = "Malformed multipart request", body = crate::error::ErrorBody),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 413, description = "Upload exceeds storage.media.max_upload_bytes", body = crate::error::ErrorBody),
        (status = 415, description = "Content type not in the configured allowlist", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
        (status = 500, description = "Backend write failed", body = crate::error::ErrorBody),
    ),
    tag = "media",
)]
pub async fn upload_media<S: AppStorage>(
    State(state): State<AppState<S>>,
    auth: RequireAuth,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<MediaView>), ApiError> {
    let backend = state
        .media_backend
        .as_ref()
        .ok_or_else(|| ApiError::Internal("media backend not configured".into()))?
        .clone();

    let max = state.media_config.max_upload_bytes;
    let allowed = &state.media_config.allowed_content_types;

    // Walk fields looking for the `file` part. Multiple fields are allowed
    // (we just pick the first `file`); the form is consumed lazily so a
    // malformed boundary surfaces as a 400 here rather than a 5xx later.
    let mut found: Option<(String, Option<String>, Bytes)> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::InvalidInput(format!("multipart: {e}")))?
    {
        if field.name() != Some(FORM_FIELD) {
            continue;
        }
        let content_type = field
            .content_type()
            .ok_or_else(|| {
                ApiError::InvalidInput(format!("`{FORM_FIELD}` is missing a Content-Type"))
            })?
            .to_owned();
        if !allowed.iter().any(|c| c == &content_type) {
            return Err(ApiError::UnsupportedMediaType(content_type));
        }
        let filename = field.file_name().map(str::to_owned);
        let data: AxumBytes = field
            .bytes()
            .await
            .map_err(|e| ApiError::InvalidInput(format!("multipart body read failed: {e}")))?;
        // Enforce the size cap *before* hashing / sanitising so a hostile
        // client can't soak CPU on a payload we're about to reject. The
        // body limit layer below the route handles the per-request cap too,
        // but we keep the field-level check so the error code is the right
        // 413 instead of the layer's stock response.
        if (data.len() as u64) > max {
            return Err(ApiError::PayloadTooLarge { limit: max });
        }
        found = Some((content_type, filename, data));
        break;
    }

    let Some((content_type, filename, raw)) = found else {
        return Err(ApiError::InvalidInput(format!(
            "missing `{FORM_FIELD}` form field"
        )));
    };

    // SVG carries embedded scripts; scrub anything dangerous before
    // storing. We hash the scrubbed bytes (not the original) so the
    // deduplication key reflects what the server actually serves.
    let (stored_bytes, stored_type) = if content_type == "image/svg+xml" {
        let raw_str = std::str::from_utf8(&raw)
            .map_err(|_| ApiError::InvalidInput("svg body is not valid UTF-8".into()))?;
        let cleaned = sanitize_svg(raw_str);
        (Bytes::from(cleaned.into_bytes()), content_type)
    } else {
        (raw, content_type)
    };

    if (stored_bytes.len() as u64) > max {
        // Sanitisation can only shrink, but defence-in-depth keeps the
        // invariant tight.
        return Err(ApiError::PayloadTooLarge { limit: max });
    }

    let mut hasher = Sha256::new();
    hasher.update(&stored_bytes);
    let digest = hasher.finalize();
    let mut content_hash = [0u8; 32];
    content_hash.copy_from_slice(&digest);

    // Dedup: a row with the same hash is the answer.
    if let Some(existing) = state
        .storage
        .media()
        .get_by_content_hash(&content_hash)
        .await?
    {
        return Ok((StatusCode::OK, Json(MediaView::from_media(&existing))));
    }

    let id = MediaId::new();
    let media = Media {
        id,
        content_hash,
        content_type: stored_type,
        byte_size: stored_bytes.len() as u64,
        original_filename: filename,
        uploaded_by: auth.user_id,
        created_at: OffsetDateTime::now_utc(),
    };

    // Order matters: insert the metadata row first so an in-flight
    // `GET /api/v1/media/{id}` from a different process can find it
    // immediately after we hand the id back. The blob write runs second;
    // if it fails we clean up the row to keep the table consistent.
    state.storage.media().create(&media).await?;
    if let Err(err) = backend.put(id, stored_bytes.clone()).await {
        // Best-effort cleanup; if it also fails we still want to surface
        // the original backend error to the caller.
        let _ = state.storage.media().delete(id).await;
        return Err(ApiError::from(err));
    }

    // Kick off thumbnail generation in a background task. The original
    // upload is already committed — failure here is logged and never
    // surfaced to the caller. (#33)
    let variants_storage = std::sync::Arc::clone(&state.storage);
    let variants_backend = backend.clone();
    let content_type_for_task = media.content_type.clone();
    tokio::spawn(async move {
        let rendered =
            thumbnail::render_in_blocking_pool(content_type_for_task, stored_bytes).await;
        if rendered.is_empty() {
            return;
        }
        let variants_repo = variants_storage.media_variants();
        thumbnail::store_variants(id, rendered, &variants_repo, &variants_backend).await;
    });

    Ok((StatusCode::OK, Json(MediaView::from_media(&media))))
}

/// Query parameters for `GET /api/v1/media/{id}` (#33).
///
/// `size` selects a pre-rendered thumbnail variant. Omitted: the original
/// is served (same behaviour as the #32 implementation).
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct GetMediaQuery {
    /// Variant label: `small` (≤320 px), `medium` (≤768 px), `large`
    /// (≤1280 px). Anything else returns 400 with the list of accepted
    /// values.
    #[serde(default)]
    pub size: Option<String>,
}

/// `GET /api/v1/media/{id}` — return the bytes for `id`.
///
/// Without `?size=`, serves the original verbatim. With a valid
/// `?size=`, serves the matching thumbnail variant (WebP). When the
/// requested variant doesn't exist (vector / animated source, or
/// generation failed) the original is returned as a graceful fallback.
///
/// Sets `Content-Type` to the stored MIME type, `Vary: Accept` (since
/// future content-negotiation can rotate the response without leaking a
/// stale cache) and an immutable `Cache-Control` since media is
/// content-addressed and will never change for the same id.
#[utoipa::path(
    get,
    path = "/{id}",
    params(
        ("id" = String, Path, description = "UUIDv7 of the media row"),
        GetMediaQuery,
    ),
    responses(
        (status = 200, description = "Blob bytes (raw)", content_type = "application/octet-stream"),
        (status = 400, description = "Malformed id or invalid size", body = crate::error::ErrorBody),
        (status = 404, description = "Media not found", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "media",
)]
pub async fn get_media<S: AppStorage>(
    State(state): State<AppState<S>>,
    Path(id_raw): Path<String>,
    Query(query): Query<GetMediaQuery>,
) -> Result<Response, ApiError> {
    let backend = state
        .media_backend
        .as_ref()
        .ok_or_else(|| ApiError::Internal("media backend not configured".into()))?
        .clone();

    let id = parse_media_id(&id_raw)?;
    let media = state.storage.media().get_by_id(id).await?;

    // Resolve the variant request if one was supplied. Unknown labels
    // are a client-side bug — return 400 with the canonical list so
    // operators don't have to guess.
    if let Some(size) = query.size.as_deref() {
        validate_variant_label(size)?;
        if let Some(variant) = state.storage.media_variants().get(id, size).await? {
            // DB backend keeps the bytes inline; S3 backend keeps them
            // in the bucket and stores `data = NULL` on the row.
            let bytes = match variant.data {
                Some(data) => data,
                None => backend.get_variant(id, size).await?,
            };
            return Ok(variant_response(&variant.content_type, bytes));
        }
        // Fall through to the original — operators don't need to special-
        // case animated GIFs or SVG in their consumers.
    }

    let bytes = backend.get(id).await?;
    Ok(variant_response(&media.content_type, bytes))
}

/// List of variant labels in canonical order. Used to build the 400
/// error body when a client supplies a typo.
const VARIANT_LABELS: [&str; 3] = [
    thumbnail::VARIANT_SMALL,
    thumbnail::VARIANT_MEDIUM,
    thumbnail::VARIANT_LARGE,
];

fn validate_variant_label(label: &str) -> Result<(), ApiError> {
    if VARIANT_LABELS.contains(&label) {
        Ok(())
    } else {
        Err(ApiError::InvalidInput(format!(
            "unknown size {label:?}; expected one of: {}",
            VARIANT_LABELS.join(", ")
        )))
    }
}

fn variant_response(content_type: &str, bytes: Bytes) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
            (header::VARY, "Accept"),
        ],
        bytes,
    )
        .into_response()
}

/// `DELETE /api/v1/media/{id}` — remove the metadata row and the blob.
///
/// Auth-gated through [`RequireAuth`] — anonymous callers always 401, even
/// if `auth.anonymous_edits = true`. A finer role check
/// (`Permissions::DELETE_MEDIA`) is a TODO once #14's role wiring lands.
#[utoipa::path(
    delete,
    path = "/{id}",
    params(
        ("id" = String, Path, description = "UUIDv7 of the media row"),
        ("cookie" = Option<String>, Header, description = "Session and CSRF cookies."),
        ("x-csrf-token" = Option<String>, Header, description = "Double-submit CSRF token."),
    ),
    responses(
        (status = 204, description = "Media deleted"),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 404, description = "Media not found", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "media",
)]
pub async fn delete_media<S: AppStorage>(
    State(state): State<AppState<S>>,
    _auth: RequireAuth,
    Path(id_raw): Path<String>,
) -> Result<StatusCode, ApiError> {
    let backend = state
        .media_backend
        .as_ref()
        .ok_or_else(|| ApiError::Internal("media backend not configured".into()))?
        .clone();

    let id = parse_media_id(&id_raw)?;
    // 404 fast-path: confirm the row exists so the response shape matches
    // the other delete endpoints. The blob delete is idempotent on its own.
    state.storage.media().get_by_id(id).await?;

    // Backend first, row second. If the backend delete fails we leave the
    // row alone so a retry succeeds; if the row delete fails after a
    // successful backend delete, the orphaned row is harmless and a follow-
    // up retry will clean it up.
    backend.delete(id).await?;
    // Best-effort: nuke variant bucket entries (S3 only — DB backend's
    // row cascade handles them already). Failure here would leave orphan
    // files but is logged rather than blocking the row delete.
    if let Err(err) = backend.delete_variants(id).await {
        tracing::warn!(
            media_id = %id.into_uuid(),
            error = %err,
            "could not clean up variant bucket entries on delete",
        );
    }
    state.storage.media().delete(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Run an SVG body through `ammonia` with a tight allowlist: no `<script>`,
/// no `on*` handlers, no `<foreignObject>`. Keeps the shape (`<svg>` and
/// its primitives) so the upload is still useful but strips attack surface.
fn sanitize_svg(input: &str) -> String {
    use std::collections::HashSet;

    let mut tags: HashSet<&str> = HashSet::new();
    for t in [
        "svg",
        "g",
        "path",
        "rect",
        "circle",
        "ellipse",
        "line",
        "polyline",
        "polygon",
        "title",
        "desc",
        "defs",
        "use",
        "symbol",
        "linearGradient",
        "radialGradient",
        "stop",
        "text",
        "tspan",
        "marker",
        "pattern",
        "clipPath",
        "mask",
        "filter",
        "feGaussianBlur",
        "feOffset",
        "feMerge",
        "feMergeNode",
        "feColorMatrix",
        "feComposite",
        "feFlood",
        "feBlend",
    ] {
        tags.insert(t);
    }
    let mut generic_attrs: HashSet<&str> = HashSet::new();
    for a in [
        "id",
        "class",
        "viewBox",
        "xmlns",
        "width",
        "height",
        "x",
        "y",
        "cx",
        "cy",
        "r",
        "rx",
        "ry",
        "d",
        "fill",
        "fill-opacity",
        "stroke",
        "stroke-width",
        "stroke-opacity",
        "stroke-linecap",
        "stroke-linejoin",
        "stroke-dasharray",
        "opacity",
        "transform",
        "points",
        "x1",
        "y1",
        "x2",
        "y2",
        "offset",
        "stop-color",
        "stop-opacity",
        "style",
        "preserveAspectRatio",
        "gradientUnits",
        "gradientTransform",
        "patternUnits",
        "patternTransform",
        "clip-path",
        "mask",
        "filter",
        "marker-start",
        "marker-mid",
        "marker-end",
        "text-anchor",
        "font-size",
        "font-family",
        "font-weight",
        "letter-spacing",
        "dominant-baseline",
    ] {
        generic_attrs.insert(a);
    }

    ammonia::Builder::new()
        .tags(tags)
        .generic_attributes(generic_attrs)
        // `ammonia` strips elements outside its tag set by default — that
        // includes `<script>` and `<foreignObject>`. Event handler
        // attributes (`onclick`, etc.) are also dropped because they
        // aren't in `generic_attributes`. We explicitly leave URL
        // attributes out: SVG `xlink:href` and friends are a vector for
        // `javascript:` schemes, and the simpler answer is "no remote
        // refs in stored SVGs".
        .clean(input)
        .to_string()
}

/// Parse a UUID path segment into a [`MediaId`], mapping malformed input to
/// a 400. Returning 400 (vs 404) is deliberate — a malformed id is a
/// client bug, not a missing resource.
fn parse_media_id(raw: &str) -> Result<MediaId, ApiError> {
    Uuid::parse_str(raw)
        .map(MediaId::from_uuid)
        .map_err(|err| ApiError::InvalidInput(format!("media id: {err}")))
}

/// Hard upper bound on the multipart body for this route.
///
/// `DefaultBodyLimit` caps the entire request body, separate from the
/// per-field [`MediaConfig::max_upload_bytes`] check. We pick a generous
/// ceiling (2 GiB) so the per-field check (which honours the operator's
/// configured limit) is the user-visible cap; this exists only so a
/// pathological multipart envelope can't blow up the worker.
const ROUTER_BODY_LIMIT: usize = 2 * 1024 * 1024 * 1024;

/// Wire up just the multipart body-limit layer — kept in this module so the
/// router builder in `mod.rs` can apply it route-locally rather than over
/// the whole API.
pub fn body_limit_layer() -> DefaultBodyLimit {
    DefaultBodyLimit::max(ROUTER_BODY_LIMIT)
}
