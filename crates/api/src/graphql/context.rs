//! Per-resolver context injected into `async_graphql::Context::data`.
//!
//! The GraphQL schema is generic over the storage facade (`S: AppStorage`)
//! and resolvers reach back into the application state to read or write. We
//! inject [`GraphQLContext`] alongside the per-request [`SessionContext`] so
//! resolvers can borrow either as needed.
//!
//! `GraphQLContext` is cheap to clone (only an `Arc<AppState<S>>` and a
//! couple of `Arc<…>` handles inside), so we always hand resolvers a borrowed
//! reference rather than asking them to clone.

use std::sync::Arc;

use crate::state::{AppState, AppStorage};

/// Per-request resolver dependencies.
///
/// Holds an `Arc<AppState<S>>` to keep the storage handle, auth config, and
/// search indexer reachable. The HTTP handler wraps the live `AppState` in
/// an `Arc` once at request time and hands the resulting context to
/// async-graphql; resolvers `data::<GraphQLContext<S>>()` it back out.
pub struct GraphQLContext<S: AppStorage> {
    pub state: Arc<AppState<S>>,
}

impl<S: AppStorage> Clone for GraphQLContext<S> {
    fn clone(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
        }
    }
}

impl<S: AppStorage> GraphQLContext<S> {
    /// Build a context wrapping the supplied `AppState`.
    #[must_use]
    pub fn new(state: Arc<AppState<S>>) -> Self {
        Self { state }
    }
}
