//! Typed application state for the auth layer.
//!
//! [`AuthState`] carries the pieces the auth handlers and extractors need —
//! the storage facade, the configured password hasher, and the session TTL.
//! It is `Clone`-cheap (everything inside is an `Arc` or a `Copy` primitive)
//! and lives in axum's `State<AuthState>`.

use std::sync::Arc;
use std::time::Duration;

use thewiki_storage::sqlite::SqliteStorage;

use crate::auth::password::Argon2Hasher;
use crate::config::AuthConfig;

/// Auth-related app state.
///
/// `storage` is the concrete SQLite facade. We could narrow it to a set of
/// repository handles, but the trait + opaque `Arc<dyn …>` route doesn't
/// work today (see the comment block in
/// [`thewiki_storage::repo`](thewiki_storage::repo)), so a concrete struct
/// is the simpler call. Swapping in `postgres`/`libsql` at M1 means swapping
/// the field type — the handlers stay the same.
#[derive(Debug, Clone)]
pub struct AuthState {
    /// Storage facade. Cloning is cheap — the inner `SqlitePool` is an `Arc`.
    pub storage: SqliteStorage,
    /// Argon2id hasher wired from `auth.argon2`.
    pub hasher: Arc<Argon2Hasher>,
    /// Per-session lifetime; passed to the session-issuance call.
    pub session_ttl: Duration,
    /// `Secure` cookie attribute switch. `true` in production / when behind
    /// TLS; `false` for local development over plain HTTP. Defaults to `true`
    /// — operators flip it via the `--insecure-cookie` CLI flag.
    pub secure_cookies: bool,
    /// Snapshot of `Config::auth` — published verbatim by
    /// `GET /api/v1/auth/policy` so the SPA can surface the right login /
    /// signup affordances. Also consulted by the configurable-auth extractors
    /// via the `AppState` indirection.
    pub config: AuthConfig,
}

impl AuthState {
    /// Build the state from its parts.
    #[must_use]
    pub fn new(
        storage: SqliteStorage,
        hasher: Arc<Argon2Hasher>,
        session_ttl: Duration,
        secure_cookies: bool,
        config: AuthConfig,
    ) -> Self {
        Self {
            storage,
            hasher,
            session_ttl,
            secure_cookies,
            config,
        }
    }
}
