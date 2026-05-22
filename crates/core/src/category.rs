//! [`Category`] — a node in the categorisation DAG (#29).
//!
//! A category has a stable [`CategoryId`], a URL-friendly slug, a human-readable
//! display name, and an optional [`parent_id`](Category::parent_id) pointing at
//! another category. Pages are assigned via the `page_categories` join table
//! (one page can sit in many categories); the parent link turns the category
//! set into a hierarchy (modelled as a DAG: a category can have one explicit
//! parent and an arbitrary number of children).
//!
//! Cycle prevention is enforced at the storage layer's `assign_parent`-style
//! API: walking the would-be parent's ancestors before writing rejects any
//! mutation that would re-introduce the current category as its own ancestor.
//! The shape of [`Category`] itself is intentionally cycle-agnostic — it
//! describes the *current* relationship, not the rules for changing it.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;

use crate::id::CategoryId;

/// A category in the wiki's hierarchical taxonomy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Category {
    /// Stable identifier.
    pub id: CategoryId,
    /// URL-friendly slug, unique across all categories. The slug doubles
    /// as the identifier used in the `/category/<slug>` route.
    pub slug: String,
    /// Human-readable label shown in the UI. Free-form text.
    pub display_name: String,
    /// Optional parent in the DAG. `None` for top-level categories.
    pub parent_id: Option<CategoryId>,
    /// When the category row was created.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "ergonomic tests")]
mod tests {
    use super::*;

    #[test]
    fn category_round_trips_serde() {
        let created = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("ts");
        let cat = Category {
            id: CategoryId::new(),
            slug: "history".into(),
            display_name: "History".into(),
            parent_id: Some(CategoryId::new()),
            created_at: created,
        };
        let json = serde_json::to_string(&cat).expect("serialise");
        let parsed: Category = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(parsed, cat);
    }

    #[test]
    fn category_without_parent_round_trips() {
        let created = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("ts");
        let cat = Category {
            id: CategoryId::new(),
            slug: "top".into(),
            display_name: "Top".into(),
            parent_id: None,
            created_at: created,
        };
        let json = serde_json::to_string(&cat).expect("serialise");
        let parsed: Category = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(parsed, cat);
        assert!(parsed.parent_id.is_none());
    }
}
