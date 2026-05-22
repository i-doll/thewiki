//! Markdown renderer.
//!
//! Wraps [`pulldown_cmark`] with the GFM-compatible options chosen in
//! [ADR-0001]: tables, footnotes, strikethrough, task lists, smart
//! punctuation, and (now, from #30) wikilinks. `ENABLE_WIKILINKS` was held
//! back at M0 because nothing downstream knew how to resolve `[[Target]]`
//! syntax; this issue lights up the full pipeline (parse → resolve → render
//! with redlink class) and turns the option on.
//!
//! The renderer walks the event stream, doing four jobs at the same time:
//!
//! 1. Build sanitised HTML via [`pulldown_cmark::html::push_html`] (run
//!    through the crate-local `sanitise::clean` before returning).
//! 2. Extract headings (level, plain text, deduplicated slug anchor) —
//!    rewritten back into the event stream as `id="…"` so the emitted HTML
//!    and the [`thewiki_core::render::Heading`] entries agree.
//! 3. Extract images (`![alt](url 'title')`) into
//!    [`thewiki_core::render::ImageRef`]s.
//! 4. Extract outbound `[[WikiLink]]` references and, when a
//!    [`thewiki_core::render::LinkResolver`] is present, rewrite each
//!    wikilink event to point at `/wiki/<namespace>/<target>` (existing) or
//!    `/wiki/<namespace>/<target>/edit?new=1` with a `redlink` class
//!    (missing). When the resolver is `None`, all wikilinks render as
//!    non-redlinks — this keeps test fixtures and renderer-only callers
//!    free of having to stand up storage.
//!
//! [ADR-0001]: ../../../docs/adr/0001-markdown-renderer.md

use pulldown_cmark::{CowStr, Event, HeadingLevel, LinkType, Options, Parser, Tag, TagEnd, html};
use thewiki_core::ContentFormat;
use thewiki_core::render::{
    Heading, ImageRef, RenderContext, RenderError, RenderedDoc, Renderer, WikiLink,
};

use crate::sanitise;
use crate::slug::SlugAllocator;

/// Default namespace label used when [`RenderContext::namespace_slug`] is
/// unset. Tests and the renderer-only call sites get the same default the API
/// layer uses for the `Main` namespace.
const DEFAULT_NAMESPACE_LABEL: &str = "Main";

/// Markdown renderer backed by [`pulldown_cmark`].
///
/// Stateless and cheap to clone; safe to share across threads.
#[derive(Debug, Clone, Default)]
pub struct MarkdownRenderer {
    _priv: (),
}

impl MarkdownRenderer {
    /// Construct a new renderer.
    #[must_use]
    pub const fn new() -> Self {
        Self { _priv: () }
    }

    fn options() -> Options {
        Options::ENABLE_TABLES
            | Options::ENABLE_FOOTNOTES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_SMART_PUNCTUATION
            | Options::ENABLE_WIKILINKS
    }
}

impl Renderer for MarkdownRenderer {
    fn format(&self) -> ContentFormat {
        ContentFormat::Markdown
    }

