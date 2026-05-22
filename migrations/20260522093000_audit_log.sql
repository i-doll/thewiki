-- Persistent administrative audit log (#36).
--
-- Rows intentionally avoid foreign keys. Audit history must survive page
-- deletion and user lifecycle changes, so actor/target labels are snapshotted
-- at write time instead of joined on demand.

CREATE TABLE audit_log (
    id             BLOB PRIMARY KEY NOT NULL, -- UUIDv7
    actor_id       BLOB NOT NULL,
    actor_username TEXT NOT NULL,
    action         TEXT NOT NULL,
    target_kind    TEXT NOT NULL,
    target_id      BLOB NOT NULL,
    target_label   TEXT,
    metadata       TEXT NOT NULL,
    created_at     TEXT NOT NULL
) STRICT;

CREATE INDEX idx_audit_log_created_at_id
    ON audit_log(created_at, id);

CREATE INDEX idx_audit_log_actor_created_at
    ON audit_log(actor_username, created_at);

CREATE INDEX idx_audit_log_action_created_at
    ON audit_log(action, created_at);
