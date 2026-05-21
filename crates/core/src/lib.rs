//! Core domain models, traits, and shared types for `thewiki`.
//!
//! This crate is the **stable seam** between concrete implementations
//! (storage, renderer, search, API) and the data they all agree on. It
//! depends on nothing internal and performs no I/O.
//!
//! # Domain entities
//!
//! ```text
//!         ┌──────────────┐         1   *  ┌──────────────┐
//!         │  Namespace   │◄───────────────│     Page     │
//!         └──────────────┘                └──────┬───────┘
//!                                                │ 1
//!                                                │
//!                                                │ *
//!                                         ┌──────▼───────┐
//!                                         │   Revision   │
//!                                         └──────┬───────┘
//!                                                │ *  authored by  1
//!                                                ▼
//!                                         ┌──────────────┐
//!                                         │     User     │
//!                                         └──────┬───────┘
//!                                                │ *
//!                                                │   holds
//!                                                ▼ *
//!                                         ┌──────────────┐
//!                                         │     Role     │── Permissions (bitflags)
//!                                         └──────────────┘
//! ```
//!
//! - A [`Namespace`] partitions the page space; `(namespace_id, slug)`
//!   uniquely identifies a [`Page`].
//! - A [`Page`] points at its current head [`Revision`] via
//!   `current_revision_id`. The body of a page is stored on the revision,
//!   not the page row itself — the page is the *identity*, the revision is
//!   the *content*.
//! - [`Revision`]s form a linear, append-only history per page. Each
//!   revision links back to its `parent_id` (`None` for the first), the
//!   `author_id` who committed it, and carries a raw `body` in the page's
//!   [`ContentFormat`]. Reverting a page means committing a new revision
//!   whose body matches an older one — old rows never change.
//! - A [`User`] holds zero or more [`Role`]s. Their effective capability
//!   set is the union of their roles' [`Permissions`]. Permissions are the
//!   only place capability lives; roles are a convenience name for a
//!   bitmask.
//! - Each [`Page`] additionally carries a [`ProtectionLevel`] which guards
//!   edits independently of the role check (e.g. "fully-protected pages
//!   require admin").
//!
//! # IDs
//!
//! Every entity uses a newtype wrapper around `uuid::Uuid` (see [`id`])
//! minted with UUIDv7 — time-ordered, which both keeps DB B-Tree index
//! locality healthy and makes log output sort by creation time.
//!
//! # Stability
//!
//! Public types derive [`serde::Serialize`], [`serde::Deserialize`] and
//! [`utoipa::ToSchema`] so they round-trip cleanly to storage and through
//! the generated OpenAPI surface. Enums that will grow new variants
//! ([`ContentFormat`], [`ProtectionLevel`]) are marked `#[non_exhaustive]`
//! so downstream `match` expressions stay forward-compatible.
//!
//! # Not yet here
//!
//! - The `Renderer` trait sketch from `docs/ARCHITECTURE.md` is owned by
//!   issue #3; it will live alongside `RenderContext`, `RenderedHtml`,
//!   `RenderError`, and `WikiLink` in a future `render` module.
//! - The repository / search-index / object-store traits are owned by
//!   later issues (#5+).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod content_format;
pub mod id;
pub mod namespace;
pub mod page;
pub mod permissions;
pub mod protection;
pub mod revision;
pub mod role;
pub mod user;
pub mod validation;

pub use content_format::ContentFormat;
pub use id::{NamespaceId, PageId, RevisionId, RoleId, UserId};
pub use namespace::{NAMESPACE_SLUG_MAX_BYTES, Namespace, NamespaceSlug};
pub use page::Page;
pub use permissions::Permissions;
pub use protection::ProtectionLevel;
pub use revision::Revision;
pub use role::{ROLE_NAME_MAX_BYTES, Role, RoleName};
pub use user::{EmailAddress, USERNAME_MAX_BYTES, User, Username};
pub use validation::ValidationError;
