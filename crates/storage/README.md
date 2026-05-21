# thewiki-storage

Storage layer for thewiki — sqlx-backed metadata and object_store-backed blobs.

## Layout

- `repo` — `Repository` trait per aggregate (`PageRepository`, `UserRepository`,
  `RevisionRepository`, `NamespaceRepository`, `RoleRepository`). The
  composition root holds `Arc<dyn …>` and stays backend-agnostic.
- `sqlite` — M0 backend; on by default behind the `sqlite` feature. M1 will
  add `postgres` and `libsql` features alongside it.
- `error` — `StorageError`, the single error every repository surfaces.

## Pool configuration

```rust,no_run
use std::time::Duration;
use thewiki_storage::sqlite::{SqliteOptions, SqliteStorage};

# async fn doc() -> Result<(), thewiki_storage::StorageError> {
let storage = SqliteStorage::new(
    "sqlite://thewiki.db",
    SqliteOptions {
        max_connections: 8,
        acquire_timeout: Duration::from_secs(5),
        foreign_keys: true,
    },
).await?;
# Ok(())
# }
```

`SqliteStorage::new` applies the migration set under `/migrations` and
explicitly enables `PRAGMA foreign_keys = ON` (sqlx does **not** do this by
default).
