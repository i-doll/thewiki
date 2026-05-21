-- Authentication sessions (#13).
--
-- A row per active login. `id` is a UUIDv7 (16-byte BLOB) used as the opaque
-- cookie value handed to clients — knowing it is sufficient to act as the
-- referenced user, so the column doubles as the bearer token.
--
-- TTL is enforced application-side by comparing `expires_at` against "now"
-- on every lookup; expired rows are pruned via `SessionRepository::prune_expired`.
-- The `idx_sessions_expires_at` index keeps that scan cheap.
--
-- `user_agent` and `ip_address` are captured at issuance for visibility in
-- the future admin UI; they are not consulted for auth decisions.

CREATE TABLE sessions (
    id           BLOB PRIMARY KEY NOT NULL,                          -- UUIDv7
    user_id      BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at   TEXT NOT NULL,                                      -- RFC3339
    expires_at   TEXT NOT NULL,                                      -- RFC3339
    last_seen_at TEXT NOT NULL,                                      -- RFC3339, bumped per request
    user_agent   TEXT,                                               -- optional, for admin UI
    ip_address   TEXT                                                -- optional, for admin UI
) STRICT;

CREATE INDEX idx_sessions_user_id    ON sessions(user_id);
CREATE INDEX idx_sessions_expires_at ON sessions(expires_at);
