//! Integration tests for `[[WikiLink]]` parsing and redlink rendering.
//!
//! Exercises [`MarkdownRenderer`] through the [`Renderer`] trait surface and
//! through a couple of fake [`LinkResolver`]s — one that always resolves,
//! one that resolves a fixed allowlist, and one that never resolves. The
//! goal is to cover both halves of the redlink/non-redlink branch and the
//! `Foo|bar` display-pipe case.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration tests prefer ergonomics"
)]

use std::collections::HashSet;

use pretty_assertions::assert_eq;
use thewiki_core::id::NamespaceId;
use thewiki_core::render::{LinkResolver, RenderContext, Renderer};
use thewiki_render::MarkdownRenderer;

/// Always-resolves resolver (every wikilink looks like an existing page).
#[derive(Debug, Default)]
struct AlwaysResolver;

impl LinkResolver for AlwaysResolver {
    fn resolves(&self, _namespace: NamespaceId, _slug: &str) -> bool {
        true
    }
}

/// Never-resolves resolver (every wikilink renders as a redlink).
#[derive(Debug, Default)]
struct NeverResolver;

impl LinkResolver for NeverResolver {
    fn resolves(&self, _namespace: NamespaceId, _slug: &str) -> bool {
        false
    }
}

/// Resolves only the targets in `known`.
#[derive(Debug)]
struct AllowlistResolver {
    known: HashSet<String>,
}

impl LinkResolver for AllowlistResolver {
    fn resolves(&self, _namespace: NamespaceId, slug: &str) -> bool {
        self.known.contains(slug)
    }
}

fn ctx() -> RenderContext {
    RenderContext::new(NamespaceId::new(), "").with_namespace_slug("Main")
}

fn ctx_with<R: LinkResolver + 'static>(resolver: R) -> RenderContext {
    RenderContext::new(NamespaceId::new(), "")
        .with_namespace_slug("Main")
        .with_link_resolver(Box::new(resolver))
}

#[test]
fn extract_links_pulls_all_wikilink_targets() {
    let r = MarkdownRenderer::new();
    let links = r.extract_links("[[Foo]] and [[Foo|bar]] and [[Foo/Sub|alt]]");

    assert_eq!(links.len(), 3, "found: {links:?}");

    // [[Foo]] — bare, no display.
    assert_eq!(links[0].target, "Foo");
    assert_eq!(links[0].display, None);

    // [[Foo|bar]] — pipe-display, target preserved.
    assert_eq!(links[1].target, "Foo");
    assert_eq!(links[1].display.as_deref(), Some("bar"));

    // [[Foo/Sub|alt]] — multi-segment target with pipe-display.
    assert_eq!(links[2].target, "Foo/Sub");
    assert_eq!(links[2].display.as_deref(), Some("alt"));
}

#[test]
fn extract_links_handles_only_wikilink_syntax() {
    let r = MarkdownRenderer::new();
    // Standard Markdown links and footnote references must not appear.
    let links = r.extract_links("[label](https://example.com) and `[[code]]`");
    assert!(links.is_empty(), "found: {links:?}");
}

#[test]
fn extract_links_records_outbound_links_into_doc() {
    // The same data is mirrored onto `RenderedDoc::outbound_links` so the
    // page-create / page-update path can populate `page_links` without a
    // second parse pass.
    let r = MarkdownRenderer::new();
    let doc = r
        .render("[[Alpha]] and [[Beta|see]]", &ctx())
        .expect("render");
    let targets: Vec<&str> = doc
        .outbound_links
        .iter()
        .map(|l| l.target.as_str())
        .collect();
    assert_eq!(targets, vec!["Alpha", "Beta"]);
    assert_eq!(doc.outbound_links[1].display.as_deref(), Some("see"));
}

#[test]
fn existing_target_renders_as_normal_wiki_link() {
    let r = MarkdownRenderer::new();
    let doc = r
        .render("[[Existing]]", &ctx_with(AlwaysResolver))
        .expect("render");

    assert!(
        doc.html.contains("href=\"/wiki/Main/Existing\""),
        "html = {}",
        doc.html
    );
    assert!(
        !doc.html.contains("redlink"),
        "existing wikilink must not get the redlink class: {}",
        doc.html
    );
    assert!(doc.html.contains(">Existing</a>"), "html = {}", doc.html);
}

#[test]
fn missing_target_renders_with_redlink_class() {
    let r = MarkdownRenderer::new();
    let doc = r
        .render("[[NotYet]]", &ctx_with(NeverResolver))
        .expect("render");

    assert!(
        doc.html.contains("class=\"redlink\""),
        "missing wikilink must carry the redlink class: {}",
        doc.html
    );
    assert!(
        doc.html.contains("href=\"/wiki/Main/NotYet/edit?new=1\""),
        "redlink must point at the create form: {}",
        doc.html
    );
    assert!(
        doc.html.contains("title=\"(missing) NotYet\""),
        "redlink must carry a helpful hover title: {}",
        doc.html
    );
}

