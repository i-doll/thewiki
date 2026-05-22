-- Persistent administrative audit log (#36) — Postgres variant.
--
-- Same shape as the portable variant, with native `UUID` + `TIMESTAMPTZ` and
-- `JSONB` for the metadata column (so operators can probe metadata server-side
-- if a follow-up admin tool needs it).
--
-- Rows intentionally avoid foreign keys. Audit history must survive page
-- deletion and user lifecycle changes, so actor/target labels are snapshotted
-- at write time instead of joined on demand.

CREATE TABLE audit_log (
    id             UUID        PRIMARY KEY NOT NULL,
    actor_id       UUID        NOT NULL,
    actor_username TEXT        NOT NULL,
    action         TEXT        NOT NULL,
    target_kind    TEXT        NOT NULL,
    target_id      UUID        NOT NULL,
    target_label   TEXT,
    metadata       JSONB       NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL
);

CREATE INDEX idx_audit_log_created_at_id
    ON audit_log(created_at, id);

CREATE INDEX idx_audit_log_actor_created_at
    ON audit_log(actor_username, created_at);

CREATE INDEX idx_audit_log_action_created_at
    ON audit_log(action, created_at);
