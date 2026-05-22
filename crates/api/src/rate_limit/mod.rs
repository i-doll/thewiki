//! Token-bucket rate limiting middleware (#35).
//!
//! Rate limits are enforced per (principal, bucket-kind) pair, where principal
//! is a user ID for authenticated requests (the [`AuthSession`](crate::auth::AuthSession)
//! has been resolved by the time this middleware runs) and the remote peer IP
//! otherwise. Bucket kind is derived from the HTTP method: safe verbs
//! (`GET`/`HEAD`/`OPTIONS`) draw from the read bucket; everything else draws
//! from the write bucket.
//!
//! Submodules:
//! - [`config`] — operator-facing knobs (capacity, refill rate, backend choice).
//! - [`key`] — [`RateLimitKey`] and the extraction logic that maps a request to it.
//! - [`error`] — [`RateLimitError`] and its 429 response shape.
//! - [`middleware`] — the Axum middleware function.
//! - [`store`] — pluggable storage backend trait + the in-memory default.
//!
//! The [`store::redis`] module is compiled when the `redis` cargo feature is
//! enabled and offers an atomic Lua-script-based token bucket suitable for
//! sharing state across replicas.

pub mod config;
pub mod error;
pub mod key;
pub mod middleware;
pub mod store;

pub use config::BucketKind;
pub use error::{RateLimitError, RateLimitErrorBody};
pub use key::RateLimitKey;
pub use middleware::{RateLimitState, rate_limit_layer};
pub use store::{InMemoryRateLimitStore, RateLimitDecision, RateLimitStore};