#[test]
fn pipe_display_uses_display_text_inside_anchor() {
    let r = MarkdownRenderer::new();
    let doc = r
        .render("[[Existing|click]]", &ctx_with(AlwaysResolver))
        .expect("render");

    assert!(
        doc.html.contains("href=\"/wiki/Main/Existing\""),
        "href must point at the target, not the display: {}",
        doc.html
    );
    assert!(
        doc.html.contains(">click</a>"),
        "anchor text must be the pipe-display: {}",
        doc.html
    );
}

#[test]
fn no_resolver_defaults_to_non_redlink_rendering() {
    // Renderer-only callers (test fixtures, the doc example in lib.rs) get a
    // sensible default: every wikilink renders as if it resolved.
    let r = MarkdownRenderer::new();
    let doc = r.render("[[Foo]] and [[Bar|baz]]", &ctx()).expect("render");

    assert!(!doc.html.contains("redlink"), "html = {}", doc.html);
    assert!(
        doc.html.contains("href=\"/wiki/Main/Foo\""),
        "html = {}",
        doc.html
    );
    assert!(
        doc.html.contains("href=\"/wiki/Main/Bar\""),
        "html = {}",
        doc.html
    );
    assert!(doc.html.contains(">baz</a>"), "html = {}", doc.html);
}

#[test]
fn allowlist_resolver_mixes_red_and_blue_links() {
    let known: HashSet<String> = ["Home"].into_iter().map(String::from).collect();
    let r = MarkdownRenderer::new();
    let doc = r
        .render(
            "Visit [[Home]] then [[Stranger]].",
            &ctx_with(AllowlistResolver { known }),
        )
        .expect("render");

    assert!(
        doc.html.contains("href=\"/wiki/Main/Home\""),
        "Home must render as a normal link: {}",
        doc.html
    );
    assert!(
        doc.html.contains("href=\"/wiki/Main/Stranger/edit?new=1\""),
        "Stranger must render as a redlink: {}",
        doc.html
    );
    // The Home link must not carry the redlink class.
    assert!(
        doc.html.matches("redlink").count() <= 1,
        "only the Stranger anchor should mention redlink: {}",
        doc.html
    );
}

#[test]
fn sanitiser_preserves_redlink_class_and_title() {
    let r = MarkdownRenderer::new();
    let doc = r
        .render("[[Missing]]", &ctx_with(NeverResolver))
        .expect("render");

    // The ammonia allowlist permits `class` and `title` on `<a>`, so both
    // survive sanitisation. The defang `rel` is force-applied alongside.
    assert!(
        doc.html.contains("class=\"redlink\""),
        "class attr stripped: {}",
        doc.html
    );
    assert!(
        doc.html.contains("title=\"(missing) Missing\""),
        "title attr stripped: {}",
        doc.html
    );
    assert!(
        doc.html.contains("rel=\"noopener noreferrer nofollow\""),
        "rel defang not applied: {}",
        doc.html
    );
}

#[test]
fn multi_segment_target_keeps_slashes_in_href() {
    let r = MarkdownRenderer::new();
    let doc = r
        .render("[[Foo/Sub|nested]]", &ctx_with(AlwaysResolver))
        .expect("render");
    assert!(
        doc.html.contains("href=\"/wiki/Main/Foo/Sub\""),
        "html = {}",
        doc.html
    );
    assert!(doc.html.contains(">nested</a>"), "html = {}", doc.html);
}

#[test]
fn target_with_spaces_is_percent_encoded_in_href() {
    let r = MarkdownRenderer::new();
    let doc = r
        .render("[[Hello World]]", &ctx_with(AlwaysResolver))
        .expect("render");
    assert!(
        doc.html.contains("href=\"/wiki/Main/Hello%20World\""),
        "html = {}",
        doc.html
    );
}

#[test]
fn surrounding_paragraph_text_is_preserved() {
    let r = MarkdownRenderer::new();
    let doc = r
        .render(
            "Read [[Existing]] before [[NotYet]].",
            &ctx_with(AllowlistResolver {
                known: ["Existing"].into_iter().map(String::from).collect(),
            }),
        )
        .expect("render");
    assert!(doc.html.contains("Read "), "html = {}", doc.html);
    assert!(doc.html.contains(" before "), "html = {}", doc.html);
    assert!(doc.html.contains("."), "html = {}", doc.html);
}

#[test]
fn extract_links_for_pages_without_wikilinks_is_empty() {
    let r = MarkdownRenderer::new();
    assert!(r.extract_links("# Heading\n\nSome body").is_empty());
}
