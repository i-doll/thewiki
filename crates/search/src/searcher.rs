//! Read-side handle for the search index.
//!
//! [`Searcher`] is the read-only counterpart to [`crate::IndexerHandle`]: it
//! wraps an `Arc<SearchIndex>` and exposes a single `search` method against
//! it. Because Tantivy's `IndexReader` reloads on commit, a `Searcher` that
//! shares its `Arc<SearchIndex>` with the live indexer worker sees committed
//! writes as they land (within the `OnCommitWithDelay` reload window).
//!
//! Cheap to clone (one `Arc` bump) — pass by value into route handlers.

use std::sync::Arc;

use crate::error::SearchError;
use crate::index::SearchIndex;
use crate::query::SearchQuery;
use crate::results::SearchResults;

/// Clonable, send-anywhere read handle.
///
/// Constructing a [`Searcher::disabled`] handle is allowed and makes
/// [`Searcher::search`] return an empty result set — useful for tests that
/// don't stand up the index.
#[derive(Clone)]
pub struct Searcher {
    index: Option<Arc<SearchIndex>>,
}

impl std::fmt::Debug for Searcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Searcher")
            .field("enabled", &self.is_enabled())
            .finish()
    }
}

impl Searcher {
    /// Wrap an index for read-only access.
    #[must_use]
    pub fn new(index: Arc<SearchIndex>) -> Self {
        Self { index: Some(index) }
    }

    /// Construct a no-op handle. All queries return an empty result set.
    #[must_use]
    pub fn disabled() -> Self {
        Self { index: None }
    }

    /// `true` if the handle is backed by a real index.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.index.is_some()
    }

    /// Run a query and return ranked hits with snippets.
    ///
    /// When the handle is disabled this returns an empty result set rather
    /// than an error — search outages should never block page rendering.
    ///
    /// # Errors
    ///
    /// Bubbles every Tantivy error up to the caller; the API layer maps it
    /// to a `500`.
    pub fn search(&self, query: &SearchQuery) -> Result<SearchResults, SearchError> {
        let Some(index) = self.index.as_ref() else {
            return Ok(SearchResults {
                hits: Vec::new(),
                next_cursor: None,
                total_estimate: 0,
            });
        };
        index.search(query)
    }
}

impl Default for Searcher {
    fn default() -> Self {
        Self::disabled()
    }
}
