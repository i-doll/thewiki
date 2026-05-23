-- Talk pages per article (#43).
--
-- Extends `namespaces` with the two columns that make MediaWiki-style
-- discussion namespaces a first-class thing:
--
--   * `is_talk`              — flag distinguishing the subject namespace
--                              (`Main`, `Help`, …) from its discussion side
--                              (`Talk_Main`, `Talk_Help`, …). Search uses
--                              this to halve the default boost so talk
--                              pages rank below subject pages by default.
--   * `paired_namespace_id`  — bidirectional FK. For a subject namespace
--                              this points at its `Talk_*` companion; for a
--                              talk namespace it points back at the subject.
--
-- Pairing is exposed through a `UNIQUE` constraint so a single namespace
-- can't be the talk side of two different subjects. The FK is left
-- nullable on purpose: legacy namespaces created before #43 sit with
-- `NULL` until the backfill (or a subsequent admin action) pairs them up.
-- `ON DELETE SET NULL` keeps a stale pointer from pinning the partner row
-- in place when an operator drops a namespace.
--
-- The forward reference (`paired_namespace_id` -> `namespaces(id)`) is
-- self-referential within the same table; SQLite tolerates the cycle
-- inside a single transaction. The Postgres variant marks it
-- `DEFERRABLE INITIALLY DEFERRED` for the same reason
-- `pages.current_revision_id` is.

ALTER TABLE namespaces
    ADD COLUMN is_talk BOOLEAN NOT NULL DEFAULT 0;

ALTER TABLE namespaces
    ADD COLUMN paired_namespace_id BLOB
        REFERENCES namespaces (id) ON DELETE SET NULL;

CREATE UNIQUE INDEX idx_namespaces_paired_namespace_id
    ON namespaces (paired_namespace_id)
    WHERE paired_namespace_id IS NOT NULL;

-- Backfill: every existing subject namespace gets a `Talk_<slug>` partner,
-- and the FK is set on both sides so the pairing is symmetric. We use a
-- deterministic-but-unique blob for the new IDs by hashing the source row's
-- id with a constant prefix; UUIDv7 generation lives in the application
-- layer so we can't mint one in pure SQL. The bytes we pick here are
-- prefixed with `0xff` (an invalid UUIDv7 version nibble) so the boot path
-- can replace them with proper UUIDs on first run if it cares — but for
-- the immediate need of the schema migration, any unique 16-byte BLOB is
-- enough.
--
-- Step 1: promote any pre-existing namespace whose slug matches
-- `Talk_<subject>` (e.g. an operator manually created `Talk_Foo` before
-- this migration shipped) into a real talk namespace — `is_talk = 1`
-- and the back-pointer wired up. We do this *before* the next INSERT
-- pass so the existing rogue row is no longer eligible to be treated
-- as a subject in step 2, and the partner doesn't get duplicated.
UPDATE namespaces AS t
SET is_talk = 1,
    paired_namespace_id = (
        SELECT s.id
        FROM namespaces AS s
        WHERE t.slug = 'Talk_' || s.slug
          AND s.is_talk = 0
    )
WHERE t.is_talk = 0
  AND EXISTS (
      SELECT 1
      FROM namespaces AS s
      WHERE t.slug = 'Talk_' || s.slug
        AND s.is_talk = 0
  );

-- Step 2: every remaining subject namespace (is_talk = 0) that has no
-- `Talk_*` partner yet gets one freshly minted. We use `randomblob(16)`
-- to mint the id; UUIDv7 generation lives in the application layer so
-- we can't mint one in pure SQL.
INSERT INTO namespaces (id, slug, display_name, created_at, is_talk, paired_namespace_id)
SELECT
    CAST(randomblob(16) AS BLOB),
    'Talk_' || n.slug,
    'Talk: ' || n.display_name,
    n.created_at,
    1,
    n.id
FROM namespaces n
WHERE n.is_talk = 0
  AND NOT EXISTS (
      SELECT 1 FROM namespaces t WHERE t.slug = 'Talk_' || n.slug
  );

-- Step 3: close the loop so every subject row points at its paired
-- talk row. We match `t.is_talk = 1` so an unrelated non-talk row that
-- happens to be named `Talk_<subject>` can't be paired by mistake.
UPDATE namespaces
SET paired_namespace_id = (
    SELECT t.id FROM namespaces t
    WHERE t.slug = 'Talk_' || namespaces.slug
      AND t.is_talk = 1
)
WHERE is_talk = 0
  AND paired_namespace_id IS NULL;
