//! Integration tests for the thumbnail pipeline (#33).
//!
//! Each test boots a fresh in-memory SQLite, seeds a user + session,
//! drives a multipart upload through the real router, and then waits
//! for the spawned thumbnail task to land its rows in `media_variants`
//! before asserting on `GET /api/v1/media/<id>?size=…`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use image::{ImageFormat, RgbaImage};
use serde_json::Value;
use thewiki_api::AppState;
use thewiki_api::app;
use thewiki_api::auth::password::Argon2Hasher;
use thewiki_api::auth::state::AuthState;
use thewiki_api::config::{Argon2Config, Config, MediaConfig, StorageBackend};
use thewiki_api::media::build_media_backend;
use thewiki_core::{MediaId, User, UserId, Username};
use thewiki_storage::repo::{MediaVariantRepository, SessionRepository, UserRepository};
use thewiki_storage::sqlite::{SqliteOptions, SqliteStorage};
use time::OffsetDateTime;
use tower::ServiceExt;
use uuid::Uuid;

const TEST_CSRF: &str = "test-csrf-token-fixed-value-32b";
const BOUNDARY: &str = "------thewikitestboundary";

fn test_argon2() -> Argon2Config {
    Argon2Config {
        memory_kib: 19_456,
        iterations: 2,
        parallelism: 1,
    }
}

fn disabled_rate_limit() -> thewiki_api::config::RateLimitConfig {
    let mut cfg = Config::defaults().rate_limit;
    cfg.enabled = false;
    cfg
}

/// Build a router with the media pipeline wired against the in-DB backend.
async fn fresh_app() -> (Router, UserId, SqliteStorage) {
    let storage = SqliteStorage::new(
        "sqlite::memory:",
        SqliteOptions {
            max_connections: 1,
            acquire_timeout: Duration::from_secs(5),
            foreign_keys: true,
        },
    )
    .await
    .expect("open + migrate sqlite");

    let user = User {
        id: UserId::new(),
        username: Username::new("uploader").expect("valid username"),
        email: None,
        display_name: Some("Uploader".into()),
        created_at: OffsetDateTime::now_utc() - time::Duration::days(30),
        last_login_at: None,
    };
    storage
        .users()
        .create(&user, None)
        .await
        .expect("seed user");

    let hasher = Arc::new(Argon2Hasher::new(test_argon2()).expect("hasher"));
    let auth_cfg = Config::defaults().auth;
    let auth_state = AuthState::new(
        storage.clone(),
        hasher,
        Duration::from_secs(60 * 60),
        false,
        auth_cfg.clone(),
    );

    // Bump the upload size cap so a 2000x1500 PNG fits.
    let media_config = MediaConfig {
        max_upload_bytes: 50 * 1024 * 1024,
        ..MediaConfig::default()
    };

    let mut state = AppState::new(storage.clone(), auth_cfg).with_auth_state(auth_state.clone());
    let backend = build_media_backend(&StorageBackend::Db, Arc::clone(&state.storage))
        .expect("DB backend init");
    state = state.with_media(media_config, backend);

    let router = app::build_full(state, auth_state, false, disabled_rate_limit());
    (router, user.id, storage)
}

async fn seed_session(storage: &SqliteStorage, user_id: UserId) -> String {
    let session = storage
        .sessions()
        .create(user_id, Duration::from_secs(60 * 60), None, None)
        .await
        .expect("seed session");
    session.id.into_uuid().to_string()
}

fn multipart_file(
    field_name: &str,
    filename: &str,
    content_type: &str,
    bytes: &[u8],
) -> (String, Vec<u8>) {
    let mut body = Vec::new();
    let preamble = format!(
        "--{BOUNDARY}\r\n\
         Content-Disposition: form-data; name=\"{field_name}\"; filename=\"{filename}\"\r\n\
         Content-Type: {content_type}\r\n\
         \r\n"
    );
    body.extend_from_slice(preamble.as_bytes());
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    let header_value = format!("multipart/form-data; boundary={BOUNDARY}");
    (header_value, body)
}

