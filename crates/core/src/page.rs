//! [`Page`] — the addressable unit of a wiki.
//!
//! A page lives in exactly one [`Namespace`](crate::namespace::Namespace) and
//! is identified by a URL slug within that namespace (`(namespace_id, slug)`
//! is unique). The body of a page lives in its
//! [`Revision`](crate::revision::Revision)s; [`Page::current_revision_id`]
//! points at the head of the linear history.
//!
//! `current_revision_id` is `Option<RevisionId>` because a freshly created
//! page row can briefly exist before its first revision is committed (storage
//! writes the page row and the first revision in the same transaction; the
//! domain type still allows for the transient state in case storage backends
//! ever need to model it explicitly).

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;

use crate::content_format::ContentFormat;
use crate::id::{NamespaceId, PageId, RevisionId};
use crate::protection::ProtectionLevel;

/// A wiki page.
///
/// See the [module docs](self) for the relationship between a page and its
/// revisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Page {
    /// Stable identifier.
    pub id: PageId,
    /// Namespace this page lives in.
    pub namespace_id: NamespaceId,
    /// URL slug, unique within `namespace_id`.
    pub slug: String,
    /// Human-readable title (may diverge from the slug).
    pub title: String,
    /// Pointer to the current head revision, or `None` if no revision has
    /// been committed yet.
    pub current_revision_id: Option<RevisionId>,
    /// Source format authors edit the page in.
    pub content_format: ContentFormat,
    /// How protected this page is from edits.
    pub protection_level: ProtectionLevel,
    /// When the page row was first created.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// When the page row was last touched (any field, including a new head
    /// revision).
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "ergonomic tests")]
mod tests {
    use super::*;

    fn sample_page() -> Page {
        let created = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("ts");
        Page {
            id: PageId::new(),
            namespace_id: NamespaceId::new(),
            slug: "introduction".into(),
            title: "Introduction".into(),
            current_revision_id: Some(RevisionId::new()),
            content_format: ContentFormat::Markdown,
            protection_level: ProtectionLevel::None,
            created_at: created,
            updated_at: created,
        }
    }

    #[test]
    fn page_round_trips_serde() {
        let page = sample_page();
        let json = serde_json::to_string(&page).expect("serialise");
        let parsed: Page = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(parsed, page);
    }

    #[test]
    fn page_without_current_revision_round_trips() {
        let mut page = sample_page();
        page.current_revision_id = None;
        let json = serde_json::to_string(&page).expect("serialise");
        let parsed: Page = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(parsed, page);
        assert!(parsed.current_revision_id.is_none());
    }
}
