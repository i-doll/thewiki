//! Integration tests for the layered config loader (#8).
//!
//! Each test that touches the environment runs inside a `figment::Jail` so the
//! `THEWIKI_*` namespace stays isolated from sibling tests and the host shell.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use figment::Jail;
use thewiki_api::config::{
    ApprovalScope, Config, ConfigError, LogFormat, RegistrationPolicy, StorageBackend,
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
