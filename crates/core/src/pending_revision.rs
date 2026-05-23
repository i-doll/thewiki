//! [`PendingRevision`] — an edit that has not yet been promoted to a real
//! [`Revision`].
//!
//! A pending revision is the queued form of an edit: the caller proposed
//! a body for a page, but the operator's approval policy (see
//! `AuthConfig::approval_required_for` on the API side, #40) decided that
//! the change should wait for a reviewer. Approving the row promotes it
//! into a real [`Revision`] against the target page; rejecting it
//! records the reason and the row stays as a historical record.
//!
//! Pending revisions are **terminal once decided**: the `status` column
//! is intentionally not reversible, and the storage layer only flips it
//! through the dedicated approve / reject methods.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;

use crate::id::{PageId, PendingRevisionId, RevisionId, UserId};

/// Lifecycle states a pending-revision row can be in.
///
/// `#[non_exhaustive]` so a future state (e.g. `Superseded`) is not a
/// breaking change for downstream matchers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum PendingRevisionStatus {
    /// Awaiting a reviewer decision.
    Pending,
    /// Promoted into a real revision. The original body now lives in the
    /// `revisions` table; the pending row is kept for audit.
    Approved,
    /// Reviewer declined the change. `rejection_reason` carries the operator-
    /// visible message.
    Rejected,
}

impl PendingRevisionStatus {
    /// Wire-form name used in JSON payloads and the `pending_revisions.status`
    /// column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        }
    }

    /// Parse the wire form back into the enum. Returns `None` on any unknown
    /// value so the storage layer can surface a typed error.
    ///
    /// Named `parse` rather than `from_str` to keep it distinct from the
    /// standard `FromStr` trait — we don't want a missing-import to silently
    /// invoke the trait method (which returns `Result`) when we mean this
    /// `Option`-returning helper.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "approved" => Some(Self::Approved),
            "rejected" => Some(Self::Rejected),
            _ => None,
        }
    }
}

/// A queued edit awaiting (or having received) a reviewer decision.
///
/// `author_id` is `None` for anonymous edits — `author_ip` carries the
/// captured IP in that case so the reviewer has something to attribute the
/// edit to. Both are kept on the same row because anonymous edits are still
/// real edits and the operator wants the option of grouping them by IP.
///
/// `parent_revision_id` records which revision the editor based their
/// change on. For a brand-new page (create-new-page proposal) it is `None`.
/// Reviewers use it to spot stale edits — if the page has moved on since
/// the pending edit was submitted, the diff against the current head will
/// flag the conflict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PendingRevision {
    /// Stable identifier for this queued row.
    pub id: PendingRevisionId,
    /// Page the edit targets.
    pub page_id: PageId,
    /// Head revision the editor based their change on, or `None` for an
    /// initial-revision proposal.
    pub parent_revision_id: Option<RevisionId>,
    /// Proposed Markdown body — exactly what would become the revision's
    /// body on approval.
    pub body: String,
    /// Authenticated author, or `None` for anonymous edits.
    pub author_id: Option<UserId>,
    /// Captured client IP for anonymous edits. `None` for authenticated
    /// rows — those can use `author_id` to recover the editor.
    pub author_ip: Option<String>,
    /// Short note from the editor — usually the edit summary they typed.
    pub comment: String,
    /// Lifecycle state.
    pub status: PendingRevisionStatus,
    /// Reviewer who acted on the row, or `None` while the row is pending.
    pub reviewer_id: Option<UserId>,
    /// When the reviewer acted, or `None` while pending.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub decided_at: Option<OffsetDateTime>,
    /// Operator-visible note attached to a rejection. `None` for approved /
    /// pending rows.
    pub rejection_reason: Option<String>,
    /// When the row was queued.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "ergonomic tests")]
mod tests {
    use super::*;

    #[test]
    fn status_round_trips_through_string() {
        for s in [
            PendingRevisionStatus::Pending,
            PendingRevisionStatus::Approved,
            PendingRevisionStatus::Rejected,
        ] {
            let txt = s.as_str();
            assert_eq!(PendingRevisionStatus::parse(txt), Some(s));
        }
        assert_eq!(PendingRevisionStatus::parse("bogus"), None);
    }
}
