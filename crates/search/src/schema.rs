//! Tantivy schema for the page index.
//!
//! Field set — kept deliberately narrow:
//!
//! | Field            | Type                  | Notes                                     |
//! |------------------|-----------------------|-------------------------------------------|
//! | `page_id`        | bytes, stored+indexed | 16-byte UUIDv7. Primary key for upsert.   |
//! | `namespace_id`   | bytes, indexed        | 16-byte UUIDv7. Used to filter by ns.     |
//! | `namespace_slug` | string, indexed+stored| Lowercased ASCII. Stored for hit display. |
//! | `slug`           | string, indexed+stored| URL slug, exact-match queryable.          |
//! | `title`          | text,   indexed+stored| Tokenised. Dominates ranking.             |
//! | `body`           | text,   indexed+stored| Tokenised. Stored for snippet generation. |
//! | `tags`           | text,   indexed       | Multi-valued, lowercased.                 |
//! | `updated_at`     | date,   indexed+stored+fast | RFC 3339. Used for "newest first".  |
//!
//! `page_id` is a bytes (not text) field so the upsert path can call
//! `delete_term(Term::from_field_bytes(page_id, &uuid))` without serialising
//! to hex. `namespace_id` is the same shape for the same reason.
//!
//! `body` is `STORED` so [`tantivy::snippet::SnippetGenerator`] can highlight
//! matches without us shipping the raw revision body around separately.
//!
//! The schema is **not stable** across PRs at this stage — adding a field is
//! a breaking schema bump and requires running `thewiki reindex`. The
//! `.last_indexed` marker is invalidated on any schema change because the
//! on-disk segments encode the schema layout in their headers; Tantivy will
//! refuse to open the index with a mismatched schema, which the worker maps
//! to a forced rebuild.

use tantivy::schema::{
    BytesOptions, DateOptions, Field, STORED, STRING, Schema, SchemaBuilder, TEXT,
};

/// Resolved field handles for the page index.
///
/// Built once via [`SearchSchema::new`] and shared (by clone, which is cheap)
/// between the writer side, the reader side, and the snippet generator.
#[derive(Debug, Clone)]
pub struct SearchSchema {
    /// Tantivy [`Schema`] used to open or create the index.
    schema: Schema,
    /// `page_id` — UUIDv7 bytes, stored + indexed (upsert key).
    pub page_id: Field,
    /// `namespace_id` — UUIDv7 bytes, indexed (filter key).
    pub namespace_id: Field,
    /// `namespace_slug` — lowercased ASCII, exact-match indexed + stored.
    pub namespace_slug: Field,
    /// `slug` — URL slug, exact-match indexed + stored.
    pub slug: Field,
    /// `title` — tokenised text, indexed + stored.
    pub title: Field,
    /// `body` — tokenised text, indexed + stored (for snippet generation).
    pub body: Field,
    /// `tags` — tokenised text, indexed only (no need to round-trip).
    pub tags: Field,
    /// `updated_at` — RFC 3339 datetime, indexed + stored + fast.
    pub updated_at: Field,
}

impl SearchSchema {
    /// Build the field set. Cheap; safe to call once at startup.
    #[must_use]
    pub fn new() -> Self {
        let mut builder: SchemaBuilder = Schema::builder();

        // `set_stored().set_indexed()` is the equivalent of MediaWiki's
        // "primary key" treatment — we look up by it (for delete-then-add
        // upserts) and we surface it back to the API layer in hits.
        let page_id_opts = BytesOptions::default().set_stored().set_indexed();
        let page_id = builder.add_bytes_field("page_id", page_id_opts);

        // Namespace filter — only needs to be queryable, not round-trippable.
        let namespace_id_opts = BytesOptions::default().set_indexed();
        let namespace_id = builder.add_bytes_field("namespace_id", namespace_id_opts);

        // STRING is "exact match, no tokenisation, lowercase-as-given". The
        // namespace slug alphabet is `[A-Za-z0-9_-]` so we lowercase upstream
        // for case-insensitive matches.
        let namespace_slug = builder.add_text_field("namespace_slug", STRING | STORED);
        let slug = builder.add_text_field("slug", STRING | STORED);

        // TEXT is "standard tokeniser + lower-case + remove punctuation". We
        // store both `title` and `body` so the snippet generator can produce
        // a highlighted excerpt without a round-trip to the database.
        let title = builder.add_text_field("title", TEXT | STORED);
        let body = builder.add_text_field("body", TEXT | STORED);

        // Tags are tokenised but not stored — the caller already knows them.
        let tags = builder.add_text_field("tags", TEXT);

        // Dates: stored so we can surface them in hits, fast so future range
        // queries / "newest first" sorts don't pay a per-doc decompress cost.
        let updated_at_opts = DateOptions::default().set_indexed().set_stored().set_fast();
        let updated_at = builder.add_date_field("updated_at", updated_at_opts);

        Self {
            schema: builder.build(),
            page_id,
            namespace_id,
            namespace_slug,
            slug,
            title,
            body,
            tags,
            updated_at,
        }
    }

    /// Borrow the underlying Tantivy [`Schema`].
    #[must_use]
    pub fn tantivy_schema(&self) -> &Schema {
        &self.schema
    }
}

impl Default for SearchSchema {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_has_expected_fields() {
        let s = SearchSchema::new();
        let names: Vec<&str> = s
            .tantivy_schema()
            .fields()
            .map(|(_, entry)| entry.name())
            .collect();
        for required in [
            "page_id",
            "namespace_id",
            "namespace_slug",
            "slug",
            "title",
            "body",
            "tags",
            "updated_at",
        ] {
            assert!(names.contains(&required), "missing field {required}");
        }
    }
}
