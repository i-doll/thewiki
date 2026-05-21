//! Shared encode/decode helpers for the SQLite backend.
//!
//! Every column the M0 schema uses lands in one of a handful of shapes:
//!
//! - 16-byte BLOB for UUIDv7 IDs,
//! - RFC 3339 TEXT for timestamps,
//! - lowercase TEXT for content-format / protection-level enums,
//! - INTEGER for `Permissions` bitflag sets.
//!
//! These helpers keep the conversion code in one place so the per-aggregate
//! implementations read as straight SQL.

use thewiki_core::{
    ContentFormat, Namespace, NamespaceId, NamespaceSlug, Page, PageId, Permissions,
    ProtectionLevel, Revision, RevisionId, Role, RoleId, RoleName, User, UserId, Username,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::error::StorageError;

/// Borrow a 16-byte view of an ID for `bind`-ing into a query.
///
/// The UUIDv7 byte layout is what the BLOB column stores; sqlx is happy to
/// bind a `Vec<u8>`, but we hand it a copy on the stack to avoid a heap
/// allocation per call site.
pub fn uuid_bytes(id: Uuid) -> [u8; 16] {
    *id.as_bytes()
}

/// Decode a UUID from a BLOB column. Surfaces an `InvalidInput` if the row
/// holds the wrong number of bytes.
pub fn decode_uuid(bytes: &[u8]) -> Result<Uuid, StorageError> {
    let arr: [u8; 16] = bytes
        .try_into()
        .map_err(|_| StorageError::invalid_input("UUID column has wrong byte length"))?;
    Ok(Uuid::from_bytes(arr))
}

/// Format an [`OffsetDateTime`] as RFC 3339 for a TEXT column.
pub fn format_ts(ts: OffsetDateTime) -> Result<String, StorageError> {
    ts.format(&Rfc3339)
        .map_err(|err| StorageError::invalid_input(format!("could not format timestamp: {err}")))
}

/// Parse an RFC 3339 string out of a TEXT column.
pub fn parse_ts(raw: &str) -> Result<OffsetDateTime, StorageError> {
    OffsetDateTime::parse(raw, &Rfc3339)
        .map_err(|err| StorageError::invalid_input(format!("malformed timestamp {raw:?}: {err}")))
}

/// Parse a [`ContentFormat`] from its storage representation.
pub fn parse_content_format(raw: &str) -> Result<ContentFormat, StorageError> {
    match raw {
        "markdown" => Ok(ContentFormat::Markdown),
        other => Err(StorageError::invalid_input(format!(
            "unknown content_format {other:?}"
        ))),
    }
}

/// Parse a [`ProtectionLevel`] from its storage representation.
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
pub fn permissions_to_i64(p: Permissions) -> i64 {
    i64::from(p.bits())
}

/// Decode the integer column back into a [`Permissions`] set, preserving
/// bits the current build doesn't know about (forward-compat).
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
// Helpers below take the raw column tuples produced by `sqlx::query_as` and
// rebuild the validated domain entity. We keep the column lists tight to a
// single ordering so the per-aggregate impl modules can stay focused on SQL.

/// Convert a raw `pages` row into a [`Page`].
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
        author_id: thewiki_core::UserId::from_uuid(decode_uuid(&author_id)?),
        body,
        edit_summary,
        created_at: parse_ts(&created_at)?,
    })
}

/// Convert a raw `users` row into a [`User`].
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

/// Convert a raw `roles` row into a [`Role`].
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

/// Classify a [`sqlx::Error`] as either a uniqueness conflict (whose detail
/// will be `kind`) or some other database error to bubble up unchanged.
pub fn map_unique_violation(err: sqlx::Error, kind: &str) -> StorageError {
    if is_unique_violation(&err) {
        StorageError::Conflict(kind.to_string())
    } else {
        StorageError::Database(err)
    }
}

/// SQLite emits error code `2067` (`SQLITE_CONSTRAINT_UNIQUE`) for unique
/// violations. `sqlx::Error::Database` carries the driver-specific code as
/// a string, so we string-match against the known prefixes.
fn is_unique_violation(err: &sqlx::Error) -> bool {
    let Some(db_err) = err.as_database_error() else {
        return false;
    };
    // SQLite's primary code is "2067" for UNIQUE, "1555" for PRIMARY KEY
    // conflicts. Both are extended codes of SQLITE_CONSTRAINT (19).
    matches!(db_err.code().as_deref(), Some("2067" | "1555"))
}

/// Lowercase hex encoding of arbitrary bytes. Used for the BLOB half of
/// list-pagination cursors.
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
