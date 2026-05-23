-- IP and URL blocklists (#42) — Postgres variant.
--
-- Native `UUID` + `TIMESTAMPTZ`; same shape as the SQLite variant otherwise.
-- See `migrations/20260523130000_blocklists.sql` for the design notes.

CREATE TABLE ip_blocklist (
    id         UUID        PRIMARY KEY NOT NULL,
    cidr       TEXT        NOT NULL UNIQUE,
    reason     TEXT        NOT NULL DEFAULT '',
    created_by UUID        NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    created_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE url_blocklist (
    id         UUID        PRIMARY KEY NOT NULL,
    pattern    TEXT        NOT NULL UNIQUE,
    reason     TEXT        NOT NULL DEFAULT '',
    created_by UUID        NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    created_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX idx_ip_blocklist_created_at  ON ip_blocklist  (created_at);
CREATE INDEX idx_url_blocklist_created_at ON url_blocklist (created_at);
