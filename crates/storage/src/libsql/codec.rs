//! libsql-specific encode/decode helpers.
//!
//! Most of the work is delegated to [`crate::codec`], which the SQLite adapter
//! also consumes. This module only adds:
//!
//! - [`map_unique_violation`] / [`map_fk_restrict_violation`], which classify
//!   a `libsql::Error::SqliteFailure(code, _)` into our error enum, and
//! - row-readers (`page_from_libsql_row`, …) which pull our common column
//!   layouts out of a `libsql::Row` and feed the shared `_from_row` helpers.
//!
//! The `BLOB`/`TEXT`/`INTEGER` wire shape matches the SQLite adapter byte for
//! byte because the schema (`/migrations/`) is portable — libsql is a fork of
//! SQLite and reads it without modification.

// Re-export the portable codec helpers so the per-aggregate impl modules can
// `use crate::libsql::codec::…` exactly like the SQLite side does.
pub use crate::codec::{
    decode_uuid, format_ts, hex_decode_id, hex_encode, parse_ts, permissions_to_i64, uuid_bytes,
};

use libsql::{Row, Value};
use thewiki_core::{AuditLogId, NamespaceId, Page, PageId, RevisionId, Session, User, UserId};
use time::OffsetDateTime;

use crate::codec::{
    namespace_from_row as ns_from_row, page_from_row, revision_from_row, role_from_row,
    session_from_row, user_from_row,
};
use crate::error::StorageError;
use crate::repo::{AuditLogEntry, RecentChange};

/// Wrap a [`libsql::Error`] in [`StorageError::Database`].
///
/// On builds where the sqlite (sqlx) feature is also on, [`StorageError::Database`]
/// carries a `sqlx::Error`; we tunnel through `sqlx::Error::Protocol` so the
/// message is preserved verbatim and the caller gets the same "lower-level
/// database failure" semantics they'd get from the SQLite adapter. On
/// libsql-only builds the variant carries a `String` directly.
#[cfg(feature = "sqlite")]
pub(crate) fn db_error(err: libsql::Error) -> StorageError {
    StorageError::Database(sqlx::Error::Protocol(err.to_string()))
}

/// libsql-only `db_error` variant (no sqlx dep in this build).
#[cfg(not(feature = "sqlite"))]
pub(crate) fn db_error(err: libsql::Error) -> StorageError {
    StorageError::Database(err.to_string())
}

/// Convert a `libsql::Result<T>` directly into a `Result<T, StorageError>`.
pub(crate) fn into_db<T>(res: libsql::Result<T>) -> Result<T, StorageError> {
    res.map_err(db_error)
}

/// Classify a [`libsql::Error`] as either a uniqueness conflict (whose detail
/// will be `kind`) or some other database error to bubble up unchanged.
///
/// libsql returns `Error::SqliteFailure(c_int, _)` for local connections where
/// the integer is the SQLite extended result code. For remote connections it
/// returns `Error::RemoteSqliteFailure(_, ext, _)` where the second field is
/// the extended code. We accept both shapes.
pub(crate) fn map_unique_violation(err: libsql::Error, kind: &str) -> StorageError {
    if is_unique_violation(&err) {
        StorageError::Conflict(kind.to_string())
    } else {
        db_error(err)
    }
}

/// Classify a [`libsql::Error`] as a foreign-key (RESTRICT) violation —
/// e.g. trying to delete a user that still owns revisions — vs. a generic
/// database error. The caller supplies the user-facing `reason`.
pub(crate) fn map_fk_restrict_violation(err: libsql::Error, reason: &str) -> StorageError {
    if is_fk_violation(&err) {
        StorageError::Conflict(reason.to_string())
    } else {
        db_error(err)
    }
}

/// `SQLITE_CONSTRAINT_UNIQUE` (2067) and `SQLITE_CONSTRAINT_PRIMARYKEY` (1555)
/// are the two extended codes that map to "duplicate row".
fn is_unique_violation(err: &libsql::Error) -> bool {
    matches!(extended_code(err), Some(2067 | 1555))
}

