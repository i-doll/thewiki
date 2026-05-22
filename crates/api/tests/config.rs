//! Integration tests for the layered config loader (#8).
//!
//! Each test that touches the environment runs inside a `figment::Jail` so the
//! `THEWIKI_*` namespace stays isolated from sibling tests and the host shell.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use figment::Jail;
use thewiki_api::config::{
    ApprovalScope, ClientIpHeader, Config, ConfigError, LogFormat, RateLimitBackendConfig,
    RegistrationPolicy, StorageBackend,
};

#[test]
fn defaults_match_documented_values() {
    let cfg = Config::defaults();

    assert_eq!(cfg.server.bind, "0.0.0.0:8080");
    assert_eq!(cfg.database.url, "sqlite://data/thewiki.db");
    assert_eq!(cfg.database.max_connections, 16);
    assert!(!cfg.auth.anonymous_edits);
    assert_eq!(cfg.auth.registration, RegistrationPolicy::Closed);
    assert_eq!(cfg.auth.approval_required_for, ApprovalScope::None);
    assert_eq!(cfg.auth.argon2.memory_kib, 65_536);
    assert_eq!(cfg.auth.argon2.iterations, 3);
    assert!(cfg.rate_limit.enabled);
    // Anonymous defaults: 60 reads/min, 10 writes/min — opinionated and
    // tunable. Authenticated users get 10× higher (600/120).
    assert_eq!(cfg.rate_limit.read.capacity, 60);
    assert_eq!(cfg.rate_limit.read.refill_tokens, 60);
    assert_eq!(cfg.rate_limit.read.refill_interval_secs, 60);
    assert_eq!(cfg.rate_limit.write.capacity, 10);
    assert_eq!(cfg.rate_limit.write.refill_tokens, 10);
    assert_eq!(cfg.rate_limit.write.refill_interval_secs, 60);
    let auth_read = cfg
        .rate_limit
        .authenticated_read
        .expect("authenticated read bucket default");
    assert_eq!(auth_read.capacity, 600);
    assert_eq!(auth_read.refill_tokens, 600);
    assert_eq!(auth_read.refill_interval_secs, 60);
    let auth_write = cfg
        .rate_limit
        .authenticated_write
        .expect("authenticated write bucket default");
    assert_eq!(auth_write.capacity, 120);
    assert_eq!(auth_write.refill_tokens, 120);
    assert_eq!(auth_write.refill_interval_secs, 60);
    assert_eq!(cfg.rate_limit.client_ip_header, None);
    assert!(cfg.rate_limit.trusted_proxies.is_empty());
    assert_eq!(cfg.rate_limit.backend, RateLimitBackendConfig::InMemory);
    assert_eq!(cfg.audit_log.retention_days, 365);
    assert_eq!(cfg.telemetry.log_format, LogFormat::Json);
    assert!(matches!(cfg.storage.backend, StorageBackend::Db));

    cfg.validate().expect("defaults validate");
}

#[test]
fn file_overrides_defaults() {
    Jail::expect_with(|jail| {
        // Belt-and-braces: make sure no env var bleeds in.
        jail.clear_env();

        jail.create_file(
            "thewiki.toml",
            r#"
[server]
bind = "127.0.0.1:1234"
request_timeout = "5s"

[database]
url = "sqlite://override.db"
max_connections = 32
acquire_timeout_secs = 15

[storage]
backend = { kind = "db" }

[auth]
anonymous_edits = true
registration = "open"
approval_required_for = "anonymous"
session_ttl_hours = 12

[auth.argon2]
memory_kib = 65536
iterations = 3
parallelism = 1

[rate_limit]
enabled = false
client_ip_header = "x-forwarded-for"
trusted_proxies = ["127.0.0.1"]

[rate_limit.read]
capacity = 10
refill_tokens = 5
refill_interval_secs = 2

[rate_limit.write]
capacity = 3
refill_tokens = 1
refill_interval_secs = 4

[rate_limit.backend]
kind = "in-memory"

[audit_log]
retention_days = 30

[telemetry]
log_format = "pretty"
log_filter = "debug"
"#,
        )?;

        let cfg =
            Config::load(Some(std::path::Path::new("thewiki.toml"))).expect("file load succeeds");

        assert_eq!(cfg.server.bind, "127.0.0.1:1234");
        assert_eq!(cfg.server.request_timeout.as_deref(), Some("5s"));
        assert_eq!(cfg.database.url, "sqlite://override.db");
        assert_eq!(cfg.database.max_connections, 32);
        assert!(cfg.auth.anonymous_edits);
        assert_eq!(cfg.auth.registration, RegistrationPolicy::Open);
        assert_eq!(cfg.auth.approval_required_for, ApprovalScope::Anonymous);
        assert!(!cfg.rate_limit.enabled);
        assert_eq!(cfg.rate_limit.read.capacity, 10);
        assert_eq!(cfg.rate_limit.read.refill_tokens, 5);
        assert_eq!(cfg.rate_limit.read.refill_interval_secs, 2);
        assert_eq!(cfg.rate_limit.write.capacity, 3);
        assert_eq!(cfg.rate_limit.write.refill_tokens, 1);
        assert_eq!(cfg.rate_limit.write.refill_interval_secs, 4);
        assert_eq!(
            cfg.rate_limit.client_ip_header,
            Some(ClientIpHeader::XForwardedFor)
        );
        assert_eq!(cfg.rate_limit.trusted_proxies.len(), 1);
        assert_eq!(cfg.rate_limit.backend, RateLimitBackendConfig::InMemory);
        assert_eq!(cfg.audit_log.retention_days, 30);
        assert_eq!(cfg.telemetry.log_format, LogFormat::Pretty);
        Ok(())
    });
}

