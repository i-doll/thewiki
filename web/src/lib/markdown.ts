//! Client-side Markdown rendering.
//!
//! TODO(#?): the server already wraps `MarkdownRenderer` (see
//! `crates/render/src/markdown.rs`) but the API returns the raw Markdown
//! source today. Once `GET /api/v1/pages/{slug}` ships pre-rendered &
//! sanitised HTML, drop this module and let the SPA consume the HTML
//! directly — no Markdown parser or sanitiser needs to live in the bundle.
//!
//! Until then we render with `marked` (GFM defaults) and run the output
//! through `DOMPurify` as defence-in-depth so XSS in user content can't
//! escape into the page.

import DOMPurify from "dompurify";
import { marked } from "marked";

marked.setOptions({
	gfm: true,
	breaks: false,
});

/**
 * Render a Markdown string to a sanitised HTML string suitable for
 * `dangerouslySetInnerHTML`. Returns an empty string for empty input so
 * callers can render conditionally without an extra guard.
 */
export function renderMarkdown(source: string): string {
	if (source.trim().length === 0) {
		return "";
	}
	// `marked.parse` is synchronous by default; the overload that returns
	// `Promise<string>` only fires when `async: true` is set. We never enable
	// async mode, so the `as string` cast is safe.
	const html = marked.parse(source) as string;
	return DOMPurify.sanitize(html, { USE_PROFILES: { html: true } });
}
