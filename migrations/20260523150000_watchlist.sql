-- Per-user page watchlist (#46).
--
-- One row per (user, page) pair: when present, the user wants notifications
-- (Atom feed entry, future inbox banner) for every revision on that page.
-- The composite primary key already enforces "watch at most once"; both
-- foreign keys cascade-delete so removing a user or a page reaps their
-- watch rows automatically.
--
-- A second index covers the inverse query — "what pages does user X watch,
-- newest watch first" — which powers the Atom feed and the `/watchlist`
-- SPA page.

CREATE TABLE watch (
    user_id    BLOB NOT NULL,
    page_id    BLOB NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (user_id, page_id),
    FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE,
    FOREIGN KEY (page_id) REFERENCES pages (id) ON DELETE CASCADE
) STRICT;

CREATE INDEX idx_watch_user_created_at ON watch (user_id, created_at DESC);
