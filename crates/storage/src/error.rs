//! Storage-layer error type.
//!
//! [`StorageError`] is the single error every [`Repository`](crate::repo)
//! method surfaces. It distinguishes "row not found" (`NotFound`) from
//! "the data conflicts with an existing row" (`Conflict`) so callers don't
//! have to match on raw `sqlx::Error` variants — those leak through the
//! `Database` arm only when something genuinely lower-level went wrong.
//!
//! The enum is `#[non_exhaustive]` to leave room for backends that need to
//! report new failure modes (e.g. libsql replication lag) without bumping the
//! crate's major version.

use thiserror::Error;

/// What went wrong inside a storage operation.
///
/// Repository implementations map their backend errors into these variants so
/// the rest of the system can pattern-match on a stable enum.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StorageError {
    /// The requested row did not exist.
    ///
    /// Issued by `get_*` calls and by mutating calls (`update`, `delete`) when
    /// they target a row that has already been removed.
    #[error("not found")]
    NotFound,

    /// The operation violated a uniqueness or referential constraint.
    ///
    /// The carried string describes which constraint, in operator-friendly
    /// terms (e.g. "username already taken", "duplicate slug in namespace").
    #[error("conflict: {0}")]
    Conflict(String),

    /// A lower-level database error escaped without a more specific mapping.
    ///
    /// Typically I/O, connection-pool exhaustion, or a malformed SQL response.
    /// Carries the original `sqlx::Error` on backends that use sqlx (`sqlite`,
    /// the future `postgres`); the libsql adapter tunnels through
    /// `sqlx::Error::Protocol` so this variant stays in the same shape across
    /// backends and `?` keeps working in the per-aggregate impl modules.
    #[cfg(feature = "sqlite")]
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    /// A lower-level driver error in a non-sqlx backend (libsql).
    ///
    /// Carries the driver's error rendering as a string so the variant stays
    /// usable on builds that don't compile `sqlx`. The two variants converge
    /// at the API layer (`StorageError -> 500`).
    #[cfg(not(feature = "sqlite"))]
    #[error("database error: {0}")]
    Database(String),

    /// A migration failed to apply.
    ///
    /// Distinct from `Database` so the caller can present "set up the schema
    /// first" instead of "your query is wrong".
    #[error("migration error: {0}")]
    Migration(String),

    /// A row stored in the database could not be reconstructed into a domain
    /// type.
    ///
    /// Raised when a string column fails validation
    /// (e.g. [`Username::new`](thewiki_core::Username::new)) or a foreign
    /// reference is malformed. Implies the database is in a state the domain
    /// model does not expect.
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

impl StorageError {
    /// Convenience for building a [`Conflict`](Self::Conflict).
    #[must_use]
    pub fn conflict(reason: impl Into<String>) -> Self {
        Self::Conflict(reason.into())
    }

    /// Convenience for building an [`InvalidInput`](Self::InvalidInput).
    #[must_use]
    pub fn invalid_input(reason: impl Into<String>) -> Self {
        Self::InvalidInput(reason.into())
    }
}
