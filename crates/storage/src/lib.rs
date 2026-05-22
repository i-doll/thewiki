//! Storage abstractions for thewiki.
//!
//! [`repo`] defines a `Repository` trait per domain aggregate
//! ([`PageRepository`](repo::PageRepository),
//! [`UserRepository`](repo::UserRepository),
//! [`RevisionRepository`](repo::RevisionRepository),
//! [`NamespaceRepository`](repo::NamespaceRepository),
//! [`RoleRepository`](repo::RoleRepository)). Each one is the entire
//! persistence surface for its aggregate; the API crate keeps `Arc<dyn …>`
//! handles in app state and stays backend-agnostic.
//!
//! Concrete backends implement those traits. M0 ships [`sqlite`] (gated
//! behind the `sqlite` feature, on by default). M1 adds [`libsql`] (#24,
//! behind the `libsql` feature) and [`postgres`] (#25, behind the `postgres`
//! feature) the same way.
//!
//! Backends share a common encode/decode layer ([`codec`]) so the wire format
//! of every row is identical: UUIDv7 as 16-byte BLOBs / native UUIDs, RFC 3339
//! timestamps / TIMESTAMPTZ, integer-packed permission bitsets. Only the
//! driver-specific error mapping (uniqueness violations, FK restrictions) is
//! implemented per-adapter.
//!
//! The `sqlite` and `libsql` features are **mutually exclusive** at build time
//! — both upstream drivers statically link their own SQLite C library
//! (`libsqlite3-sys` vs. `libsql-ffi`) and the linker rejects the duplicate
//! `sqlite3_*` symbols. Operators pick one backend per build (e.g.
//! `--no-default-features --features libsql`); the runtime dispatch story
//! will respect this by routing only one backend through.
//!
//! All trait methods return [`Result<T, StorageError>`](error::StorageError).
//! See [`docs/ARCHITECTURE.md` § "Database story"][arch] for the cross-backend
//! plan.
//!
//! [arch]: https://github.com/i-doll/thewiki/blob/main/docs/ARCHITECTURE.md

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod codec;
pub mod error;
pub mod repo;

#[cfg(feature = "sqlite")]
pub mod sqlite;

#[cfg(feature = "libsql")]
pub mod libsql;

#[cfg(feature = "postgres")]
pub mod postgres;

pub use error::StorageError;