/// `SQLITE_CONSTRAINT_FOREIGNKEY` (787) is the only FK violation code SQLite
/// emits; libsql preserves it on local connections.
fn is_fk_violation(err: &libsql::Error) -> bool {
    matches!(extended_code(err), Some(787 | 1811))
}

fn extended_code(err: &libsql::Error) -> Option<i32> {
    match err {
        libsql::Error::SqliteFailure(code, _) => Some(*code),
        libsql::Error::RemoteSqliteFailure(_, ext, _) => Some(*ext),
        _ => None,
    }
}

// ───── Column readers ─────────────────────────────────────────────────────
//
// libsql's `Row::get::<T>(idx)` decodes a column into a target type. We pull
// out the column tuples the shared `*_from_row` helpers expect.

/// Read a 16-byte BLOB id column.
fn col_blob(row: &Row, idx: i32) -> Result<Vec<u8>, StorageError> {
    into_db(row.get::<Vec<u8>>(idx))
}

/// Read an optional 16-byte BLOB id column (the column may be NULL).
fn col_blob_opt(row: &Row, idx: i32) -> Result<Option<Vec<u8>>, StorageError> {
    into_db(row.get::<Option<Vec<u8>>>(idx))
}

/// Read a TEXT column.
fn col_text(row: &Row, idx: i32) -> Result<String, StorageError> {
    into_db(row.get::<String>(idx))
}

/// Read an optional TEXT column.
fn col_text_opt(row: &Row, idx: i32) -> Result<Option<String>, StorageError> {
    into_db(row.get::<Option<String>>(idx))
}

/// Read an INTEGER column.
fn col_int(row: &Row, idx: i32) -> Result<i64, StorageError> {
    into_db(row.get::<i64>(idx))
}

/// Decode a `pages` row.
///
/// Column order: `id, namespace_id, slug, title, current_revision_id,
/// content_format, protection_level, created_at, updated_at`.
pub(crate) fn page_from_libsql_row(row: &Row) -> Result<Page, StorageError> {
    page_from_row(
        col_blob(row, 0)?,
        col_blob(row, 1)?,
        col_text(row, 2)?,
        col_text(row, 3)?,
        col_blob_opt(row, 4)?,
        col_text(row, 5)?,
        col_text(row, 6)?,
        col_text(row, 7)?,
        col_text(row, 8)?,
    )
}

/// Decode a `revisions` row.
///
/// Column order: `id, page_id, parent_id, author_id, body, edit_summary,
/// created_at`.
pub(crate) fn revision_from_libsql_row(row: &Row) -> Result<thewiki_core::Revision, StorageError> {
    revision_from_row(
        col_blob(row, 0)?,
        col_blob(row, 1)?,
        col_blob_opt(row, 2)?,
        col_blob(row, 3)?,
        col_text(row, 4)?,
        col_text_opt(row, 5)?,
        col_text(row, 6)?,
    )
}

/// Decode a `users` row.
///
/// Column order: `id, username, email, display_name, created_at, last_login_at`.
pub(crate) fn user_from_libsql_row(row: &Row) -> Result<User, StorageError> {
    user_from_row(
        col_blob(row, 0)?,
        col_text(row, 1)?,
        col_text_opt(row, 2)?,
        col_text_opt(row, 3)?,
        col_text(row, 4)?,
        col_text_opt(row, 5)?,
    )
}

/// Decode a `namespaces` row.
///
/// Column order: `id, slug, display_name, created_at`.
pub(crate) fn namespace_from_libsql_row(
    row: &Row,
) -> Result<thewiki_core::Namespace, StorageError> {
    ns_from_row(
        col_blob(row, 0)?,
        col_text(row, 1)?,
        col_text(row, 2)?,
        col_text(row, 3)?,
    )
}

/// Decode a `roles` row.
///
/// Column order: `id, name, display_name, permissions`.
pub(crate) fn role_from_libsql_row(row: &Row) -> Result<thewiki_core::Role, StorageError> {
    role_from_row(
        col_blob(row, 0)?,
        col_text(row, 1)?,
        col_text(row, 2)?,
        col_int(row, 3)?,
    )
}

