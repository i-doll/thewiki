//! HTML sanitisation.
//!
//! Every renderer in this crate funnels its raw HTML through a single
//! [`ammonia::Builder`] configured here, so the allowlist policy lives in
//! one place and downstream consumers do not need to scrub again.
//!
//! Policy (kept conservative on purpose):
//!
//! - **Tags**: the usual semantic set plus tables, definition lists,
//!   figures, super/subscript, and the `s`/`del`/`ins` strikethrough crew.
//!   No `<style>`, no `<iframe>`, no `<script>`.
//! - **Attributes**:
//!   - `a` — `href`, `title`, `class` (`rel` is force-set by `link_rel`).
//!     The `class` allowance is what makes the `redlink` styling for missing
//!     wikilinks survive sanitisation (#30).
//!   - `img` — `src`, `alt`, `title`, `width`, `height`
//!   - `code` — `class` (language hints from fenced code blocks)
//!   - `td`/`th` — `align` (table-column alignment)
//!   - Every element — `id` (heading anchors)
//! - **Link schemes**: `http`, `https`, `mailto`. Plus relative URLs and
//!   `#`-fragment anchors via `url_relative = PassThrough`.
//! - **Defanging**: every `<a>` gets `rel="noopener noreferrer nofollow"`
//!   added so an attacker who slips a link past the writer cannot pivot
//!   into a tabnabbing attack.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use ammonia::Builder;

/// Build (and memoise) the [`ammonia::Builder`] used to sanitise rendered
/// HTML.
pub(crate) fn builder() -> &'static Builder<'static> {
    static BUILDER: OnceLock<Builder<'static>> = OnceLock::new();
    BUILDER.get_or_init(build)
}

fn build() -> Builder<'static> {
    let tags: HashSet<&'static str> = [
        "a",
        "blockquote",
        "br",
        "code",
        "del",
        "div",
        "dl",
        "dd",
        "dt",
        "em",
        "figcaption",
        "figure",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "hr",
        "img",
        "input",
        "ins",
        "li",
        "ol",
        "p",
        // `<picture>` + `<source>` carry the responsive variants emitted by
        // the Markdown renderer when a `![]()` points at `/api/v1/media/<id>`
        // (#33). Limiting `<source>` attributes to `srcset` / `sizes` /
        // `type` keeps the surface tight — no `media=`-driven UA detection,
        // no `srcdoc` injection.
        "picture",
        "source",
        "pre",
        "s",
        "span",
        "strong",
        "sub",
        "sup",
        "table",
        "tbody",
        "td",
        "th",
        "thead",
        "tr",
        "ul",
    ]
    .into_iter()
    .collect();

    let mut tag_attributes: HashMap<&'static str, HashSet<&'static str>> = HashMap::new();
    // NB: do **not** allow `rel` here — it's force-applied by `link_rel`
    // below, and ammonia panics if both routes try to manage it.
    // We also do not allow `target` — it's unnecessary for a wiki and lets
    // authors break out of the tab without us getting a say (rel=noopener
    // would still apply, but the UX surprise is the actual issue).
    // `class` is permitted so the `redlink` styling applied to missing
    // wikilinks (#30) survives the sanitiser. Aside from that, the wiki has
    // no use for `<a>` classes — authors writing raw HTML get the class
    // stripped on every other element by the generic allowlist below.
    tag_attributes.insert("a", ["href", "title", "class"].into_iter().collect());
    tag_attributes.insert(
        "img",
        [
            "src", "alt", "title", "width", "height", "loading", "decoding",
        ]
        .into_iter()
        .collect(),
    );
    tag_attributes.insert("source", ["srcset", "sizes", "type"].into_iter().collect());
    tag_attributes.insert("code", ["class"].into_iter().collect());
    tag_attributes.insert("td", ["align"].into_iter().collect());
    tag_attributes.insert("th", ["align"].into_iter().collect());
    // Inline template-error spans (#45) carry `class="template-error"` plus
    // `data-line` / `data-col` pinning the diagnostic back at the source.
    // Letting these through keeps the editor able to surface the error
    // location after sanitisation. No other attributes are allowed on
    // `<span>` — authors writing raw HTML still get stripped.
    tag_attributes.insert(
        "span",
        ["class", "data-line", "data-col"].into_iter().collect(),
    );
    // `pulldown-cmark` emits `<input type="checkbox" disabled checked?>` for
    // GFM task lists. Letting these specific attributes through preserves
    // rendered checkboxes; the `disabled` flag means they remain inert.
    tag_attributes.insert(
        "input",
        ["type", "checked", "disabled"].into_iter().collect(),
    );

    let generic_attributes: HashSet<&'static str> = ["id"].into_iter().collect();

    let url_schemes: HashSet<&'static str> = ["http", "https", "mailto"].into_iter().collect();

    let mut b = Builder::default();
    b.tags(tags)
        .tag_attributes(tag_attributes)
        .generic_attributes(generic_attributes)
        .url_schemes(url_schemes)
        .url_relative(ammonia::UrlRelative::PassThrough)
        .link_rel(Some("noopener noreferrer nofollow"));
    b
}

/// Sanitise `raw` with the renderer's allowlist policy.
pub(crate) fn clean(raw: &str) -> String {
    builder().clean(raw).to_string()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "ergonomic tests")]
mod tests {
    use super::*;

    #[test]
    fn strips_script_tag() {
        let out = clean("<p>hi<script>alert(1)</script></p>");
        assert!(!out.contains("<script>"));
        assert!(out.contains("hi"));
    }

    #[test]
    fn drops_javascript_href() {
        let out = clean("<a href=\"javascript:alert(1)\">x</a>");
        assert!(!out.contains("javascript:"));
    }

    #[test]
    fn adds_defang_rel() {
        let out = clean("<a href=\"https://example.com\">x</a>");
        assert!(out.contains("rel=\"noopener noreferrer nofollow\""));
    }

    #[test]
    fn preserves_heading_id() {
        let out = clean("<h2 id=\"hello\">Hello</h2>");
        assert!(out.contains("id=\"hello\""));
    }

    #[test]
    fn preserves_code_language_class() {
        let out = clean("<pre><code class=\"language-rust\">fn x() {}</code></pre>");
        assert!(out.contains("class=\"language-rust\""));
    }
}
