//! Admin read-only viewer for the runtime configuration (#47).
//!
//! `GET /api/v1/admin/config` serialises the loaded [`Config`] to JSON with
//! sensitive fields redacted before they leave the server. Redacted leaves
//! are emitted as the literal string `"<redacted>"` when populated and the
//! empty string `""` when unset, so the SPA can render
//! "configured" / "not configured" without leaking the value. The
//! serialised shape otherwise mirrors the TOML the operator deployed
//! verbatim — admins can spot misconfigurations without shell access to
//! the host.
//!
//! Redacted fields:
//!
//! * `database.url` — for a Postgres deploy this carries embedded
//!   credentials (`postgres://user:pass@host/db`). We blank it uniformly
//!   even for backends that don't carry credentials (sqlite) so the
//!   response shape is independent of the operator's backend choice.
//! * `captcha.secret_key` — server-side hCaptcha secret.
//! * `rate_limit.backend.url` — Redis URLs can carry an inline password
//!   (`redis://:pass@host`).
//!
//! Authorisation: gated by [`Permissions::MANAGE_USERS`]. The issue (#47)
//! explicitly asks us not to add a new permission flag for the config
//! viewer, so we reuse the "real admin" bit that already gates user
//! management. Operators who only hold `MANAGE_BLOCKLIST` or
//! `VIEW_AUDIT_LOG` get a 403 here.

use axum::Json;
use axum::extract::State;
use serde::Serialize;
use serde_json::{Value, json};
use thewiki_core::Permissions;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::config::Config;
use crate::error::ApiError;
use crate::extractors::RequireAuth;
use crate::state::{AppState, AppStorage};

/// Response from `GET /api/v1/admin/config`.
#[derive(Debug, Serialize, ToSchema)]
pub struct AdminConfigResponse {
    /// `true` when the binary was booted with a config snapshot wired into
    /// state. `false` in test fixtures that don't supply one — the
    /// `config` field is `null` in that case.
    pub available: bool,
    /// Redacted runtime configuration as a free-form JSON object.
    ///
    /// Operators see every value the binary loaded, with secret-bearing
    /// leaves replaced by `"<redacted>"` (when populated) or `""` (when
    /// unset). The structure mirrors the TOML 1:1 so a `diff` against the
    /// on-disk file is straightforward.
    #[schema(schema_with = config_schema, value_type = Object)]
    pub config: Value,
}

fn config_schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
    use utoipa::openapi::schema::{AdditionalProperties, ObjectBuilder, Type};
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .description(Some(
            "Free-form JSON object mirroring the loaded TOML, with secrets redacted.",
        ))
        .additional_properties(Some(AdditionalProperties::FreeForm(true)))
        .into()
}

