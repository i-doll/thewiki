//! The renderer seam.
//!
//! `thewiki` aims to support multiple authoring formats (Markdown at v1;
//! AsciiDoc, MediaWiki wikitext, and reStructuredText post-v1). Concrete
//! renderers live in the [`thewiki-render`] crate; this module defines the
//! stable trait they all implement, the value types they return, and a
//! lightweight [`RendererRegistry`] that maps [`ContentFormat`] →
//! [`Renderer`].
//!
//! Implementations are responsible for HTML sanitisation. The API layer
//! treats [`RenderedDoc::html`] as already safe to embed.
//!
//! [`thewiki-render`]: ../../thewiki_render/index.html
//!
//! # Layout
//!
//! - [`Renderer`] — the trait every concrete format implementation lives
//!   behind. Has to be `Send + Sync` so app state can carry an
//!   `Arc<dyn Renderer>` across Axum tasks.
//! - [`RenderContext`] — what the renderer is allowed to know about the
//!   surrounding page (namespace, slug, optional [`LinkResolver`] for redlink
//!   work in M1).
//! - [`RenderedDoc`] — the output: sanitised HTML plus the extracted
//!   metadata used by ToC, backlinks, and media handling.
//! - [`RenderError`] — the error type. `#[non_exhaustive]` so future formats
//!   can add variants without breaking downstream `match` expressions.
//! - [`RendererRegistry`] — a tiny map keyed on [`ContentFormat`]; concrete
//!   defaults are installed by `thewiki-render::new_registry_with_defaults`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;

use crate::content_format::ContentFormat;
use crate::id::NamespaceId;

/// A document renderer.
///
/// Every supported authoring format ([`ContentFormat`]) is implemented by
/// exactly one type behind this trait. Renderers are *sync* — async work
/// belongs to storage and the API layer, not to the parser.
///
/// Implementations must produce **sanitised** HTML in
/// [`RenderedDoc::html`]; consumers of the trait do not post-process.
pub trait Renderer: Send + Sync {
    /// Which [`ContentFormat`] this renderer handles. Used by the
    /// [`RendererRegistry`] to dispatch.
    fn format(&self) -> ContentFormat;

    /// Render `source` to a [`RenderedDoc`].
    ///
    /// # Errors
    ///
    /// Returns [`RenderError`] if the input is unrenderable (empty, syntax
    /// the renderer refuses to handle, etc.).
    fn render(&self, source: &str, ctx: &RenderContext) -> Result<RenderedDoc, RenderError>;

    /// Extract the outbound `[[WikiLink]]` targets without rendering.
    ///
    /// Used by the backlink/redlink machinery (M1) to avoid a full render
    /// pass when only the link graph is needed.
    ///
    /// v1 returns an empty `Vec` for Markdown — `[[WikiLink]]` syntax lands
    /// in M1.
    fn extract_links(&self, source: &str) -> Vec<WikiLink> {
        let _ = source;
        Vec::new()
    }
}

/// Per-render context.
///
/// Renderers receive a `RenderContext` so they know *where* the source is
/// being rendered: which namespace, which slug, and (M1) a [`LinkResolver`]
/// that can answer "does the target page exist?" for redlink colouring.
#[derive(Debug)]
pub struct RenderContext {
    /// Namespace the page being rendered lives in.
    pub namespace: NamespaceId,
    /// URL slug of the page being rendered.
    pub page_slug: String,
    /// Optional resolver used to decide which `[[WikiLink]]`s are red.
    ///
    /// `None` for M0; the API layer wires this in M1.
    pub link_resolver: Option<Box<dyn LinkResolver>>,
}

impl RenderContext {
    /// Build a context with just a namespace and slug — no link resolver.
    #[must_use]
    pub fn new(namespace: NamespaceId, page_slug: impl Into<String>) -> Self {
        Self {
            namespace,
            page_slug: page_slug.into(),
            link_resolver: None,
        }
    }
}

// Note: `RenderContext` deliberately does not implement `Default`. Production
// code paths must come through `RenderContext::new(namespace, slug)` so the
// namespace is a deliberate choice rather than a freshly-minted throwaway.
// Tests construct contexts via `RenderContext::new(NamespaceId::new(), "")`.

/// Decides whether a `[[WikiLink]]` target resolves to an existing page.
///
/// Skeleton only at M0; M1 wires this up to the page repository.
pub trait LinkResolver: Send + Sync + std::fmt::Debug {
    /// `true` if `slug` exists inside `namespace`.
    fn resolves(&self, namespace: NamespaceId, slug: &str) -> bool;
}

/// A rendered document.
///
/// Carries the sanitised HTML plus everything that callers later need
/// without re-parsing: headings for a ToC, outbound link targets for
/// backlinks/redlinks, and image references for media handling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RenderedDoc {
    /// Sanitised HTML, ready to embed.
    pub html: String,
    /// Headings encountered, in document order — feeds the table of contents.
    pub headings: Vec<Heading>,
    /// `[[WikiLink]]` targets extracted from the source (M1).
    pub outbound_links: Vec<WikiLink>,
    /// `![alt](url)` image references encountered.
    pub images: Vec<ImageRef>,
}

