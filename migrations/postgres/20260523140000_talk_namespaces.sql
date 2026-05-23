-- Talk pages per article — Postgres variant (#43).
--
-- Same shape as the portable SQLite/libsql migration; uses native types and
-- marks the self-referential FK `DEFERRABLE INITIALLY DEFERRED` so the
-- backfill block can flip both sides of a pairing in a single transaction
-- without the constraint firing mid-statement.
--
-- See the SQLite variant for the design rationale.

ALTER TABLE namespaces
    ADD COLUMN is_talk BOOLEAN NOT NULL DEFAULT FALSE;

ALTER TABLE namespaces
    ADD COLUMN paired_namespace_id UUID;

ALTER TABLE namespaces
    ADD CONSTRAINT namespaces_paired_namespace_id_fkey
    FOREIGN KEY (paired_namespace_id) REFERENCES namespaces (id)
    ON DELETE SET NULL
    DEFERRABLE INITIALLY DEFERRED;

CREATE UNIQUE INDEX idx_namespaces_paired_namespace_id
    ON namespaces (paired_namespace_id)
    WHERE paired_namespace_id IS NOT NULL;

-- Backfill: every existing subject namespace gets a `Talk_<slug>` partner.
-- Postgres has `gen_random_uuid()` (since 13, via the built-in
-- `pgcrypto`-free `gen_random_uuid()` shipped in pg_catalog from 13+); we
-- use it to mint the partner ids. The Rust app layer keeps minting
-- UUIDv7 for fresh inserts going forward.
INSERT INTO namespaces (id, slug, display_name, created_at, is_talk, paired_namespace_id)
SELECT
    gen_random_uuid(),
    'Talk_' || n.slug,
    'Talk: ' || n.display_name,
    n.created_at,
    TRUE,
    n.id
FROM namespaces n
WHERE n.is_talk = FALSE
  AND NOT EXISTS (
      SELECT 1 FROM namespaces t WHERE t.slug = 'Talk_' || n.slug
  );

-- Close the loop: every subject row now points at its paired talk row.
UPDATE namespaces
SET paired_namespace_id = sub.talk_id
FROM (
    SELECT subj.id AS subj_id, talk.id AS talk_id
    FROM namespaces subj
    JOIN namespaces talk ON talk.slug = 'Talk_' || subj.slug
    WHERE subj.is_talk = FALSE
      AND subj.paired_namespace_id IS NULL
) sub
WHERE namespaces.id = sub.subj_id;
