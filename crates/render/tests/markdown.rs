//! Integration tests for the Markdown renderer.
//!
//! These exercise the public surface of `thewiki_render::MarkdownRenderer`
//! through the [`thewiki_core::render::Renderer`] trait, so they double as
//! a check that the trait abstraction in `core` is the seam we actually
//! ship.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration tests prefer ergonomics"
)]

use pretty_assertions::assert_eq;
use thewiki_core::ContentFormat;
use thewiki_core::id::NamespaceId;
use thewiki_core::render::{RenderContext, RenderError, Renderer, RendererRegistry};
use thewiki_render::{MarkdownRenderer, new_registry_with_defaults};

fn ctx() -> RenderContext {
    RenderContext::new(NamespaceId::new(), "")
}

fn render(source: &str) -> thewiki_core::render::RenderedDoc {
    MarkdownRenderer::new()
        .render(source, &ctx())
        .expect("rendering must succeed for this input")
}

#[test]
fn paragraph_produces_p_tag() {
    let doc = render("Hello, world.");
    assert!(doc.html.contains("<p>"), "html = {}", doc.html);
    assert!(doc.html.contains("Hello,"), "html = {}", doc.html);
    assert!(doc.html.contains("world."), "html = {}", doc.html);
}

#[test]
fn all_heading_levels_round_trip_with_anchors() {
    let source = "# H1\n\n## H2\n\n### H3\n\n#### H4\n\n##### H5\n\n###### H6";
    let doc = render(source);
    assert_eq!(doc.headings.len(), 6);
    for (idx, expected_level) in (1..=6).enumerate() {
        let h = doc.headings.get(idx).expect("heading");
        assert_eq!(h.level, expected_level);
        assert_eq!(h.text, format!("H{expected_level}"));
        assert_eq!(h.anchor, format!("h{expected_level}"));
        let id_attr = format!("id=\"{}\"", h.anchor);
        assert!(
            doc.html.contains(&id_attr),
            "expected {id_attr} in html: {}",
            doc.html
        );
    }
}

#[test]
fn heading_anchors_are_slugified_and_deduplicated() {
    let doc = render("# Hello World\n\n## Hello, World!\n\n### Hello World");
    let anchors: Vec<&str> = doc.headings.iter().map(|h| h.anchor.as_str()).collect();
    assert_eq!(
        anchors,
        vec!["hello-world", "hello-world-2", "hello-world-3"]
    );
    assert!(doc.html.contains("id=\"hello-world\""), "{}", doc.html);
    assert!(doc.html.contains("id=\"hello-world-2\""), "{}", doc.html);
    assert!(doc.html.contains("id=\"hello-world-3\""), "{}", doc.html);
}

#[test]
fn fenced_code_block_carries_language_class() {
    let source = "```rust\nfn main() {}\n```";
    let doc = render(source);
    assert!(
        doc.html.contains("class=\"language-rust\""),
        "html = {}",
        doc.html
    );
    assert!(doc.html.contains("fn main()"), "html = {}", doc.html);
}

#[test]
fn table_renders_with_thead_and_tbody() {
    let source = "| h1 | h2 |\n|----|----|\n| a  | b  |\n";
    let doc = render(source);
    assert!(doc.html.contains("<table>"), "html = {}", doc.html);
    assert!(doc.html.contains("<thead>"), "html = {}", doc.html);
    assert!(doc.html.contains("<tbody>"), "html = {}", doc.html);
    assert!(doc.html.contains("<th>h1</th>"), "html = {}", doc.html);
    assert!(doc.html.contains("<td>a</td>"), "html = {}", doc.html);
}

#[test]
fn external_link_gets_defang_rel() {
    let source = "[example](https://example.com)";
    let doc = render(source);
    assert!(
        doc.html.contains("rel=\"noopener noreferrer nofollow\""),
        "html = {}",
        doc.html
    );
    assert!(
        doc.html.contains("href=\"https://example.com\""),
        "html = {}",
        doc.html
    );
}

#[test]
fn image_is_recorded_and_rendered() {
    let source = "![an apple](https://example.com/apple.png \"red fruit\")";
    let doc = render(source);
    assert!(doc.html.contains("<img"), "html = {}", doc.html);
    assert!(
        doc.html.contains("src=\"https://example.com/apple.png\""),
        "html = {}",
        doc.html
    );
    assert!(doc.html.contains("alt=\"an apple\""), "html = {}", doc.html);
    assert_eq!(doc.images.len(), 1);
    let img = doc.images.first().expect("image");
    assert_eq!(img.url, "https://example.com/apple.png");
    assert_eq!(img.alt.as_deref(), Some("an apple"));
    assert_eq!(img.title.as_deref(), Some("red fruit"));
}

#[test]
fn script_tag_is_stripped_but_inner_text_survives() {
    let source = "Hello <script>alert(1)</script> world";
    let doc = render(source);
    assert!(!doc.html.contains("<script"), "html = {}", doc.html);
    assert!(!doc.html.contains("alert(1)"), "html = {}", doc.html);
    assert!(doc.html.contains("Hello"), "html = {}", doc.html);
    assert!(doc.html.contains("world"), "html = {}", doc.html);
}

