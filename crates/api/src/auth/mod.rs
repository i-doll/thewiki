//! Authentication scaffold for the HTTP API (#13).
//!
//! What lives here, in order from lowest-level to handler-facing:
//!
//! - [`password`] — Argon2id [`PasswordHasher`] + [`Argon2Hasher`], wired from
//!   the operator-supplied [`Argon2Config`](crate::config::Argon2Config).
//! - [`session`] — server-side session issuance + the cookie shape we hand
//!   back to clients. Persistent storage lives in `thewiki-storage`.
//! - [`extractors`] — Axum extractors that pull the `thewiki_session` cookie
//!   off the request, resolve it through the [`SessionRepository`], and stamp
//!   the resulting [`AuthSession`] (and the user's effective [`Permissions`])
//!   on the request scope.
//! - [`csrf`] — double-submit cookie middleware. `SameSite=Strict` carries
//!   most of the weight; this layer is defence in depth for mutating routes.
//! - [`routes`] — the three handlers from the spec
//!   (`/api/v1/auth/login`, `/logout`, `/me`).
//! - [`state`] — typed [`AuthState`] container the router hands to handlers.
//!
//! Page CRUD wiring lives in #14 and #9 — this crate only ships the
//! machinery.
//!
//! [`SessionRepository`]: thewiki_storage::repo::SessionRepository
//! [`Permissions`]: thewiki_core::Permissions

pub mod csrf;
pub mod error;
pub mod extractors;
pub mod password;
pub mod routes;
pub mod session;
pub mod state;

pub use error::AuthError;
pub use extractors::{AuthSession, RequireRole};
pub use password::{Argon2Hasher, PasswordHasher};
pub use state::AuthState;
