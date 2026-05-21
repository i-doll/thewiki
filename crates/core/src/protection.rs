//! Page-level edit protection.
//!
//! A [`ProtectionLevel`] is a coarse-grained guard on top of the role system:
//! the role check decides "can this user edit pages at all?", and the
//! protection level decides "is this particular page locked down further?".
//!
//! Fine-grained per-page ACLs (M1) layer on top by augmenting the variants
//! later. The enum is `#[non_exhaustive]` so adding `CustomAcl(_)` (or
//! similar) post-v1 is non-breaking.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// How aggressively a page is protected from edits.
///
/// The semantics:
///
/// - [`None`](ProtectionLevel::None) — anyone with the `EDIT` permission can
///   edit. The default for new pages.
/// - [`SemiProtected`](ProtectionLevel::SemiProtected) — requires an
///   established account (no anonymous edits).
/// - [`Protected`](ProtectionLevel::Protected) — requires an editor-or-higher
///   role.
/// - [`FullyProtected`](ProtectionLevel::FullyProtected) — requires an admin.
///
/// The exact mapping from variant to required role is decided in the auth
/// layer; this enum is purely a data carrier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProtectionLevel {
    /// No additional protection beyond the normal role check.
    #[default]
    None,
    /// Anonymous users cannot edit.
    SemiProtected,
    /// Only editors-or-higher may edit.
    Protected,
    /// Only administrators may edit.
    FullyProtected,
}

impl ProtectionLevel {
    /// Stable identifier used in storage columns and the OpenAPI surface.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::SemiProtected => "semi_protected",
            Self::Protected => "protected",
            Self::FullyProtected => "fully_protected",
        }
    }
}

impl core::fmt::Display for ProtectionLevel {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "ergonomic tests")]
mod tests {
    use super::*;

    #[test]
    fn default_is_none() {
        assert_eq!(ProtectionLevel::default(), ProtectionLevel::None);
    }

    #[test]
    fn round_trip_serde() {
        for level in [
            ProtectionLevel::None,
            ProtectionLevel::SemiProtected,
            ProtectionLevel::Protected,
            ProtectionLevel::FullyProtected,
        ] {
            let json = serde_json::to_string(&level).expect("serialise");
            let parsed: ProtectionLevel = serde_json::from_str(&json).expect("deserialise");
            assert_eq!(parsed, level);
        }
    }

    #[test]
    fn snake_case_wire_form() {
        let json = serde_json::to_string(&ProtectionLevel::SemiProtected).expect("serialise");
        assert_eq!(json, "\"semi_protected\"");
    }
}
