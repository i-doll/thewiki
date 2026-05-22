//! Per-page protection enforcement (#34).
//!
//! Every mutating page handler funnels through [`check_protection`] before it
//! touches storage. The check is a pure decision over a small triple — page
//! protection level, whether the caller is anonymous, and the union of the
//! caller's permission bits — so it is exhaustively unit-testable here and
//! the route handlers stay readable.
//!
//! The mapping mirrors the variants documented on
//! [`thewiki_core::ProtectionLevel`]:
//!
//! | Protection level   | Allowed callers                                |
//! |--------------------|------------------------------------------------|
//! | `None`             | anyone the [`EditorExtractor`] already let in  |
//! | `SemiProtected`    | any authenticated user (anonymous → 403)       |
//! | `Protected`        | any caller with `Permissions::EDIT`            |
//! | `FullyProtected`   | any caller with `Permissions::PROTECT`         |
//!
//! Wiki-wide gates (the `anonymous_edits` flag, the approval queue) still
//! apply before this point — the protection check tightens what those gates
//! let through, never relaxes it.

use thewiki_core::{Permissions, ProtectionLevel};

use crate::error::ApiError;

/// Snapshot of the calling editor as far as protection enforcement cares.
///
/// Carved out so handlers don't have to spread the same `is_anonymous` /
/// `permissions` plumbing across every call site, and so the unit tests can
/// drive the decision function with plain values.
#[derive(Debug, Clone, Copy)]
pub struct EditorContext {
    /// `true` when the editor is the singleton anonymous user (no session
    /// cookie + `anonymous_edits = true`).
    pub is_anonymous: bool,
    /// Effective permissions granted by the editor's roles. The empty set
    /// matches both an anonymous caller and an authenticated user with no
    /// roles — the [`ProtectionLevel::SemiProtected`] branch distinguishes
    /// the two via [`is_anonymous`](Self::is_anonymous).
    pub permissions: Permissions,
}

/// Reject `Err(ApiError::PageProtected)` when `editor` is not allowed to
/// mutate a page at `level`. Returns `Ok(())` when the edit may proceed.
///
/// The mapping is documented on the [`ProtectionLevel`] enum and on the
/// module docs.
pub fn check_protection(level: ProtectionLevel, editor: EditorContext) -> Result<(), ApiError> {
    match level {
        ProtectionLevel::None => Ok(()),
        ProtectionLevel::SemiProtected => {
            if editor.is_anonymous {
                Err(ApiError::PageProtected {
                    level: level.as_str(),
                    required: "authenticated",
                })
            } else {
                Ok(())
            }
        }
        ProtectionLevel::Protected => {
            if editor.permissions.contains(Permissions::EDIT) {
                Ok(())
            } else {
                Err(ApiError::PageProtected {
                    level: level.as_str(),
                    required: "EDIT",
                })
            }
        }
        ProtectionLevel::FullyProtected => {
            if editor.permissions.contains(Permissions::PROTECT) {
                Ok(())
            } else {
                Err(ApiError::PageProtected {
                    level: level.as_str(),
                    required: "PROTECT",
                })
            }
        }
        // `ProtectionLevel` is `#[non_exhaustive]` so new variants land here
        // until they're given a more specific mapping above. We choose 403
        // by default — refusing the edit is the conservative outcome.
        _ => Err(ApiError::PageProtected {
            level: level.as_str(),
            required: "PROTECT",
        }),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "ergonomic tests")]
mod tests {
    use super::*;

    fn anon() -> EditorContext {
        EditorContext {
            is_anonymous: true,
            permissions: Permissions::empty(),
        }
    }

    fn auth_no_perms() -> EditorContext {
        EditorContext {
            is_anonymous: false,
            permissions: Permissions::empty(),
        }
    }

    fn editor() -> EditorContext {
        EditorContext {
            is_anonymous: false,
            permissions: Permissions::EDIT,
        }
    }

    fn admin() -> EditorContext {
        EditorContext {
            is_anonymous: false,
            permissions: Permissions::EDIT | Permissions::PROTECT,
        }
    }

    #[test]
    fn none_lets_anyone_through() {
        check_protection(ProtectionLevel::None, anon()).expect("none allows anon");
        check_protection(ProtectionLevel::None, auth_no_perms()).expect("none allows auth");
        check_protection(ProtectionLevel::None, editor()).expect("none allows editor");
    }

    #[test]
    fn semi_protected_blocks_anonymous() {
        let err = check_protection(ProtectionLevel::SemiProtected, anon())
            .expect_err("anonymous must be blocked");
        match err {
            ApiError::PageProtected { level, required } => {
                assert_eq!(level, "semi_protected");
                assert_eq!(required, "authenticated");
            }
            other => panic!("expected PageProtected, got {other:?}"),
        }
        check_protection(ProtectionLevel::SemiProtected, auth_no_perms())
            .expect("semi allows any logged-in user");
    }

    #[test]
    fn protected_requires_edit_bit() {
        let err = check_protection(ProtectionLevel::Protected, auth_no_perms())
            .expect_err("no EDIT must be blocked");
        match err {
            ApiError::PageProtected { level, required } => {
                assert_eq!(level, "protected");
                assert_eq!(required, "EDIT");
            }
            other => panic!("expected PageProtected, got {other:?}"),
        }
        check_protection(ProtectionLevel::Protected, editor()).expect("editor allowed");
    }

    #[test]
    fn fully_protected_requires_protect_bit() {
        let err = check_protection(ProtectionLevel::FullyProtected, editor())
            .expect_err("plain EDIT must be blocked");
        match err {
            ApiError::PageProtected { level, required } => {
                assert_eq!(level, "fully_protected");
                assert_eq!(required, "PROTECT");
            }
            other => panic!("expected PageProtected, got {other:?}"),
        }
        check_protection(ProtectionLevel::FullyProtected, admin()).expect("admin allowed");
    }
}
