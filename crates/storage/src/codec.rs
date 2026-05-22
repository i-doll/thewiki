//! Encode/decode helpers shared by the backend adapters.
//!
//! Both the SQLite (`sqlite::*`) and libsql (`libsql::*`) adapters use the
//! same on-disk encoding for the M0 schema:
//!
//! - 16-byte BLOB for UUIDv7 IDs,
//! - RFC 3339 TEXT for timestamps,
//! - lowercase TEXT for content-format / protection-level enums,
//! - INTEGER for `Permissions` bitflag sets.
//!
//! The driver-specific error-mapping helpers (e.g. classifying a uniqueness
//! violation) stay in their respective backend modules — only the pure
//! value-shuffling lives here so the two adapters share one source of truth.

use thewiki_core::{
    CONTENT_HASH_BYTES, ContentFormat, Media, MediaId, Namespace, NamespaceId, NamespaceSlug, Page,
    PageId, Permissions, ProtectionLevel, Revision, RevisionId, Role, RoleId, RoleName, Session,
    SessionId, User, UserId, Username,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::error::StorageError;

/// Borrow a 16-byte view of an ID for `bind`-ing into a query.
///
/// The UUIDv7 byte layout is what the BLOB column stores; we hand the caller a
/// copy on the stack so they can pass a slice to the driver without a heap
/// allocation per call site.
#[must_use]
pub fn uuid_bytes(id: Uuid) -> [u8; 16] {
    *id.as_bytes()
}

/// Decode a UUID from a BLOB column.
///
/// # Errors
///
/// [`StorageError::InvalidInput`] if the column doesn't hold exactly 16 bytes.
pub fn decode_uuid(bytes: &[u8]) -> Result<Uuid, StorageError> {
    let arr: [u8; 16] = bytes
        .try_into()
        .map_err(|_| StorageError::invalid_input("UUID column has wrong byte length"))?;
    Ok(Uuid::from_bytes(arr))
}

/// Format an [`OffsetDateTime`] as RFC 3339 for a TEXT column.
///
/// # Errors
///
/// [`StorageError::InvalidInput`] if the underlying formatter fails (typically
/// only when the runtime year is out of range — i.e. effectively never).
pub fn format_ts(ts: OffsetDateTime) -> Result<String, StorageError> {
    ts.format(&Rfc3339)
        .map_err(|err| StorageError::invalid_input(format!("could not format timestamp: {err}")))
}

/// Parse an RFC 3339 string out of a TEXT column.
///
/// # Errors
///
/// [`StorageError::InvalidInput`] if the string is not a valid RFC 3339
/// timestamp — implies the database is in an unexpected state.
pub fn parse_ts(raw: &str) -> Result<OffsetDateTime, StorageError> {
    OffsetDateTime::parse(raw, &Rfc3339)
        .map_err(|err| StorageError::invalid_input(format!("malformed timestamp {raw:?}: {err}")))
}

/// Parse a [`ContentFormat`] from its storage representation.
///
/// # Errors
///
/// [`StorageError::InvalidInput`] if `raw` isn't a known content-format token.
pub fn parse_content_format(raw: &str) -> Result<ContentFormat, StorageError> {
    match raw {
        "markdown" => Ok(ContentFormat::Markdown),
        other => Err(StorageError::invalid_input(format!(
            "unknown content_format {other:?}"
        ))),
    }
}

/// Parse a [`ProtectionLevel`] from its storage representation.
///
/// # Errors
///
/// [`StorageError::InvalidInput`] if `raw` isn't a known protection-level token.
pub fn parse_protection_level(raw: &str) -> Result<ProtectionLevel, StorageError> {
    match raw {
        "none" => Ok(ProtectionLevel::None),
        "semi_protected" => Ok(ProtectionLevel::SemiProtected),
        "protected" => Ok(ProtectionLevel::Protected),
        "fully_protected" => Ok(ProtectionLevel::FullyProtected),
        other => Err(StorageError::invalid_input(format!(
            "unknown protection_level {other:?}"
        ))),
    }
}

/// Pack a [`Permissions`] set into the signed integer column.
///
/// `Permissions` is a `u32` bitset; SQLite's `INTEGER` is 64-bit signed, so
/// the cast is widening and lossless.
#[must_use]
pub fn permissions_to_i64(p: Permissions) -> i64 {
    i64::from(p.bits())
}

/// Decode the integer column back into a [`Permissions`] set, preserving
/// bits the current build doesn't know about (forward-compat).
///
/// # Errors
///
/// [`StorageError::InvalidInput`] if the column value doesn't fit in a `u32`.
pub fn permissions_from_i64(raw: i64) -> Result<Permissions, StorageError> {
    let bits = u32::try_from(raw).map_err(|_| {
        StorageError::invalid_input(format!("permissions column out of range: {raw}"))
    })?;
    // `from_bits_retain` keeps unknown bits so a newer DB row roundtrips
    // through an older binary without losing flags.
    Ok(Permissions::from_bits_retain(bits))
}

// ───── Row shapes ─────────────────────────────────────────────────────────
//
// Helpers below take the raw column tuples produced by the driver and rebuild
// the validated domain entity.

/// Convert a raw `pages` row into a [`Page`].
///
/// # Errors
///
/// Surfaces any [`StorageError::InvalidInput`] produced by the decoding
/// helpers (UUID byte length, timestamp parsing, enum tokens).
#[allow(clippy::too_many_arguments)]
pub fn page_from_row(
    id: Vec<u8>,
    namespace_id: Vec<u8>,
    slug: String,
    title: String,
    current_revision_id: Option<Vec<u8>>,
    content_format: String,
    protection_level: String,
    created_at: String,
    updated_at: String,
) -> Result<Page, StorageError> {
    Ok(Page {
        id: PageId::from_uuid(decode_uuid(&id)?),
        namespace_id: NamespaceId::from_uuid(decode_uuid(&namespace_id)?),
        slug,
        title,
        current_revision_id: current_revision_id
            .as_deref()
            .map(decode_uuid)
            .transpose()?
            .map(RevisionId::from_uuid),
        content_format: parse_content_format(&content_format)?,
        protection_level: parse_protection_level(&protection_level)?,
        created_at: parse_ts(&created_at)?,
        updated_at: parse_ts(&updated_at)?,
    })
}

/// Convert a raw `revisions` row into a [`Revision`].
///
/// # Errors
///
/// As [`page_from_row`].
pub fn revision_from_row(
    id: Vec<u8>,
    page_id: Vec<u8>,
    parent_id: Option<Vec<u8>>,
    author_id: Vec<u8>,
    body: String,
    edit_summary: Option<String>,
    created_at: String,
) -> Result<Revision, StorageError> {
    Ok(Revision {
        id: RevisionId::from_uuid(decode_uuid(&id)?),
        page_id: PageId::from_uuid(decode_uuid(&page_id)?),
        parent_id: parent_id
            .as_deref()
            .map(decode_uuid)
            .transpose()?
            .map(RevisionId::from_uuid),
        author_id: UserId::from_uuid(decode_uuid(&author_id)?),
        body,
        edit_summary,
        created_at: parse_ts(&created_at)?,
    })
}

/// Convert a raw `users` row into a [`User`].
///
/// # Errors
///
/// As [`page_from_row`], plus [`StorageError::InvalidInput`] if a stored
/// `username` or `email` fails domain validation.
pub fn user_from_row(
    id: Vec<u8>,
    username: String,
    email: Option<String>,
    display_name: Option<String>,
    created_at: String,
    last_login_at: Option<String>,
) -> Result<User, StorageError> {
    let username = Username::new(username)
        .map_err(|err| StorageError::invalid_input(format!("stored username invalid: {err}")))?;
    let email = email
        .map(thewiki_core::EmailAddress::new)
        .transpose()
        .map_err(|err| StorageError::invalid_input(format!("stored email invalid: {err}")))?;
    Ok(User {
        id: UserId::from_uuid(decode_uuid(&id)?),
        username,
        email,
        display_name,
        created_at: parse_ts(&created_at)?,
        last_login_at: last_login_at.as_deref().map(parse_ts).transpose()?,
    })
}

/// Convert a raw `namespaces` row into a [`Namespace`].
///
/// # Errors
///
/// As [`page_from_row`], plus [`StorageError::InvalidInput`] if the slug
/// fails domain validation.
pub fn namespace_from_row(
    id: Vec<u8>,
    slug: String,
    display_name: String,
    _created_at: String,
) -> Result<Namespace, StorageError> {
    let slug = NamespaceSlug::new(slug)
        .map_err(|err| StorageError::invalid_input(format!("stored slug invalid: {err}")))?;
    Ok(Namespace {
        id: NamespaceId::from_uuid(decode_uuid(&id)?),
        slug,
        display_name,
    })
}

/// Convert a raw `sessions` row into a [`Session`].
///
/// # Errors
///
/// As [`page_from_row`].
#[allow(clippy::too_many_arguments)]
pub fn session_from_row(
    id: Vec<u8>,
    user_id: Vec<u8>,
    created_at: String,
    expires_at: String,
    last_seen_at: String,
    user_agent: Option<String>,
    ip_address: Option<String>,
) -> Result<Session, StorageError> {
    Ok(Session {
        id: SessionId::from_uuid(decode_uuid(&id)?),
        user_id: UserId::from_uuid(decode_uuid(&user_id)?),
        created_at: parse_ts(&created_at)?,
        expires_at: parse_ts(&expires_at)?,
        last_seen_at: parse_ts(&last_seen_at)?,
        user_agent,
        ip_address,
    })
}

/// Convert a raw `media` row into a [`Media`].
///
/// # Errors
///
/// As [`page_from_row`], plus [`StorageError::InvalidInput`] if the
/// `content_hash` column doesn't hold exactly 32 bytes or `byte_size` is
/// negative (the column is signed at the SQL level).
pub fn media_from_row(
    id: Vec<u8>,
    content_hash: Vec<u8>,
    content_type: String,
    byte_size: i64,
    original_filename: Option<String>,
    uploaded_by: Vec<u8>,
    created_at: String,
) -> Result<Media, StorageError> {
    let content_hash: [u8; CONTENT_HASH_BYTES] =
        content_hash.as_slice().try_into().map_err(|_| {
            StorageError::invalid_input(format!(
                "stored content_hash has wrong byte length: expected {CONTENT_HASH_BYTES}, \
             got {}",
                content_hash.len()
            ))
        })?;
    let byte_size = u64::try_from(byte_size)
        .map_err(|_| StorageError::invalid_input(format!("byte_size out of range: {byte_size}")))?;
    Ok(Media {
        id: MediaId::from_uuid(decode_uuid(&id)?),
        content_hash,
        content_type,
        byte_size,
        original_filename,
        uploaded_by: UserId::from_uuid(decode_uuid(&uploaded_by)?),
        created_at: parse_ts(&created_at)?,
    })
}

/// Convert a raw `roles` row into a [`Role`].
///
/// # Errors
///
/// As [`page_from_row`], plus [`StorageError::InvalidInput`] if the role
/// name fails domain validation.
pub fn role_from_row(
    id: Vec<u8>,
    name: String,
    display_name: String,
    permissions: i64,
) -> Result<Role, StorageError> {
    let name = RoleName::new(name)
        .map_err(|err| StorageError::invalid_input(format!("stored role name invalid: {err}")))?;
    Ok(Role {
        id: RoleId::from_uuid(decode_uuid(&id)?),
        name,
        display_name,
        permissions: permissions_from_i64(permissions)?,
    })
}

/// Lowercase hex encoding of arbitrary bytes. Used for the BLOB half of
/// list-pagination cursors.
#[must_use]
pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Decode a 32-character hex string back into a 16-byte UUID.
///
/// # Errors
///
/// [`StorageError::InvalidInput`] if `s` is the wrong length or contains a
/// non-hex character.
pub fn hex_decode_id(s: &str) -> Result<[u8; 16], StorageError> {
    if s.len() != 32 {
        return Err(StorageError::invalid_input(
            "cursor id must be 32 hex chars",
        ));
    }
    let bytes = s.as_bytes();
    let mut out = [0u8; 16];
    for (i, chunk) in bytes.chunks_exact(2).enumerate() {
        let hi = from_hex_digit(chunk[0])?;
        let lo = from_hex_digit(chunk[1])?;
        out[i] = (hi << 4) | lo;
    }
    Ok(out)
}

fn from_hex_digit(b: u8) -> Result<u8, StorageError> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(StorageError::invalid_input("cursor contains non-hex digit")),
    }
}