async fn upload(
    router: &Router,
    session: &str,
    filename: &str,
    content_type: &str,
    bytes: &[u8],
) -> Value {
    let (ct_header, body) = multipart_file("file", filename, content_type, bytes);
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/media")
        .header(header::CONTENT_TYPE, ct_header)
        .header(
            header::COOKIE,
            format!("thewiki_session={session}; thewiki_csrf={TEST_CSRF}"),
        )
        .header("x-csrf-token", TEST_CSRF)
        .body(Body::from(body))
        .expect("build request");
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("router responded");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes()
        .to_vec();
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| panic!("response not JSON: {:?}", String::from_utf8_lossy(&bytes)))
}

/// Encode a solid-coloured RGBA PNG of the given dimensions.
fn solid_png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    let mut img = RgbaImage::new(w, h);
    for px in img.pixels_mut() {
        *px = image::Rgba(rgba);
    }
    let mut buf = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut buf), ImageFormat::Png)
        .expect("encode png");
    buf
}

fn solid_jpeg(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    let mut img = image::RgbImage::new(w, h);
    for px in img.pixels_mut() {
        *px = image::Rgb(rgb);
    }
    let mut buf = Vec::new();
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut std::io::Cursor::new(&mut buf), ImageFormat::Jpeg)
        .expect("encode jpeg");
    buf
}

/// Tiny 2-frame animated GIF. Hand-rolled so the test doesn't pull in a
/// GIF encoder dependency. Two frames of a 1x1 image, palette of red /
/// green, both with 1-tick delays.
fn animated_gif() -> Vec<u8> {
    // Global palette: red, green (2 entries, 6 bytes RGB).
    let palette: Vec<u8> = vec![255, 0, 0, 0, 255, 0];
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut encoder = gif::Encoder::new(&mut buf, 1, 1, &palette).expect("gif encoder");
        encoder.set_repeat(gif::Repeat::Infinite).expect("repeat");
        let frame_a = gif::Frame {
            width: 1,
            height: 1,
            buffer: std::borrow::Cow::Owned(vec![0]),
            delay: 5,
            ..Default::default()
        };
        let frame_b = gif::Frame {
            width: 1,
            height: 1,
            buffer: std::borrow::Cow::Owned(vec![1]),
            delay: 5,
            ..Default::default()
        };
        encoder.write_frame(&frame_a).expect("frame a");
        encoder.write_frame(&frame_b).expect("frame b");
    }
    buf
}

/// Block until the variants repository reports at least one row for
/// `media_id` (or the timeout elapses). The upload handler spawns the
/// thumbnail task on the tokio runtime, so the row lands a few
/// milliseconds after the response. We poll rather than sleep to keep
/// the test snappy when the worker beat us to it.
async fn await_variants(storage: &SqliteStorage, media_id: MediaId) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let variants = storage.media_variants();
        if variants
            .get(media_id, "small")
            .await
            .ok()
            .flatten()
            .is_some()
        {
            return true;
        }
        if std::time::Instant::now() > deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Same shape as `await_variants` but for the empty case — we sleep a
/// short window so the worker has a chance to *not* produce variants
/// before we assert their absence. 500 ms is generous on CI.
async fn await_no_variants(storage: &SqliteStorage, media_id: MediaId) -> bool {
    tokio::time::sleep(Duration::from_millis(500)).await;
    let variants = storage.media_variants();
    for size in ["small", "medium", "large"] {
        if variants.get(media_id, size).await.ok().flatten().is_some() {
            return false;
        }
    }
    true
}

fn parse_media_id(view: &Value) -> MediaId {
    let id = view["id"].as_str().expect("id field");
    MediaId::from_uuid(Uuid::parse_str(id).expect("uuid"))
}

