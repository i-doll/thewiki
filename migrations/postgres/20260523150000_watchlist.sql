-- Per-user page watchlist (#46) — Postgres variant.
--
-- Same shape as the portable variant with native `UUID` + `TIMESTAMPTZ`. The
-- composite PK enforces "watch at most once" and both FKs cascade so user /
-- page deletion reaps watch rows automatically.

CREATE TABLE watch (
    user_id    UUID        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    page_id    UUID        NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (user_id, page_id)
);

CREATE INDEX idx_watch_user_created_at ON watch (user_id, created_at DESC);
