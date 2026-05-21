//! Markdown renderer.
//!
//! Wraps [`pulldown_cmark`] with the GFM-compatible options chosen in
//! [ADR-0001]: tables, footnotes, strikethrough, task lists, and smart
//! punctuation. `ENABLE_WIKILINKS` is **off** at M0 — `[[WikiLink]]`
//! handling is an M1 issue, and turning it on prematurely would change
//! event-stream semantics in ways the rest of the pipeline does not yet
//! handle.
//!
//! The renderer walks the event stream once, doing three jobs at the same
//! time:
//!
//! 1. Build sanitised HTML via [`pulldown_cmark::html::push_html`] (run
//!    through the crate-local `sanitise::clean` before returning).
//! 2. Extract headings (level, plain text, deduplicated slug anchor) —
//!    rewritten back into the event stream as `id="…"` so the emitted HTML
//!    and the [`thewiki_core::render::Heading`] entries agree.
//! 3. Extract images (`![alt](url 'title')`) into
//!    [`thewiki_core::render::ImageRef`]s.
//!
//! Outbound `[[WikiLink]]`s remain empty for v1, per the brief — M1 lights
//! them up.
//!
//! [ADR-0001]: ../../../docs/adr/0001-markdown-renderer.md

use pulldown_cmark::{CowStr, Event, HeadingLevel, Options, Parser, Tag, TagEnd, html};
use thewiki_core::ContentFormat;
use thewiki_core::render::{
    Heading, ImageRef, RenderContext, RenderError, RenderedDoc, Renderer, WikiLink,
};

use crate::sanitise;
use crate::slug::SlugAllocator;

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
    }
}

impl Renderer for MarkdownRenderer {
    fn format(&self) -> ContentFormat {
        ContentFormat::Markdown
    }

    fn render(&self, source: &str, _ctx: &RenderContext) -> Result<RenderedDoc, RenderError> {
        if source.trim().is_empty() {
            return Err(RenderError::EmptyInput);
        }

        let parser = Parser::new_ext(source, Self::options());

        let mut headings: Vec<Heading> = Vec::new();
        let mut images: Vec<ImageRef> = Vec::new();
        let mut slugs = SlugAllocator::default();

        // Buffer events so we can rewrite Heading tags with stable `id`s
        // before push_html consumes them.
        let mut events: Vec<Event<'_>> = parser.collect();

        // Walk and rewrite. We keep an index into `headings`-in-flight: when
        // we hit Start(Heading) we open a new entry, and from then until the
        // matching End(Heading) every Text/Code event contributes to its
        // plain-text content. Once we have the text we slugify it, allocate
        // the unique anchor, and rewrite the Start tag in-place.
        let mut open_heading: Option<usize> = None;
        // Track Start indices for each open Heading so we can backfill the
        // computed `id` on close.
        let mut open_heading_start_idx: Option<usize> = None;
        let mut heading_text_buf = String::new();

        for i in 0..events.len() {
            match &events[i] {
                Event::Start(Tag::Heading { .. }) => {
                    open_heading_start_idx = Some(i);
                    open_heading = Some(headings.len());
                    heading_text_buf.clear();
                    // Push a placeholder Heading; we backfill on End.
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
                        // Rewrite the Start event with the computed id.
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

        // Second pass: fill in `alt` from the text between Start(Image) and
        // End(Image). We do this separately so the heading walker above
        // stays readable.
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

        let mut raw_html = String::new();
        html::push_html(&mut raw_html, events.into_iter());
        let html = sanitise::clean(&raw_html);

        Ok(RenderedDoc {
            html,
            headings,
            outbound_links: Vec::new(),
            images,
        })
    }

    fn extract_links(&self, _source: &str) -> Vec<WikiLink> {
        // M1 lands `[[WikiLink]]` parsing. v1 keeps the door open with an
        // empty Vec — callers that rely on the trait remain happy.
        Vec::new()
    }
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
