-- thewiki initial schema — Postgres variant.
--
-- The portable variant (`migrations/00000000000000_init.sql`) targets SQLite,
-- where IDs are BLOB(16) and timestamps are RFC3339 TEXT. Postgres has native
-- types for both, so this file uses them directly:
--
--   * IDs are stored as `UUID` (16-byte native, indexed efficiently).
--   * Timestamps are stored as `TIMESTAMPTZ` (microsecond precision, TZ-aware).
--   * `roles.permissions` is `BIGINT` (Postgres lacks an unsigned 32-bit type,
--     and `INTEGER` is signed 32-bit — too narrow if the bitflag set grows past
--     bit 30, so we widen to 64-bit signed up front).
--   * `pages.current_revision_id` references `revisions(id)`, which in turn
--     references `pages(id)`. The cycle is fine inside one transaction with
--     `DEFERRABLE INITIALLY DEFERRED`; without it, the FK is checked at every
--     statement and the seed sequence would have to dance around the cycle.

CREATE TABLE namespaces (
    id           UUID        PRIMARY KEY NOT NULL,
    slug         TEXT        NOT NULL UNIQUE,
    display_name TEXT        NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL
);

CREATE TABLE users (
    id            UUID        PRIMARY KEY NOT NULL,
    username      TEXT        NOT NULL UNIQUE,
    email         TEXT,
    display_name  TEXT,
    password_hash TEXT,
    created_at    TIMESTAMPTZ NOT NULL,
    last_login_at TIMESTAMPTZ
);

CREATE TABLE roles (
    id           UUID   PRIMARY KEY NOT NULL,
    name         TEXT   NOT NULL UNIQUE,
    display_name TEXT   NOT NULL,
    permissions  BIGINT NOT NULL
);

CREATE TABLE user_roles (
    user_id UUID NOT NULL,
    role_id UUID NOT NULL,
    PRIMARY KEY (user_id, role_id),
    FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE,
    FOREIGN KEY (role_id) REFERENCES roles (id) ON DELETE CASCADE
);

CREATE TABLE pages (
    id                  UUID        PRIMARY KEY NOT NULL,
    namespace_id        UUID        NOT NULL,
    slug                TEXT        NOT NULL,
    title               TEXT        NOT NULL,
    current_revision_id UUID,
    content_format      TEXT        NOT NULL,
    protection_level    TEXT        NOT NULL,
    created_at          TIMESTAMPTZ NOT NULL,
    updated_at          TIMESTAMPTZ NOT NULL,
    UNIQUE (namespace_id, slug),
    FOREIGN KEY (namespace_id) REFERENCES namespaces (id) ON DELETE RESTRICT
);

CREATE TABLE revisions (
    id           UUID        PRIMARY KEY NOT NULL,
    page_id      UUID        NOT NULL,
    parent_id    UUID,
    author_id    UUID        NOT NULL,
    body         TEXT        NOT NULL,
    edit_summary TEXT,
    created_at   TIMESTAMPTZ NOT NULL,
    FOREIGN KEY (page_id)   REFERENCES pages (id) ON DELETE CASCADE,
    FOREIGN KEY (parent_id) REFERENCES revisions (id) ON DELETE SET NULL,
    FOREIGN KEY (author_id) REFERENCES users (id) ON DELETE RESTRICT
);

-- The forward FK from pages.current_revision_id -> revisions(id) is added
-- last (so `revisions` exists at this point) and marked
-- `DEFERRABLE INITIALLY DEFERRED` so a single transaction can insert a page
-- with the FK pointing at a not-yet-inserted revision and then append the
-- revision row before COMMIT. SQLite tolerates this implicitly; Postgres
-- needs the constraint to be deferrable.
ALTER TABLE pages
    ADD CONSTRAINT pages_current_revision_id_fkey
    FOREIGN KEY (current_revision_id) REFERENCES revisions (id)
    ON DELETE SET NULL
    DEFERRABLE INITIALLY DEFERRED;

-- `(namespace_id, slug)` and `users(username)` are already covered by UNIQUE
-- constraints above. The history lookup is the one that benefits from an
-- explicit index.
CREATE INDEX idx_revisions_page_id_created_at ON revisions (page_id, created_at);
