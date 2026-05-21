//! Storage layer for `thewiki`.
//!
//! This crate owns the persistence side of the system: connection pools,
//! `Repository` trait implementations per backend (SQLite at M0; libsql and
//! Postgres at M1), the migration runner, and `object_store` glue for blob
//! storage.
//!
//! The crate is currently a shell — the SQLite adapter lands in issue #6 and
//! the migration assets live at the repo root under `/migrations`. This file
//! exists so `cargo build` keeps working and so downstream crates can already
//! depend on `thewiki-storage` without conditional compilation.
//!
//! See `docs/ARCHITECTURE.md` § "Database story" for the cross-backend plan.