    fn render(&self, source: &str, ctx: &RenderContext) -> Result<RenderedDoc, RenderError> {
        if source.trim().is_empty() {
            return Err(RenderError::EmptyInput);
        }

        let parser = Parser::new_ext(source, Self::options());

        let mut headings: Vec<Heading> = Vec::new();
        let mut images: Vec<ImageRef> = Vec::new();
        let mut outbound_links: Vec<WikiLink> = Vec::new();
        let mut slugs = SlugAllocator::default();

        // Buffer events so we can rewrite Heading tags with stable `id`s
        // before push_html consumes them.
        let mut events: Vec<Event<'_>> = parser.collect();

        // ---- Pass 1: heading rewriting (computes stable `id` from text).
        let mut open_heading: Option<usize> = None;
        let mut open_heading_start_idx: Option<usize> = None;
        let mut heading_text_buf = String::new();

        for i in 0..events.len() {
            match &events[i] {
                Event::Start(Tag::Heading { .. }) => {
                    open_heading_start_idx = Some(i);
                    open_heading = Some(headings.len());
                    heading_text_buf.clear();
                    headings.push(Heading {
                        level: 0,
                        anchor: String::new(),
                        text: String::new(),
                    });
                }
                Event::End(TagEnd::Heading(level)) => {
                    if let (Some(idx), Some(start_idx)) = (open_heading, open_heading_start_idx) {
                        let anchor = slugs.allocate(&heading_text_buf);
                        let level_num = u8_from_heading_level(*level);
                        if let Some(slot) = headings.get_mut(idx) {
                            slot.level = level_num;
                            slot.text = std::mem::take(&mut heading_text_buf);
                            slot.anchor = anchor.clone();
                        }
                        if let Some(Event::Start(Tag::Heading {
                            level: l,
                            id: _,
                            classes,
                            attrs,
                        })) = events.get(start_idx).cloned()
                        {
                            events[start_idx] = Event::Start(Tag::Heading {
                                level: l,
                                id: Some(CowStr::Boxed(anchor.into_boxed_str())),
                                classes,
                                attrs,
                            });
                        }
                    }
                    open_heading = None;
                    open_heading_start_idx = None;
                }
                Event::Text(t) | Event::Code(t) => {
                    if open_heading.is_some() {
                        heading_text_buf.push_str(t);
                    }
                }
                Event::Start(Tag::Image {
                    dest_url, title, ..
                }) => {
                    images.push(ImageRef {
                        url: dest_url.to_string(),
                        alt: None,
                        title: if title.is_empty() {
                            None
                        } else {
                            Some(title.to_string())
                        },
                    });
                }
                _ => {}
            }
        }

        // ---- Pass 2: backfill image `alt` from the events between
        // Start(Image) and End(Image).
        {
            let mut current_image: Option<(usize, String)> = None;
            let mut idx = 0;
            for event in &events {
                match event {
                    Event::Start(Tag::Image { .. }) => {
                        current_image = Some((idx, String::new()));
                        idx += 1;
                    }
                    Event::End(TagEnd::Image) => {
                        if let Some((image_idx, alt)) = current_image.take()
                            && let Some(img) = images.get_mut(image_idx)
                            && !alt.is_empty()
                        {
                            img.alt = Some(alt);
                        }
                    }
                    Event::Text(t) | Event::Code(t) => {
                        if let Some((_, alt)) = current_image.as_mut() {
                            alt.push_str(t);
                        }
                    }
                    _ => {}
                }
            }
        }

        // ---- Pass 3: rewrite wikilink events.
        //
        // For every `Tag::Link { link_type: WikiLink { has_pothole } }`:
        //   1. Push the `(target, display)` onto `outbound_links`.
        //   2. Ask the resolver whether the target exists. With no resolver
        //      attached, treat every wikilink as resolved (non-red).
        //   3. Replace the Start(Link)/End(Link) pair with Event::Html
        //      fragments — pulldown-cmark's `Tag::Link` has no `classes`
        //      field, so emitting raw HTML is the cleanest way to attach
        //      `class="redlink"`. The inner Text events that carry the
        //      display are left in place.
        let namespace_label = ctx
            .namespace_slug
            .as_deref()
            .unwrap_or(DEFAULT_NAMESPACE_LABEL);
        let resolver = ctx.link_resolver.as_deref();

        // Tracks open Link starts so the matching End event can find its
        // partner — wikilinks and ordinary links share TagEnd::Link, so we
        // can't pattern-match on End alone.
        let mut link_stack: Vec<LinkStackEntry> = Vec::new();

        // We snapshot indices up front so the iteration cost stays O(N).
        let total = events.len();
        for i in 0..total {
            match events[i].clone() {
                Event::Start(Tag::Link {
                    link_type: LinkType::WikiLink { has_pothole },
                    dest_url,
                    ..
                }) => {
                    let target = dest_url.to_string();
                    let display_text = collect_link_display(&events, i);
                    let display = has_pothole.then(|| display_text.clone());
                    outbound_links.push(WikiLink {
                        target: target.clone(),
                        display,
                    });

                    let exists = match resolver {
                        Some(r) => r.resolves(ctx.namespace, &target),
                        None => true,
                    };
                    let opening_html = if exists {
                        format!(
                            "<a href=\"/wiki/{}/{}\">",
                            escape_attr(namespace_label),
                            escape_attr(&encode_path_segment(&target)),
                        )
                    } else {
                        format!(
                            "<a class=\"redlink\" href=\"/wiki/{}/{}/edit?new=1\" title=\"(missing) {}\">",
                            escape_attr(namespace_label),
                            escape_attr(&encode_path_segment(&target)),
                            escape_attr(&target),
                        )
                    };
                    events[i] = Event::Html(CowStr::Boxed(opening_html.into_boxed_str()));
                    link_stack.push(LinkStackEntry::Wikilink);
                }
                Event::Start(Tag::Link { .. }) => {
                    link_stack.push(LinkStackEntry::Other);
                }
                Event::End(TagEnd::Link) => {
                    if let Some(LinkStackEntry::Wikilink) = link_stack.pop() {
                        events[i] = Event::Html(CowStr::Borrowed("</a>"));
                    }
                }
                _ => {}
            }
        }

        let mut raw_html = String::new();
        html::push_html(&mut raw_html, events.into_iter());
        let html = sanitise::clean(&raw_html);

        Ok(RenderedDoc {
            html,
            headings,
            outbound_links,
            images,
        })
    }