#[tokio::test]
async fn upload_100x100_png_generates_one_collapsed_variant() {
    // 100x100 is below the small ceiling (320); every target collapses to
    // the same 100x100 render, so we keep one row.
    let (router, user_id, storage) = fresh_app().await;
    let session = seed_session(&storage, user_id).await;
    let view = upload(
        &router,
        &session,
        "tiny.png",
        "image/png",
        &solid_png(100, 100, [10, 20, 30, 255]),
    )
    .await;
    let id = parse_media_id(&view);
    assert!(await_variants(&storage, id).await, "variants did not land");

    let variants = storage.media_variants();
    let small = variants.get(id, "small").await.expect("get small");
    let medium = variants.get(id, "medium").await.expect("get medium");
    let large = variants.get(id, "large").await.expect("get large");

    // Exactly one row stored — the small one wins because variants are
    // iterated in small/medium/large order and identical renders are
    // collapsed.
    assert!(small.is_some());
    assert!(medium.is_none());
    assert!(large.is_none());
    let small = small.expect("small row");
    assert_eq!(small.width, 100);
    assert_eq!(small.height, 100);
    assert_eq!(small.content_type, "image/webp");
}

#[tokio::test]
async fn upload_2000x1500_jpeg_produces_three_variants_with_expected_dimensions() {
    let (router, user_id, storage) = fresh_app().await;
    let session = seed_session(&storage, user_id).await;
    let view = upload(
        &router,
        &session,
        "big.jpg",
        "image/jpeg",
        &solid_jpeg(2000, 1500, [200, 100, 50]),
    )
    .await;
    let id = parse_media_id(&view);
    assert!(await_variants(&storage, id).await, "variants did not land");

    let variants = storage.media_variants();
    let small = variants
        .get(id, "small")
        .await
        .expect("small")
        .expect("row");
    let medium = variants
        .get(id, "medium")
        .await
        .expect("medium")
        .expect("row");
    let large = variants
        .get(id, "large")
        .await
        .expect("large")
        .expect("row");
    assert_eq!(small.width, 320);
    assert_eq!(small.height, 240);
    assert_eq!(small.content_type, "image/webp");
    assert_eq!(medium.width, 768);
    assert_eq!(medium.height, 576);
    assert_eq!(large.width, 1280);
    assert_eq!(large.height, 960);
}

#[tokio::test]
async fn get_media_with_size_small_returns_variant_bytes() {
    let (router, user_id, storage) = fresh_app().await;
    let session = seed_session(&storage, user_id).await;
    let view = upload(
        &router,
        &session,
        "wide.png",
        "image/png",
        &solid_png(800, 600, [10, 200, 50, 255]),
    )
    .await;
    let id = parse_media_id(&view);
    assert!(await_variants(&storage, id).await);

    let url = format!("/api/v1/media/{}?size=small", id.into_uuid());
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(&url)
                .body(Body::empty())
                .expect("build get"),
        )
        .await
        .expect("router responded");
    assert_eq!(resp.status(), StatusCode::OK);
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .map(str::to_owned);
    assert_eq!(content_type.as_deref(), Some("image/webp"));
    let vary = resp
        .headers()
        .get(header::VARY)
        .and_then(|h| h.to_str().ok())
        .map(str::to_owned);
    assert_eq!(vary.as_deref(), Some("Accept"));
    let body = resp.into_body().collect().await.expect("body").to_bytes();
    // Smoke check the WebP signature.
    assert_eq!(&body[..4], b"RIFF");
    assert_eq!(&body[8..12], b"WEBP");
}

