//! Cookie shape for the session token + the CSRF defence-in-depth token.
//!
//! The actual session row is created by the storage layer
//! ([`SessionRepository::create`]); this module owns the *cookie* side of the
//! handshake: choosing flags, formatting the `Set-Cookie` value, and producing
//! the matching CSRF token.
//!
//! Two cookies are issued at login:
//!
//! - `thewiki_session` — opaque session ID. `HttpOnly`, `Secure` (when behind
//!   TLS), `SameSite=Strict`, `Path=/`, expiry matches the session row.
//! - `thewiki_csrf` — random per-session token. **Not** `HttpOnly`, so the
//!   frontend can read it and echo it in the `X-CSRF-Token` header on
//!   mutating requests (double-submit pattern).
//!
//! On logout we clear both with `Max-Age=0`.
//!
//! [`SessionRepository::create`]: thewiki_storage::repo::SessionRepository::create

use std::time::Duration;

use thewiki_core::SessionId;
use time::OffsetDateTime;
use tower_cookies::cookie::{Cookie, SameSite, time as cookie_time};
use uuid::Uuid;

/// Name of the cookie that carries the session ID.
pub const SESSION_COOKIE: &str = "thewiki_session";

/// Name of the (non-HttpOnly) cookie that carries the CSRF double-submit
/// token.
pub const CSRF_COOKIE: &str = "thewiki_csrf";

/// Header name the CSRF middleware reads for the echoed token.
pub const CSRF_HEADER: &str = "x-csrf-token";

/// CSRF token length, in bytes (32 = 256 bits of entropy).
const CSRF_TOKEN_BYTES: usize = 32;

/// Encode a [`SessionId`] for the cookie value.
///
/// We use the UUIDv7 hyphenated form (36 chars, ASCII). It's URL-safe, easy to
/// eyeball in logs, and the cookie crate doesn't have to escape anything.
#[must_use]
pub fn encode_session_id(id: SessionId) -> String {
    id.into_uuid().to_string()
}

/// Parse a cookie value back into a [`SessionId`].
///
/// Returns `None` if the value isn't a hyphenated UUID — callers treat that
/// the same as "no cookie present" (401 path).
#[must_use]
pub fn decode_session_id(raw: &str) -> Option<SessionId> {
    Uuid::parse_str(raw).ok().map(SessionId::from_uuid)
}

/// Build the `Set-Cookie` value for the session token.
///
/// `expires_at` comes straight from the session row; the cookie expiry will
/// match so a closed browser still loses the session at the same time the
/// server-side row does.
#[must_use]
pub fn build_session_cookie(
    id: SessionId,
    expires_at: OffsetDateTime,
    secure: bool,
) -> Cookie<'static> {
    let mut builder = Cookie::build((SESSION_COOKIE, encode_session_id(id)))
        .http_only(true)
        .secure(secure)
        .same_site(SameSite::Strict)
        .path("/")
        .expires(
            cookie_time::OffsetDateTime::from_unix_timestamp(expires_at.unix_timestamp()).ok(),
        );
    // `tower_cookies::cookie::time` re-exports `time` v0.3 so the conversion
    // above is "round-trip via unix timestamp"; calling `.expires()` with the
    // builder is the canonical idiom.
    let _ = &mut builder;
    builder.build()
}

/// Build the matching `thewiki_csrf` cookie. Not `HttpOnly` so the SPA can
/// read it and echo it back in [`CSRF_HEADER`].
#[must_use]
pub fn build_csrf_cookie(token: &str, expires_at: OffsetDateTime, secure: bool) -> Cookie<'static> {
    Cookie::build((CSRF_COOKIE, token.to_owned()))
        .http_only(false)
        .secure(secure)
        .same_site(SameSite::Strict)
        .path("/")
        .expires(cookie_time::OffsetDateTime::from_unix_timestamp(expires_at.unix_timestamp()).ok())
        .build()
}

