//! Top-level GraphQL schema assembly.
//!
//! Brings the Query and Mutation roots together, configures the depth /
//! complexity guards, and wires the optional Apollo persisted-query cache
//! when the operator opts in. The schema is generic over the storage
//! facade so SQLite (M0) and Postgres / libsql (M1) share one definition.

use async_graphql::extensions::apollo_persisted_queries::{
    ApolloPersistedQueries, LruCacheStorage,
};
use async_graphql::{EmptySubscription, Schema, SchemaBuilder};

use crate::config::GraphQLConfig;
use crate::graphql::mutation::Mutation;
use crate::graphql::query::Query;
use crate::state::AppStorage;

/// Concrete schema type alias, parametric over the storage facade.
pub type AppSchema<S> = Schema<Query<S>, Mutation<S>, EmptySubscription>;

/// Build a schema with the supplied [`GraphQLConfig`] applied.
///
/// The returned schema is `Clone` (it shares a single inner `Arc`), so the
/// route handler can stash one copy in axum state.
pub fn build<S: AppStorage>(config: &GraphQLConfig) -> AppSchema<S> {
    let mut builder: SchemaBuilder<_, _, _> = Schema::build(
        Query::<S>::default(),
        Mutation::<S>::default(),
        EmptySubscription,
    )
    .limit_depth(config.max_query_depth as usize)
    .limit_complexity(config.max_query_complexity as usize);
    if !config.introspection_enabled {
        builder = builder.disable_introspection();
    }
    if config.persisted_queries_enabled {
        // 1024-entry LRU is plenty for a small wiki — the wire protocol
        // expects this to be a process-local cache, so we don't reach for
        // Redis here even when the rate limiter does. The cache is sized
        // for "active client diversity" rather than "total query corpus".
        builder = builder.extension(ApolloPersistedQueries::new(LruCacheStorage::new(1_024)));
    }
    builder.finish()
}
