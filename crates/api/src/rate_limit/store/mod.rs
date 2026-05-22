//! Storage backends for rate-limit token buckets.
//!
//! All backends implement the [`RateLimitStore`] trait, which exposes a single
//! atomic `check` that consumes one token if available and otherwise returns
//! the wall-clock duration until the next token will be available.
//!
//! Two backends ship in this crate:
//!
//! - [`InMemoryRateLimitStore`] — process-local `DashMap` indexed by
//!   `(RateLimitKey, BucketKind)`. Suitable for single-replica deploys and the
//!   default. A background GC task drops idle buckets so the map size stays
//!   bounded for high-churn anonymous traffic.
//! - [`redis::RedisRateLimitStore`] — atomic Lua-script-based bucket suitable
//!   for sharing state across replicas. Compiled when the `redis` cargo feature
//!   is enabled; gated so the default build does not pull in the redis crate.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;

use crate::rate_limit::config::{BucketKind, RateLimitBucketConfig};
use crate::rate_limit::error::RateLimitError;
use crate::rate_limit::key::RateLimitKey;

#[cfg(feature = "redis")]
pub mod redis;

/// Outcome of consulting a bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitDecision {
    /// A token was available and has been consumed.
    Allowed,
    /// The bucket is empty; `retry_after` is how long until the next token.
    Denied {
        /// How long until the bucket will have at least one token again.
        retry_after: Duration,
    },
}

/// Pluggable storage backend for token buckets.
///
/// Implementations must be `Send + Sync + 'static` because the middleware
/// shares them across tasks via an `Arc`. The check is async so a Redis
/// backend can be wired in without breaking the trait shape; the in-memory
/// backend resolves immediately.
#[async_trait::async_trait]
pub trait RateLimitStore: Send + Sync + 'static {
    /// Atomically consume one token from the bucket identified by
    /// `(key, kind)`, using `bucket` to size the bucket on first sight.
    ///
    /// # Errors
    ///
    /// Returns [`RateLimitError::Backend`] for transient backend failures
    /// (Redis disconnection, malformed responses, ...). The in-memory backend
    /// is infallible. Middleware fails open on backend errors so a single
    /// Redis blip does not 503 the whole API.
    async fn check(
        &self,
        key: RateLimitKey,
        kind: BucketKind,
        bucket: RateLimitBucketConfig,
    ) -> Result<RateLimitDecision, RateLimitError>;

    /// Like [`Self::check`] but does not consume a token — it just reports
    /// whether the bucket currently has one.
    ///
    /// Used by the middleware as a cheap preflight on the IP-keyed bucket
    /// before paying for session resolution. The default implementation
    /// always allows; backends that can do better should override (the
    /// in-memory backend does). Refusing to override is safe — the middleware
    /// will fall through to a real `check` call on the user-keyed bucket
    /// after session resolution, which is still rate-limited.
    async fn peek(
        &self,
        key: RateLimitKey,
        kind: BucketKind,
        bucket: RateLimitBucketConfig,
    ) -> Result<RateLimitDecision, RateLimitError> {
        let _ = (key, kind, bucket);
        Ok(RateLimitDecision::Allowed)
    }
}

/// Per-bucket state for the in-memory backend.
///
/// Tokens are tracked as `f64` so partial refills accumulate accurately — we
/// don't want a 1-token-per-second refill rate to be quantised to 1Hz check
/// granularity.
#[derive(Debug, Clone, Copy)]
struct BucketState {
    /// Current token count. Bounded `[0, bucket.capacity]`.
    tokens: f64,
    /// Last time `tokens` was recomputed.
    last_refill: Instant,
    /// Last time a request actually hit this bucket. Used by the GC task to
    /// decide which buckets are safe to drop.
    last_seen: Instant,
}

