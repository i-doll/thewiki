//! Layered configuration loader for the `thewiki` binary.
//!
//! Loading order (later layers override earlier ones):
//!
//! 1. [`Config::defaults`] — built-in sane defaults baked into the binary.
//! 2. A TOML file (typically `thewiki.toml`), loaded only when an explicit path
//!    is supplied. Missing-but-requested is a hard error; the operator asked
//!    for it, so silently falling back would be a footgun.
//! 3. Environment variables prefixed `THEWIKI_`. Nested keys use a double
//!    underscore separator: `THEWIKI_SERVER__BIND` overrides `server.bind`,
//!    `THEWIKI_AUTH__ARGON2__MEMORY_KIB` overrides `auth.argon2.memory_kib`.
//!
//! The double-underscore separator is a deliberate choice — single underscores
//! collide with snake_case field names (e.g. `acquire_timeout_secs`).
//!
//! Configuration is read once at startup; reloading is a v2 concern.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
use serde::{Deserialize, Serialize};

/// Top-level configuration, populated by merging the layered providers.
///
/// Every field has a default so a brand-new deploy with no config file and no
/// environment overrides still boots successfully.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// HTTP server (bind address, timeouts, runtime sizing).
    pub server: ServerConfig,
    /// Persistence layer (database URL + pool tuning).
    pub database: DatabaseConfig,
    /// Object storage backend (DB blobs by default, S3-compatible at M1).
    pub storage: StorageConfig,
    /// Auth model defaults (anonymous edits, registration, hashing parameters).
    pub auth: AuthConfig,
    /// Full-text search (Tantivy) index location and commit cadence.
    pub search: SearchConfig,
    /// Abuse protection for public endpoints.
    pub rate_limit: RateLimitConfig,
    /// Administrative audit-log retention.
    pub audit_log: AuditLogConfig,
    /// Observability (log format and filter).
    pub telemetry: TelemetryConfig,
}

/// HTTP server tuning.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Address the HTTP listener binds to (`host:port`).
    ///
    /// Defaults to `0.0.0.0:8080`. Operators behind a reverse proxy will often
    /// flip this to `127.0.0.1:8080` so the binary only listens on the loopback
    /// interface.
    pub bind: String,

    /// Per-request timeout. Parsed as a humantime duration (e.g. `"30s"`,
    /// `"2m"`). `None` disables the timeout layer entirely — only sensible if
    /// you have a reverse proxy enforcing one.
    #[serde(default)]
    pub request_timeout: Option<String>,

    /// Override Tokio's worker thread count. `None` falls back to the runtime
    /// default (one worker per logical CPU).
    #[serde(default)]
    pub worker_threads: Option<usize>,

    /// Serve the embedded SPA bundle (`web/dist/`) as a fallback for any
    /// request that doesn't match an API route (#16).
    ///
    /// Default: `true`. The single-binary production deploy expects this.
    /// Local frontend developers running `pnpm dev` flip it to `false` so
    /// unmatched routes return 404 — Vite proxies `/api/*` to the Rust
    /// backend and serves the SPA itself.
    #[serde(default = "default_serve_frontend")]
    pub serve_frontend: bool,
}

/// `serde` default for [`ServerConfig::serve_frontend`].
#[must_use]
fn default_serve_frontend() -> bool {
    true
}

/// Persistence layer configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseConfig {
    /// Which backend the URL targets. Defaults to `sqlite`. The Postgres
    /// adapter (#25) is selected with `driver = "postgres"`; the libsql
    /// adapter lands in #24. The dispatch reading this field is wired in
    /// alongside the runtime that owns it; the field is parsed here so
    /// operators can already declare their target backend.
    #[serde(default)]
    pub driver: DatabaseDriver,
    /// Database URL, in sqlx's URL syntax. SQLite is the only M0 backend;
    /// `postgres://` is supported behind the `postgres` cargo feature
    /// (M1, #25) and `libsql://` lands in #24.
    pub url: String,
    /// Maximum number of pooled connections.
    pub max_connections: u32,
    /// Timeout (seconds) for acquiring a connection from the pool before
    /// returning an error to the caller.
    pub acquire_timeout_secs: u64,
}

/// Database driver selector. `sqlite` is the default and the only backend
/// `thewiki` ships at M0; `postgres` becomes available once the binary is
/// built with `--features thewiki-storage/postgres`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum DatabaseDriver {
    /// SQLite via `sqlx::SqlitePool`. URL form: `sqlite://path` or
    /// `sqlite::memory:`.
    #[default]
    Sqlite,
    /// Postgres via `sqlx::PgPool`. URL form: `postgres://user:pass@host/db`.
    Postgres,
}

