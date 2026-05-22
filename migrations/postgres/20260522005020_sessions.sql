-- Authentication sessions (#13) — Postgres variant.
--
-- Same shape as the portable SQLite variant, with native `UUID` and
-- `TIMESTAMPTZ` columns instead of BLOB + RFC3339 TEXT.

CREATE TABLE sessions (
    id           UUID        PRIMARY KEY NOT NULL,
    user_id      UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at   TIMESTAMPTZ NOT NULL,
    expires_at   TIMESTAMPTZ NOT NULL,
    last_seen_at TIMESTAMPTZ NOT NULL,
    user_agent   TEXT,
    ip_address   TEXT
);

CREATE INDEX idx_sessions_user_id    ON sessions(user_id);
CREATE INDEX idx_sessions_expires_at ON sessions(expires_at);
