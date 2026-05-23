-- Edit approval queue + in-app notifications (#40).
--
-- Two tables:
--   * `pending_revisions` — queued edits awaiting a reviewer. The status
--     column is constrained to a small enum and writes only happen via the
--     dedicated repository, so the application layer is the single point
--     that flips a row from `pending` to a terminal state.
--   * `notifications` — per-user inbox rows. Used today to tell an
--     authenticated editor that their pending edit was approved or
--     rejected; the schema is intentionally generic so future kinds (e.g.
--     `page_protection_changed`) can reuse it without a migration.
--
-- IDs follow the same BLOB(16) UUIDv7 convention as the rest of the schema;
-- timestamps are RFC 3339 TEXT. `payload` is a JSON-encoded TEXT blob —
-- SQLite has no native JSON type but stores the string as-is and the
-- application layer parses it on read.

CREATE TABLE pending_revisions (
    id                 BLOB    PRIMARY KEY NOT NULL,
    page_id            BLOB    NOT NULL,
    parent_revision_id BLOB,
    body               TEXT    NOT NULL,
    author_id          BLOB,
    author_ip          TEXT,
    comment            TEXT    NOT NULL DEFAULT '',
    status             TEXT    NOT NULL CHECK (status IN ('pending', 'approved', 'rejected')),
    reviewer_id        BLOB,
    decided_at         TEXT,
    rejection_reason   TEXT,
    created_at         TEXT    NOT NULL,
    FOREIGN KEY (page_id)            REFERENCES pages     (id) ON DELETE CASCADE,
    FOREIGN KEY (parent_revision_id) REFERENCES revisions (id) ON DELETE SET NULL,
    FOREIGN KEY (author_id)          REFERENCES users     (id) ON DELETE SET NULL,
    FOREIGN KEY (reviewer_id)        REFERENCES users     (id) ON DELETE SET NULL
);

-- Reviewer-facing list query: "what's still in the queue?" — newest first
-- by `(status, created_at)`. Approved / rejected history queries hit the
-- same index by passing a different `status` filter.
CREATE INDEX idx_pending_revisions_status_created_at
    ON pending_revisions (status, created_at);

-- Page-scoped history: "what edits have ever been queued for this page?"
-- is occasionally useful for the page-level moderation log.
CREATE INDEX idx_pending_revisions_page_id
    ON pending_revisions (page_id);

CREATE TABLE notifications (
    id         BLOB PRIMARY KEY NOT NULL,
    user_id    BLOB NOT NULL,
    kind       TEXT NOT NULL,
    payload    TEXT,
    read_at    TEXT,
    created_at TEXT NOT NULL,
    FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE
);

-- "Show me my unread inbox": the index is on `(user_id, read_at)` so the
-- partial-null filter scans contiguously. Without it the inbox bell query
-- would table-scan once a user accumulated history.
CREATE INDEX idx_notifications_user_id_read_at
    ON notifications (user_id, read_at);
