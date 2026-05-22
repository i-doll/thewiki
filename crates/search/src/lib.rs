//! Full-text search for thewiki.
//!
//! This crate wraps Tantivy in a small, opinionated facade designed for
//! incremental indexing of wiki pages:
//!
//! - [`schema`] declares the field set used for every indexed page. The
//!   fields are intentionally narrow (id, namespace, slug, title, body, tags,
//!   updated_at); adding a new one is a backwards-incompatible schema bump
//!   and requires a `thewiki reindex`.
//! - [`index::SearchIndex`] owns the Tantivy `IndexWriter` + `IndexReader`
//!   pair. Writes are serialised through a single owner (the [`indexer`]
//!   worker) so the runtime never touches the synchronous writer directly.
//! - [`indexer`] is the async glue: page-handler code on the HTTP path
//!   pushes [`indexer::IndexJob`]s into an `mpsc` channel and a dedicated
//!   blocking worker drains them, commits periodically, and writes a
//!   `.last_indexed` marker for crash recovery.
//! - [`query`] / [`results`] are the read side — a small typed query
//!   builder plus the hit-list shape returned to the API layer.
//!
//! # Crash safety
//!
//! Tantivy is itself crash-safe at the segment level: an uncommitted writer
//! state is discarded on reopen, so partial writes never corrupt the index.
//! What we add on top is **idempotency**: every indexable change is sent as a
//! delete-then-add keyed by `page_id`, so replaying the same job twice (after
//! a crash) leaves the index in the same state. The worker drops a marker
//! file (`<index_path>/.last_indexed`) after every successful commit; on
//! startup, callers that find the marker missing should enqueue a
//! [`IndexJob::Rebuild`](indexer::IndexJob::Rebuild) to repopulate from
//! authoritative storage.
//!
//! # Threading model
//!
//! Tantivy's writer is `!Sync` and explicitly synchronous. We never hold the
//! writer across an `.await` point; the worker task pulls jobs on the tokio
//! runtime, then dispatches the actual writes through `spawn_blocking`. The
//! reader, by contrast, is cheap to clone and search calls run on the runtime
//! directly.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod error;
pub mod index;
pub mod indexer;
pub mod query;
pub mod results;
pub mod schema;

pub use error::SearchError;
pub use index::{PageDoc, SearchIndex};
pub use indexer::{IndexJob, Indexer, IndexerHandle};
pub use query::SearchQuery;
pub use results::{SearchHit, SearchResults};
pub use schema::SearchSchema;
