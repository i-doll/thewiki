//! Thumbnail generation for the media pipeline (#33).
//!
//! Three pre-rendered variants are produced for every static raster image
//! the upload pipeline accepts. The originals stay untouched on disk / in
//! the bucket; the variants live in `media_variants`.
//!
//! ## Size table
//!
//! | Variant | Max long edge | Output format |
//! |---------|---------------|---------------|
//! | small   | 320 px        | WebP          |
//! | medium  | 768 px        | WebP          |
//! | large   | 1280 px       | WebP          |
//!
//! The "max long edge" is applied to the longer of `(width, height)` so the
//! aspect ratio is preserved. Images smaller than a target size are not
//! upscaled — that would waste bytes without giving the browser anything
//! useful, and the original is already addressable via the no-`size`
//! endpoint.
//!
//! ## EXIF stripping
//!
//! `image` does not retain EXIF blocks when it re-encodes an image, so
//! decoding through [`image::ImageReader`] and writing back through
//! [`image::DynamicImage::write_to`] produces a clean copy. The privacy
//! property — no GPS, camera serial, or capture timestamps in the served
//! variant — is therefore inherent to the encoder rather than a special
//! scrub pass. This applies to every variant; the original blob is left
//! verbatim because the user submitted it and the dedup hash depends on
//! the exact bytes.
//!
//! ## Animation
//!
//! Animated GIF and animated WebP are detected via frame count and skipped:
//! re-encoding to a static WebP would silently drop the animation, and
//! re-encoding to an animated GIF / WebP would require a multi-frame
//! encoder we don't pull in. The `?size=` endpoint falls back to the
//! original for these — see [`crate::media::routes::get_media`].
//!
//! ## CPU model
//!
//! Decoding / resizing is CPU-bound and runs on the tokio blocking pool
//! via [`tokio::task::spawn_blocking`] (see [`generate_variants_async`]).
//! Failure is logged and swallowed so a hostile or malformed upload never
//! breaks the success path of `POST /api/v1/media`.

use std::io::Cursor;

use bytes::Bytes;
use image::codecs::gif::GifDecoder;
use image::codecs::webp::WebPDecoder;
use image::{AnimationDecoder, DynamicImage, ImageFormat, ImageReader};
use thewiki_core::MediaId;
use time::OffsetDateTime;
use tracing::warn;

use crate::media::MediaBackend;
use thewiki_storage::repo::{MediaVariant, MediaVariantRepository};

/// Variant labels stored in `media_variants.variant`.
pub const VARIANT_SMALL: &str = "small";
/// Medium variant label.
pub const VARIANT_MEDIUM: &str = "medium";
/// Large variant label.
pub const VARIANT_LARGE: &str = "large";

/// Ordered list of `(label, max_long_edge)` for the generated variants.
///
/// Order matters: bigger variants come last so a downstream `srcset`
/// renderer can iterate the slice and emit ascending widths in a single
/// pass.
pub const VARIANTS: [(&str, u32); 3] = [
    (VARIANT_SMALL, 320),
    (VARIANT_MEDIUM, 768),
    (VARIANT_LARGE, 1280),
];

/// Output content type for static WebP variants.
const WEBP_CONTENT_TYPE: &str = "image/webp";

/// IANA media types we know how to make thumbnails for.
///
/// SVG is intentionally absent — it's a vector format and re-encoding it
/// to a raster WebP would be lossy without giving us anything we can't
/// already serve from the original.
fn is_thumbnailable(content_type: &str) -> bool {
    matches!(
        content_type,
        "image/png" | "image/jpeg" | "image/jpg" | "image/gif" | "image/webp"
    )
}

/// Detect animated content. Animated frames need a multi-frame encoder we
/// don't ship; the upload pipeline serves the original for these.
fn is_animated(content_type: &str, bytes: &[u8]) -> bool {
    match content_type {
        "image/gif" => GifDecoder::new(Cursor::new(bytes))
            .map(|d| {
                let frames: Vec<_> = d.into_frames().take(2).collect();
                frames.len() > 1
            })
            .unwrap_or(false),
        "image/webp" => {
            // The WebP decoder reports `has_animation` once the headers
            // have been parsed. We instantiate it with a cheap clone of
            // the buffer; failure to parse means "not a valid animated
            // WebP", which is the answer we want.
            WebPDecoder::new(Cursor::new(bytes))
                .map(|d| d.has_animation())
                .unwrap_or(false)
        }
        _ => false,
    }
}

