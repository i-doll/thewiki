-- Edit approval queue + in-app notifications — Postgres variant (#40).
--
-- Same shape as the portable SQLite/libsql migration; uses native `UUID`
-- and `TIMESTAMPTZ` columns, and a native `JSONB` payload column for the
-- notification table. The `CHECK` constraint on `pending_revisions.status`
-- mirrors the SQLite variant — the application layer always writes one of
-- the three permitted strings.

CREATE TABLE pending_revisions (
    id                 UUID        PRIMARY KEY NOT NULL,
    page_id            UUID        NOT NULL,
    parent_revision_id UUID,
    body               TEXT        NOT NULL,
    author_id          UUID,
    author_ip          TEXT,
    comment            TEXT        NOT NULL DEFAULT '',
    status             TEXT        NOT NULL CHECK (status IN ('pending', 'approved', 'rejected')),
    reviewer_id        UUID,
    decided_at         TIMESTAMPTZ,
    rejection_reason   TEXT,
    created_at         TIMESTAMPTZ NOT NULL,
    FOREIGN KEY (page_id)            REFERENCES pages     (id) ON DELETE CASCADE,
    FOREIGN KEY (parent_revision_id) REFERENCES revisions (id) ON DELETE SET NULL,
    FOREIGN KEY (author_id)          REFERENCES users     (id) ON DELETE SET NULL,
    FOREIGN KEY (reviewer_id)        REFERENCES users     (id) ON DELETE SET NULL
);

CREATE INDEX idx_pending_revisions_status_created_at
    ON pending_revisions (status, created_at);

CREATE INDEX idx_pending_revisions_page_id
    ON pending_revisions (page_id);

CREATE TABLE notifications (
    id         UUID        PRIMARY KEY NOT NULL,
    user_id    UUID        NOT NULL,
    kind       TEXT        NOT NULL,
    payload    JSONB,
    read_at    TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL,
    FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE
);

CREATE INDEX idx_notifications_user_id_read_at
    ON notifications (user_id, read_at);