/// Process-local in-memory token bucket store.
///
/// Backed by [`DashMap`] so concurrent checks across worker threads don't
/// serialise on a single mutex. Each shard locks independently.
///
/// # GC strategy
///
/// Idle buckets accumulate as a function of (number of distinct
/// `(key, kind)` pairs the server has seen). For an anonymous-heavy load
/// that's effectively unbounded — every distinct client IP creates an entry.
/// We bound the map size with a background sweep that runs every
/// [`InMemoryRateLimitStore::gc_interval`]: each pass drops every bucket
/// where `now - last_seen >= bucket.refill_interval_secs + slack`. The slack
/// is the maximum refill interval the limiter could be configured to use
/// (the GC task doesn't know the configured buckets) so we be conservative
/// and use a constant 10 minutes. A dropped bucket that re-fires will be
/// reseeded full, which is the safe direction — a returning client that
/// would otherwise be denied gets one bucket-worth of leeway, never the
/// other way around.
pub struct InMemoryRateLimitStore {
    buckets: Arc<DashMap<(RateLimitKey, BucketKind), BucketState>>,
    /// Handle to the background GC task. Kept so dropping the store aborts
    /// the task and we don't leak it through tests.
    _gc_task: Option<tokio::task::JoinHandle<()>>,
}

impl std::fmt::Debug for InMemoryRateLimitStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InMemoryRateLimitStore")
            .field("entries", &self.buckets.len())
            .finish_non_exhaustive()
    }
}

/// Idle window after which the GC task drops a bucket. Conservative — much
/// longer than any realistic configured refill interval (defaults run on a
/// 60s interval) so we never evict a bucket that a returning client might
/// hit. Anything actively in-use gets a `last_seen` bump on each check.
const GC_IDLE_WINDOW: Duration = Duration::from_secs(10 * 60);

/// How often the background GC task runs.
const GC_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

impl Default for InMemoryRateLimitStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryRateLimitStore {
    /// Construct a new in-memory store and spawn its background GC task.
    ///
    /// The GC task is spawned on the current tokio runtime; outside of a
    /// runtime (e.g. unit tests using `Instant::now()` directly without
    /// `#[tokio::test]`) the task is omitted and the map will not be GC'd —
    /// fine for short-lived tests, but production callers always wire this
    /// up inside the axum server's runtime.
    #[must_use]
    pub fn new() -> Self {
        let buckets: Arc<DashMap<_, _>> = Arc::new(DashMap::new());
        let gc_task = if tokio::runtime::Handle::try_current().is_ok() {
            let buckets = Arc::clone(&buckets);
            Some(tokio::spawn(gc_loop(buckets)))
        } else {
            None
        };
        Self {
            buckets,
            _gc_task: gc_task,
        }
    }

    /// Construct a store *without* spawning the background GC task. Used by
    /// the synchronous unit tests below. Production callers should use
    /// [`InMemoryRateLimitStore::new`].
    #[must_use]
    pub fn without_gc() -> Self {
        Self {
            buckets: Arc::new(DashMap::new()),
            _gc_task: None,
        }
    }

    /// Synchronous core of `check`, parameterised on the current time so the
    /// unit tests can step time forward without sleeping.
    fn check_at(
        &self,
        key: RateLimitKey,
        kind: BucketKind,
        bucket: RateLimitBucketConfig,
        now: Instant,
    ) -> RateLimitDecision {
        let mut entry = self
            .buckets
            .entry((key, kind))
            .or_insert_with(|| BucketState {
                tokens: f64::from(bucket.capacity),
                last_refill: now,
                last_seen: now,
            });

        refill(&mut entry, bucket, now);
        entry.last_seen = now;

        if entry.tokens >= 1.0 {
            entry.tokens -= 1.0;
            RateLimitDecision::Allowed
        } else {
            RateLimitDecision::Denied {
                retry_after: retry_after(entry.tokens, bucket),
            }
        }
    }

    /// Synchronous core of `peek`. Reports whether the bucket currently has
    /// a token, without consuming one.
    fn peek_at(
        &self,
        key: RateLimitKey,
        kind: BucketKind,
        bucket: RateLimitBucketConfig,
        now: Instant,
    ) -> RateLimitDecision {
        let Some(state) = self.buckets.get(&(key, kind)) else {
            // No bucket seen yet — assume the full capacity, which means at
            // least one token is available (config validation rejects
            // `capacity == 0`).
            return RateLimitDecision::Allowed;
        };

        // Read a snapshot, refill on the snapshot, decide. Do not mutate the
        // shared state — that's `check`'s job.
        let mut snapshot = *state;
        drop(state);
        refill(&mut snapshot, bucket, now);

        if snapshot.tokens >= 1.0 {
            RateLimitDecision::Allowed
        } else {
            RateLimitDecision::Denied {
                retry_after: retry_after(snapshot.tokens, bucket),
            }
        }
    }