/// Object storage backend selector.
///
/// `Db` keeps blobs in the primary database (simple, works out of the box).
/// `S3` is selected by setting `backend = { kind = "s3", … }`. The media
/// upload pipeline (#32) consults this struct to pick where blob payloads
/// live; metadata always stays in the primary DB.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    /// Where blob payloads land — DB row vs S3-compatible bucket.
    pub backend: StorageBackend,
    /// Upload validation tuning: size limit and accepted content types.
    #[serde(default)]
    pub media: MediaConfig,
}

/// Tuning for the [`POST /api/v1/media`](crate) upload endpoint.
///
/// Operators can tighten the content-type allowlist or shrink the size cap
/// without touching code. The defaults match the issue spec — common image
/// types plus a 10 MiB cap.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaConfig {
    /// Hard ceiling on the byte length of any single upload. Requests over
    /// this limit get a 413 Payload Too Large before the blob is persisted.
    ///
    /// Default: 10 MiB.
    #[serde(default = "default_max_upload_bytes")]
    pub max_upload_bytes: u64,
    /// IANA media types the upload endpoint accepts. Anything outside this
    /// set is rejected with 415 Unsupported Media Type.
    ///
    /// SVGs are allowed but sanitised through `ammonia` before storage —
    /// see the `<script>` / `on*` handler scrubbing in the media handler.
    ///
    /// Default: the set listed in #32.
    #[serde(default = "default_allowed_content_types")]
    pub allowed_content_types: Vec<String>,
}

/// `serde` default for [`MediaConfig::max_upload_bytes`].
fn default_max_upload_bytes() -> u64 {
    10 * 1024 * 1024
}

/// `serde` default for [`MediaConfig::allowed_content_types`].
fn default_allowed_content_types() -> Vec<String> {
    vec![
        "image/png".to_string(),
        "image/jpeg".to_string(),
        "image/gif".to_string(),
        "image/webp".to_string(),
        "image/svg+xml".to_string(),
    ]
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            max_upload_bytes: default_max_upload_bytes(),
            allowed_content_types: default_allowed_content_types(),
        }
    }
}

/// Available object-storage backends.
///
/// `#[non_exhaustive]` so adding new backends (e.g. local filesystem) isn't a
/// breaking change for downstream matchers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[non_exhaustive]
pub enum StorageBackend {
    /// Store blobs in the primary database. Default for small deploys.
    Db,
    /// S3-compatible object store (AWS S3, R2, MinIO, Backblaze B2, ...).
    S3 {
        /// Bucket name.
        bucket: String,
        /// AWS region (or region-like value for S3-compatible providers).
        region: String,
        /// Optional custom endpoint URL (for R2, MinIO, etc.). When `None` the
        /// default AWS endpoint for `region` is used.
        #[serde(default)]
        endpoint_url: Option<String>,
    },
}

/// Auth model defaults. Runtime enforcement lives in #14; this struct is the
/// surface operators configure.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthConfig {
    /// When `true`, anonymous (logged-out) visitors can edit pages. Defaults
    /// to `false` for safety.
    pub anonymous_edits: bool,
    /// Account registration policy.
    pub registration: RegistrationPolicy,
    /// Which classes of users land in the moderator approval queue.
    pub approval_required_for: ApprovalScope,
    /// Session lifetime in hours. Sessions are stored in the database (#9), so
    /// shortening this is the right knob if you want forced re-auth.
    pub session_ttl_hours: u32,
    /// Argon2id parameters used for password hashing.
    pub argon2: Argon2Config,
}

/// Registration policy. `#[non_exhaustive]` because adding e.g. `OAuthOnly`
/// later should not be a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum RegistrationPolicy {
    /// Anyone can register an account.
    Open,
    /// Registration requires an invite code.
    Invite,
    /// Registration is disabled. Administrators create accounts manually.
    Closed,
}

/// Which edit submissions land in the approval queue before going live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ApprovalScope {
    /// Edits go live immediately.
    None,
    /// Anonymous edits require approval; logged-in edits go live.
    Anonymous,
    /// Anonymous and new (recently-registered) accounts require approval.
    NewUsers,
    /// Every edit requires approval. Useful for high-security deploys.
    All,
}

