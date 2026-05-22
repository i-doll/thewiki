//! Media uploads (#32).
//!
//! A [`Media`] row is the metadata side of an uploaded blob: who uploaded
//! it, what it claims to be, how big it is, and — crucially — its
//! content-addressing SHA-256 hash. The blob bytes themselves live in one of
//! two places depending on the deployment configuration:
//!
//! - The `media_blobs` table in the same primary database (the default).
//! - An `object_store`-backed bucket (S3 / R2 / MinIO) when the operator
//!   configures one.
//!
//! Either way, the API surface is identical: callers `POST` a multipart
//! form, get a `MediaView` back with a `url`, and `GET` that URL to fetch
//! the bytes. Deduplication is via [`Media::content_hash`] (SHA-256) so two
//! uploads of the same content collapse into one row.
//!
//! Note: the upload validation (size / content-type allowlist / SVG
//! scrubbing) is performed in the API layer — this struct just records the
//! resulting metadata.
//!
//! See the upload pipeline plan in `crates/api/src/media/`.
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;

use crate::id::{MediaId, UserId};

/// Number of bytes in a SHA-256 digest. Pulled out as a constant so the
/// storage layer's column-length checks and the API layer's hashing loop
/// agree on a single name.
pub const CONTENT_HASH_BYTES: usize = 32;

/// One uploaded media asset.
///
/// `content_hash` is the SHA-256 digest of the stored bytes. Together with
/// the unique index on the storage side it gives us free deduplication —
/// repeated uploads of the same image collapse into a single row.
///
/// `original_filename` is metadata only: it does not participate in the
/// dedup key and is never trusted to derive paths or content-types on the
/// way back out. The wire `Content-Type` always comes from
/// [`Self::content_type`], which the API validated against the operator's
/// allowlist before storing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Media {
    /// Primary key (UUIDv7).
    pub id: MediaId,
    /// SHA-256 digest of the stored bytes. Unique across the table.
    #[schema(value_type = String, format = Byte)]
    pub content_hash: [u8; CONTENT_HASH_BYTES],
    /// The IANA media type the upload was validated against (e.g.
    /// `image/png`).
    pub content_type: String,
    /// Stored length in bytes. Tracks the byte count after any sanitisation
    /// the API layer applied (SVG scrubbing, etc.).
    pub byte_size: u64,
    /// Filename the client supplied on upload. Optional and never trusted
    /// to derive paths or content-types.
    pub original_filename: Option<String>,
    /// User who uploaded the asset.
    pub uploaded_by: UserId,
    /// Upload timestamp.
    pub created_at: OffsetDateTime,
}
