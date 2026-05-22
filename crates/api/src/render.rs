//! Per-request render plumbing for the API layer.
//!
//! The [`MarkdownRenderer`] (and any future renderer) needs a synchronous
//! [`LinkResolver`] to decide which `[[WikiLink]]`s render as redlinks. The
//! repository methods this layer wants to consult are `async fn`, so we
//! can't drive them from inside the sync trait method. Instead we
//! pre-resolve every target referenced in the source — via
//! [`Renderer::extract_links`] — and hand the renderer a
//! [`PrecomputedLinkResolver`] backed by a `HashSet` of `(namespace,
//! page_slug)` pairs that actually exist. Lookups inside `render` are then
//! O(1) hashes.
//!
//! The full pipeline (extract → resolve → render) is bundled into
//! [`render_markdown`] so handlers consume it as a single call.

use std::collections::HashSet;

use thewiki_core::id::NamespaceId;
use thewiki_core::render::{LinkResolver, RenderContext, RenderedDoc, Renderer};
use thewiki_render::MarkdownRenderer;
use thewiki_storage::StorageError;
use thewiki_storage::repo::PageRepository;

use crate::error::ApiError;
use crate::state::AppStorage;

/// Synchronous resolver backed by a fixed allowlist of `(namespace, slug)`
/// pairs.
///
/// Built once per request by [`render_markdown`] after pre-flighting every
/// `[[Target]]` against the page repository. The renderer's
/// [`Renderer::render`] consumes it inside its sync event walk and decides
/// redlink vs. non-redlink in O(1).
#[derive(Debug, Default)]
pub struct PrecomputedLinkResolver {
    known: HashSet<(NamespaceId, String)>,
}

impl PrecomputedLinkResolver {
    /// Wrap a set of `(namespace, slug)` pairs known to exist.
    #[must_use]
    pub fn new(known: HashSet<(NamespaceId, String)>) -> Self {
        Self { known }
    }
}

impl LinkResolver for PrecomputedLinkResolver {
    fn resolves(&self, namespace: NamespaceId, slug: &str) -> bool {
        // HashSet<(_, String)>::contains requires a (T, &str) -> Borrow
        // dance to avoid allocating; the dance isn't worth it here because
        // wikilink density per page is small. Allocate the lookup key.
        self.known.contains(&(namespace, slug.to_string()))
    }
}

/// Render the Markdown `source` for the page identified by `(namespace,
/// page_slug)`, resolving every `[[Target]]` against the page repository so
/// missing targets render as redlinks.
///
/// `namespace_slug` is the URL slug of the namespace, used by the renderer
/// to build `/wiki/<namespace>/<target>` hrefs. We do not have multiple
/// namespaces at M0 — `namespace_slug` is the same `Main` literal the rest
/// of the API uses today — but threading it through means the future
/// namespace-prefix wiring (#28) is a one-liner.
///
/// # Errors
///
/// Propagates [`ApiError`] for storage failures. A missing target is **not**
/// an error: it simply renders as a redlink.
pub async fn render_markdown<S: AppStorage>(
    storage: &S,
    renderer: &MarkdownRenderer,
    namespace_id: NamespaceId,
    namespace_slug: &str,
    page_slug: &str,
    source: &str,
) -> Result<RenderedDoc, ApiError> {
    let resolver = build_resolver(storage, renderer, namespace_id, source).await?;
    let ctx = RenderContext::new(namespace_id, page_slug.to_string())
        .with_namespace_slug(namespace_slug.to_string())
        .with_link_resolver(Box::new(resolver));
    renderer
        .render(source, &ctx)
        .map_err(|err| ApiError::Internal(format!("render: {err}")))
}

/// Build a [`PrecomputedLinkResolver`] for every wikilink target in `source`.
///
/// We extract the link set once (linear-time event walk) and then issue one
/// `get_by_namespace_and_slug` per unique target. M0 has a single namespace
/// (`Main`) so the namespace argument is the page's own; multi-namespace
/// link resolution (`[[User:Alice]]`) lands with #28.
pub async fn build_resolver<S: AppStorage>(
    storage: &S,
    renderer: &MarkdownRenderer,
    namespace_id: NamespaceId,
    source: &str,
) -> Result<PrecomputedLinkResolver, ApiError> {
    let links = renderer.extract_links(source);
    let mut known: HashSet<(NamespaceId, String)> = HashSet::new();
    let mut seen_targets: HashSet<String> = HashSet::new();
    let pages = storage.pages();
    for link in &links {
        // Skip empties (the parser shouldn't emit them but be defensive).
        if link.target.is_empty() {
            continue;
        }
        if !seen_targets.insert(link.target.clone()) {
            continue;
        }
        match pages
            .get_by_namespace_and_slug(namespace_id, &link.target)
            .await
        {
            Ok(_) => {
                known.insert((namespace_id, link.target.clone()));
            }
            Err(StorageError::NotFound) => {
                // Redlink — the renderer will style it.
            }
            Err(other) => return Err(ApiError::from(other)),
        }
    }
    Ok(PrecomputedLinkResolver::new(known))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use thewiki_core::id::NamespaceId;

    #[test]
    fn precomputed_resolver_contains_match() {
        let ns = NamespaceId::new();
        let mut set = HashSet::new();
        set.insert((ns, "Foo".to_string()));
        let resolver = PrecomputedLinkResolver::new(set);
        assert!(resolver.resolves(ns, "Foo"));
        assert!(!resolver.resolves(ns, "Bar"));
    }

    #[test]
    fn precomputed_resolver_namespace_isolated() {
        let ns_a = NamespaceId::new();
        let ns_b = NamespaceId::new();
        let mut set = HashSet::new();
        set.insert((ns_a, "Foo".to_string()));
        let resolver = PrecomputedLinkResolver::new(set);
        assert!(resolver.resolves(ns_a, "Foo"));
        assert!(!resolver.resolves(ns_b, "Foo"));
    }
}