/// Outcome of a single variant render.
#[derive(Debug)]
pub struct RenderedVariant {
    /// Label — one of `small`/`medium`/`large`.
    pub label: &'static str,
    /// Output content type.
    pub content_type: &'static str,
    /// Re-encoded bytes ready to store.
    pub data: Bytes,
    /// Width of the rendered image.
    pub width: u32,
    /// Height of the rendered image.
    pub height: u32,
}

/// Decode `bytes` according to `content_type` and render every variant
/// whose `max_long_edge` makes sense for the source (no upscaling).
///
/// Returns `Ok(Vec::new())` when the input is animated, SVG, or otherwise
/// not a static raster the encoder understands — callers treat that as
/// "no variants to store" and serve the original. An `Err` is reserved
/// for true decode failures so the caller can log and move on without
/// silently swallowing a bug in the decoder path.
///
/// This is synchronous because the underlying `image` calls are. Run it
/// inside [`tokio::task::spawn_blocking`] from async contexts.
pub fn render_variants(
    content_type: &str,
    bytes: &[u8],
) -> Result<Vec<RenderedVariant>, image::ImageError> {
    if !is_thumbnailable(content_type) {
        return Ok(Vec::new());
    }
    if is_animated(content_type, bytes) {
        return Ok(Vec::new());
    }

    let format = match content_type {
        "image/png" => ImageFormat::Png,
        "image/jpeg" | "image/jpg" => ImageFormat::Jpeg,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::WebP,
        _ => return Ok(Vec::new()),
    };

    let reader = ImageReader::with_format(Cursor::new(bytes), format);
    let image = reader.decode()?;
    let src_w = image.width();
    let src_h = image.height();
    let long_edge = src_w.max(src_h);

    let mut out = Vec::with_capacity(VARIANTS.len());
    let mut last_long_edge = u32::MAX;
    for (label, max_edge) in VARIANTS {
        // No upscaling: if the source is already at or below the variant
        // ceiling, render at the source resolution. Skip the variant if
        // the previous (smaller) one ended up at the same dimensions,
        // since the second copy would just waste bytes.
        let effective_edge = max_edge.min(long_edge);
        if effective_edge == last_long_edge {
            continue;
        }
        last_long_edge = effective_edge;

        let rendered = render_one(&image, effective_edge, label)?;
        out.push(rendered);
    }
    Ok(out)
}

fn render_one(
    image: &DynamicImage,
    max_edge: u32,
    label: &'static str,
) -> Result<RenderedVariant, image::ImageError> {
    let src_w = image.width();
    let src_h = image.height();
    let scale = if src_w >= src_h {
        f64::from(max_edge) / f64::from(src_w)
    } else {
        f64::from(max_edge) / f64::from(src_h)
    };
    let new_w = ((f64::from(src_w) * scale).round() as u32).max(1);
    let new_h = ((f64::from(src_h) * scale).round() as u32).max(1);

    let resized = if new_w == src_w && new_h == src_h {
        // Same size — copy through without paying for a resample.
        image.clone()
    } else {
        image.resize(new_w, new_h, image::imageops::FilterType::Lanczos3)
    };

    let mut buf: Vec<u8> = Vec::new();
    resized.write_to(&mut Cursor::new(&mut buf), ImageFormat::WebP)?;
    Ok(RenderedVariant {
        label,
        content_type: WEBP_CONTENT_TYPE,
        data: Bytes::from(buf),
        width: new_w,
        height: new_h,
    })
}

