//! Admin read-only viewer for the runtime configuration (#47).
//!
//! `GET /api/v1/admin/config` serialises the loaded [`Config`] to JSON with
//! sensitive fields redacted before they leave the server. Today the only
//! such field is `captcha.secret_key`; we project it to `null` in the
//! response. The serialised shape otherwise mirrors the TOML the operator
//! deployed verbatim — admins can spot misconfigurations without shell
//! access to the host.
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
    /// Operators see every value the binary loaded, with secrets blanked
    /// to `null`. The structure mirrors the TOML 1:1 so a `diff` against
    /// the on-disk file is straightforward.
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

    if let Some(captcha) = value.get_mut("captcha")
        && let Some(map) = captcha.as_object_mut()
        && let Some(secret) = map.get_mut("secret_key")
    {
        // Replace with the literal sentinel string so the SPA can render
        // "configured" / "not configured" without leaking the value.
        let configured = secret.as_str().map(|s| !s.trim().is_empty()).unwrap_or(false);
        *secret = json!(if configured { "<redacted>" } else { "" });
    }

    Ok(value)
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
}
