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

-- Step 1: promote any pre-existing namespace whose slug matches
-- `Talk_<subject>` (e.g. an operator manually created `Talk_Foo`
-- before this migration shipped) into a real talk namespace —
-- `is_talk = TRUE` and the back-pointer wired up. We do this *before*
-- the next INSERT pass so the existing rogue row is no longer eligible
-- to be treated as a subject below, and so its partner row doesn't get
-- duplicated.
UPDATE namespaces AS t
SET is_talk = TRUE,
    paired_namespace_id = sub.id
FROM namespaces AS sub
WHERE t.is_talk = FALSE
  AND t.slug = 'Talk_' || sub.slug
  AND sub.is_talk = FALSE;

-- Step 2: every remaining subject namespace gets a freshly minted
-- `Talk_<slug>` partner. Postgres has `gen_random_uuid()` (built-in
-- since 13); the Rust app layer keeps minting UUIDv7 for fresh
-- inserts going forward.
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
      SELECT 1
      FROM namespaces t
      WHERE t.slug = 'Talk_' || n.slug
  );

-- Step 3: close the loop so every subject row points at its paired
-- talk row. Match `talk.is_talk = TRUE` so an unrelated non-talk row
-- that happens to be named `Talk_<subject>` can't be paired by
-- mistake.
UPDATE namespaces
SET paired_namespace_id = sub.talk_id
FROM (
    SELECT subj.id AS subj_id, talk.id AS talk_id
    FROM namespaces subj
    JOIN namespaces talk
      ON talk.slug = 'Talk_' || subj.slug
     AND talk.is_talk = TRUE
    WHERE subj.is_talk = FALSE
      AND subj.paired_namespace_id IS NULL
) sub
WHERE namespaces.id = sub.subj_id;