/// Argon2id tuning. Defaults target a sensible production posture
/// (64 MiB / 3 iterations / 1 lane).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Argon2Config {
    /// Memory cost in KiB. OWASP recommends ≥ 19 MiB (19456 KiB).
    pub memory_kib: u32,
    /// Number of iterations (time cost). OWASP recommends ≥ 2.
    pub iterations: u32,
    /// Parallelism (lanes). OWASP recommends ≥ 1.
    pub parallelism: u32,
}

/// Rate limiting configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateLimitConfig {
    /// Global switch. Keep enabled in production; tests and trusted private
    /// deployments can disable it explicitly.
    pub enabled: bool,
    /// Bucket used for safe/read methods (`GET`, `HEAD`, `OPTIONS`).
    pub read: RateLimitBucketConfig,
    /// Bucket used for mutating methods (`POST`, `PUT`, `PATCH`, `DELETE`, ...).
    pub write: RateLimitBucketConfig,
    /// Optional proxy header used to derive the client IP. Only honored when
    /// the socket peer is in `trusted_proxies`.
    #[serde(default)]
    pub client_ip_header: Option<ClientIpHeader>,
    /// Proxy IPs that are allowed to supply `client_ip_header`.
    #[serde(default)]
    pub trusted_proxies: Vec<IpAddr>,
    /// Storage backend used for bucket state.
    pub backend: RateLimitBackendConfig,
}

/// Token-bucket shape for one request class.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateLimitBucketConfig {
    /// Maximum burst size.
    pub capacity: u32,
    /// Number of tokens restored every `refill_interval_secs`.
    pub refill_tokens: u32,
    /// Refill interval in seconds.
    pub refill_interval_secs: u64,
}

/// Reverse-proxy header used for rate-limit client IP extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ClientIpHeader {
    /// `X-Forwarded-For` chain. When the socket peer is trusted, the limiter
    /// scans the comma-separated list from right to left and selects the first
    /// IP that is not listed in `rate_limit.trusted_proxies`.
    XForwardedFor,
    /// Single IP in `X-Real-IP`.
    XRealIp,
}

/// Rate-limit bucket storage backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[non_exhaustive]
pub enum RateLimitBackendConfig {
    /// Per-process in-memory buckets. This is the default and is suitable for
    /// single-binary/single-replica deploys.
    InMemory,
}

/// Full-text search (Tantivy) configuration (#26).
///
/// `index_path` is where the Tantivy segments live on disk. The default
/// (`./data/search/`) keeps the index next to the SQLite database for
/// single-binary deploys. Operators running on a separate disk (or a
/// network mount) typically override this.
///
/// `commit_interval_ms` / `batch_size` tune the async indexer worker —
/// lowering either improves freshness at the cost of write amplification.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchConfig {
    /// Where the Tantivy index lives on disk. The directory is created on
    /// first boot.
    pub index_path: PathBuf,
    /// Commit cadence, in milliseconds. The worker commits every
    /// `commit_interval_ms` or every `batch_size` jobs, whichever comes
    /// first. Default: 200 ms.
    pub commit_interval_ms: u64,
    /// Per-commit job-count threshold. Default: 100.
    pub batch_size: u32,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            index_path: PathBuf::from("data/search"),
            commit_interval_ms: 200,
            batch_size: 100,
        }
    }
}

/// Audit-log storage policy.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditLogConfig {
    /// Number of days to retain audit rows. Defaults to 365.
    pub retention_days: u32,
}

/// Observability configuration consumed by [`crate::telemetry`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetryConfig {
    pub log_format: LogFormat,
    /// `tracing_subscriber` env-filter directive (e.g. `"info,thewiki=debug"`).
    pub log_filter: String,
}

/// Log output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum LogFormat {
    /// Structured JSON, one event per line. Default — friendly to log shippers.
    Json,
    /// Human-readable pretty output. Use during local development.
    Pretty,
}

