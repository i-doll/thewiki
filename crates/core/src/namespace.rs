//! [`Namespace`] — a top-level partition of the page space.
//!
//! MediaWiki famously expresses this as the `Talk:` / `User:` / `File:`
//! prefix on a page title. `thewiki` uses the same idea: a namespace has a
//! slug (used in URLs and storage) and a display name.
//!
//! Slug rules follow the same alphabet as [`Username`](crate::user::Username)
//! but explicitly forbid the `:` character — the slug is what would precede
//! the colon in a `Namespace:Page` reference, so allowing colons inside it
//! would break parsing.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::id::NamespaceId;
use crate::validation::ValidationError;

/// Maximum length, in bytes, of a [`NamespaceSlug`].
pub const NAMESPACE_SLUG_MAX_BYTES: usize = 64;

/// A validated namespace slug.
///
/// Constraints:
///
/// - non-empty,
/// - at most [`NAMESPACE_SLUG_MAX_BYTES`] bytes,
/// - ASCII alphanumeric, `_`, or `-` (`:` is explicitly forbidden).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(transparent)]
pub struct NamespaceSlug(String);

impl NamespaceSlug {
    /// Validate `value` and wrap it in a `NamespaceSlug`.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError`] if `value` is empty, longer than
    /// [`NAMESPACE_SLUG_MAX_BYTES`] bytes, contains `:`, or contains a
    /// character outside the allowed alphabet.
    pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ValidationError::Empty);
        }
        if value.len() > NAMESPACE_SLUG_MAX_BYTES {
            return Err(ValidationError::TooLong {
                max: NAMESPACE_SLUG_MAX_BYTES,
                actual: value.len(),
            });
        }
        if let Some(bad) = value
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || *c == '_' || *c == '-'))
        {
            return Err(ValidationError::InvalidCharacter { character: bad });
        }
        Ok(Self(value))
    }

    /// Borrow the underlying string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the wrapper and return the inner `String`.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl core::fmt::Display for NamespaceSlug {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A namespace groups pages under a common slug prefix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Namespace {
    /// Stable identifier.
    pub id: NamespaceId,
    /// URL-safe slug. Forms the `Namespace:` part of a page reference.
    pub slug: NamespaceSlug,
    /// Human-readable label.
    pub display_name: String,
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "ergonomic tests")]
mod tests {
    use super::*;

    #[test]
    fn slug_accepts_valid() {
        NamespaceSlug::new("main").expect("simple");
        NamespaceSlug::new("User_talk").expect("underscore");
        NamespaceSlug::new("help-pages").expect("dash");
    }

    #[test]
    fn slug_rejects_colon() {
        assert_eq!(
            NamespaceSlug::new("User:Talk"),
            Err(ValidationError::InvalidCharacter { character: ':' })
        );
    }

    #[test]
    fn slug_rejects_empty() {
        assert_eq!(NamespaceSlug::new(""), Err(ValidationError::Empty));
    }

    #[test]
    fn slug_rejects_too_long() {
        let too_long = "a".repeat(NAMESPACE_SLUG_MAX_BYTES + 1);
        assert!(matches!(
            NamespaceSlug::new(too_long),
            Err(ValidationError::TooLong { .. })
        ));
    }

    #[test]
    fn slug_rejects_disallowed_chars() {
        assert_eq!(
            NamespaceSlug::new("with space"),
            Err(ValidationError::InvalidCharacter { character: ' ' })
        );
        assert_eq!(
            NamespaceSlug::new("dot.separated"),
            Err(ValidationError::InvalidCharacter { character: '.' })
        );
    }

    #[test]
    fn namespace_round_trips_serde() {
        let ns = Namespace {
            id: NamespaceId::new(),
            slug: NamespaceSlug::new("main").expect("slug"),
            display_name: "Main".into(),
        };
        let json = serde_json::to_string(&ns).expect("serialise");
        let parsed: Namespace = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(parsed, ns);
    }
}