fn ensure_admin(actor: &RequireAuth) -> Result<(), ApiError> {
    if actor.permissions.contains(Permissions::MANAGE_USERS) {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

/// Produce the redacted JSON projection of a loaded [`Config`].
///
/// The function is `pub(crate)` so the integration tests can exercise the
/// redaction path independently of the HTTP wrapper.
pub(crate) fn redact(config: &Config) -> Result<Value, ApiError> {
    // Round-trip through serde_json. The `Config` derives `Serialize`, so
    // we don't have to mirror field names by hand; we only have to mutate
    // the well-known secret leaves before sending the value back.
    let mut value = serde_json::to_value(config)
        .map_err(|err| ApiError::Internal(format!("serialising runtime config: {err}")))?;

    // `database.url` — Postgres URLs carry inline credentials
    // (`postgres://user:pass@host/db`). SQLite URLs don't, but we blank
    // uniformly so the response shape is independent of the operator's
    // backend choice and a future driver that *does* carry credentials
    // is safe by default.
    redact_string_at(&mut value, &["database", "url"]);
    // `captcha.secret_key` — server-side hCaptcha secret.
    redact_string_at(&mut value, &["captcha", "secret_key"]);
    // `rate_limit.backend.url` — present when `backend = { kind = "redis",
    // url = "redis://[:pass@]host" }`; Redis URLs may carry an inline
    // password.
    redact_string_at(&mut value, &["rate_limit", "backend", "url"]);

    Ok(value)
}

/// Replace the JSON string leaf at `path` with the redaction sentinel.
///
/// * `"<redacted>"` when the value is a non-empty string.
/// * `""` when the value is an empty string (preserved so the SPA can
///   distinguish "configured" from "not configured").
///
/// Silently does nothing if the path is absent or doesn't point at a
/// string — the only realistic case for that is the optional
/// `rate_limit.backend.url` field, which simply isn't there for the
/// `in-memory` variant.
fn redact_string_at(value: &mut Value, path: &[&str]) {
    let Some((leaf_key, parents)) = path.split_last() else {
        return;
    };
    let mut cursor = value;
    for segment in parents {
        let Some(next) = cursor.get_mut(*segment) else {
            return;
        };
        cursor = next;
    }
    let Some(map) = cursor.as_object_mut() else {
        return;
    };
    let Some(leaf) = map.get_mut(*leaf_key) else {
        return;
    };
    // Only redact string-shaped leaves. Anything else (object, number,
    // null) isn't a secret in the current schema.
    if let Some(s) = leaf.as_str() {
        let configured = !s.trim().is_empty();
        *leaf = json!(if configured { "<redacted>" } else { "" });
    }
}

/// `GET /api/v1/admin/config` — return the redacted runtime configuration.
#[utoipa::path(
    get,
    path = "",
    responses(
        (status = 200, description = "Redacted runtime configuration", body = AdminConfigResponse),
        (status = 401, description = "Unauthenticated", body = crate::error::ErrorBody),
        (status = 403, description = "Caller lacks MANAGE_USERS", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    security(("SessionCookie" = [])),
    tag = "admin-config",
)]
pub async fn get_config<S: AppStorage>(
    State(state): State<AppState<S>>,
    actor: RequireAuth,
) -> Result<Json<AdminConfigResponse>, ApiError> {
    ensure_admin(&actor)?;
    match state.runtime_config.as_ref() {
        Some(cfg) => {
            let redacted = redact(cfg)?;
            Ok(Json(AdminConfigResponse {
                available: true,
                config: redacted,
            }))
        }
        None => Ok(Json(AdminConfigResponse {
            available: false,
            config: Value::Null,
        })),
    }
}

/// Build the config-viewer subrouter (`/api/v1/admin/config`).
pub fn router<S: AppStorage>() -> OpenApiRouter<AppState<S>> {
    OpenApiRouter::new().routes(routes!(get_config))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "ergonomic tests")]
mod tests {
    use super::*;
    use crate::config::RateLimitBackendConfig;

    #[test]
    fn redact_blanks_captcha_secret_when_set() {
        let mut cfg = Config::defaults();
        cfg.captcha.secret_key = "topsecret".into();
        let value = redact(&cfg).expect("redact");
        let secret = value
            .pointer("/captcha/secret_key")
            .and_then(|v| v.as_str());
        assert_eq!(secret, Some("<redacted>"));
        // Sanity check: site_key remains visible.
        assert!(value.pointer("/captcha/site_key").is_some());
    }

    #[test]
    fn redact_leaves_unset_secret_empty() {
        let cfg = Config::defaults();
        let value = redact(&cfg).expect("redact");
        let secret = value
            .pointer("/captcha/secret_key")
            .and_then(|v| v.as_str());
        assert_eq!(secret, Some(""));
    }

    #[test]
    fn redact_blanks_postgres_database_url() {
        let mut cfg = Config::defaults();
        cfg.database.url = "postgres://alice:s3cret@db.example.com:5432/wiki".into();
        let value = redact(&cfg).expect("redact");
        let url = value.pointer("/database/url").and_then(|v| v.as_str());
        assert_eq!(url, Some("<redacted>"));
        // Sweep the entire serialised body — neither the password nor the
        // raw URL substring should appear anywhere downstream of redaction.
        let serialised = serde_json::to_string(&value).expect("serialise");
        assert!(
            !serialised.contains("s3cret"),
            "leaked db password in body: {serialised}"
        );
        assert!(
            !serialised.contains("postgres://"),
            "leaked db URL in body: {serialised}"
        );
    }

    #[test]
    fn redact_blanks_sqlite_database_url_uniformly() {
        // SQLite URLs don't carry credentials but we redact uniformly so
        // the response shape is independent of the operator's backend
        // choice — and a future driver that *does* carry credentials is
        // safe by default.
        let cfg = Config::defaults();
        assert!(
            cfg.database.url.starts_with("sqlite://"),
            "sanity: default is sqlite, got {:?}",
            cfg.database.url
        );
        let value = redact(&cfg).expect("redact");
        let url = value.pointer("/database/url").and_then(|v| v.as_str());
        assert_eq!(url, Some("<redacted>"));
    }

    #[test]
    fn redact_blanks_redis_backend_url() {
        let mut cfg = Config::defaults();
        cfg.rate_limit.backend = RateLimitBackendConfig::Redis {
            url: "redis://:hunter2@redis.example.com:6379".into(),
        };
        let value = redact(&cfg).expect("redact");
        let url = value
            .pointer("/rate_limit/backend/url")
            .and_then(|v| v.as_str());
        assert_eq!(url, Some("<redacted>"));
        let serialised = serde_json::to_string(&value).expect("serialise");
        assert!(
            !serialised.contains("hunter2"),
            "leaked redis password: {serialised}"
        );
    }

    #[test]
    fn redact_in_memory_backend_has_no_url_to_redact() {
        // The default backend is `in-memory`, which serialises without a
        // `url` leaf. `redact_string_at` is a no-op in that case.
        let cfg = Config::defaults();
        let value = redact(&cfg).expect("redact");
        assert!(value.pointer("/rate_limit/backend/url").is_none());
    }
}
