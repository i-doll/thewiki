//! thewiki HTTP API library.
//!
//! The crate is both a library and a binary: the binary (`thewiki`) is a thin
//! `main` that parses the CLI and calls into this library. Exposing the app as
//! a library lets integration tests construct a [`Router`](axum::Router)
//! directly via [`app::build_with_state`] without spinning up a real listener.

pub mod app;
pub mod audit_log;
pub mod auth;
pub mod cli;
pub mod config;
pub mod error;
pub mod extractors;
pub mod graphql;
pub mod media;
pub mod pages;
pub mod rate_limit;
pub mod recent_changes;
pub mod render;
pub mod search;
pub mod state;
pub mod static_assets;
pub mod telemetry;

pub use app::{
    build, build_auth_app, build_auth_app_with_rate_limit, build_full,
    build_full_with_rate_limit_state, build_with_state,
};
pub use config::Config;
pub use error::ApiError;
pub use state::{AppState, AppStorage, RouteConfig};
