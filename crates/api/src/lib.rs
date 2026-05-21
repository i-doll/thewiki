//! thewiki HTTP API library.
//!
//! The crate is both a library and a binary: the binary (`thewiki`) is a thin
//! `main` that parses the CLI and calls into this library. Exposing the app as
//! a library lets integration tests construct a [`Router`](axum::Router)
//! directly via [`app::build`] without spinning up a real listener.

pub mod app;
pub mod cli;
pub mod config;
pub mod telemetry;

pub use app::build;
pub use config::Config;
