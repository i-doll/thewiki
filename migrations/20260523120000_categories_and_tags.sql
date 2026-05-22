-- Categories (DAG) + tags (#29).
--
-- Three tables:
--   * `categories` — one row per category, holding the display label and the
--     optional `parent_id` that turns the set into a directed graph. A page
--     can belong to multiple categories and a category can have multiple
--     parents in principle, but the column models the most common
--     "primary parent" relationship explicitly; arbitrary multi-parent
--     hierarchies are handled by simply registering a page under both
--     branches. Cycle prevention is enforced by the application layer
--     (`CategoryRepository::assign_parent` walks ancestors before writing)
--     because SQLite has no recursive CHECK.
--   * `page_categories` — many-to-many between pages and categories.
--     `ON DELETE CASCADE` on the page side so deleting a page silently drops
--     its memberships; `ON DELETE RESTRICT` on the category side so deleting
--     a category that still has members is a hard error the caller has to
--     resolve.
--   * `page_tags` — flat strings, lowercased at the validation boundary so
--     the unique constraint can match case-insensitively. `ON DELETE CASCADE`
--     on the page side keeps the orphan tag rows from lingering when a page
--     is removed.
--
-- IDs follow the same BLOB(16) UUIDv7 convention as the rest of the schema;
-- timestamps are RFC 3339 TEXT for portability with the SQLite + libsql
-- adapters that share this file.

CREATE TABLE categories (
    id           BLOB    PRIMARY KEY NOT NULL,
    slug         TEXT    NOT NULL UNIQUE,
    display_name TEXT    NOT NULL,
    parent_id    BLOB,
    created_at   TEXT    NOT NULL,
    FOREIGN KEY (parent_id) REFERENCES categories (id) ON DELETE SET NULL
);

CREATE INDEX idx_categories_parent_id ON categories (parent_id);

CREATE TABLE page_categories (
    page_id     BLOB NOT NULL,
    category_id BLOB NOT NULL,
    PRIMARY KEY (page_id, category_id),
    FOREIGN KEY (page_id)     REFERENCES pages      (id) ON DELETE CASCADE,
    FOREIGN KEY (category_id) REFERENCES categories (id) ON DELETE RESTRICT
);

-- Reverse index: "what pages are in this category?". The PK already covers
-- the (page_id, category_id) prefix order; this is the matching index for
-- the inverse query.
CREATE INDEX idx_page_categories_category_id ON page_categories (category_id);

CREATE TABLE page_tags (
    page_id BLOB NOT NULL,
    tag     TEXT NOT NULL,
    PRIMARY KEY (page_id, tag),
    FOREIGN KEY (page_id) REFERENCES pages (id) ON DELETE CASCADE
);

-- Reverse index: "what pages carry this tag?", and the prefix scan that
-- backs the autocomplete endpoint.
CREATE INDEX idx_page_tags_tag ON page_tags (tag);