/// Errors surfaced by [`Config::load`] and [`Config::validate`].
///
/// `#[non_exhaustive]` so we can grow the enum (e.g. add `Io`) without breaking
/// callers.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// An explicit config file path was supplied but the file does not exist.
    #[error("config file not found: {0}")]
    NotFound(PathBuf),

    /// `figment` failed to parse or merge the layered providers.
    ///
    /// Boxed because `figment::Error` is wide (~200 B) and pushes the whole
    /// enum into clippy's `result_large_err` warning bucket.
    #[error("failed to parse configuration: {0}")]
    Parse(#[from] Box<figment::Error>),

    /// Cross-field validation failed (e.g. empty `database.url`, unparseable
    /// bind address, or Argon2 parameters below the OWASP floor).
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

// Minimum Argon2id parameters, per OWASP password storage cheat sheet (2024).
// Going below these is a misconfiguration that we reject up front.
const ARGON2_MIN_MEMORY_KIB: u32 = 19_456;
const ARGON2_MIN_ITERATIONS: u32 = 2;
const ARGON2_MIN_PARALLELISM: u32 = 1;

impl Config {
    /// Built-in defaults.
    ///
    /// Tuned for an out-of-the-box small deploy: SQLite under `data/`, blobs in
    /// the DB, registration closed, JSON logs.
    #[must_use]
    pub fn defaults() -> Self {
        Self {
            server: ServerConfig {
                bind: "0.0.0.0:8080".to_string(),
                request_timeout: Some("30s".to_string()),
                worker_threads: None,
                serve_frontend: default_serve_frontend(),
            },
            database: DatabaseConfig {
                driver: DatabaseDriver::Sqlite,
                url: "sqlite://data/thewiki.db".to_string(),
                max_connections: 16,
                acquire_timeout_secs: 10,
            },
            storage: StorageConfig {
                backend: StorageBackend::Db,
                media: MediaConfig::default(),
            },
            auth: AuthConfig {
                anonymous_edits: false,
                registration: RegistrationPolicy::Closed,
                approval_required_for: ApprovalScope::None,
                session_ttl_hours: 24,
                argon2: Argon2Config {
                    memory_kib: 65_536,
                    iterations: 3,
                    parallelism: 1,
                },
            },
            search: SearchConfig::default(),
            rate_limit: RateLimitConfig {
                enabled: true,
                read: RateLimitBucketConfig {
                    capacity: 120,
                    refill_tokens: 120,
                    refill_interval_secs: 60,
                },
                write: RateLimitBucketConfig {
                    capacity: 30,
                    refill_tokens: 30,
                    refill_interval_secs: 60,
                },
                client_ip_header: None,
                trusted_proxies: Vec::new(),
                backend: RateLimitBackendConfig::InMemory,
            },
            audit_log: AuditLogConfig {
                retention_days: 365,
            },
            telemetry: TelemetryConfig {
                log_format: LogFormat::Json,
                log_filter: "info,thewiki=debug".to_string(),
            },
        }
    }

    /// Load configuration, merging built-in defaults, optional TOML file, and
    /// the `THEWIKI_*` environment.
    ///
    /// # Errors
    ///
    /// - [`ConfigError::NotFound`] if `file_path` is `Some` but the file is
    ///   missing.
    /// - [`ConfigError::Parse`] for malformed TOML or env values that don't
    ///   deserialise into [`Config`].
    pub fn load(file_path: Option<&Path>) -> Result<Self, ConfigError> {
        let mut figment = Figment::from(Serialized::defaults(Self::defaults()));

        if let Some(path) = file_path {
            // Read the file ourselves rather than probing existence with
            // `exists()` followed by `Toml::file(path)` — the latter is a
            // TOCTOU race (the file can vanish between calls, especially
            // with mounted secrets in containers).
            let body = match std::fs::read_to_string(path) {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(ConfigError::NotFound(path.to_path_buf()));
                }
                Err(e) => {
                    return Err(ConfigError::Invalid(format!(
                        "could not read config file {}: {e}",
                        path.display()
                    )));
                }
            };
            figment = figment.merge(Toml::string(&body));
        }

        // Double-underscore separator: `THEWIKI_SERVER__BIND` -> `server.bind`.
        // A single underscore would collide with snake_case field names like
        // `acquire_timeout_secs`.
        figment = figment.merge(Env::prefixed("THEWIKI_").split("__"));

        let cfg: Self = figment.extract().map_err(Box::new)?;
        Ok(cfg)
    }

    /// Cross-field validation. Run after [`Config::load`] before standing up
    /// the runtime.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Invalid`] when any of the structural invariants
    /// fail (empty DB URL, unparseable bind address, Argon2 params below the
    /// OWASP floor, ...).
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.database.url.trim().is_empty() {
            return Err(ConfigError::Invalid("database.url is empty".to_string()));
        }

        if self.server.bind.parse::<SocketAddr>().is_err() {
            return Err(ConfigError::Invalid(format!(
                "server.bind is not a valid socket address: {:?}",
                self.server.bind
            )));
        }

        let a = &self.auth.argon2;
        if a.memory_kib < ARGON2_MIN_MEMORY_KIB {
            return Err(ConfigError::Invalid(format!(
                "auth.argon2.memory_kib = {} is below the OWASP floor of {} KiB",
                a.memory_kib, ARGON2_MIN_MEMORY_KIB
            )));
        }
        if a.iterations < ARGON2_MIN_ITERATIONS {
            return Err(ConfigError::Invalid(format!(
                "auth.argon2.iterations = {} is below the OWASP floor of {}",
                a.iterations, ARGON2_MIN_ITERATIONS
            )));
        }
        if a.parallelism < ARGON2_MIN_PARALLELISM {
            return Err(ConfigError::Invalid(format!(
                "auth.argon2.parallelism = {} is below the OWASP floor of {}",
                a.parallelism, ARGON2_MIN_PARALLELISM
            )));
        }

        // Soft-warn combinations that aren't strictly invalid but tend to bite
        // operators. Tracing is initialised by the binary before validate is
        // called, so these are visible in startup logs.
        if self.auth.anonymous_edits && self.auth.registration == RegistrationPolicy::Closed {
            tracing::warn!(
                "auth.anonymous_edits = true with auth.registration = closed: anonymous \
                 visitors can edit but cannot create accounts to take ownership of edits"
            );
        }

        if let StorageBackend::S3 { bucket, region, .. } = &self.storage.backend
            && (bucket.trim().is_empty() || region.trim().is_empty())
        {
            return Err(ConfigError::Invalid(
                "storage.backend = s3 requires both `bucket` and `region` to be non-empty"
                    .to_string(),
            ));
        }
        if self.storage.media.max_upload_bytes == 0 {
            return Err(ConfigError::Invalid(
                "storage.media.max_upload_bytes must be > 0".to_string(),
            ));
        }
        if self.storage.media.allowed_content_types.is_empty() {
            return Err(ConfigError::Invalid(
                "storage.media.allowed_content_types must list at least one MIME type".to_string(),
            ));
        }

        validate_rate_limit_bucket("rate_limit.read", &self.rate_limit.read)?;
        validate_rate_limit_bucket("rate_limit.write", &self.rate_limit.write)?;
        if self.rate_limit.client_ip_header.is_some() && self.rate_limit.trusted_proxies.is_empty()
        {
            return Err(ConfigError::Invalid(
                "rate_limit.client_ip_header requires at least one trusted proxy".to_string(),
            ));
        }
        if self.audit_log.retention_days == 0 {
            return Err(ConfigError::Invalid(
                "audit_log.retention_days must be > 0".to_string(),
            ));
        }

        if self.search.index_path.as_os_str().is_empty() {
            return Err(ConfigError::Invalid(
                "search.index_path must be non-empty".to_string(),
            ));
        }
        if self.search.commit_interval_ms == 0 {
            return Err(ConfigError::Invalid(
                "search.commit_interval_ms must be > 0".to_string(),
            ));
        }
        if self.search.batch_size == 0 {
            return Err(ConfigError::Invalid(
                "search.batch_size must be > 0".to_string(),
            ));
        }

        Ok(())
    }
}

