-- Outbound wikilink graph (#30).
--
-- One row per `[[Target]]` reference inside the *current* revision of a
-- source page. Populated by the API layer on page create / update by
-- running `MarkdownRenderer::extract_links` over the new body and replacing
-- the source's existing rows with the fresh set. Backlinks become a simple
-- index scan instead of a `LIKE`-over-bodies pass at query time.
--
-- We snapshot the target's `(namespace_slug, page_slug)` rather than a page
-- UUID so dangling references survive page deletion and so a wikilink to a
-- not-yet-created page (redlinks) is representable from the moment the
-- referring page is saved. The pair is what the renderer's resolver also
-- uses to decide red vs. blue, so the column shape mirrors the runtime
-- decision.

CREATE TABLE page_links (
    source_page_id       BLOB NOT NULL,
    target_namespace_slug TEXT NOT NULL,
    target_page_slug      TEXT NOT NULL,
    PRIMARY KEY (source_page_id, target_namespace_slug, target_page_slug),
    FOREIGN KEY (source_page_id) REFERENCES pages (id) ON DELETE CASCADE
);

-- Backlinks lookup: "who links *to* (ns, slug)?"
CREATE INDEX idx_page_links_target
    ON page_links (target_namespace_slug, target_page_slug);
