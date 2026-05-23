//! [`SearchIndex`] — the writer + reader pair, plus the page document shape.
//!
//! This is the only module that touches Tantivy directly. The async layer
//! (see [`crate::indexer`]) wraps every method that touches the writer in
//! `spawn_blocking`; the reader-side `search` method is cheap enough to call
//! on the runtime.
//!
//! ## Upsert semantics
//!
//! Tantivy is append-only: there is no "update document". The standard
//! pattern is `delete_term(by_id) + add_document(new)`, both on the same
//! writer, with the delete batched into the next commit. That means a stale
//! version of a page is visible to readers until the next commit lands;
//! the indexer worker commits every 200ms or every 100 jobs, whichever
//! comes first, so the worst-case lag stays well under the 1 s
//! "eventually consistent" bound the issue calls for.
//!
//! ## Crash safety
//!
//! After every successful commit, the index writes a small `.last_indexed`
//! marker into the index directory. On startup the binary checks for the
//! marker — if it is missing or older than the last revision in storage,
//! the operator (or the `serve` boot path) should enqueue a
//! [`IndexJob::Rebuild`](crate::IndexJob::Rebuild) so the index catches up.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tantivy::collector::TopDocs;
use tantivy::directory::MmapDirectory;
use tantivy::query::QueryParser;
use tantivy::schema::Value;
use tantivy::snippet::SnippetGenerator;
use tantivy::{
    DateTime, DocAddress, Index, IndexReader, IndexWriter, ReloadPolicy, Score, SegmentReader,
    TantivyDocument, Term,
};
use thewiki_core::{NamespaceId, PageId};
use time::OffsetDateTime;
use tracing::debug;

use crate::error::SearchError;
use crate::query::SearchQuery;
use crate::results::{SearchHit, SearchResults};
use crate::schema::SearchSchema;

/// Filename of the on-disk marker the indexer writes after every successful
/// commit. Stored inside the index directory so the marker is wiped if the
/// directory is — it must never out-survive its own index segments.
pub const LAST_INDEXED_MARKER: &str = ".last_indexed";

/// Memory budget passed to Tantivy's writer. 50 MiB is the documented
/// "small-deploy" default; large public wikis can override this through the
/// indexer config in a follow-up.
const WRITER_MEMORY_BUDGET: usize = 50_000_000;

/// Maximum snippet length, in characters, returned to the API layer.
/// Tantivy's default is 150 — we keep that.
const SNIPPET_MAX_CHARS: usize = 150;

/// A document ready to be indexed. Constructed by the API layer from a
/// `Page` + its head `Revision` and pushed to the indexer through
/// [`IndexerHandle::upsert`](crate::IndexerHandle::upsert).
#[derive(Debug, Clone)]
pub struct PageDoc {
    /// Primary key — used for upsert by delete-then-add.
    pub page_id: PageId,
    /// Namespace this page lives in. Used for namespace-scoped queries.
    pub namespace_id: NamespaceId,
    /// URL slug of the namespace. Round-tripped in hits.
    pub namespace_slug: String,
    /// URL slug of the page. Round-tripped in hits.
    pub slug: String,
    /// Human-readable title.
    pub title: String,
    /// Source body — the renderer's input, before HTML conversion. We
    /// tokenise this for matching and store a copy for the snippet
    /// generator.
    pub body: String,
    /// Tag set. May be empty (the canonical state today — tags ship in #29).
    pub tags: Vec<String>,
    /// Last-edited timestamp (typically the head revision's `created_at`).
    pub updated_at: OffsetDateTime,
    /// `true` if the page belongs to a discussion ("talk") namespace
    /// (#43). The reader-side scorer multiplies the BM25 score for these
    /// rows by [`SearchQuery::talk_boost`](crate::SearchQuery::talk_boost)
    /// so subject pages outrank their discussion threads by default.
    pub is_talk: bool,
}

/// Owning handle to the on-disk Tantivy index.
///
/// Cheap to clone — the inner `Arc` shares the directory handle and reader.
/// **Writes are not** safe to issue concurrently; the [`crate::Indexer`]
/// worker owns the writer and is the only thing that mutates the index.
pub struct SearchIndex {
    schema: SearchSchema,
    /// `Arc` so [`SearchIndex::reader_clone`] can hand out cheap clones.
    index: Arc<Index>,
    /// Path the index lives at. Used to write the `.last_indexed` marker.
    path: PathBuf,
    /// Cached reader. Tantivy's `IndexReader` reloads automatically on
    /// commit so we don't need to recreate it per call.
    reader: IndexReader,
}

