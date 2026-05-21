-- thewiki initial schema (M0).
--
-- This migration is intentionally portable across SQLite and Postgres so a
-- single file covers both backends until something forces divergence. When
-- that happens, the dialect-specific variant lives under
-- `migrations/postgres/<name>.sql` (see `migrations/README.md`).
--
-- Conventions:
--   * IDs are UUIDv7 stored as 16-byte BLOBs.
--   * Timestamps are RFC3339 strings in TEXT columns.
--   * `permissions` is a u32 bitflag packed into an INTEGER, matching
--     `thewiki_core::Permissions`.
--   * `pages.current_revision_id` is a forward reference to `revisions(id)`;
--     SQLite tolerates the cycle (rows are inserted with the FK null first,
--     then updated). The Postgres adapter (M1) will mark it DEFERRABLE in its
--     own variant file.

CREATE TABLE namespaces (
    id           BLOB    PRIMARY KEY NOT NULL,
    slug         TEXT    NOT NULL UNIQUE,
    display_name TEXT    NOT NULL,
    created_at   TEXT    NOT NULL
);

CREATE TABLE users (
    id            BLOB PRIMARY KEY NOT NULL,
    username      TEXT NOT NULL UNIQUE,
    email         TEXT,
    display_name  TEXT,
    password_hash TEXT,
    created_at    TEXT NOT NULL,
    last_login_at TEXT
);

CREATE TABLE roles (
    id           BLOB    PRIMARY KEY NOT NULL,
    name         TEXT    NOT NULL UNIQUE,
    display_name TEXT    NOT NULL,
    permissions  INTEGER NOT NULL
);

CREATE TABLE user_roles (
    user_id BLOB NOT NULL,
    role_id BLOB NOT NULL,
    PRIMARY KEY (user_id, role_id),
    FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE,
    FOREIGN KEY (role_id) REFERENCES roles (id) ON DELETE CASCADE
);

CREATE TABLE pages (
    id                  BLOB PRIMARY KEY NOT NULL,
    namespace_id        BLOB NOT NULL,
    slug                TEXT NOT NULL,
    title               TEXT NOT NULL,
    current_revision_id BLOB,
    content_format      TEXT NOT NULL,
    protection_level    TEXT NOT NULL,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    UNIQUE (namespace_id, slug),
    FOREIGN KEY (namespace_id)        REFERENCES namespaces (id) ON DELETE RESTRICT,
    FOREIGN KEY (current_revision_id) REFERENCES revisions  (id) ON DELETE SET NULL
);

CREATE TABLE revisions (
    id           BLOB PRIMARY KEY NOT NULL,
    page_id      BLOB NOT NULL,
    parent_id    BLOB,
    author_id    BLOB NOT NULL,
    body         TEXT NOT NULL,
    edit_summary TEXT,
    created_at   TEXT NOT NULL,
    FOREIGN KEY (page_id)   REFERENCES pages (id) ON DELETE CASCADE,
    FOREIGN KEY (parent_id) REFERENCES revisions (id) ON DELETE SET NULL,
    FOREIGN KEY (author_id) REFERENCES users (id) ON DELETE RESTRICT
);

-- `(namespace_id, slug)` and `users(username)` are already covered by UNIQUE
-- constraints above. The history lookup is the one that benefits from an
-- explicit index.
CREATE INDEX idx_revisions_page_id_created_at ON revisions (page_id, created_at);