    /// Number of bucket entries currently held. Test-only — production code
    /// should not depend on the size of the map.
    #[cfg(test)]
    #[must_use]
    #[allow(
        clippy::len_without_is_empty,
        reason = "test-only diagnostic; production code does not consult the map size"
    )]
    pub fn len(&self) -> usize {
        self.buckets.len()
    }

    /// Run a single GC pass manually. Test-only.
    #[cfg(test)]
    pub fn gc_once(&self, now: Instant) {
        prune_idle(&self.buckets, now);
    }
}

#[async_trait::async_trait]
impl RateLimitStore for InMemoryRateLimitStore {
    async fn check(
        &self,
        key: RateLimitKey,
        kind: BucketKind,
        bucket: RateLimitBucketConfig,
    ) -> Result<RateLimitDecision, RateLimitError> {
        Ok(self.check_at(key, kind, bucket, Instant::now()))
    }

    async fn peek(
        &self,
        key: RateLimitKey,
        kind: BucketKind,
        bucket: RateLimitBucketConfig,
    ) -> Result<RateLimitDecision, RateLimitError> {
        Ok(self.peek_at(key, kind, bucket, Instant::now()))
    }
}

async fn gc_loop(buckets: Arc<DashMap<(RateLimitKey, BucketKind), BucketState>>) {
    let mut interval = tokio::time::interval(GC_SWEEP_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // First tick fires immediately; skip it so we don't sweep a freshly-built
    // map.
    interval.tick().await;
    loop {
        interval.tick().await;
        prune_idle(&buckets, Instant::now());
    }
}

fn prune_idle(buckets: &DashMap<(RateLimitKey, BucketKind), BucketState>, now: Instant) {
    buckets.retain(|_, state| {
        // Guard against clock skew: if `last_seen` is somehow in the future,
        // treat the bucket as fresh and keep it.
        if now <= state.last_seen {
            return true;
        }
        now.duration_since(state.last_seen) < GC_IDLE_WINDOW
    });
}

/// Refill `state` based on the bucket's configured rate. Safe against clock
/// skew (a `now` in the past is a no-op).
fn refill(state: &mut BucketState, bucket: RateLimitBucketConfig, now: Instant) {
    if now <= state.last_refill {
        return;
    }
    let elapsed = now.duration_since(state.last_refill).as_secs_f64();
    let interval = Duration::from_secs(bucket.refill_interval_secs).as_secs_f64();
    if interval <= 0.0 {
        // Validated up front — defensive guard so a misconfiguration cannot
        // produce a NaN/Inf token count.
        state.last_refill = now;
        return;
    }
    let refill_rate = f64::from(bucket.refill_tokens) / interval;
    state.tokens = f64::from(bucket.capacity).min(state.tokens + elapsed * refill_rate);
    state.last_refill = now;
}

/// Compute `Retry-After` for a bucket with the given current token count.
///
/// Returns at least 1 second so a 429 response cannot hand back
/// `Retry-After: 0` (which clients can interpret as "retry now").
fn retry_after(tokens: f64, bucket: RateLimitBucketConfig) -> Duration {
    let interval = Duration::from_secs(bucket.refill_interval_secs).as_secs_f64();
    let refill_rate = f64::from(bucket.refill_tokens) / interval;
    if refill_rate <= 0.0 || !refill_rate.is_finite() {
        return Duration::from_secs(bucket.refill_interval_secs.max(1));
    }
    let raw = ((1.0 - tokens).max(0.0) / refill_rate).ceil();
    // f64 -> u64 saturates rather than overflows; guard against negative/NaN
    // anyway.
    let seconds = if raw.is_finite() && raw >= 0.0 {
        (raw as u64).max(1)
    } else {
        1
    };
    Duration::from_secs(seconds)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;

    fn bucket() -> RateLimitBucketConfig {
        RateLimitBucketConfig {
            capacity: 3,
            refill_tokens: 1,
            refill_interval_secs: 2,
        }
    }

    fn key(octet: u8) -> RateLimitKey {
        RateLimitKey::Anonymous(IpAddr::V4(Ipv4Addr::new(203, 0, 113, octet)))
    }

    #[test]
    fn capacity_consumed_then_refilled() {
        let store = InMemoryRateLimitStore::without_gc();
        let now = Instant::now();
        let k = key(1);

        for _ in 0..3 {
            assert_eq!(
                store.check_at(k, BucketKind::Read, bucket(), now),
                RateLimitDecision::Allowed
            );
        }
        let denied = store.check_at(k, BucketKind::Read, bucket(), now);
        match denied {
            RateLimitDecision::Denied { retry_after } => {
                assert!(retry_after >= Duration::from_secs(1));
            }
            other => panic!("expected denied, got {other:?}"),
        }

        // One refill interval later, one token should be back.
        let later = now + Duration::from_secs(2);
        assert_eq!(
            store.check_at(k, BucketKind::Read, bucket(), later),
            RateLimitDecision::Allowed
        );
    }

    #[test]
    fn read_and_write_buckets_are_independent() {
        let store = InMemoryRateLimitStore::without_gc();
        let now = Instant::now();
        let k = key(2);
        let b = RateLimitBucketConfig {
            capacity: 1,
            refill_tokens: 1,
            refill_interval_secs: 60,
        };

        assert_eq!(
            store.check_at(k, BucketKind::Read, b, now),
            RateLimitDecision::Allowed
        );
        assert!(matches!(
            store.check_at(k, BucketKind::Read, b, now),
            RateLimitDecision::Denied { .. }
        ));
        // Write bucket still has its full allowance.
        assert_eq!(
            store.check_at(k, BucketKind::Write, b, now),
            RateLimitDecision::Allowed
        );
    }

    #[test]
    fn distinct_keys_do_not_interfere() {
        let store = InMemoryRateLimitStore::without_gc();
        let now = Instant::now();
        let b = RateLimitBucketConfig {
            capacity: 1,
            refill_tokens: 1,
            refill_interval_secs: 60,
        };

        assert_eq!(
            store.check_at(key(3), BucketKind::Read, b, now),
            RateLimitDecision::Allowed
        );
        // Different IP — fresh bucket.
        assert_eq!(
            store.check_at(key(4), BucketKind::Read, b, now),
            RateLimitDecision::Allowed
        );
    }

    #[test]
    fn negative_clock_skew_is_safe() {
        let store = InMemoryRateLimitStore::without_gc();
        let now = Instant::now();
        let k = key(5);
        let b = bucket();

        assert_eq!(
            store.check_at(k, BucketKind::Read, b, now),
            RateLimitDecision::Allowed
        );
        // `earlier` is before `last_refill`. Must not panic or refill negatively.
        let earlier = now - Duration::from_millis(500);
        // We can't directly observe internal state, but the next decision
        // must still be deterministic and non-panicking.
        let _ = store.check_at(k, BucketKind::Read, b, earlier);
    }

    #[test]
    fn gc_drops_idle_buckets() {
        let store = InMemoryRateLimitStore::without_gc();
        let now = Instant::now();
        let b = bucket();

        store.check_at(key(6), BucketKind::Read, b, now);
        store.check_at(key(7), BucketKind::Read, b, now + Duration::from_secs(5));
        assert_eq!(store.len(), 2);

        // Sweep at a point past the idle window for the first bucket but not
        // the second.
        store.gc_once(now + GC_IDLE_WINDOW + Duration::from_secs(1));
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn retry_after_is_at_least_one_second() {
        let b = RateLimitBucketConfig {
            capacity: 1,
            refill_tokens: 1_000_000,
            refill_interval_secs: 1,
        };
        // Even with a huge refill rate, retry-after must be >= 1s so clients
        // don't hammer the endpoint.
        assert_eq!(retry_after(0.999, b), Duration::from_secs(1));
    }
}