    fn extract_links(&self, source: &str) -> Vec<WikiLink> {
        // Walk the event stream collecting Start(WikiLink) entries. The
        // display string is the textual content between Start and End; for
        // bare `[[Foo]]` (no pothole) we leave `display` as `None`, for
        // `[[Foo|bar]]` we record `Some("bar")`.
        let parser = Parser::new_ext(source, Self::options());
        let mut out = Vec::new();
        let mut open: Option<(String, bool, String)> = None;
        for event in parser {
            match event {
                Event::Start(Tag::Link {
                    link_type: LinkType::WikiLink { has_pothole },
                    dest_url,
                    ..
                }) => {
                    open = Some((dest_url.to_string(), has_pothole, String::new()));
                }
                Event::End(TagEnd::Link) => {
                    if let Some((target, has_pothole, display_text)) = open.take() {
                        let display = has_pothole.then_some(display_text);
                        out.push(WikiLink { target, display });
                    }
                }
                Event::Text(t) | Event::Code(t) => {
                    if let Some((_, _, display)) = open.as_mut() {
                        display.push_str(&t);
                    }
                }
                _ => {}
            }
        }
        out
    }
}

/// Stack entry used by [`MarkdownRenderer::render`] to pair Start/End link
/// events. Wikilinks need their End event replaced with raw HTML; ordinary
/// links pass through unchanged.
#[derive(Copy, Clone)]
enum LinkStackEntry {
    Wikilink,
    Other,
}

const fn u8_from_heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Walk forward from `start_idx` (which must point at a Start(Link))
/// collecting the textual content up to the matching End(Link). Wikilinks
/// cannot legally nest, so a simple counter suffices.
fn collect_link_display(events: &[Event<'_>], start_idx: usize) -> String {
    let mut depth: i32 = 0;
    let mut out = String::new();
    for event in events.iter().skip(start_idx) {
        match event {
            Event::Start(Tag::Link { .. }) => {
                depth += 1;
            }
            Event::End(TagEnd::Link) => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            Event::Text(t) | Event::Code(t) => {
                if depth >= 1 {
                    out.push_str(t);
                }
            }
            _ => {}
        }
    }
    out
}

/// Percent-encode a wikilink target as a URL path segment.
///
/// Keeps `/` unescaped so multi-segment targets like `Foo/Sub` retain their
/// hierarchical form. Everything outside the RFC 3986 unreserved set is
/// `%xx`-escaped so spaces and punctuation in targets produce a valid URL.
fn encode_path_segment(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        let unreserved = matches!(
            byte,
            b'A'..=b'Z'
                | b'a'..=b'z'
                | b'0'..=b'9'
                | b'-'
                | b'.'
                | b'_'
                | b'~'
                | b'/'
        );
        if unreserved {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Minimal HTML attribute escaping for the manually-emitted wikilink
/// opening tags. The sanitiser performs its own escaping on the way out, but
/// emitting well-formed HTML keeps the parser happy and avoids surprises.
fn escape_attr(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}
