-- Media uploads (#32) — Postgres variant.
--
-- Same shape as the portable SQLite variant with native types:
--   * `id`, `uploaded_by`: `UUID` (16-byte native).
--   * `content_hash`: `BYTEA` (variable length, but we always store 32 bytes
--     for SHA-256).
--   * `byte_size`: `BIGINT` (a 32-bit signed integer is too narrow for
--     gigabyte-sized files — `INTEGER`'s upper bound of ~2 GiB is
--     uncomfortably close to credible upload sizes).
--   * `created_at`: `TIMESTAMPTZ`.
--   * Blob payload (when the DB backend is configured): `BYTEA`.

CREATE TABLE media (
    id                UUID        PRIMARY KEY NOT NULL,
    content_hash      BYTEA       NOT NULL UNIQUE,
    content_type      TEXT        NOT NULL,
    byte_size         BIGINT      NOT NULL,
    original_filename TEXT,
    uploaded_by       UUID        NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    created_at        TIMESTAMPTZ NOT NULL
);

CREATE TABLE media_blobs (
    media_id UUID  PRIMARY KEY NOT NULL REFERENCES media(id) ON DELETE CASCADE,
    data     BYTEA NOT NULL
);

CREATE INDEX idx_media_content_hash ON media (content_hash);