#[test]
fn env_overrides_file() {
    Jail::expect_with(|jail| {
        jail.clear_env();
        jail.create_file(
            "thewiki.toml",
            r#"
[server]
bind = "127.0.0.1:1234"

[database]
url = "sqlite://from-file.db"
max_connections = 8
acquire_timeout_secs = 10

[storage]
backend = { kind = "db" }

[auth]
anonymous_edits = false
registration = "closed"
approval_required_for = "none"
session_ttl_hours = 24
[auth.argon2]
memory_kib = 65536
iterations = 3
parallelism = 1

[telemetry]
log_format = "json"
log_filter = "info"
"#,
        )?;

        jail.set_env("THEWIKI_SERVER__BIND", "127.0.0.1:9000");
        jail.set_env("THEWIKI_DATABASE__MAX_CONNECTIONS", "64");
        jail.set_env("THEWIKI_RATE_LIMIT__WRITE__CAPACITY", "7");
        jail.set_env("THEWIKI_AUDIT_LOG__RETENTION_DAYS", "90");

        let cfg = Config::load(Some(std::path::Path::new("thewiki.toml")))
            .expect("layered load succeeds");

        assert_eq!(
            cfg.server.bind, "127.0.0.1:9000",
            "env should override file"
        );
        assert_eq!(
            cfg.database.max_connections, 64,
            "env should override file for nested keys too"
        );
        assert_eq!(cfg.rate_limit.write.capacity, 7);
        assert_eq!(cfg.audit_log.retention_days, 90);
        // Fields not touched by env keep their file value.
        assert_eq!(cfg.database.url, "sqlite://from-file.db");
        Ok(())
    });
}

#[test]
fn env_alone_overrides_defaults_when_no_file_is_supplied() {
    Jail::expect_with(|jail| {
        jail.clear_env();
        jail.set_env("THEWIKI_SERVER__BIND", "127.0.0.1:5555");

        let cfg = Config::load(None).expect("env-only load succeeds");
        assert_eq!(cfg.server.bind, "127.0.0.1:5555");
        // Untouched fields fall back to defaults.
        assert_eq!(cfg.database.url, "sqlite://data/thewiki.db");
        Ok(())
    });
}

#[test]
fn missing_file_at_explicit_path_is_an_error() {
    Jail::expect_with(|jail| {
        jail.clear_env();
        let err = Config::load(Some(std::path::Path::new("/nonexistent/thewiki.toml")))
            .expect_err("missing file must error");
        assert!(matches!(err, ConfigError::NotFound(_)));
        Ok(())
    });
}

#[test]
fn validate_rejects_empty_database_url() {
    let mut cfg = Config::defaults();
    cfg.database.url = String::new();
    let err = cfg.validate().expect_err("empty url must be rejected");
    assert!(matches!(err, ConfigError::Invalid(_)));
}

#[test]
fn validate_rejects_zero_rate_limit_capacity() {
    let mut cfg = Config::defaults();
    cfg.rate_limit.read.capacity = 0;
    let err = cfg
        .validate()
        .expect_err("zero read capacity must be rejected");
    assert!(matches!(err, ConfigError::Invalid(_)));
}

#[test]
fn validate_rejects_zero_rate_limit_refill_tokens() {
    let mut cfg = Config::defaults();
    cfg.rate_limit.write.refill_tokens = 0;
    let err = cfg
        .validate()
        .expect_err("zero write refill tokens must be rejected");
    assert!(matches!(err, ConfigError::Invalid(_)));
}

#[test]
fn validate_rejects_zero_rate_limit_refill_interval() {
    let mut cfg = Config::defaults();
    cfg.rate_limit.write.refill_interval_secs = 0;
    let err = cfg
        .validate()
        .expect_err("zero write refill interval must be rejected");
    assert!(matches!(err, ConfigError::Invalid(_)));
}

#[test]
fn validate_rejects_proxy_header_without_trusted_proxy() {
    let mut cfg = Config::defaults();
    cfg.rate_limit.client_ip_header = Some(ClientIpHeader::XForwardedFor);
    let err = cfg
        .validate()
        .expect_err("proxy header without trusted proxies must be rejected");
    assert!(matches!(err, ConfigError::Invalid(_)));
}

#[test]
fn validate_rejects_zero_audit_log_retention() {
    let mut cfg = Config::defaults();
    cfg.audit_log.retention_days = 0;
    let err = cfg
        .validate()
        .expect_err("zero audit retention must be rejected");
    assert!(matches!(err, ConfigError::Invalid(_)));
}

#[test]
fn redis_backend_round_trips_through_toml() {
    Jail::expect_with(|jail| {
        jail.clear_env();
        jail.create_file(
            "thewiki.toml",
            r#"
[database]
url = "sqlite::memory:"
max_connections = 1
acquire_timeout_secs = 5

[rate_limit.backend]
kind = "redis"
url = "redis://127.0.0.1:6379/0"
"#,
        )?;
        let cfg =
            Config::load(Some(std::path::Path::new("thewiki.toml"))).expect("redis backend parses");
        match cfg.rate_limit.backend {
            RateLimitBackendConfig::Redis { url } => {
                assert_eq!(url, "redis://127.0.0.1:6379/0");
            }
            other => panic!("expected redis backend, got {other:?}"),
        }
        Ok(())
    });
}

#[test]
fn validate_rejects_redis_backend_without_url() {
    let mut cfg = Config::defaults();
    cfg.rate_limit.backend = RateLimitBackendConfig::Redis { url: String::new() };
    let err = cfg
        .validate()
        .expect_err("empty redis url must be rejected");
    assert!(matches!(err, ConfigError::Invalid(_)));
}
