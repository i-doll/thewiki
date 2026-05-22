# Migrations

This directory holds the SQL migrations for `thewiki`'s metadata database.
Migrations are applied by `sqlx`'s migrator, driven from `xtask`:

```sh
cargo run -p xtask -- migrate run --database-url sqlite::memory:
```

`DATABASE_URL` can be provided via the environment or a `.env` file at the
repo root; the `--database-url` flag overrides both.

## Filename convention

```
YYYYMMDDHHMMSS_<slug>.sql
```

This matches the `sqlx-cli` convention so a stock `sqlx migrate add` (or
`cargo run -p xtask -- migrate add <slug>`) drops files in the right place
without bespoke tooling.

The very first migration uses the **all-zeros prefix**
`00000000000000_init.sql` on purpose: it sorts before every real timestamp
and makes the inaugural migration obvious in directory listings. This is
intentional, not a bug. Subsequent migrations use real UTC timestamps.

## Forward-only

`thewiki` migrations are **forward-only**. There are no `down.sql` files and
no rollbacks. If a migration ships a mistake, write a new migration with a
fresh timestamp that fixes it forward. This keeps schema history a linear
log and avoids the "did production really run the down script?" class of
incident.

## Dialect-specific variants

The default location (`migrations/<name>.sql`) holds **portable SQL** that
works on every backend `thewiki` supports. The SQLite adapter loads from
that directory directly.

Postgres-flavoured copies live under `migrations/postgres/<name>.sql` —
native `UUID`, `TIMESTAMPTZ`, `JSONB`, and the
`pages.current_revision_id` FK marked `DEFERRABLE INITIALLY DEFERRED` so a
single transaction can insert a page that forward-references a revision
not yet appended. The Postgres adapter (`storage::postgres::PostgresStorage`)
runs `sqlx::migrate!("../../migrations/postgres")` instead of the portable
set.

Filenames must stay in sync across the two directories — the migrator tracks
applied migrations by their numeric prefix, so a Postgres-only addition with
a fresh timestamp is fine, but renaming an existing migration on one side
without the other will skew schema history. The libsql adapter (#24) is
expected to reuse the portable directory.

## Adding a new migration

```sh
cargo run -p xtask -- migrate add <slug>
```

This creates `migrations/<timestamp>_<slug>.sql` (and, when invoked with
`--postgres`, a paired stub under `migrations/postgres/`). It does not need
`sqlx-cli` on `PATH` — `xtask` handles the filename in-process.

## Running migrations

```sh
cargo run -p xtask -- migrate run                          # uses $DATABASE_URL / .env
cargo run -p xtask -- migrate run --database-url sqlite::memory:
```

`migrate status` and `migrate revert` are stubs at M0 and land properly in
M1 alongside the second backend.

## Schema notes

- **IDs** are stored as 16-byte `BLOB`s (UUIDv7). SQLite stores them as
  `BLOB`; Postgres will store them as `uuid` once that backend ships.
- **Timestamps** are stored as RFC3339 strings in `TEXT` columns. This keeps
  the schema portable to SQLite without falling back to integer epochs and
  losing readability in `sqlite3` sessions.
- **Permissions** are stored as a single `INTEGER` holding a `u32` bitflag,
  matching `thewiki_core::Permissions`.
