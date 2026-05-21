//! [`User`] — the human (or service) behind a [`Revision`](crate::revision::Revision).
//!
//! A [`Username`] is the case-sensitive login handle; it is constrained to
//! ASCII alphanumerics, underscores, and dashes, and capped at 64 bytes so it
//! fits in every plausible storage column without trimming.
//!
//! The display name is free-form. Email is optional because anonymous or
//! externally-authenticated users may not surface one.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;

use crate::id::UserId;
use crate::validation::ValidationError;

/// Maximum length, in bytes, of a [`Username`].
pub const USERNAME_MAX_BYTES: usize = 64;

/// A validated login handle.
///
/// Construction goes through [`Username::new`], which enforces the rules:
///
/// - non-empty,
/// - at most [`USERNAME_MAX_BYTES`] bytes,
/// - ASCII alphanumeric, `_`, or `-` only.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(transparent)]
pub struct Username(String);

impl Username {
    /// Validate `value` and wrap it in a `Username`.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError`] if `value` is empty, longer than
    /// [`USERNAME_MAX_BYTES`] bytes, or contains a character other than
    /// ASCII alphanumerics, `_`, or `-`.
    pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ValidationError::Empty);
        }
        if value.len() > USERNAME_MAX_BYTES {
            return Err(ValidationError::TooLong {
                max: USERNAME_MAX_BYTES,
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

impl core::fmt::Display for Username {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A user's email address, stored in normalised lowercase form.
///
/// We do not attempt RFC-5322 grammar validation in `core`; the API layer
/// runs the heavier check. This newtype simply ensures the string is
/// non-empty and contains an `@`, which is enough to reject the most common
/// mis-pastes without dragging an email-parser crate into `core`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(transparent)]
pub struct EmailAddress(String);

impl EmailAddress {
    /// Validate `value` and wrap it.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::Empty`] if `value` is empty, or
    /// [`ValidationError::InvalidCharacter`] with `'@'` if no at-sign is
    /// present.
    pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ValidationError::Empty);
        }
        if !value.contains('@') {
            return Err(ValidationError::InvalidCharacter { character: '@' });
        }
        Ok(Self(value))
    }

    /// Borrow the underlying string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for EmailAddress {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A registered user.
///
/// `last_login_at` is `None` until the user successfully authenticates for
/// the first time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct User {
    /// Stable identifier.
    pub id: UserId,
    /// Login handle. Case-sensitive.
    pub username: Username,
    /// Contact email, if known.
    pub email: Option<EmailAddress>,
    /// Free-form name shown in the UI; falls back to the username when absent.
    pub display_name: Option<String>,
    /// When the account was created.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// When the user last successfully logged in. `None` for never.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_login_at: Option<OffsetDateTime>,
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "ergonomic tests")]
mod tests {
    use super::*;

    #[test]
    fn username_accepts_valid() {
        Username::new("alice").expect("simple");
        Username::new("Alice_99").expect("mixed");
        Username::new("a-b-c").expect("dashes");
    }

    #[test]
    fn username_rejects_empty() {
        assert_eq!(Username::new(""), Err(ValidationError::Empty));
    }

    #[test]
    fn username_rejects_too_long() {
        let too_long = "a".repeat(USERNAME_MAX_BYTES + 1);
        assert_eq!(
            Username::new(too_long),
            Err(ValidationError::TooLong {
                max: USERNAME_MAX_BYTES,
                actual: USERNAME_MAX_BYTES + 1,
            })
        );
    }

    #[test]
    fn username_accepts_max_length() {
        let max = "a".repeat(USERNAME_MAX_BYTES);
        Username::new(max).expect("max length is permitted");
    }

    #[test]
    fn username_rejects_disallowed_chars() {
        assert_eq!(
            Username::new("alice@example"),
            Err(ValidationError::InvalidCharacter { character: '@' })
        );
        assert_eq!(
            Username::new("space invader"),
            Err(ValidationError::InvalidCharacter { character: ' ' })
        );
        // Non-ASCII letters are rejected even though they are alphabetic.
        assert_eq!(
            Username::new("naïve"),
            Err(ValidationError::InvalidCharacter { character: 'ï' })
        );
    }

    #[test]
    fn email_requires_at_sign() {
        assert_eq!(EmailAddress::new(""), Err(ValidationError::Empty));
        assert_eq!(
            EmailAddress::new("not-an-email"),
            Err(ValidationError::InvalidCharacter { character: '@' })
        );
        EmailAddress::new("hi@example.com").expect("ok");
    }

    #[test]
    fn user_round_trips_serde() {
        let user = User {
            id: UserId::new(),
            username: Username::new("alice").expect("username"),
            email: Some(EmailAddress::new("alice@example.com").expect("email")),
            display_name: Some("Alice".into()),
            created_at: OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("ts"),
            last_login_at: None,
        };
        let json = serde_json::to_string(&user).expect("serialise");
        let parsed: User = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(parsed, user);
    }
}
