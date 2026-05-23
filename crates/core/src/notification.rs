//! [`Notification`] — an in-app inbox row.
//!
//! Notifications are the user-visible side of asynchronous events that
//! happen on behalf of a logged-in user. The first kinds introduced (in
//! #40) are decisions on the user's queued edits, but the schema is
//! intentionally generic — `kind` is a free-form string and `payload`
//! holds arbitrary JSON so future kinds (e.g. `page_protection_changed`)
//! can reuse the same table without a migration.
//!
//! Anonymous editors do not receive notifications: there is no `User`
//! identity to attach an inbox row to. The API enforces that on the
//! producer side.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use utoipa::ToSchema;

use crate::id::{NotificationId, UserId};

/// Stable machine-readable kinds. Stored on the row as a free-form `TEXT`,
/// these constants let producers / consumers share the same names without
/// risk of drift.
pub mod kind {
    /// The user's queued edit was approved and promoted to a real revision.
    pub const PENDING_REVISION_APPROVED: &str = "pending_revision_approved";
    /// The user's queued edit was rejected by a reviewer.
    pub const PENDING_REVISION_REJECTED: &str = "pending_revision_rejected";
}

/// One row in a user's in-app inbox.
///
/// `read_at` is `None` for unread notifications; the API flips it to the
/// current time when the user opens the entry. The row stays in the table
/// forever today — retention is operator-configurable in a follow-up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Notification {
    /// Stable identifier.
    pub id: NotificationId,
    /// User the notification is addressed to.
    pub user_id: UserId,
    /// Stable kind string. See [`kind`].
    pub kind: String,
    /// Arbitrary structured payload. Producers attach context the SPA can
    /// render without a follow-up lookup (e.g. the page slug for a
    /// pending-revision decision).
    pub payload: Option<Value>,
    /// When the user marked it read, or `None` while unread.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub read_at: Option<OffsetDateTime>,
    /// When the notification was produced.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// Input for inserting a new notification row.
///
/// Kept separate from [`Notification`] so producers don't have to mint a
/// `NotificationId` or stamp `created_at` themselves — the storage layer
/// does both. `read_at` is implicitly `None` on creation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewNotification {
    /// Recipient.
    pub user_id: UserId,
    /// Stable kind string. See [`kind`].
    pub kind: String,
    /// Optional structured payload.
    pub payload: Option<Value>,
}
