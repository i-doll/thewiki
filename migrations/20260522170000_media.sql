-- Media uploads (#32).
--
-- The `media` table tracks one row per stored asset: metadata, content type,
-- content hash (SHA-256, 32 bytes) and the user who uploaded it. The actual
-- blob bytes live in one of two places:
--
--   * `media_blobs` (this migration) when the operator runs with the default
--     in-database backend — simple, no external service required, and
--     useful for small deployments.
--   * An `object_store`-backed bucket (S3 / R2 / MinIO / local fs) when the
--     `storage.backend` config knob selects that path. Those rows are
--     keyed by `media.id` (UUIDv7) in the bucket; only the metadata stays
--     in `media`.
--
-- Deduplication: `media.content_hash` is unique. Two uploads of the same
-- file produce one row + one blob; the second upload returns the existing
-- record. The original filename is metadata only — it does not participate
-- in the dedup key (a screenshot named `paste.png` should dedupe against
-- the same content uploaded as `attachment.png`).

CREATE TABLE media (
    id                BLOB    PRIMARY KEY NOT NULL,           -- UUIDv7
    content_hash      BLOB    NOT NULL UNIQUE,                -- SHA-256, 32 bytes
    content_type      TEXT    NOT NULL,
    byte_size         INTEGER NOT NULL,
    original_filename TEXT,
    uploaded_by       BLOB    NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    created_at        TEXT    NOT NULL                        -- RFC3339
);

-- Backs the `Db` storage backend. One blob per media row, deleted with it
-- via `ON DELETE CASCADE` so the API delete path doesn't need a second
-- statement for housekeeping.
CREATE TABLE media_blobs (
    media_id BLOB PRIMARY KEY NOT NULL REFERENCES media(id) ON DELETE CASCADE,
    data     BLOB NOT NULL
);

-- Lookup by content hash is the hot path for dedup on upload.
CREATE INDEX idx_media_content_hash ON media (content_hash);