/// Decode a `sessions` row.
///
/// Column order: `id, user_id, created_at, expires_at, last_seen_at,
/// user_agent, ip_address`.
pub(crate) fn session_from_libsql_row(row: &Row) -> Result<Session, StorageError> {
    session_from_row(
        col_blob(row, 0)?,
        col_blob(row, 1)?,
        col_text(row, 2)?,
        col_text(row, 3)?,
        col_text(row, 4)?,
        col_text_opt(row, 5)?,
        col_text_opt(row, 6)?,
    )
}

/// Decode a `recent_changes` JOIN row.
///
/// Column order: `r.id, r.page_id, p.slug, p.namespace_id, n.slug,
/// r.author_id, u.username, r.edit_summary, r.created_at, p.protection_level`.
pub(crate) fn recent_change_from_libsql_row(row: &Row) -> Result<RecentChange, StorageError> {
    Ok(RecentChange {
        revision_id: RevisionId::from_uuid(decode_uuid(&col_blob(row, 0)?)?),
        page_id: PageId::from_uuid(decode_uuid(&col_blob(row, 1)?)?),
        page_slug: col_text(row, 2)?,
        namespace_id: NamespaceId::from_uuid(decode_uuid(&col_blob(row, 3)?)?),
        namespace_slug: col_text(row, 4)?,
        author_id: UserId::from_uuid(decode_uuid(&col_blob(row, 5)?)?),
        author_username: col_text(row, 6)?,
        edit_summary: col_text_opt(row, 7)?,
        created_at: parse_ts(&col_text(row, 8)?)?,
        protection_level: crate::codec::parse_protection_level(&col_text(row, 9)?)?,
    })
}

/// Decode an `audit_log` row.
///
/// Column order: `id, actor_id, actor_username, action, target_kind,
/// target_id, target_label, metadata, created_at`.
pub(crate) fn audit_log_from_libsql_row(row: &Row) -> Result<AuditLogEntry, StorageError> {
    let metadata_str = col_text(row, 7)?;
    let metadata = serde_json::from_str::<serde_json::Value>(&metadata_str).map_err(|err| {
        StorageError::invalid_input(format!("stored audit metadata invalid: {err}"))
    })?;
    Ok(AuditLogEntry {
        id: AuditLogId::from_uuid(decode_uuid(&col_blob(row, 0)?)?),
        actor_id: UserId::from_uuid(decode_uuid(&col_blob(row, 1)?)?),
        actor_username: col_text(row, 2)?,
        action: col_text(row, 3)?,
        target_kind: col_text(row, 4)?,
        target_id: decode_uuid(&col_blob(row, 5)?)?,
        target_label: col_text_opt(row, 6)?,
        metadata,
        created_at: parse_ts(&col_text(row, 8)?)?,
    })
}

/// Build the bind value for an optional 16-byte ID column.
///
/// `libsql::Value::Blob` is the wire representation. We use a small wrapper
/// rather than relying on `Option<&[u8]>` auto-conversion so the binding sites
/// stay easy to scan.
#[must_use]
pub(crate) fn opt_blob(bytes: Option<&[u8]>) -> Value {
    match bytes {
        Some(b) => Value::Blob(b.to_vec()),
        None => Value::Null,
    }
}

/// Build the bind value for an optional TEXT column.
#[must_use]
pub(crate) fn opt_text(s: Option<&str>) -> Value {
    match s {
        Some(t) => Value::Text(t.to_owned()),
        None => Value::Null,
    }
}

/// Build the bind value for an optional RFC3339 timestamp column.
///
/// # Errors
///
/// Propagates the [`StorageError::InvalidInput`] from [`format_ts`] if the
/// timestamp can't be formatted.
pub(crate) fn opt_ts(ts: Option<OffsetDateTime>) -> Result<Value, StorageError> {
    Ok(match ts {
        Some(t) => Value::Text(format_ts(t)?),
        None => Value::Null,
    })
}