fn validate_rate_limit_bucket(
    name: &str,
    bucket: &RateLimitBucketConfig,
) -> Result<(), ConfigError> {
    if bucket.capacity == 0 {
        return Err(ConfigError::Invalid(format!("{name}.capacity must be > 0")));
    }
    if bucket.refill_tokens == 0 {
        return Err(ConfigError::Invalid(format!(
            "{name}.refill_tokens must be > 0"
        )));
    }
    if bucket.refill_interval_secs == 0 {
        return Err(ConfigError::Invalid(format!(
            "{name}.refill_interval_secs must be > 0"
        )));
    }
    Ok(())
}

impl Default for Config {
    fn default() -> Self {
        Self::defaults()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        let cfg = Config::defaults();
        cfg.validate().expect("built-in defaults must validate");
    }

    #[test]
    fn validate_rejects_empty_database_url() {
        let mut cfg = Config::defaults();
        cfg.database.url = String::new();
        let err = cfg.validate().expect_err("empty url must be rejected");
        assert!(matches!(err, ConfigError::Invalid(_)));
    }

    #[test]
    fn validate_rejects_unparseable_bind() {
        let mut cfg = Config::defaults();
        cfg.server.bind = "not-an-addr".to_string();
        let err = cfg.validate().expect_err("bad bind must be rejected");
        assert!(matches!(err, ConfigError::Invalid(_)));
    }

    #[test]
    fn validate_rejects_weak_argon2() {
        let mut cfg = Config::defaults();
        cfg.auth.argon2.memory_kib = 1024;
        let err = cfg.validate().expect_err("weak argon2 must be rejected");
        assert!(matches!(err, ConfigError::Invalid(_)));
    }
}
