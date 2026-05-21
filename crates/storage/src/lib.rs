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
//! behind the `sqlite` feature, on by default). M1 adds libsql and Postgres
//! the same way.
//!
//! All trait methods return [`Result<T, StorageError>`](error::StorageError).
//! See [`docs/ARCHITECTURE.md` § "Database story"][arch] for the cross-backend
//! plan.
//!
//! [arch]: https://github.com/i-doll/thewiki/blob/main/docs/ARCHITECTURE.md

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod error;
pub mod repo;

#[cfg(feature = "sqlite")]
pub mod sqlite;

pub use error::StorageError;