#[test]
fn javascript_url_link_is_dropped() {
    let source = "[click](javascript:alert(1))";
    let doc = render(source);
    assert!(!doc.html.contains("javascript:"), "html = {}", doc.html);
    // The link element itself should not have an href that smuggles the
    // dangerous scheme through. `ammonia` keeps the visible text but drops
    // the disallowed-scheme href entirely.
    assert!(
        !doc.html.contains("href=\"javascript"),
        "html = {}",
        doc.html
    );
    assert!(doc.html.contains("click"), "html = {}", doc.html);
}

#[test]
fn strikethrough_renders_as_del() {
    let doc = render("normal ~~struck~~ text");
    // pulldown-cmark emits <del> for strikethrough.
    assert!(doc.html.contains("<del>"), "html = {}", doc.html);
    assert!(doc.html.contains("struck"), "html = {}", doc.html);
}

#[test]
fn task_list_renders_checkboxes() {
    let doc = render("- [ ] todo\n- [x] done\n");
    // pulldown-cmark emits inert checkbox inputs for GFM task lists.
    assert!(
        doc.html.contains("type=\"checkbox\""),
        "html = {}",
        doc.html
    );
    assert!(doc.html.contains("disabled"), "html = {}", doc.html);
    assert!(doc.html.contains("checked"), "html = {}", doc.html);
    assert!(doc.html.contains("todo"), "html = {}", doc.html);
    assert!(doc.html.contains("done"), "html = {}", doc.html);
}

#[test]
fn extract_links_ignores_non_wikilink_syntax() {
    // Plain text and standard `[label](url)` Markdown links never count as
    // wikilinks. The dedicated `[[Target]]` extraction is exercised in the
    // `tests/wikilinks.rs` suite added by #30.
    let r = MarkdownRenderer::new();
    assert!(r.extract_links("plain text").is_empty());
    assert!(
        r.extract_links("[ext](https://example.com)").is_empty(),
        "standard Markdown links must not count as wikilinks"
    );
}

#[test]
fn outbound_links_skip_standard_markdown_links() {
    // Standard `[label](url)` links never count as outbound wikilinks.
    let doc = render("[ext](https://example.com) and a paragraph");
    assert!(doc.outbound_links.is_empty());
}

#[test]
fn empty_input_returns_empty_input_error() {
    let r = MarkdownRenderer::new();
    let err = r
        .render("", &ctx())
        .expect_err("empty input must be rejected");
    assert_eq!(err, RenderError::EmptyInput);
    // Whitespace-only is also empty for our purposes.
    let err = r
        .render("   \n\n  ", &ctx())
        .expect_err("whitespace-only must be rejected");
    assert_eq!(err, RenderError::EmptyInput);
}

#[test]
fn sample_round_trip_from_brief_test_plan() {
    // Doubles as the PR description's example.
    let doc = render("# Hello\n\nWorld");
    assert!(doc.html.contains("<h1 id=\"hello\">Hello</h1>"));
    assert!(doc.html.contains("<p>World</p>"));
}

#[test]
fn default_registry_contains_markdown() {
    let registry = new_registry_with_defaults();
    let r = registry
        .get(ContentFormat::Markdown)
        .expect("markdown registered");
    assert_eq!(r.format(), ContentFormat::Markdown);
}

#[test]
fn registry_can_render_through_trait_object() {
    let registry: RendererRegistry = new_registry_with_defaults();
    let r = registry.get(ContentFormat::Markdown).expect("registered");
    let doc = r.render("# Title\n\nbody", &ctx()).expect("render");
    assert_eq!(doc.headings.len(), 1);
    assert_eq!(doc.headings[0].anchor, "title");
}

#[test]
fn media_image_renders_as_picture_with_srcset() {
    // The src points at `/api/v1/media/<uuid>` — the renderer rewrites
    // the surrounding `<img>` into a `<picture>` carrying the three
    // thumbnail variants (#33).
    let uuid = "019325cd-3a4f-7c1e-9ad2-1234567890ab";
    let src = format!("![cat]({}{uuid})", "/api/v1/media/");
    let doc = render(&src);
    assert!(doc.html.contains("<picture>"), "html = {}", doc.html);
    assert!(
        doc.html
            .contains(&format!("/api/v1/media/{uuid}?size=small 320w")),
        "html = {}",
        doc.html,
    );
    assert!(
        doc.html
            .contains(&format!("/api/v1/media/{uuid}?size=medium 768w")),
        "html = {}",
        doc.html,
    );
    assert!(
        doc.html
            .contains(&format!("/api/v1/media/{uuid}?size=large 1280w")),
        "html = {}",
        doc.html,
    );
    assert!(doc.html.contains("loading=\"lazy\""), "html = {}", doc.html);
    assert!(
        doc.html.contains("decoding=\"async\""),
        "html = {}",
        doc.html
    );
    assert!(doc.html.contains("alt=\"cat\""), "html = {}", doc.html);
}

#[test]
fn external_image_url_stays_a_plain_img() {
    let doc = render("![logo](https://example.com/pic.png)");
    assert!(!doc.html.contains("<picture>"));
    assert!(
        doc.html.contains("src=\"https://example.com/pic.png\""),
        "html = {}",
        doc.html,
    );
}