impl SearchIndex {
    /// Open the index at `path`, creating it (and the directory) if missing.
    ///
    /// This is synchronous because Tantivy is — call from `spawn_blocking`
    /// if you're on the tokio runtime and care about latency.
    ///
    /// # Errors
    ///
    /// - [`SearchError::Io`] if the directory cannot be created.
    /// - [`SearchError::Tantivy`] if the index segments on disk are
    ///   corrupted or the schema in the segments does not match
    ///   [`SearchSchema`]. The caller's recovery path is to wipe the
    ///   directory and trigger a rebuild.
    pub fn open(path: &Path) -> Result<Self, SearchError> {
        std::fs::create_dir_all(path)?;
        let schema = SearchSchema::new();
        let dir = MmapDirectory::open(path).map_err(|e| {
            SearchError::schema(format!("open index directory {}: {e}", path.display()))
        })?;
        let index = Index::open_or_create(dir, schema.tantivy_schema().clone())?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;
        Ok(Self {
            schema,
            index: Arc::new(index),
            path: path.to_path_buf(),
            reader,
        })
    }

    /// Borrow the schema. Useful when callers need the same field handles
    /// (e.g. snippet generation against a custom query).
    #[must_use]
    pub fn schema(&self) -> &SearchSchema {
        &self.schema
    }

    /// Borrow the index path. Mainly for `.last_indexed` housekeeping.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Build a fresh writer. The indexer owns exactly one at a time.
    ///
    /// # Errors
    ///
    /// As [`tantivy::Index::writer`] — typically a lockfile collision or
    /// out-of-memory.
    pub fn new_writer(&self) -> Result<IndexWriter<TantivyDocument>, SearchError> {
        Ok(self.index.writer(WRITER_MEMORY_BUDGET)?)
    }

    /// Apply an upsert by delete-then-add on the supplied writer.
    ///
    /// The caller is responsible for committing — batching multiple upserts
    /// per commit is the whole point of having an async indexer worker.
    pub fn upsert_on(
        &self,
        writer: &IndexWriter<TantivyDocument>,
        doc: &PageDoc,
    ) -> Result<(), SearchError> {
        // Delete-then-add: the delete is recorded against the upcoming
        // commit and applied before the new add is visible to readers.
        let id_bytes = doc.page_id.as_uuid().as_bytes();
        let term = Term::from_field_bytes(self.schema.page_id, id_bytes);
        writer.delete_term(term);

        let mut td = TantivyDocument::default();
        td.add_bytes(self.schema.page_id, id_bytes);
        td.add_bytes(
            self.schema.namespace_id,
            doc.namespace_id.as_uuid().as_bytes(),
        );
        td.add_text(self.schema.namespace_slug, &doc.namespace_slug);
        td.add_text(self.schema.slug, &doc.slug);
        td.add_text(self.schema.title, &doc.title);
        td.add_text(self.schema.body, &doc.body);
        for tag in &doc.tags {
            td.add_text(self.schema.tags, tag);
        }
        td.add_date(self.schema.updated_at, DateTime::from_utc(doc.updated_at));
        td.add_i64(self.schema.is_talk, i64::from(doc.is_talk));
        writer.add_document(td)?;
        Ok(())
    }

    /// Delete a page by primary key on the supplied writer.
    pub fn delete_on(
        &self,
        writer: &IndexWriter<TantivyDocument>,
        page_id: PageId,
    ) -> Result<(), SearchError> {
        let id_bytes = page_id.as_uuid().as_bytes();
        let term = Term::from_field_bytes(self.schema.page_id, id_bytes);
        writer.delete_term(term);
        Ok(())
    }

