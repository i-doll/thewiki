//! SPA asset embedding (#16).
//!
//! In release builds, `rust-embed` reads `web/dist/` at compile time and bakes
//! every file into the binary. In debug builds, it reads from disk on each
//! request — so editing `web/dist/index.html` and reloading the browser
//! works without rebuilding the Rust code. (Toggle with the `debug-embed`
//! feature on `rust-embed` if you want the release-mode behaviour locally.)
//!
//! Routing model: this module exposes a `Router` suitable for
//! `.fallback_service(...)`. It catches every request that didn't match an
//! API route, looks for the corresponding file under the embed root, and
//! falls back to `index.html` for SPA history routes (any path that doesn't
//! map to a file).
//!
//! Cache strategy:
//!
//! - `/assets/...` — Vite emits content-hashed filenames here, so we mark
//!   them `Cache-Control: public, max-age=31536000, immutable`. Browsers
//!   never revalidate them; a content change ships a fresh hash.
//! - Everything else (incl. `index.html` and any top-level files) —
//!   `Cache-Control: no-cache`. We still serve a strong validator (ETag) so
//!   the browser can revalidate cheaply.
//!
//! `ETag`s are derived from the SHA-256 hash `rust-embed` already computes
//! per file (`metadata().sha256_hash()`), formatted as a hex-encoded strong
//! validator. If a future `rust-embed` version stops exposing that, fall
//! back to a `Sha256` digest of the bytes here — but this keeps the cost at
//! a `to_hex` per request.

use axum::{
    Router,
    body::Body,
    http::{HeaderValue, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::any,
};
use rust_embed::RustEmbed;

/// Compile-time-embedded view of `web/dist/`.
///
/// Paths inside the struct are relative to `web/dist/`:
/// `EmbeddedAssets::get("index.html")`, `EmbeddedAssets::get("assets/foo.js")`.
///
/// The folder path is relative to this crate's `Cargo.toml` (i.e.
/// `crates/api/Cargo.toml`), hence `../../web/dist`. The corresponding
/// `build.rs` writes a placeholder `index.html` if the directory is missing,
/// so a cold `cargo build` after a fresh clone never fails on this derive.
#[derive(RustEmbed)]
#[folder = "../../web/dist"]
#[prefix = ""]
pub struct EmbeddedAssets;

/// `Cache-Control` value for hashed Vite assets under `/assets/`.
const CACHE_IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// `Cache-Control` value for everything else (HTML entry point, top-level
/// files like `robots.txt`). We still return an `ETag` so revalidation is
/// cheap.
const CACHE_NO_CACHE: &str = "no-cache";

/// Build the SPA-serving router suitable for `.fallback_service(...)`.
///
/// The router matches every method + path combination via `any(...)` and
/// dispatches into [`serve_asset`]. `Router` is itself `#[must_use]`, so we
/// don't need a redundant attribute here.
pub fn static_routes() -> Router {
    Router::new().fallback(any(serve_asset))
}

/// Resolve `uri` to an embedded asset.
///
/// Behaviour:
///
/// 1. Strip the leading `/` from `uri.path()`. Empty path (`/`) becomes
///    `index.html`.
/// 2. Look up the file via [`EmbeddedAssets::get`]. On hit, respond with the
///    body + computed headers.
/// 3. On miss, fall back to `index.html` so SPA history routes
///    (`/wiki/Some-Page`) render the React shell. Return `200` rather than
///    `404` — the client-side router picks up the path and renders the
///    actual not-found state if needed.
/// 4. If `index.html` itself is missing (i.e. nothing was embedded), return
///    `404`. That should only happen in a broken build.
async fn serve_asset(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let lookup = if path.is_empty() { "index.html" } else { path };

    if let Some(file) = EmbeddedAssets::get(lookup) {
        return render(lookup, file);
    }

    // SPA history fallback: any path that didn't match a real file gets the
    // entry-point HTML so the client router can take over.
    match EmbeddedAssets::get("index.html") {
        Some(file) => render("index.html", file),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

/// Build a `Response` for an embedded file at `path`, choosing `Content-Type`,
/// `Cache-Control`, and `ETag` headers.
fn render(path: &str, file: rust_embed::EmbeddedFile) -> Response {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let content_type = mime.as_ref();

    let cache_control = if path.starts_with("assets/") {
        CACHE_IMMUTABLE
    } else {
        CACHE_NO_CACHE
    };

    // Strong ETag from rust-embed's pre-computed SHA-256. The hash is a
    // `[u8; 32]`, formatted as lowercase hex.
    let etag = format!("\"{}\"", hex_of(&file.metadata.sha256_hash()));

    let mut response = Response::builder().status(StatusCode::OK);

    // `HeaderValue::from_str` cannot fail for any of these — `content_type`
    // comes from `mime`'s static table, `cache_control` is a compile-time
    // constant, and `etag` is hex + quotes. We still avoid `unwrap` /
    // `expect` to honour the workspace lint policy.
    if let Some(headers) = response.headers_mut() {
        if let Ok(v) = HeaderValue::from_str(content_type) {
            headers.insert(header::CONTENT_TYPE, v);
        }
        if let Ok(v) = HeaderValue::from_str(cache_control) {
            headers.insert(header::CACHE_CONTROL, v);
        }
        if let Ok(v) = HeaderValue::from_str(&etag) {
            headers.insert(header::ETAG, v);
        }
    }

    match response.body(Body::from(file.data.into_owned())) {
        Ok(r) => r,
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to build response",
        )
            .into_response(),
    }
}

/// Lowercase hex encoding of a byte slice. We hand-roll this rather than
/// pulling in the `hex` crate for a one-liner — keeps the dependency surface
/// small.
fn hex_of(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn hex_of_pads_each_byte() {
        assert_eq!(hex_of(&[0x00, 0x0f, 0xff]), "000fff");
    }
}
