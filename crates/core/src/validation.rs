//! Validation errors raised by newtype constructors in this crate.
//!
//! The variants are intentionally narrow — they describe *what* was wrong
//! with a value (empty, too long, illegal character), not which field it
//! came from. Callers translate these into field-scoped messages at the API
//! boundary.

use thiserror::Error;

/// What went wrong while validating a string-shaped newtype.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValidationError {
    /// The value was empty.
    #[error("value must not be empty")]
    Empty,

    /// The value exceeded the maximum length (in bytes).
    #[error("value exceeds maximum length of {max} bytes (got {actual})")]
    TooLong {
        /// Inclusive maximum length permitted.
        max: usize,
        /// Observed length.
        actual: usize,
    },

    /// The value contained a character that is not allowed by the type.
    #[error("value contains disallowed character {character:?}")]
    InvalidCharacter {
        /// The offending character.
        character: char,
    },
}