/// Build a clearing cookie for `name` (used on logout). `Max-Age=0` plus an
/// empty value evicts the cookie from the user-agent jar.
#[must_use]
pub fn build_clearing_cookie(name: &'static str, secure: bool) -> Cookie<'static> {
    Cookie::build((name, ""))
        .http_only(name == SESSION_COOKIE)
        .secure(secure)
        .same_site(SameSite::Strict)
        .path("/")
        .max_age(cookie_time::Duration::seconds(0))
        .build()
}

/// Generate a fresh CSRF token. 256 bits of entropy, base64url-encoded.
#[must_use]
pub fn fresh_csrf_token() -> String {
    use argon2::password_hash::rand_core::{OsRng, RngCore};

    let mut buf = [0u8; CSRF_TOKEN_BYTES];
    OsRng.fill_bytes(&mut buf);
    base64url(&buf)
}

/// Convert a TTL ([`std::time::Duration`]) into the `time::Duration` flavour
/// the cookie crate expects, clamped to `time::Duration::MAX` on overflow.
#[must_use]
pub fn ttl_to_time_duration(d: Duration) -> time::Duration {
    time::Duration::try_from(d).unwrap_or(time::Duration::MAX)
}

/// Tiny base64url encoder (no padding). Avoiding a dedicated crate keeps the
/// dependency surface small; correctness is covered by the unit test below.
fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let n =
            (u32::from(bytes[i]) << 16) | (u32::from(bytes[i + 1]) << 8) | u32::from(bytes[i + 2]);
        out.push(char::from(ALPHABET[((n >> 18) & 0x3f) as usize]));
        out.push(char::from(ALPHABET[((n >> 12) & 0x3f) as usize]));
        out.push(char::from(ALPHABET[((n >> 6) & 0x3f) as usize]));
        out.push(char::from(ALPHABET[(n & 0x3f) as usize]));
        i += 3;
    }
    let remaining = bytes.len() - i;
    if remaining == 1 {
        let n = u32::from(bytes[i]) << 16;
        out.push(char::from(ALPHABET[((n >> 18) & 0x3f) as usize]));
        out.push(char::from(ALPHABET[((n >> 12) & 0x3f) as usize]));
    } else if remaining == 2 {
        let n = (u32::from(bytes[i]) << 16) | (u32::from(bytes[i + 1]) << 8);
        out.push(char::from(ALPHABET[((n >> 18) & 0x3f) as usize]));
        out.push(char::from(ALPHABET[((n >> 12) & 0x3f) as usize]));
        out.push(char::from(ALPHABET[((n >> 6) & 0x3f) as usize]));
    }
    out
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn session_id_roundtrips_through_cookie_value() {
        let id = SessionId::new();
        let encoded = encode_session_id(id);
        let decoded = decode_session_id(&encoded).expect("decode");
        assert_eq!(decoded, id);
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode_session_id("not-a-uuid").is_none());
        assert!(decode_session_id("").is_none());
    }

    #[test]
    fn fresh_csrf_token_is_unique_and_url_safe() {
        let a = fresh_csrf_token();
        let b = fresh_csrf_token();
        assert_ne!(a, b);
        for c in a.chars() {
            assert!(
                c.is_ascii_alphanumeric() || c == '-' || c == '_',
                "csrf token contains non-urlsafe char {c:?} (full: {a})"
            );
        }
    }

    #[test]
    fn base64url_known_vectors() {
        // Cross-check against well-known RFC4648 §10 vectors (sans padding).
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(b"foob"), "Zm9vYg");
        assert_eq!(base64url(b"fooba"), "Zm9vYmE");
        assert_eq!(base64url(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn session_cookie_has_expected_flags() {
        let id = SessionId::new();
        let expires = OffsetDateTime::now_utc() + time::Duration::hours(1);
        let cookie = build_session_cookie(id, expires, true);
        assert_eq!(cookie.name(), SESSION_COOKIE);
        assert_eq!(cookie.http_only(), Some(true));
        assert_eq!(cookie.secure(), Some(true));
        assert_eq!(cookie.same_site(), Some(SameSite::Strict));
        assert_eq!(cookie.path(), Some("/"));
        assert!(cookie.expires().is_some());
    }

    #[test]
    fn clearing_cookie_has_zero_max_age() {
        let cookie = build_clearing_cookie(SESSION_COOKIE, true);
        assert_eq!(cookie.value(), "");
        assert_eq!(cookie.max_age(), Some(cookie_time::Duration::seconds(0)));
    }
}