/// A heading extracted from a rendered document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Heading {
    /// Heading depth (`1` for `<h1>`, …, `6` for `<h6>`).
    pub level: u8,
    /// Slugified anchor (`id` on the heading element).
    pub anchor: String,
    /// Plain-text content of the heading (HTML stripped).
    pub text: String,
}

/// A wiki-link target inside the source.
///
/// Models `[[Target]]` and `[[Target|Display]]`. The renderer extracts
/// these; the consumer decides how to render them given a
/// [`LinkResolver`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WikiLink {
    /// Page reference inside the brackets (`Foo/Bar`, `User:Alice`, etc.).
    pub target: String,
    /// Optional display label (`[[Target|Display]]`).
    pub display: Option<String>,
}

/// An image reference encountered while rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImageRef {
    /// `src` URL.
    pub url: String,
    /// Alt text, if any.
    pub alt: Option<String>,
    /// Title attribute, if any.
    pub title: Option<String>,
}

/// Errors raised while rendering.
///
/// `#[non_exhaustive]` so future renderers can add format-specific failure
/// modes without breaking downstream `match` expressions.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum RenderError {
    /// The source was empty (or whitespace-only).
    #[error("source is empty")]
    EmptyInput,

    /// The source contained syntax the renderer refuses to handle.
    #[error("unsupported syntax: {detail}")]
    UnsupportedSyntax {
        /// Human-readable description of what tripped the renderer.
        detail: String,
    },
}

/// A map of [`ContentFormat`] → [`Renderer`].
///
/// The registry lives in `core` so any crate can store one in app state;
/// the concrete renderers (and a `new_with_defaults` constructor that
/// installs them) live in `thewiki-render`, which is allowed to know about
/// `ammonia` and `pulldown-cmark`.
#[derive(Default)]
pub struct RendererRegistry {
    renderers: HashMap<ContentFormat, Box<dyn Renderer>>,
}

impl RendererRegistry {
    /// Empty registry. Use `thewiki-render`'s constructor for the
    /// production-ready set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a renderer. If one was already registered for the same
    /// [`ContentFormat`], the new value replaces it.
    pub fn register(&mut self, renderer: Box<dyn Renderer>) {
        self.renderers.insert(renderer.format(), renderer);
    }

    /// Look up the renderer for `format`, if any.
    #[must_use]
    pub fn get(&self, format: ContentFormat) -> Option<&dyn Renderer> {
        self.renderers.get(&format).map(AsRef::as_ref)
    }

    /// Number of registered renderers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.renderers.len()
    }

    /// `true` if no renderers are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.renderers.is_empty()
    }
}

impl std::fmt::Debug for RendererRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RendererRegistry")
            .field("formats", &self.renderers.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "ergonomic tests")]
mod tests {
    use super::*;

    /// A toy renderer that records nothing — used to test the registry
    /// without pulling in a heavyweight dependency.
    #[derive(Debug)]
    struct StubRenderer;

    impl Renderer for StubRenderer {
        fn format(&self) -> ContentFormat {
            ContentFormat::Markdown
        }

        fn render(&self, _: &str, _: &RenderContext) -> Result<RenderedDoc, RenderError> {
            Ok(RenderedDoc {
                html: String::new(),
                headings: Vec::new(),
                outbound_links: Vec::new(),
                images: Vec::new(),
            })
        }
    }

    #[test]
    fn registry_register_and_get() {
        let mut registry = RendererRegistry::new();
        assert!(registry.is_empty());
        registry.register(Box::new(StubRenderer));
        assert_eq!(registry.len(), 1);
        let renderer = registry.get(ContentFormat::Markdown).expect("registered");
        assert_eq!(renderer.format(), ContentFormat::Markdown);
    }

    #[test]
    fn registry_replaces_on_re_register() {
        let mut registry = RendererRegistry::new();
        registry.register(Box::new(StubRenderer));
        registry.register(Box::new(StubRenderer));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn rendered_doc_round_trips_serde() {
        let doc = RenderedDoc {
            html: "<p>hi</p>".into(),
            headings: vec![Heading {
                level: 1,
                anchor: "hi".into(),
                text: "Hi".into(),
            }],
            outbound_links: vec![WikiLink {
                target: "Foo".into(),
                display: Some("Foo Page".into()),
            }],
            images: vec![ImageRef {
                url: "https://example.com/i.png".into(),
                alt: Some("alt".into()),
                title: None,
            }],
        };
        let json = serde_json::to_string(&doc).expect("serialise");
        let parsed: RenderedDoc = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(parsed, doc);
    }

    #[test]
    fn render_error_empty_input_message() {
        assert_eq!(RenderError::EmptyInput.to_string(), "source is empty");
    }

    #[test]
    fn render_error_unsupported_syntax_message() {
        let err = RenderError::UnsupportedSyntax {
            detail: "weird".into(),
        };
        assert_eq!(err.to_string(), "unsupported syntax: weird");
    }

    #[test]
    fn extract_links_default_is_empty() {
        let r = StubRenderer;
        assert!(r.extract_links("anything").is_empty());
    }
}
