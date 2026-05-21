//! [`Revision`] — an immutable snapshot of a [`Page`](crate::page::Page)'s body.
//!
//! Every change to a page produces a new `Revision`. The history is linear
//! per page: each revision points back at its parent (`parent_id`), and the
//! page's `current_revision_id` names the head of the chain. The first
//! revision of a page has `parent_id == None`.
//!
//! Revisions are **append-only**. Editing an existing revision row is a bug;
//! reverting a page means committing a new revision whose body matches an
//! older one.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;

use crate::id::{PageId, RevisionId, UserId};

/// An immutable snapshot of a page's body.
///
/// Construct fresh revisions through [`Revision::new`]; that enforces the
/// invariant that a revision always references a real `PageId`. Storage
/// rebuilds revisions through the field-level `Deserialize` impl.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Revision {
    /// Stable identifier.
    pub id: RevisionId,
    /// Page this revision belongs to. The (`page_id`, `id`) pair is unique.
    pub page_id: PageId,
    /// Previous revision in this page's history, or `None` for the first
    /// revision.
    pub parent_id: Option<RevisionId>,
    /// User who authored the revision.
    pub author_id: UserId,
    /// Raw source body. Format is determined by the parent page's
    /// [`content_format`](crate::page::Page::content_format).
    pub body: String,
    /// Optional short note describing the change.
    pub edit_summary: Option<String>,
    /// When the revision was committed.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl Revision {
    /// Build a new revision for `page_id`, freshly minting a [`RevisionId`]
    /// and stamping `created_at = now`.
    ///
    /// This is the canonical constructor used when accepting an edit; it
    /// makes it impossible to forget linking the revision back to its page.
    #[must_use]
    pub fn new(
        page_id: PageId,
        parent_id: Option<RevisionId>,
        author_id: UserId,
        body: String,
        edit_summary: Option<String>,
    ) -> Self {
        Self {
            id: RevisionId::new(),
            page_id,
            parent_id,
            author_id,
            body,
            edit_summary,
            created_at: OffsetDateTime::now_utc(),
        }
    }

    /// `true` if this is the first revision in its page's history.
    #[must_use]
    pub const fn is_initial(&self) -> bool {
        self.parent_id.is_none()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "ergonomic tests")]
mod tests {
    use super::*;

    #[test]
    fn new_links_to_provided_page() {
        let page_id = PageId::new();
        let author_id = UserId::new();
        let rev = Revision::new(page_id, None, author_id, "# hello".into(), None);

        assert_eq!(rev.page_id, page_id);
        assert_eq!(rev.author_id, author_id);
        assert!(rev.is_initial());
        assert!(rev.parent_id.is_none());
    }

    #[test]
    fn subsequent_revision_points_at_parent() {
        let page_id = PageId::new();
        let author_id = UserId::new();
        let first = Revision::new(page_id, None, author_id, "v1".into(), None);
        let second = Revision::new(
            page_id,
            Some(first.id),
            author_id,
            "v2".into(),
            Some("fix typo".into()),
        );

        assert_eq!(second.page_id, page_id);
        assert_eq!(second.parent_id, Some(first.id));
        assert!(!second.is_initial());
        assert_ne!(first.id, second.id);
    }

    #[test]
    fn revision_round_trips_serde() {
        let page_id = PageId::new();
        let rev = Revision::new(
            page_id,
            None,
            UserId::new(),
            "body".into(),
            Some("initial".into()),
        );
        let json = serde_json::to_string(&rev).expect("serialise");
        let parsed: Revision = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(parsed, rev);
        assert_eq!(parsed.page_id, page_id);
    }
}
