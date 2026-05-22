//! Validated [`Tag`] newtype.
//!
//! Tags are flat strings attached to pages (#29). Constraints, enforced by
//! [`Tag::new`]:
//!
//! - non-empty,
//! - at most [`TAG_MAX_BYTES`] bytes,
//! - ASCII lowercase alphanumerics, `-`, or `_`.
//!
//! Case-insensitivity: the constructor accepts any-case input and normalises
//! to lowercase before storing — so `Tag::new("RUST")` and `Tag::new("rust")`
//! produce equal values, which is what the lookup-by-tag and unique-index
//! contracts both expect.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::validation::ValidationError;

/// Maximum length, in bytes, of a [`Tag`].
///
/// 32 bytes is plenty for the "single-word slug" shape tags take in practice
/// (`#linux`, `#how-to`, `#geography-of-france-1804`) while keeping the
/// `page_tags.tag` column index narrow.
pub const TAG_MAX_BYTES: usize = 32;

/// A validated tag string.
///
/// Construct one with [`Tag::new`] — direct construction is intentionally
/// blocked by the private inner field, so any value in the system has been
/// through the validation gauntlet.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(transparent)]
pub struct Tag(String);

impl Tag {
    /// Validate `value`, lowercase it, and wrap in a `Tag`.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError`] if the value is empty, longer than
    /// [`TAG_MAX_BYTES`] bytes, or contains a character outside the allowed
    /// alphabet (`a-z`, `0-9`, `-`, `_`). Uppercase letters are *not*
    /// rejected — they are lowercased first.
    pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ValidationError::Empty);
        }
        if value.len() > TAG_MAX_BYTES {
            return Err(ValidationError::TooLong {
                max: TAG_MAX_BYTES,
                actual: value.len(),
            });
        }
        let lowered = value.to_ascii_lowercase();
        if let Some(bad) = lowered
            .chars()
            .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-' || *c == '_'))
        {
            return Err(ValidationError::InvalidCharacter { character: bad });
        }
        Ok(Self(lowered))
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

impl core::fmt::Display for Tag {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "ergonomic tests")]
mod tests {
    use super::*;

    #[test]
    fn accepts_simple_lowercase() {
        let tag = Tag::new("rust").expect("rust");
        assert_eq!(tag.as_str(), "rust");
    }

    #[test]
    fn lowercases_input() {
        let tag = Tag::new("Rust-Lang_2024").expect("mixed case");
        assert_eq!(tag.as_str(), "rust-lang_2024");
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(Tag::new(""), Err(ValidationError::Empty));
    }

    #[test]
    fn rejects_too_long() {
        let oversize = "a".repeat(TAG_MAX_BYTES + 1);
        assert!(matches!(
            Tag::new(oversize),
            Err(ValidationError::TooLong { .. })
        ));
    }

    #[test]
    fn rejects_disallowed_chars() {
        assert_eq!(
            Tag::new("with space"),
            Err(ValidationError::InvalidCharacter { character: ' ' }),
        );
        assert_eq!(
            Tag::new("dot.tag"),
            Err(ValidationError::InvalidCharacter { character: '.' }),
        );
        assert_eq!(
            Tag::new("slash/tag"),
            Err(ValidationError::InvalidCharacter { character: '/' }),
        );
    }

    #[test]
    fn round_trips_serde() {
        let tag = Tag::new("history").expect("ok");
        let json = serde_json::to_string(&tag).expect("serialise");
        let parsed: Tag = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(parsed, tag);
    }
}
