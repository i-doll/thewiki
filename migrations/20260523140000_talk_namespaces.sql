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
-- We avoid the `Talk_` prefix collision case by only inserting rows whose
-- partner doesn't already exist; if an admin manually created a
-- `Talk_Main` namespace before the migration ran, we leave it alone and
-- let the boot-time pairing in `get_or_create_default()` link them up
-- later.
INSERT INTO namespaces (id, slug, display_name, created_at, is_talk, paired_namespace_id)
SELECT
    -- Synthesize a 16-byte BLOB id by xoring the source id with a fixed
    -- 16-byte constant. Deterministic, collision-free w.r.t. the source
    -- row, and fits the `BLOB(16)` shape the rest of the schema expects.
    CAST(
        randomblob(16)
        AS BLOB
    ),
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

-- Close the loop: now that every subject has a talk partner, point each
-- subject row at its paired talk row.
UPDATE namespaces
SET paired_namespace_id = (
    SELECT t.id FROM namespaces t
    WHERE t.slug = 'Talk_' || namespaces.slug
)
WHERE is_talk = 0
  AND paired_namespace_id IS NULL;
