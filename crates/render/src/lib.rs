//! Renderer implementations for `thewiki`.
//!
//! M0 ships a single concrete renderer: [`MarkdownRenderer`], implementing
//! [`thewiki_core::render::Renderer`] for
//! [`thewiki_core::ContentFormat::Markdown`] on top of [`pulldown_cmark`].
//! HTML is post-processed by [`ammonia`] against a conservative allowlist
//! (private `sanitise` module) before being handed back to callers, so the
//! [`thewiki_core::render::RenderedDoc::html`] field is always safe to
//! embed.
//!
//! Future formats (AsciiDoc, MediaWiki wikitext, reStructuredText) plug in
//! by implementing the same trait — `thewiki-core` and `thewiki-api` never
//! learn about Markdown specifically.
//!
//! # Quick start
//!
//! ```
//! use thewiki_core::{ContentFormat, id::NamespaceId, render::{RenderContext, Renderer}};
//! use thewiki_render::MarkdownRenderer;
//!
//! let renderer = MarkdownRenderer::new();
//! let ctx = RenderContext::new(NamespaceId::new(), "");
//! let doc = renderer
//!     .render("# Hello\n\nWorld", &ctx)
//!     .expect("render");
//! assert!(doc.html.contains("<h1"));
//! assert_eq!(doc.headings[0].anchor, "hello");
//! # let _ = ContentFormat::Markdown;
//! ```
//!
//! # Wiring up the registry
//!
//! [`new_registry_with_defaults`] returns a
//! [`thewiki_core::render::RendererRegistry`] pre-populated with every
//! renderer this crate ships. The API layer calls it once at startup and
//! stores the result in app state.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod markdown;
mod sanitise;
mod slug;
pub mod template;

use thewiki_core::render::RendererRegistry;

pub use markdown::MarkdownRenderer;
pub use template::{
    DEFAULT_MAX_RECURSION_DEPTH, NoopResolver, TEMPLATE_ERROR_CLASS, TEMPLATE_NAMESPACE,
    TemplateResolver, TemplateSource,
};

/// Build a [`RendererRegistry`] containing every concrete renderer this
/// crate ships.
///
/// M0: Markdown only. As new formats land they are added here so the API
/// layer keeps a single call site for "give me a fully populated registry".
#[must_use]
pub fn new_registry_with_defaults() -> RendererRegistry {
    let mut registry = RendererRegistry::new();
    registry.register(Box::new(MarkdownRenderer::new()));
    registry
}
