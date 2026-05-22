-- Categories (DAG) + tags — Postgres variant (#29).
--
-- Same shape as the portable SQLite/libsql migration; uses native UUID and
-- TIMESTAMPTZ columns. Cycle prevention is handled by the application
-- layer because Postgres has no `CHECK` that walks a recursive graph
-- without a trigger, and the application can prefetch the would-be parent's
-- ancestor set in a single query before writing.

CREATE TABLE categories (
    id           UUID        PRIMARY KEY NOT NULL,
    slug         TEXT        NOT NULL UNIQUE,
    display_name TEXT        NOT NULL,
    parent_id    UUID,
    created_at   TIMESTAMPTZ NOT NULL,
    FOREIGN KEY (parent_id) REFERENCES categories (id) ON DELETE SET NULL
);

CREATE INDEX idx_categories_parent_id ON categories (parent_id);

CREATE TABLE page_categories (
    page_id     UUID NOT NULL,
    category_id UUID NOT NULL,
    PRIMARY KEY (page_id, category_id),
    FOREIGN KEY (page_id)     REFERENCES pages      (id) ON DELETE CASCADE,
    FOREIGN KEY (category_id) REFERENCES categories (id) ON DELETE RESTRICT
);

CREATE INDEX idx_page_categories_category_id ON page_categories (category_id);

CREATE TABLE page_tags (
    page_id UUID NOT NULL,
    tag     TEXT NOT NULL,
    PRIMARY KEY (page_id, tag),
    FOREIGN KEY (page_id) REFERENCES pages (id) ON DELETE CASCADE
);

CREATE INDEX idx_page_tags_tag ON page_tags (tag);