#[tokio::test]
async fn get_media_with_invalid_size_returns_400_listing_valid_sizes() {
    let (router, user_id, storage) = fresh_app().await;
    let session = seed_session(&storage, user_id).await;
    let view = upload(
        &router,
        &session,
        "tiny.png",
        "image/png",
        &solid_png(50, 50, [0, 0, 0, 255]),
    )
    .await;
    let url = format!("/api/v1/media/{}?size=huge", view["id"].as_str().unwrap());
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(&url)
                .body(Body::empty())
                .expect("build get"),
        )
        .await
        .expect("router responded");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = resp.into_body().collect().await.expect("body").to_bytes();
    let json: Value = serde_json::from_slice(&body).expect("json");
    let msg = json["message"].as_str().unwrap_or_default();
    assert!(msg.contains("small"));
    assert!(msg.contains("medium"));
    assert!(msg.contains("large"));
}

#[tokio::test]
async fn get_with_size_falls_back_to_original_when_variant_missing() {
    let (router, user_id, storage) = fresh_app().await;
    let session = seed_session(&storage, user_id).await;
    let svg_body =
        b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\"></svg>".to_vec();
    let view = upload(&router, &session, "vec.svg", "image/svg+xml", &svg_body).await;
    let id = parse_media_id(&view);
    // SVG produces no variants — confirm the row stays empty, then
    // assert the fallback path serves the original verbatim.
    assert!(await_no_variants(&storage, id).await);

    let url = format!("/api/v1/media/{}?size=small", id.into_uuid());
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(&url)
                .body(Body::empty())
                .expect("build get"),
        )
        .await
        .expect("router responded");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes()
        .to_vec();
    let ct = resp_content_type_after(&router, id, "small").await;
    assert_eq!(ct.as_deref(), Some("image/svg+xml"));
    // Sanitiser may rewrite the SVG, but the response is the stored
    // bytes — assert via prefix that the SVG opener is still there.
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("<svg"), "expected SVG body, got: {text}");
}

async fn resp_content_type_after(router: &Router, id: MediaId, size: &str) -> Option<String> {
    let url = format!("/api/v1/media/{}?size={}", id.into_uuid(), size);
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(&url)
                .body(Body::empty())
                .expect("build get"),
        )
        .await
        .expect("router responded");
    resp.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .map(str::to_owned)
}

#[tokio::test]
async fn upload_animated_gif_skips_thumbnails_and_size_serves_original() {
    let (router, user_id, storage) = fresh_app().await;
    let session = seed_session(&storage, user_id).await;
    let gif_bytes = animated_gif();
    let view = upload(&router, &session, "anim.gif", "image/gif", &gif_bytes).await;
    let id = parse_media_id(&view);
    assert!(await_no_variants(&storage, id).await);

    let url = format!("/api/v1/media/{}?size=small", id.into_uuid());
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(&url)
                .body(Body::empty())
                .expect("build get"),
        )
        .await
        .expect("router responded");
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .map(str::to_owned);
    assert_eq!(ct.as_deref(), Some("image/gif"));
    let body = resp.into_body().collect().await.expect("body").to_bytes();
    assert_eq!(body.as_ref(), gif_bytes.as_slice());
}

#[tokio::test]
async fn upload_svg_skips_thumbnails() {
    let (router, user_id, storage) = fresh_app().await;
    let session = seed_session(&storage, user_id).await;
    let svg_body = b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\"></svg>";
    let view = upload(&router, &session, "vec.svg", "image/svg+xml", svg_body).await;
    let id = parse_media_id(&view);
    assert!(await_no_variants(&storage, id).await);
}