/// Persist a slice of [`RenderedVariant`]s for `media_id` via
/// `variants_repo` + `backend`.
///
/// Best-effort: every failure is logged and swallowed. Used by the
/// upload path (after rendering synchronously inside a spawn_blocking
/// worker) and the regen-thumbnails CLI.
pub async fn store_variants<V>(
    media_id: MediaId,
    rendered: Vec<RenderedVariant>,
    variants_repo: &V,
    backend: &std::sync::Arc<dyn MediaBackend>,
) where
    V: MediaVariantRepository,
{
    if rendered.is_empty() {
        return;
    }
    let in_db_storage = backend.variants_inline_in_db();

    // Drop any stale variants before inserting (e.g. when we re-run after
    // a thumbnail spec change). Failure here doesn't block the new write
    // — `INSERT OR REPLACE` on the repository covers row collisions.
    if let Err(err) = variants_repo.delete_for_media(media_id).await {
        warn!(
            media_id = %media_id.into_uuid(),
            error = %err,
            "could not clear existing variants before insert",
        );
    }

    for variant in rendered {
        // For the in-DB backend we keep the variant bytes in the row.
        // For S3 we push the bytes to the bucket and store `data = NULL`.
        let data_for_row = if in_db_storage {
            Some(variant.data.clone())
        } else {
            None
        };
        if !in_db_storage
            && let Err(err) = backend
                .put_variant(media_id, variant.label, variant.data.clone())
                .await
        {
            warn!(
                media_id = %media_id.into_uuid(),
                variant = %variant.label,
                error = %err,
                "variant bucket upload failed",
            );
            continue;
        }
        let row = MediaVariant {
            media_id,
            variant: variant.label.to_owned(),
            content_type: variant.content_type.to_owned(),
            byte_size: variant.data.len() as u64,
            width: variant.width,
            height: variant.height,
            data: data_for_row,
            created_at: OffsetDateTime::now_utc(),
        };
        if let Err(err) = variants_repo.put(&row).await {
            warn!(
                media_id = %media_id.into_uuid(),
                variant = %variant.label,
                error = %err,
                "variant metadata insert failed",
            );
        }
    }
}

/// Render variants synchronously inside a `spawn_blocking` worker. On
/// failure (malformed input, decoder error) the result is `Ok(vec![])`
/// so callers can treat "no variants" uniformly and never block the
/// upload success path.
pub async fn render_in_blocking_pool(content_type: String, bytes: Bytes) -> Vec<RenderedVariant> {
    let join = tokio::task::spawn_blocking(move || render_variants(&content_type, &bytes)).await;
    match join {
        Ok(Ok(v)) => v,
        Ok(Err(err)) => {
            warn!(error = %err, "thumbnail render failed");
            Vec::new()
        }
        Err(join_err) => {
            warn!(error = %join_err, "thumbnail worker join failed");
            Vec::new()
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Sample 4x4 RGBA PNG, all opaque red. Hand-rolled to keep tests
    /// independent of the encoder we're exercising.
    fn red_png(w: u32, h: u32) -> Vec<u8> {
        let mut img = image::RgbaImage::new(w, h);
        for px in img.pixels_mut() {
            *px = image::Rgba([255, 0, 0, 255]);
        }
        let mut buf: Vec<u8> = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut buf), ImageFormat::Png)
            .expect("encode png");
        buf
    }

    #[test]
    fn render_skips_when_below_threshold_for_all_variants() {
        let bytes = red_png(100, 100);
        let variants = render_variants("image/png", &bytes).expect("render");
        // The source is 100x100 → every variant would render at the same
        // 100x100, so we collapse them and store exactly one row.
        assert_eq!(variants.len(), 1, "expected single collapsed variant");
        let v = &variants[0];
        assert_eq!(v.width, 100);
        assert_eq!(v.height, 100);
    }

    #[test]
    fn render_three_variants_for_large_source() {
        let bytes = red_png(2000, 1500);
        let variants = render_variants("image/png", &bytes).expect("render");
        assert_eq!(variants.len(), 3, "expected small/medium/large");
        for v in &variants {
            assert_eq!(v.content_type, WEBP_CONTENT_TYPE);
            assert!(!v.data.is_empty());
        }
        // Aspect ratio is preserved: 2000x1500 → small (max 320) →
        // 320x240.
        let small = variants.iter().find(|v| v.label == VARIANT_SMALL).unwrap();
        assert_eq!(small.width, 320);
        assert_eq!(small.height, 240);
        let medium = variants.iter().find(|v| v.label == VARIANT_MEDIUM).unwrap();
        assert_eq!(medium.width, 768);
        assert_eq!(medium.height, 576);
        let large = variants.iter().find(|v| v.label == VARIANT_LARGE).unwrap();
        assert_eq!(large.width, 1280);
        assert_eq!(large.height, 960);
    }

    #[test]
    fn render_returns_empty_for_svg() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"/>"#;
        let variants = render_variants("image/svg+xml", svg).expect("render");
        assert!(variants.is_empty());
    }
}
