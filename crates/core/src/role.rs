//! [`Role`] — a named bundle of [`Permissions`](crate::permissions::Permissions).
//!
//! A user holds zero or more roles; their effective capability set is the
//! union of the roles' [`Permissions`]. The role name is the stable
//! identifier used in config (`anonymous`, `user`, `editor`, `moderator`,
//! `admin`); `display_name` is the human label shown in the admin UI.
//!
//! Roles validate their `name` against the same alphabet as
//! [`Username`](crate::user::Username) so they survive being embedded in URLs
//! and config keys.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::id::RoleId;
use crate::permissions::Permissions;
use crate::validation::ValidationError;

/// Maximum length, in bytes, of a [`RoleName`].
pub const ROLE_NAME_MAX_BYTES: usize = 64;

/// A validated role identifier.
///
/// Constraints match [`Username`](crate::user::Username): non-empty, at most
/// [`ROLE_NAME_MAX_BYTES`] bytes, ASCII alphanumerics, `_`, or `-`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(transparent)]
pub struct RoleName(String);

impl RoleName {
    /// Validate `value` and wrap it.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError`] for empty values, values exceeding
    /// [`ROLE_NAME_MAX_BYTES`], or values containing disallowed characters.
    pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ValidationError::Empty);
        }
        if value.len() > ROLE_NAME_MAX_BYTES {
            return Err(ValidationError::TooLong {
                max: ROLE_NAME_MAX_BYTES,
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

impl core::fmt::Display for RoleName {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A named permission bundle. Assign roles to users; a user's effective
/// permissions are the union of their roles'.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Role {
    /// Stable identifier.
    pub id: RoleId,
    /// Machine-friendly identifier used in config and URLs.
    pub name: RoleName,
    /// Human-readable label for the admin UI.
    pub display_name: String,
    /// Capability set granted by this role.
    pub permissions: Permissions,
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "ergonomic tests")]
mod tests {
    use super::*;

    #[test]
    fn role_name_accepts_valid() {
        RoleName::new("editor").expect("simple");
        RoleName::new("auto-confirmed").expect("dash");
    }

    #[test]
    fn role_name_rejects_empty() {
        assert_eq!(RoleName::new(""), Err(ValidationError::Empty));
    }

    #[test]
    fn role_name_rejects_disallowed_chars() {
        assert_eq!(
            RoleName::new("role:name"),
            Err(ValidationError::InvalidCharacter { character: ':' })
        );
    }

    #[test]
    fn role_round_trips_serde() {
        let role = Role {
            id: RoleId::new(),
            name: RoleName::new("editor").expect("name"),
            display_name: "Editor".into(),
            permissions: Permissions::READ | Permissions::EDIT | Permissions::CREATE,
        };
        let json = serde_json::to_string(&role).expect("serialise");
        let parsed: Role = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(parsed, role);
    }
}
