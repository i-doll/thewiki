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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use thewiki_core::NamespaceSlug;
use thewiki_core::id::NamespaceId;
use thewiki_core::render::{LinkResolver, RenderContext, RenderedDoc, Renderer};
use thewiki_render::{MarkdownRenderer, TemplateResolver, TemplateSource};
use thewiki_storage::StorageError;
use thewiki_storage::repo::{NamespaceRepository, PageRepository, RevisionRepository};

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
    // Pre-fetch every template referenced by the source (transitively) and
    // attach a precomputed resolver so the synchronous template pre-pass
    // can resolve calls without re-entering async. The walk is bounded by
    // the renderer's configured depth cap.
    let template_resolver =
        build_template_resolver(storage, source, renderer.max_recursion_depth()).await?;
    let r = renderer
        .clone()
        .with_template_resolver(Arc::new(template_resolver));
    let ctx = RenderContext::new(namespace_id, page_slug.to_string())
        .with_namespace_slug(namespace_slug.to_string())
        .with_link_resolver(Box::new(resolver));
    r.render(source, &ctx)
        .map_err(|err| ApiError::Internal(format!("render: {err}")))
}

/// Precomputed template resolver — the renderer's sync `TemplateResolver`
/// backed by a `HashMap` filled by an async walk over the page store.
///
/// Built by [`build_template_resolver`] before each render; key is
/// `(namespace, name)`. Missing entries surface as `None` and the renderer
/// emits a `[template error: ... not found]` inline diagnostic.
///
/// `known_namespaces` carries the slugs that actually exist in storage so
/// the renderer can distinguish "unknown namespace" from "template not
/// found" when a user writes `{{Foo:Bar}}` with `Foo` not being a real
/// namespace.
#[derive(Debug, Default)]
pub struct PrecomputedTemplateResolver {
    sources: HashMap<(String, String), TemplateSource>,
    known_namespaces: HashSet<String>,
}

impl PrecomputedTemplateResolver {
    /// Wrap a populated `(namespace, name) -> body` map plus the set of
    /// known namespace slugs.
    #[must_use]
    pub fn new(
        sources: HashMap<(String, String), TemplateSource>,
        known_namespaces: HashSet<String>,
    ) -> Self {
        Self {
            sources,
            known_namespaces,
        }
    }
}

impl TemplateResolver for PrecomputedTemplateResolver {
    fn resolve(&self, ns: &str, name: &str) -> Option<TemplateSource> {
        self.sources
            .get(&(ns.to_string(), name.to_string()))
            .cloned()
    }

    fn namespace_exists(&self, ns: &str) -> bool {
        self.known_namespaces.contains(ns)
    }
}

/// Walk every `{{ns:Name}}` reference in `source` (and in any template body
/// we resolve along the way), fetch the bodies up-front, and return a
/// [`PrecomputedTemplateResolver`] the synchronous renderer can consume.
///
/// The walk is bounded by `max_depth`; templates beyond that depth are
/// loaded *enough* that the renderer can produce a "recursion limit
/// exceeded" diagnostic — we don't try to short-circuit the limit
/// detection at the fetch layer.
///
/// # Errors
///
/// Storage errors other than `NotFound` propagate. A missing template is
/// expected (the renderer turns it into an inline diagnostic), so the walk
/// silently drops only that specific case. Any other storage error must
/// fail the render loudly rather than silently producing a misleading
/// "template not found" diagnostic during a DB hiccup.
pub async fn build_template_resolver<S: AppStorage>(
    storage: &S,
    source: &str,
    max_depth: usize,
) -> Result<PrecomputedTemplateResolver, ApiError> {
    let mut sources: HashMap<(String, String), TemplateSource> = HashMap::new();
    let mut known_namespaces: HashSet<String> = HashSet::new();
    let mut queue: Vec<(String, String)> =
        scan_template_calls(source).into_iter().collect();
    let mut depth = 0;
    // Cache namespace ids so we don't re-fetch the same `Namespace` row.
    let mut ns_cache: HashMap<String, Option<NamespaceId>> = HashMap::new();
    while !queue.is_empty() && depth <= max_depth {
        let mut next: HashSet<(String, String)> = HashSet::new();
        for (ns, name) in std::mem::take(&mut queue) {
            let key = (ns.clone(), name.clone());
            if sources.contains_key(&key) {
                continue;
            }
            let ns_id = match ns_cache.get(&ns) {
                Some(v) => *v,
                None => {
                    let id = resolve_namespace_id(storage, &ns).await?;
                    ns_cache.insert(ns.clone(), id);
                    id
                }
            };
            let Some(ns_id) = ns_id else {
                // Missing namespace -> renderer will emit a distinct
                // "unknown namespace" diagnostic. We do NOT record it in
                // `known_namespaces`.
                continue;
            };
            // Record that we've confirmed this namespace exists, so the
            // renderer can distinguish "ns missing" from "page missing".
            known_namespaces.insert(ns.clone());
            let page = match storage.pages().get_by_namespace_and_slug(ns_id, &name).await {
                Ok(p) => p,
                // Page missing — caller emits "template not found" inline.
                // This is the *only* storage error we silently swallow.
                Err(StorageError::NotFound) => continue,
                Err(e) => return Err(ApiError::from(e)),
            };
            let body = match page.current_revision_id {
                Some(rev_id) => match storage.revisions().get_by_id(rev_id).await {
                    Ok(r) => r.body,
                    // A page that points at a missing revision is a data
                    // bug, but treat it as an empty body so the rest of
                    // the render proceeds — same as the prior behaviour.
                    Err(StorageError::NotFound) => String::new(),
                    Err(e) => return Err(ApiError::from(e)),
                },
                None => String::new(),
            };
            // Surface new calls in this body so the next loop pass picks
            // them up.
            for call in scan_template_calls(&body) {
                if !sources.contains_key(&call) {
                    next.insert(call);
                }
            }
            sources.insert(
                key,
                TemplateSource {
                    id: format!("{ns}:{name}"),
                    revision_id: page
                        .current_revision_id
                        .map(|r| r.into_uuid().to_string())
                        .unwrap_or_default(),
                    body,
                },
            );
        }
        queue = next.into_iter().collect();
        depth += 1;
    }
    Ok(PrecomputedTemplateResolver::new(sources, known_namespaces))
}

