-- Media thumbnail variants (#33).
--
-- The upload pipeline generates up to three pre-rendered thumbnails per static
-- image (small / medium / large; see the `crates/api/src/media/thumbnail.rs`
-- size constants). Variants are additive — the original `media` row is the
-- source of truth and is always served when no `?size=` query parameter is
-- supplied.
--
-- One row per `(media_id, variant)` pair. The `data` column carries the
-- variant bytes for the in-DB backend; for the S3 backend it stays NULL and
-- the object is stored under `<bucket>/media/<media_id>/<variant>.<ext>`.
-- The PRIMARY KEY enforces "at most one row per variant per media", which
-- keeps the regen-thumbnails path idempotent (insert-or-replace).

CREATE TABLE media_variants (
    media_id     BLOB    NOT NULL REFERENCES media(id) ON DELETE CASCADE, -- UUIDv7
    variant      TEXT    NOT NULL,                                        -- small / medium / large
    content_type TEXT    NOT NULL,
    byte_size    INTEGER NOT NULL,
    width        INTEGER NOT NULL,
    height       INTEGER NOT NULL,
    data         BLOB,                                                    -- in-DB backend only
    created_at   TEXT    NOT NULL,                                        -- RFC3339
    PRIMARY KEY (media_id, variant)
);

-- Lookup by `media_id` (every read of `?size=` does one) is covered by the
-- composite PK's leading column, so no extra index is needed.
