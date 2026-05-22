-- Media thumbnail variants (#33) — Postgres variant.
--
-- Same shape as the portable SQLite migration with native types:
--   * `media_id`: `UUID`.
--   * `byte_size`/`width`/`height`: `INTEGER` (variants are bounded; the
--     1280px ceiling on the `large` size means even an uncompressed RGBA
--     buffer sits comfortably under `i32::MAX`).
--   * `data`: `BYTEA` and nullable, only populated by the in-DB backend.
--   * `created_at`: `TIMESTAMPTZ`.

CREATE TABLE media_variants (
    media_id     UUID        NOT NULL REFERENCES media(id) ON DELETE CASCADE,
    variant      TEXT        NOT NULL,
    content_type TEXT        NOT NULL,
    byte_size    INTEGER     NOT NULL,
    width        INTEGER     NOT NULL,
    height       INTEGER     NOT NULL,
    data         BYTEA,
    created_at   TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (media_id, variant)
);
