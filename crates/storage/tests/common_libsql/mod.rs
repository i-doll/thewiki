//! Shared helpers for the libsql integration tests.
//!
//! Spins up a fresh in-memory libsql instance per test with the full migration
//! set already applied, plus seed-builder helpers for the common shapes
//! (`User`, `Namespace`, `Page`, `Role`). Mirrors the SQLite side
//! ([`tests/common/mod.rs`]).

#![cfg(feature = "libsql")]
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(dead_code, reason = "shared helpers; each test uses a subset")]

use thewiki_core::{
    ContentFormat, EmailAddress, Namespace, NamespaceId, NamespaceSlug, Page, PageId, Permissions,
    ProtectionLevel, Role, RoleId, RoleName, User, UserId, Username,
};
use thewiki_storage::libsql::{LibsqlOptions, LibsqlStorage};
use time::OffsetDateTime;

/// Boot a fresh in-memory libsql instance, apply migrations, return the
/// storage handle.
pub async fn fresh_storage() -> LibsqlStorage {
    LibsqlStorage::new(LibsqlOptions::in_memory())
        .await
        .expect("open + migrate in-memory libsql")
}

/// Build a [`Namespace`] with a deterministic slug and display name.
pub fn make_namespace(slug: &str) -> Namespace {
    Namespace {
        id: NamespaceId::new(),
        slug: NamespaceSlug::new(slug).expect("valid slug"),
        display_name: slug.to_string(),
        is_talk: false,
        paired_namespace_id: None,
    }
}

/// Build a [`User`] with the given username; everything else is filled in.
pub fn make_user(username: &str) -> User {
    User {
        id: UserId::new(),
        username: Username::new(username).expect("valid username"),
        email: Some(EmailAddress::new(format!("{username}@example.com")).expect("valid email")),
        display_name: Some(username.to_string()),
        created_at: OffsetDateTime::now_utc(),
        last_login_at: None,
    }
}

/// Build a [`Page`] in `namespace_id` with the given slug.
pub fn make_page(namespace_id: NamespaceId, slug: &str) -> Page {
    let now = OffsetDateTime::now_utc();
    Page {
        id: PageId::new(),
        namespace_id,
        slug: slug.to_string(),
        title: slug.to_string(),
        current_revision_id: None,
        content_format: ContentFormat::Markdown,
        protection_level: ProtectionLevel::None,
        created_at: now,
        updated_at: now,
    }
}

/// Build a [`Role`] with the given name + permissions.
pub fn make_role(name: &str, permissions: Permissions) -> Role {
    Role {
        id: RoleId::new(),
        name: RoleName::new(name).expect("valid role name"),
        display_name: name.to_string(),
        permissions,
    }
}
