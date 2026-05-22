//! Rate-limit configuration surface.
//!
//! The wire-level types (`RateLimitConfig`, `RateLimitBucketConfig`,
//! `RateLimitBackendConfig`, `ClientIpHeader`) live in [`crate::config`] so they
//! are flat alongside the rest of the operator-facing config. This module
//! re-exports them and adds the runtime-only [`BucketKind`] enum used by the
//! middleware to route a request to the read- or write-bucket.

use axum::http::Method;

pub use crate::config::{
    ClientIpHeader, RateLimitBackendConfig, RateLimitBucketConfig, RateLimitConfig,
};

/// Which token bucket a request draws from.
///
/// Mapping is by HTTP method: safe verbs go to [`BucketKind::Read`]; everything
/// else (mutations and unknown verbs) goes to [`BucketKind::Write`]. The two
/// buckets are independent — exhausting one does not affect the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BucketKind {
    /// Bucket consulted for `GET`, `HEAD`, `OPTIONS`.
    Read,
    /// Bucket consulted for every other method.
    Write,
}

impl BucketKind {
    /// Pick the right bucket for `method`.
    #[must_use]
    pub fn for_method(method: &Method) -> Self {
        if matches!(method, &Method::GET | &Method::HEAD | &Method::OPTIONS) {
            Self::Read
        } else {
            Self::Write
        }
    }
}
