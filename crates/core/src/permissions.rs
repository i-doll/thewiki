//! Capability flags carried by a [`Role`](crate::role::Role).
//!
//! Permissions are a `bitflags`-style set so a single `u32` can express any
//! combination, the union of two permission sets is `a | b`, and the
//! intersection is `a & b`. This is convenient both at the domain layer
//! (combining a user's roles) and at the storage layer (a single integer
//! column per role).
//!
//! The serde representation is the JSON-friendly textual form provided by the
//! `bitflags` crate (e.g. `"READ | EDIT"`), which is stable across changes to
//! the bit positions and survives round-tripping through OpenAPI clients.
//!
//! For OpenAPI we expose `Permissions` as a `string` — clients see the
//! human-readable flag set rather than an opaque integer.

use bitflags::bitflags;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

bitflags! {
    /// Set of capabilities a role grants to its members.
    ///
    /// The empty set [`Permissions::empty`] grants nothing; combine flags with
    /// `|` to build a richer set.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[serde(transparent)]
    pub struct Permissions: u32 {
        /// View pages and revisions.
        const READ              = 1 << 0;
        /// Edit existing pages.
        const EDIT              = 1 << 1;
        /// Create new pages.
        const CREATE            = 1 << 2;
        /// Delete pages.
        const DELETE            = 1 << 3;
        /// Move (rename) pages.
        const MOVE              = 1 << 4;
        /// Change a page's protection level.
        const PROTECT           = 1 << 5;
        /// Create, edit and disable user accounts.
        const MANAGE_USERS      = 1 << 6;
        /// Create, edit and delete roles.
        const MANAGE_ROLES      = 1 << 7;
        /// Create, edit and delete namespaces.
        const MANAGE_NAMESPACES = 1 << 8;
        /// View the administrative audit log.
        const VIEW_AUDIT_LOG    = 1 << 9;
        /// Manage the IP / URL blocklists (#42).
        const MANAGE_BLOCKLIST  = 1 << 10;
    }
}

// `bitflags` does not generate a `ToSchema` impl, so we describe the type to
// `utoipa` by hand: a string in the form `"READ | EDIT | CREATE"`.
impl utoipa::PartialSchema for Permissions {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{SchemaType, Type};
        use utoipa::openapi::{ObjectBuilder, RefOr, Schema};

        let object = ObjectBuilder::new()
            .schema_type(SchemaType::Type(Type::String))
            .description(Some(
                "Pipe-separated permission flags (e.g. `READ | EDIT | CREATE`).",
            ))
            .build();
        RefOr::T(Schema::Object(object))
    }
}

impl ToSchema for Permissions {
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Permissions")
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "ergonomic tests")]
mod tests {
    use super::*;

    #[test]
    fn empty_is_empty() {
        assert!(Permissions::empty().is_empty());
        assert_eq!(Permissions::empty().bits(), 0);
    }

    #[test]
    fn union_combines_flags() {
        let combined = Permissions::READ | Permissions::EDIT;
        assert!(combined.contains(Permissions::READ));
        assert!(combined.contains(Permissions::EDIT));
        assert!(!combined.contains(Permissions::DELETE));
    }

    #[test]
    fn intersection_isolates_flags() {
        let admin = Permissions::all();
        let editor = Permissions::READ | Permissions::EDIT | Permissions::CREATE;
        let common = admin & editor;
        assert_eq!(common, editor);
    }

    #[test]
    fn difference_removes_flags() {
        let editor = Permissions::READ | Permissions::EDIT | Permissions::CREATE;
        let read_only = editor - (Permissions::EDIT | Permissions::CREATE);
        assert_eq!(read_only, Permissions::READ);
    }

    #[test]
    fn all_contains_each_flag() {
        let all = Permissions::all();
        for flag in [
            Permissions::READ,
            Permissions::EDIT,
            Permissions::CREATE,
            Permissions::DELETE,
            Permissions::MOVE,
            Permissions::PROTECT,
            Permissions::MANAGE_USERS,
            Permissions::MANAGE_ROLES,
            Permissions::MANAGE_NAMESPACES,
            Permissions::VIEW_AUDIT_LOG,
            Permissions::MANAGE_BLOCKLIST,
        ] {
            assert!(all.contains(flag), "missing {flag:?}");
        }
    }

    #[test]
    fn round_trip_serde() {
        let perms = Permissions::READ | Permissions::EDIT | Permissions::CREATE;
        let json = serde_json::to_string(&perms).expect("serialise");
        let parsed: Permissions = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(perms, parsed);
    }
}
