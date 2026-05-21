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

use std::net::SocketAddr;
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
    /// Database URL, in sqlx's URL syntax. SQLite is the only M0 backend;
    /// `postgres://` and `libsql://` land in M1.
    pub url: String,
    /// Maximum number of pooled connections.
    pub max_connections: u32,
    /// Timeout (seconds) for acquiring a connection from the pool before
    /// returning an error to the caller.
    pub acquire_timeout_secs: u64,
}

/// Object storage backend selector.
///
/// `Db` keeps blobs in the primary database (simple, works out of the box).
/// `S3` ships in M1 alongside the rest of the media-upload flow.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    pub backend: StorageBackend,
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
                url: "sqlite://data/thewiki.db".to_string(),
                max_connections: 16,
                acquire_timeout_secs: 10,
            },
            storage: StorageConfig {
                backend: StorageBackend::Db,
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

        Ok(())
    }
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
