//! Encode/decode helpers for the SQLite backend.
//!
//! Pure value-shuffling lives in [`crate::codec`] and is shared with the libsql
//! adapter. Only sqlx-specific error mapping stays here.

// Re-export the shared helpers under the same paths the per-aggregate impl
// modules already consume so the SQLite call sites don't need to learn about
// the move.
pub use crate::codec::{
    decode_uuid, format_ts, hex_decode_id, hex_encode, namespace_from_row, page_from_row, parse_ts,
    permissions_to_i64, revision_from_row, role_from_row, session_from_row, user_from_row,
    uuid_bytes,
};

use crate::error::StorageError;

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
