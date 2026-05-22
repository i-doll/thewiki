//! Redis-backed rate-limit store.
//!
//! Compiled only when the `redis` cargo feature is enabled. Uses a Lua script
//! to make `(refill, consume)` a single atomic step on the server side — there
//! is no race window where two replicas can both observe `tokens >= 1.0` and
//! both consume.
//!
//! # Key shape
//!
//! Each bucket lives at a Redis key shaped `rate_limit:{kind}:{principal}`:
//!
//! - `rate_limit:r:ip:203.0.113.5`
//! - `rate_limit:r:u:b8f7c4f4-…`
//! - `rate_limit:w:ip:…`
//!
//! Two fields are tracked in a hash: `tokens` (float) and `ts` (Unix
//! millisecond timestamp of the last refill). On each call:
//!
//! 1. The script reads `tokens`/`ts`, defaulting to `(capacity, now)` if
//!    absent.
//! 2. Adds `elapsed_ms * refill_rate_per_ms` tokens, clamped at `capacity`.
//! 3. If `tokens >= 1`, decrements by 1 and returns `{allowed=1, retry_ms=0}`.
//! 4. Otherwise computes the milliseconds until at least 1 token will be
//!    available and returns `{allowed=0, retry_ms=…}`.
//! 5. Writes back the new `(tokens, ts)` with a TTL of `refill_interval_secs
//!    * 4` so idle buckets self-expire and the keyspace stays bounded.
//!
//! The script is shipped as a string literal so deploys do not need to
//! pre-load it into Redis. `redis` caches script SHAs after the first call.

use std::time::Duration;

use redis::Script;
use redis::aio::ConnectionManager;
use tokio::sync::Mutex;

use crate::rate_limit::config::{BucketKind, RateLimitBucketConfig};
use crate::rate_limit::error::RateLimitError;
use crate::rate_limit::key::RateLimitKey;
use crate::rate_limit::store::{RateLimitDecision, RateLimitStore};

const RATE_LIMIT_LUA: &str = r#"
local key = KEYS[1]
local capacity = tonumber(ARGV[1])
local refill_tokens = tonumber(ARGV[2])
local refill_interval_ms = tonumber(ARGV[3])
local now_ms = tonumber(ARGV[4])
local ttl_secs = tonumber(ARGV[5])

local data = redis.call('HMGET', key, 'tokens', 'ts')
local tokens = tonumber(data[1])
local ts = tonumber(data[2])
if tokens == nil or ts == nil then
  tokens = capacity
  ts = now_ms
end

local elapsed = math.max(0, now_ms - ts)
local refill_rate = refill_tokens / refill_interval_ms
tokens = math.min(capacity, tokens + elapsed * refill_rate)

local allowed = 0
local retry_ms = 0
if tokens >= 1 then
  tokens = tokens - 1
  allowed = 1
else
  local needed = 1 - tokens
  if refill_rate > 0 then
    retry_ms = math.ceil(needed / refill_rate)
  else
    retry_ms = refill_interval_ms
  end
  if retry_ms < 1 then retry_ms = 1 end
end

redis.call('HMSET', key, 'tokens', tokens, 'ts', now_ms)
redis.call('EXPIRE', key, ttl_secs)
return {allowed, retry_ms}
"#;

/// Redis-backed token bucket store.
///
/// The connection is wrapped in [`ConnectionManager`] for auto-reconnect, and
/// the script is loaded once and cached by SHA on the Redis side.
pub struct RedisRateLimitStore {
    conn: Mutex<ConnectionManager>,
    script: Script,
}

impl std::fmt::Debug for RedisRateLimitStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisRateLimitStore")
            .finish_non_exhaustive()
    }
}

impl RedisRateLimitStore {
    /// Open a connection to `url` and prepare the script.
    ///
    /// # Errors
    ///
    /// Returns [`RateLimitError::Backend`] if the URL cannot be parsed or the
    /// initial connection fails. After construction, transient failures are
    /// surfaced on each `check` call so the middleware can fail open.
    pub async fn connect(url: &str) -> Result<Self, RateLimitError> {
        let client = redis::Client::open(url)
            .map_err(|e| RateLimitError::Backend(format!("redis url: {e}")))?;
        let manager = ConnectionManager::new(client)
            .await
            .map_err(|e| RateLimitError::Backend(format!("redis connect: {e}")))?;
        Ok(Self {
            conn: Mutex::new(manager),
            script: Script::new(RATE_LIMIT_LUA),
        })
    }

    fn key_string(key: RateLimitKey, kind: BucketKind) -> String {
        let kind_tag = match kind {
            BucketKind::Read => "r",
            BucketKind::Write => "w",
        };
        match key {
            RateLimitKey::Anonymous(ip) => format!("rate_limit:{kind_tag}:ip:{ip}"),
            RateLimitKey::User(uid) => format!("rate_limit:{kind_tag}:u:{}", uid.as_uuid()),
        }
    }
}

#[async_trait::async_trait]
impl RateLimitStore for RedisRateLimitStore {
    async fn check(
        &self,
        key: RateLimitKey,
        kind: BucketKind,
        bucket: RateLimitBucketConfig,
    ) -> Result<RateLimitDecision, RateLimitError> {
        let now_ms = u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| RateLimitError::Backend(format!("clock skew: {e}")))?
                .as_millis(),
        )
        .map_err(|e| RateLimitError::Backend(format!("clock overflow: {e}")))?;
        let refill_interval_ms = bucket
            .refill_interval_secs
            .checked_mul(1000)
            .ok_or_else(|| RateLimitError::Backend("refill_interval overflow".to_owned()))?;
        let ttl_secs = bucket.refill_interval_secs.saturating_mul(4).max(1);

        let redis_key = Self::key_string(key, kind);
        let mut invocation = self.script.prepare_invoke();
        invocation
            .key(redis_key)
            .arg(bucket.capacity)
            .arg(bucket.refill_tokens)
            .arg(refill_interval_ms)
            .arg(now_ms)
            .arg(ttl_secs);

        let mut conn = self.conn.lock().await;
        let result: (i64, i64) = invocation
            .invoke_async(&mut *conn)
            .await
            .map_err(|e| RateLimitError::Backend(format!("redis eval: {e}")))?;

        let (allowed, retry_ms) = result;
        if allowed == 1 {
            Ok(RateLimitDecision::Allowed)
        } else {
            let retry_ms = u64::try_from(retry_ms.max(1)).unwrap_or(1);
            Ok(RateLimitDecision::Denied {
                retry_after: Duration::from_millis(retry_ms),
            })
        }
    }
}