/// Look up a namespace by slug, returning `Ok(None)` on a clean miss so the
/// caller can treat it as "render-time error".
async fn resolve_namespace_id<S: AppStorage>(
    storage: &S,
    ns: &str,
) -> Result<Option<NamespaceId>, ApiError> {
    let slug = match NamespaceSlug::new(ns.to_owned()) {
        Ok(s) => s,
        Err(_) => return Ok(None),
    };
    match storage.namespaces().get_by_slug(&slug).await {
        Ok(n) => Ok(Some(n.id)),
        Err(StorageError::NotFound) => Ok(None),
        Err(other) => Err(ApiError::from(other)),
    }
}

/// Scan a body for top-level `{{ns:Name|...}}` and `{{Name|...}}` calls,
/// returning the unique `(namespace, name)` pairs found. Parser-function
/// calls (`{{#foo:...}}`) and triple-brace parameter refs are ignored —
/// the renderer's pre-pass handles them and we don't need to pre-fetch
/// anything for them.
fn scan_template_calls(source: &str) -> HashSet<(String, String)> {
    let mut out: HashSet<(String, String)> = HashSet::new();
    let bytes = source.as_bytes();
    let mut i = 0;
    while i + 2 <= bytes.len() {
        if &bytes[i..i + 2] != b"{{" {
            i += 1;
            continue;
        }
        // Triple-brace param ref — skip past its close so we don't see the
        // inner `{{` again.
        if i + 3 <= bytes.len() && &bytes[i..i + 3] == b"{{{" {
            if let Some(rel) = find_close(&source[i + 3..], b"}}}") {
                i += 3 + rel + 3;
                continue;
            }
            break;
        }
        // Regular `{{...}}`. Find matching `}}` honouring nesting.
        if let Some(rel) = find_matching_double(&source[i + 2..]) {
            let inner = &source[i + 2..i + 2 + rel];
            // Recurse into the inner content for nested calls before we
            // record this one.
            for nested in scan_template_calls(inner) {
                out.insert(nested);
            }
            // Skip parser functions.
            let trimmed = inner.trim_start();
            if !trimmed.starts_with('#') {
                let head = inner.split('|').next().unwrap_or("").trim();
                if !head.is_empty() {
                    let (ns, name) = match head.split_once(':') {
                        Some((n, m)) => (n.trim().to_string(), m.trim().to_string()),
                        None => ("Template".to_string(), head.trim().to_string()),
                    };
                    out.insert((ns, name));
                }
            }
            i += 2 + rel + 2;
        } else {
            break;
        }
    }
    out
}

/// Find the byte offset of `needle` inside `s`. Used for the triple-brace
/// skip case.
fn find_close(s: &str, needle: &[u8]) -> Option<usize> {
    s.as_bytes()
        .windows(needle.len())
        .position(|w| w == needle)
}

/// Like `find_close(s, b"}}")` but honours `{{...}}` nesting so we don't
/// split a call body in half.
fn find_matching_double(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth: i32 = 0;
    let mut i = 0;
    while i < bytes.len() {
        if i + 3 <= bytes.len() && &bytes[i..i + 3] == b"{{{" {
            if let Some(rel) = find_close(&s[i + 3..], b"}}}") {
                i += 3 + rel + 3;
                continue;
            }
            return None;
        }
        if i + 2 <= bytes.len() && &bytes[i..i + 2] == b"{{" {
            depth += 1;
            i += 2;
            continue;
        }
        if i + 2 <= bytes.len() && &bytes[i..i + 2] == b"}}" {
            if depth == 0 {
                return Some(i);
            }
            depth -= 1;
            i += 2;
            continue;
        }
        i += 1;
    }
    None
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
