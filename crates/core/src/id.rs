//! Strongly-typed identifiers for the domain entities.
//!
//! Every entity owns a newtype wrapper around [`uuid::Uuid`]. We use UUIDv7
//! ([RFC 9562]) — the timestamp prefix gives us time-ordered IDs, which helps
//! both B-Tree index locality on the database side and makes debugging logs
//! more pleasant (IDs sort by creation time).
//!
//! The newtypes are deliberately not `From<Uuid>` / `Into<Uuid>` — callers must
//! use [`Self::from_uuid`] / [`Self::as_uuid`] explicitly. This keeps a
//! `PageId` from being silently used where a `UserId` is wanted.
//!
//! All ID types share the same derive set so they round-trip through
//! `serde`, hash into `HashMap`s, and participate in `utoipa`-generated
//! OpenAPI schemas without further effort.
//!
//! [RFC 9562]: https://www.rfc-editor.org/rfc/rfc9562

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Generate a newtype ID over `Uuid`.
macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug,
            Clone,
            Copy,
            PartialEq,
            Eq,
            Hash,
            PartialOrd,
            Ord,
            Serialize,
            Deserialize,
            ToSchema,
        )]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Mint a fresh ID using UUIDv7 (time-ordered).
            ///
            /// Equivalent to [`Uuid::now_v7`]. Two IDs minted at the same
            /// instant are still distinct because the lower 74 bits are
            /// randomised.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            /// Wrap an existing `Uuid`. Useful when loading rows from storage.
            #[must_use]
            pub const fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            /// Borrow the inner `Uuid`.
            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }

            /// Copy the inner `Uuid` by value.
            #[must_use]
            pub const fn into_uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl core::fmt::Display for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                core::fmt::Display::fmt(&self.0, f)
            }
        }
    };
}

define_id! {
    /// Identifier for a [`crate::page::Page`].
    PageId
}

define_id! {
    /// Identifier for a [`crate::revision::Revision`].
    RevisionId
}

define_id! {
    /// Identifier for a [`crate::user::User`].
    UserId
}

define_id! {
    /// Identifier for a [`crate::role::Role`].
    RoleId
}

define_id! {
    /// Identifier for a [`crate::namespace::Namespace`].
    NamespaceId
}

define_id! {
    /// Identifier for a [`crate::session::Session`].
    ///
    /// Doubles as the bearer cookie value: clients hand this back to the
    /// server unchanged. UUIDv7 is used so the ID is unguessable (74 bits of
    /// entropy) while staying sortable for index locality.
    SessionId
}

define_id! {
    /// Identifier for an audit-log entry.
    AuditLogId
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "ergonomic tests")]
mod tests {
    use super::*;

    /// UUIDv7 layout: the high 48 bits encode the Unix-millisecond timestamp
    /// and the 4-bit version field is `7`. Both must be non-zero for a
    /// freshly minted ID.
    fn assert_uuid_v7(uuid: Uuid) {
        assert_eq!(uuid.get_version_num(), 7, "expected UUIDv7, got {uuid}");
        let bytes = uuid.as_bytes();
        let mut ts: u64 = 0;
        for &b in &bytes[..6] {
            ts = (ts << 8) | u64::from(b);
        }
        assert!(ts > 0, "UUIDv7 timestamp portion was zero: {uuid}");
    }

    #[test]
    fn page_id_new_is_uuid_v7() {
        assert_uuid_v7(PageId::new().into_uuid());
    }

    #[test]
    fn revision_id_new_is_uuid_v7() {
        assert_uuid_v7(RevisionId::new().into_uuid());
    }

    #[test]
    fn user_id_new_is_uuid_v7() {
        assert_uuid_v7(UserId::new().into_uuid());
    }

    #[test]
    fn role_id_new_is_uuid_v7() {
        assert_uuid_v7(RoleId::new().into_uuid());
    }

    #[test]
    fn namespace_id_new_is_uuid_v7() {
        assert_uuid_v7(NamespaceId::new().into_uuid());
    }

    #[test]
    fn fresh_ids_are_unique() {
        let a = PageId::new();
        let b = PageId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn round_trip_serde() {
        let id = PageId::new();
        let json = serde_json::to_string(&id).expect("serialise");
        let parsed: PageId = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(id, parsed);
        // `#[serde(transparent)]` means the wire form is a bare UUID string.
        assert!(json.starts_with('"'));
        assert!(json.ends_with('"'));
    }
}