    /// Run a search and produce ranked hits with highlighted snippets.
    ///
    /// `next_cursor` is `None` today — relevance-cursor pagination is wired
    /// up to the query type so the API surface is stable, but the cursor
    /// generation itself lands in a follow-up. The current behaviour is to
    /// return the top `limit` results by score.
    ///
    /// # Errors
    ///
    /// - [`SearchError::Tantivy`] if the query fails to parse or the
    ///   searcher cannot read a segment.
    pub fn search(&self, query: &SearchQuery) -> Result<SearchResults, SearchError> {
        let searcher = self.reader.searcher();
        let s = &self.schema;
        let mut qp = QueryParser::for_index(&self.index, vec![s.title, s.body, s.tags]);
        // Apply the operator-tunable title boost. Tantivy expects a strictly
        // positive score; clamp anything <= 0 to 1.0 (i.e. "boost disabled").
        // The default the API layer passes is 2.0 — meaningfully promoting
        // title hits while keeping body matches in the running.
        if query.title_boost > 0.0 {
            qp.set_field_boost(s.title, query.title_boost);
        }
        let mut text = query.text.trim().to_string();
        // Apply optional filters via a `+` prefix using Tantivy's classic
        // query syntax. Cheap, avoids hand-rolling a BooleanQuery for the
        // common case.
        if let Some(ns) = &query.namespace_slug {
            text = format!("{text} +namespace_slug:{ns}");
        }
        if let Some(tag) = &query.tag {
            text = format!("{text} +tags:{tag}");
        }
        if text.trim().is_empty() {
            return Ok(SearchResults {
                hits: Vec::new(),
                next_cursor: None,
                total_estimate: 0,
            });
        }
        let parsed = qp
            .parse_query(text.trim())
            .map_err(|e| SearchError::schema(format!("query parse: {e}")))?;
        let limit = usize::try_from(query.limit.max(1)).unwrap_or(usize::MAX);

        // Talk pages from discussion namespaces (#43) get their BM25 score
        // scaled by `talk_boost` so subject pages outrank their discussion
        // threads by default. `1.0` is the no-op default; the API layer
        // ships `0.5` via `Config::search.talk_boost`.
        let talk_boost = if query.talk_boost > 0.0 {
            query.talk_boost
        } else {
            1.0
        };
        let top: Vec<(Score, DocAddress)> = if (talk_boost - 1.0).abs() < f32::EPSILON {
            // Fast path: no rescore needed.
            searcher.search(&*parsed, &TopDocs::with_limit(limit).order_by_score())?
        } else {
            searcher.search(
                &*parsed,
                &TopDocs::with_limit(limit).tweak_score(move |segment: &SegmentReader| {
                    // The `is_talk` column was added by [`SearchSchema::new`]
                    // and is required for every document the writer adds, so
                    // a missing reader here means the schema is out of sync
                    // with the on-disk index — a programming error, not a
                    // runtime failure mode. We log the issue and fall back to
                    // "no demotion" so a misconfigured deploy still serves
                    // results instead of 500-ing.
                    let reader = segment.fast_fields().i64("is_talk").ok();
                    move |doc, original_score: Score| match reader.as_ref() {
                        Some(column) if matches!(column.values_for_doc(doc).next(), Some(1)) => {
                            original_score * talk_boost
                        }
                        _ => original_score,
                    }
                }),
            )?
        };

        let snippet_gen = SnippetGenerator::create(&searcher, &*parsed, s.body)?;
        let mut hits = Vec::with_capacity(top.len());
        for (score, addr) in &top {
            let doc: TantivyDocument = searcher.doc(*addr)?;
            let mut snippet = snippet_gen.snippet_from_doc(&doc);
            snippet.set_snippet_prefix_postfix("<mark>", "</mark>");
            let snippet_html = if snippet.is_empty() {
                doc_first_str(&doc, s.body)
                    .unwrap_or_default()
                    .chars()
                    .take(SNIPPET_MAX_CHARS)
                    .collect()
            } else {
                snippet.to_html()
            };

            let page_id_bytes = doc_first_bytes(&doc, s.page_id)
                .ok_or_else(|| SearchError::schema("hit missing page_id"))?;
            let page_id = uuid_from_bytes(page_id_bytes)
                .ok_or_else(|| SearchError::schema("page_id bytes not 16 wide"))?;
            let namespace_slug = doc_first_str(&doc, s.namespace_slug)
                .unwrap_or_default()
                .to_owned();
            let slug = doc_first_str(&doc, s.slug).unwrap_or_default().to_owned();
            let title = doc_first_str(&doc, s.title).unwrap_or_default().to_owned();
            let updated_at = doc_first_date(&doc, s.updated_at).map(|d| d.into_utc());

            hits.push(SearchHit {
                page_id: PageId::from_uuid(page_id),
                namespace_slug,
                slug,
                title,
                snippet: snippet_html,
                score: *score,
                updated_at,
            });
        }
        let total_estimate = u64::try_from(top.len()).unwrap_or(u64::MAX);
        Ok(SearchResults {
            hits,
            next_cursor: None,
            total_estimate,
        })
    }

    /// Persist the `.last_indexed` marker, signalling a successful commit.
    pub fn write_last_indexed_marker(&self) -> Result<(), SearchError> {
        let path = self.path.join(LAST_INDEXED_MARKER);
        let now = OffsetDateTime::now_utc();
        std::fs::write(&path, now.unix_timestamp().to_string())?;
        debug!(path = %path.display(), "wrote .last_indexed marker");
        Ok(())
    }

    /// `true` if a `.last_indexed` marker is present. Callers use this to
    /// decide whether the index needs a startup rebuild.
    #[must_use]
    pub fn has_last_indexed_marker(&self) -> bool {
        self.path.join(LAST_INDEXED_MARKER).exists()
    }
}

/// Extract the first textual value for `field` from a Tantivy document.
fn doc_first_str(doc: &TantivyDocument, field: tantivy::schema::Field) -> Option<&str> {
    doc.get_first(field).and_then(|v| v.as_str())
}

/// Extract the first bytes value for `field` from a Tantivy document.
fn doc_first_bytes(doc: &TantivyDocument, field: tantivy::schema::Field) -> Option<&[u8]> {
    doc.get_first(field).and_then(|v| v.as_bytes())
}

/// Extract the first date value for `field` from a Tantivy document.
fn doc_first_date(doc: &TantivyDocument, field: tantivy::schema::Field) -> Option<DateTime> {
    doc.get_first(field).and_then(|v| v.as_datetime())
}

/// Build a [`uuid::Uuid`] from a 16-byte slice, returning `None` on a
/// mismatched length.
fn uuid_from_bytes(bytes: &[u8]) -> Option<uuid::Uuid> {
    let arr: [u8; 16] = bytes.try_into().ok()?;
    Some(uuid::Uuid::from_bytes(arr))
}
