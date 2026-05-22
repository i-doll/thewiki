//! Error type surfaced by every public function in this crate.
//!
//! [`SearchError`] is `#[non_exhaustive]` so future failure modes (a
//! degraded-reader variant, a snippet-generator error, ...) can land without
//! a breaking change. The variants today cover the three observable failure
//! sources: filesystem I/O on the index directory, Tantivy itself, and
//! schema/lookup invariants.
//!
//! Callers map this to their own error space — the API crate converts it to
//! a 5xx (search outages should not break page CRUD) and the CLI prints it
//! to stderr.

use thiserror::Error;

/// What can go wrong inside the search crate.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SearchError {
    /// Filesystem error while opening the index directory or writing the
    /// `.last_indexed` marker.
    #[error("search I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Tantivy returned an error. Boxed because `tantivy::TantivyError` is a
    /// wide enum (panicking-style invariants, query parser failures, segment
    /// load problems, ...) that we deliberately do not pattern-match on.
    #[error("tantivy error: {0}")]
    Tantivy(#[from] tantivy::TantivyError),

    /// A schema invariant was violated — typically a missing field name, an
    /// indexed-but-stored-only field read back as text, or a malformed
    /// cursor token. The message names the offending field / cursor.
    #[error("search schema error: {0}")]
    Schema(String),
}

impl SearchError {
    /// Convenience for building a [`Schema`](Self::Schema) error.
    #[must_use]
    pub fn schema(reason: impl Into<String>) -> Self {
        Self::Schema(reason.into())
    }
}
