//! Shared encode/decode helpers for the Postgres backend.
//!
//! The schema uses native types so the codec is much thinner than the SQLite
//! counterpart:
//!
//! - `UUID` columns map straight onto [`uuid::Uuid`]; sqlx handles the binary
//!   protocol with no extra coding on our side.
//! - `TIMESTAMPTZ` columns map onto [`time::OffsetDateTime`] via sqlx's
//!   `time` feature, so we can bind / fetch them as the domain type directly.
//! - `roles.permissions` is `BIGINT` (signed 64-bit); the domain type packs a
//!   `u32` so the cast is widening and lossless.
//!
//! Cursor encoding mirrors the SQLite codec — `<rfc3339-timestamp>|<hyphenated-uuid>`
//! — to keep the wire form identical across backends.

use thewiki_core::{
    ContentFormat, Namespace, NamespaceId, NamespaceSlug, Page, PageId, Permissions,
    ProtectionLevel, Revision, RevisionId, Role, RoleId, RoleName, Session, SessionId, User,
    UserId, Username,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::error::StorageError;

/// Format an [`OffsetDateTime`] as RFC 3339 for the cursor wire form.
///
/// Storage columns are `TIMESTAMPTZ` and sqlx binds those natively, so this
/// helper is only used when packing a cursor token for callers.
pub fn format_cursor_ts(ts: OffsetDateTime) -> Result<String, StorageError> {
    ts.format(&Rfc3339)
        .map_err(|err| StorageError::invalid_input(format!("could not format timestamp: {err}")))
}

/// Parse an RFC 3339 string back into an [`OffsetDateTime`] (cursor side).
pub fn parse_cursor_ts(raw: &str) -> Result<OffsetDateTime, StorageError> {
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

/// Pack a [`Permissions`] set into the signed 64-bit column.
///
/// `Permissions` is a `u32` bitset; `BIGINT` is 64-bit signed, so the cast is
/// widening and lossless.
pub fn permissions_to_i64(p: Permissions) -> i64 {
    i64::from(p.bits())
}

/// Decode the integer column back into a [`Permissions`] set, preserving
/// bits the current build doesn't know about (forward-compat).
pub fn permissions_from_i64(raw: i64) -> Result<Permissions, StorageError> {
    let bits = u32::try_from(raw).map_err(|_| {
        StorageError::invalid_input(format!("permissions column out of range: {raw}"))
    })?;
    Ok(Permissions::from_bits_retain(bits))
}

// ───── Row shapes ─────────────────────────────────────────────────────────

/// Convert a raw `pages` row into a [`Page`].
#[allow(clippy::too_many_arguments)]
pub fn page_from_row(
    id: Uuid,
    namespace_id: Uuid,
    slug: String,
    title: String,
    current_revision_id: Option<Uuid>,
    content_format: String,
    protection_level: String,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
) -> Result<Page, StorageError> {
    Ok(Page {
        id: PageId::from_uuid(id),
        namespace_id: NamespaceId::from_uuid(namespace_id),
        slug,
        title,
        current_revision_id: current_revision_id.map(RevisionId::from_uuid),
        content_format: parse_content_format(&content_format)?,
        protection_level: parse_protection_level(&protection_level)?,
        created_at,
        updated_at,
    })
}

/// Convert a raw `revisions` row into a [`Revision`].
pub fn revision_from_row(
    id: Uuid,
    page_id: Uuid,
    parent_id: Option<Uuid>,
    author_id: Uuid,
    body: String,
    edit_summary: Option<String>,
    created_at: OffsetDateTime,
) -> Result<Revision, StorageError> {
    Ok(Revision {
        id: RevisionId::from_uuid(id),
        page_id: PageId::from_uuid(page_id),
        parent_id: parent_id.map(RevisionId::from_uuid),
        author_id: UserId::from_uuid(author_id),
        body,
        edit_summary,
        created_at,
    })
}

/// Convert a raw `users` row into a [`User`].
pub fn user_from_row(
    id: Uuid,
    username: String,
    email: Option<String>,
    display_name: Option<String>,
    created_at: OffsetDateTime,
    last_login_at: Option<OffsetDateTime>,
) -> Result<User, StorageError> {
    let username = Username::new(username)
        .map_err(|err| StorageError::invalid_input(format!("stored username invalid: {err}")))?;
    let email = email
        .map(thewiki_core::EmailAddress::new)
        .transpose()
        .map_err(|err| StorageError::invalid_input(format!("stored email invalid: {err}")))?;
    Ok(User {
        id: UserId::from_uuid(id),
        username,
        email,
        display_name,
        created_at,
        last_login_at,
    })
}

/// Convert a raw `namespaces` row into a [`Namespace`].
pub fn namespace_from_row(
    id: Uuid,
    slug: String,
    display_name: String,
    is_talk: bool,
    paired_namespace_id: Option<Uuid>,
) -> Result<Namespace, StorageError> {
    let slug = NamespaceSlug::new(slug)
        .map_err(|err| StorageError::invalid_input(format!("stored slug invalid: {err}")))?;
    Ok(Namespace {
        id: NamespaceId::from_uuid(id),
        slug,
        display_name,
        is_talk,
        paired_namespace_id: paired_namespace_id.map(NamespaceId::from_uuid),
    })
}

/// Convert a raw `sessions` row into a [`Session`].
#[allow(clippy::too_many_arguments)]
pub fn session_from_row(
    id: Uuid,
    user_id: Uuid,
    created_at: OffsetDateTime,
    expires_at: OffsetDateTime,
    last_seen_at: OffsetDateTime,
    user_agent: Option<String>,
    ip_address: Option<String>,
) -> Result<Session, StorageError> {
    Ok(Session {
        id: SessionId::from_uuid(id),
        user_id: UserId::from_uuid(user_id),
        created_at,
        expires_at,
        last_seen_at,
        user_agent,
        ip_address,
    })
}

/// Convert a raw `roles` row into a [`Role`].
pub fn role_from_row(
    id: Uuid,
    name: String,
    display_name: String,
    permissions: i64,
) -> Result<Role, StorageError> {
    let name = RoleName::new(name)
        .map_err(|err| StorageError::invalid_input(format!("stored role name invalid: {err}")))?;
    Ok(Role {
        id: RoleId::from_uuid(id),
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

/// Postgres returns SQLSTATE `23505` for unique violations
/// (`unique_violation`). sqlx exposes the SQLSTATE string through
/// `DatabaseError::code()`.
fn is_unique_violation(err: &sqlx::Error) -> bool {
    let Some(db_err) = err.as_database_error() else {
        return false;
    };
    matches!(db_err.code().as_deref(), Some("23505"))
}

/// Postgres SQLSTATE `23503` (`foreign_key_violation`) is the realistic
/// failure mode for `users` deletion under `ON DELETE RESTRICT`.
pub fn is_fk_violation(err: &sqlx::Error) -> bool {
    let Some(db_err) = err.as_database_error() else {
        return false;
    };
    matches!(db_err.code().as_deref(), Some("23503"))
}