#[tokio::test]
async fn served_variant_has_no_exif_after_re_encode() {
    let (router, user_id, storage) = fresh_app().await;
    let session = seed_session(&storage, user_id).await;
    // Build a JPEG with a synthetic EXIF block carrying a GPS-like
    // identifier. We compose APP1 manually because the `image` encoder
    // doesn't write EXIF on the way out (which is the property under
    // test).
    let mut jpeg = solid_jpeg(800, 600, [80, 90, 100]);
    inject_exif_after_soi(&mut jpeg);

    // Sanity check: kamadak-exif must see EXIF on the *source* bytes,
    // otherwise the negative assertion below would be a tautology.
    let reader = exif::Reader::new();
    let parsed = reader
        .read_from_container(&mut std::io::Cursor::new(&jpeg))
        .expect("source EXIF parsed");
    assert!(parsed.fields().count() > 0);

    let view = upload(&router, &session, "geo.jpg", "image/jpeg", &jpeg).await;
    let id = parse_media_id(&view);
    assert!(await_variants(&storage, id).await);

    let url = format!("/api/v1/media/{}?size=medium", id.into_uuid());
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(&url)
                .body(Body::empty())
                .expect("build get"),
        )
        .await
        .expect("router responded");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.expect("body").to_bytes();
    // WebP carries EXIF in a `EXIF` RIFF chunk — kamadak-exif handles
    // it transparently. We expect *no* EXIF.
    let result = reader.read_from_container(&mut std::io::Cursor::new(&body));
    match result {
        Err(_) => {} // No EXIF chunk — the property we want.
        Ok(parsed) => {
            assert_eq!(
                parsed.fields().count(),
                0,
                "expected no EXIF fields in re-encoded variant",
            );
        }
    }
}

/// Splice a minimal APP1/EXIF segment after the JPEG's SOI marker so
/// `kamadak-exif` can see "GPSInfo" on the input. The segment encodes a
/// single GPS Version ID tag — enough that the parser reports
/// non-zero fields without needing a full GPS IFD.
fn inject_exif_after_soi(jpeg: &mut Vec<u8>) {
    // EXIF segment: APP1 marker + length (excl. marker) + "Exif\0\0" +
    // TIFF header (II, 42) + IFD0 offset 8 + one entry (GPSInfo pointer
    // tag 0x8825 with offset to GPS IFD) + GPS IFD with GPSVersionID
    // (0x0000) = "2.3.0.0".
    let exif: Vec<u8> = {
        let mut v = Vec::new();
        // TIFF header (little-endian, magic 42, offset to IFD0 = 8).
        v.extend_from_slice(b"II");
        v.extend_from_slice(&42u16.to_le_bytes());
        v.extend_from_slice(&8u32.to_le_bytes());
        // IFD0: 1 entry.
        v.extend_from_slice(&1u16.to_le_bytes());
        // Entry: tag=GPSInfo(0x8825), type=LONG(4), count=1, value=offset
        // 0x1A (right after IFD0).
        v.extend_from_slice(&0x8825u16.to_le_bytes());
        v.extend_from_slice(&4u16.to_le_bytes());
        v.extend_from_slice(&1u32.to_le_bytes());
        v.extend_from_slice(&0x1Au32.to_le_bytes());
        // Next IFD offset = 0.
        v.extend_from_slice(&0u32.to_le_bytes());
        // GPS IFD: 1 entry.
        v.extend_from_slice(&1u16.to_le_bytes());
        // Entry: tag=GPSVersionID(0x0000), type=BYTE(1), count=4, value
        // bytes inline = 2,3,0,0.
        v.extend_from_slice(&0x0000u16.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&4u32.to_le_bytes());
        v.extend_from_slice(&[2u8, 3, 0, 0]);
        // Next IFD offset = 0.
        v.extend_from_slice(&0u32.to_le_bytes());
        v
    };

    let mut payload: Vec<u8> = Vec::new();
    payload.extend_from_slice(b"Exif\0\0");
    payload.extend_from_slice(&exif);

    let segment_len: u16 = (payload.len() + 2) as u16; // includes the length field itself
    let mut segment: Vec<u8> = Vec::new();
    segment.extend_from_slice(&[0xFFu8, 0xE1u8]);
    segment.extend_from_slice(&segment_len.to_be_bytes());
    segment.extend_from_slice(&payload);

    // Insert directly after the SOI (FFD8) marker — bytes [0..2].
    jpeg.splice(2..2, segment.iter().copied());
}
