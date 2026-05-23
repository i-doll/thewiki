-- IP and URL blocklists (#42).
--
-- Two narrow tables. Both are operator-curated and small (hundreds of rows
-- at most), so we keep the schema simple: a UUIDv7 PK, the pattern column
-- with a unique constraint to prevent silent duplicates, a free-form reason
-- (defaults to empty so the column is never NULL), the creator user id (FK
-- to `users` so the row chases account deletion), and an RFC3339 timestamp.
--
-- The runtime loads both tables into memory at boot and reloads on every
-- mutation; queries during request handling never touch SQLite. The DB
-- exists purely so the operator-curated set survives restarts.

CREATE TABLE ip_blocklist (
    id         BLOB PRIMARY KEY NOT NULL,           -- UUIDv7
    cidr       TEXT NOT NULL UNIQUE,                -- e.g. `203.0.113.0/24`
    reason     TEXT NOT NULL DEFAULT '',
    created_by BLOB NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    created_at TEXT NOT NULL                        -- RFC3339
);

CREATE TABLE url_blocklist (
    id         BLOB PRIMARY KEY NOT NULL,           -- UUIDv7
    pattern    TEXT NOT NULL UNIQUE,                -- Rust `regex` syntax
    reason     TEXT NOT NULL DEFAULT '',
    created_by BLOB NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    created_at TEXT NOT NULL                        -- RFC3339
);

-- Listing pages newest-first is the only hot read path (admin UI),
-- so a single index on `created_at` is enough for the v1 surface.
CREATE INDEX idx_ip_blocklist_created_at  ON ip_blocklist  (created_at);
CREATE INDEX idx_url_blocklist_created_at ON url_blocklist (created_at);
