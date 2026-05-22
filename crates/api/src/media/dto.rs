//! Wire shapes for the media upload endpoints (#32).

use serde::{Deserialize, Serialize};
use thewiki_core::{Media, MediaId, UserId};
use time::OffsetDateTime;
use utoipa::ToSchema;

/// Public projection of a [`Media`] row.
///
/// `content_hash_hex` is the lowercase hex form of the SHA-256 digest — the
/// raw bytes would round-trip as a base64 string under `serde`, which is
/// less useful to clients that want to copy/paste the hash. `url` is the
/// canonical fetch path for the bytes.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MediaView {
    /// Primary key.
    pub id: MediaId,
    /// Hex-encoded SHA-256 digest of the stored bytes.
    pub content_hash_hex: String,
    /// IANA media type the upload was stored as.
    pub content_type: String,
    /// Stored length in bytes.
    pub byte_size: u64,
    /// Filename the client supplied at upload time.
    pub original_filename: Option<String>,
    /// User who uploaded the asset.
    pub uploaded_by: UserId,
    /// Upload timestamp.
    pub created_at: OffsetDateTime,
    /// Canonical fetch URL: `/api/v1/media/{id}`.
    pub url: String,
}

impl MediaView {
    /// Build a [`MediaView`] from a stored [`Media`] row.
    #[must_use]
    pub fn from_media(media: &Media) -> Self {
        Self {
            id: media.id,
            content_hash_hex: hex_lower(&media.content_hash),
            content_type: media.content_type.clone(),
            byte_size: media.byte_size,
            original_filename: media.original_filename.clone(),
            uploaded_by: media.uploaded_by,
            created_at: media.created_at,
            url: format!("/api/v1/media/{}", media.id.into_uuid()),
        }
    }
}

/// Lowercase hex of a byte slice. Avoid pulling a dedicated `hex` crate for
/// one allocation per response.
fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
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
    fn hex_lower_matches_known_vector() {
        assert_eq!(hex_lower(b""), "");
        assert_eq!(hex_lower(&[0x00, 0xff, 0xab, 0xcd]), "00ffabcd");
        // SHA-256 of "" — useful as a sanity vector for the hash path.
        let zero = [
            0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
            0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
            0x78, 0x52, 0xb8, 0x55,
        ];
        assert_eq!(
            hex_lower(&zero),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
