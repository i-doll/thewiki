//! SQLite [`MediaRepository`](crate::repo::MediaRepository) and
//! [`MediaBlobRepository`](crate::repo::MediaBlobRepository) impls (#32).

use bytes::Bytes;
use sqlx::SqlitePool;
use thewiki_core::{Media, MediaId};

use crate::codec::media_from_row;
use crate::error::StorageError;
use crate::repo::{MediaBlobRepository, MediaRepository};
use crate::sqlite::codec::{format_ts, map_unique_violation, uuid_bytes};

/// Raw row shape for the `media` table.
type MediaRow = (
    Vec<u8>,        // id
    Vec<u8>,        // content_hash
    String,         // content_type
    i64,            // byte_size
    Option<String>, // original_filename
    Vec<u8>,        // uploaded_by
    String,         // created_at
);

fn row_to_media(row: MediaRow) -> Result<Media, StorageError> {
    let (id, content_hash, content_type, byte_size, original_filename, uploaded_by, created_at) =
        row;
    media_from_row(
        id,
        content_hash,
        content_type,
        byte_size,
        original_filename,
        uploaded_by,
        created_at,
    )
}

/// SQLite-backed media metadata repository.
pub struct SqliteMediaRepository<'a> {
    pool: &'a SqlitePool,
}

impl<'a> SqliteMediaRepository<'a> {
    pub(super) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }
}

impl MediaRepository for SqliteMediaRepository<'_> {
    async fn create(&self, media: &Media) -> Result<(), StorageError> {
        let id = uuid_bytes(media.id.into_uuid());
        let uploader = uuid_bytes(media.uploaded_by.into_uuid());
        let created_at = format_ts(media.created_at)?;
        let byte_size = i64::try_from(media.byte_size).map_err(|_| {
            StorageError::invalid_input(format!("byte_size {} exceeds i64::MAX", media.byte_size))
        })?;

        let result = sqlx::query(
            "INSERT INTO media
                (id, content_hash, content_type, byte_size, original_filename,
                 uploaded_by, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(id.as_slice())
        .bind(media.content_hash.as_slice())
        .bind(&media.content_type)
        .bind(byte_size)
        .bind(&media.original_filename)
        .bind(uploader.as_slice())
        .bind(&created_at)
        .execute(self.pool)
        .await;

        match result {
            Ok(_) => Ok(()),
            Err(err) => Err(map_unique_violation(
                err,
                "media content_hash already stored",
            )),
        }
    }

    async fn get_by_id(&self, id: MediaId) -> Result<Media, StorageError> {
        let id_bytes = uuid_bytes(id.into_uuid());
        let row: Option<MediaRow> = sqlx::query_as(
            "SELECT id, content_hash, content_type, byte_size, original_filename,
                    uploaded_by, created_at
             FROM media WHERE id = ?1",
        )
        .bind(id_bytes.as_slice())
        .fetch_optional(self.pool)
        .await?;

        match row {
            Some(row) => row_to_media(row),
            None => Err(StorageError::NotFound),
        }
    }

    async fn get_by_content_hash(
        &self,
        content_hash: &[u8; 32],
    ) -> Result<Option<Media>, StorageError> {
        let row: Option<MediaRow> = sqlx::query_as(
            "SELECT id, content_hash, content_type, byte_size, original_filename,
                    uploaded_by, created_at
             FROM media WHERE content_hash = ?1",
        )
        .bind(content_hash.as_slice())
        .fetch_optional(self.pool)
        .await?;

        row.map(row_to_media).transpose()
    }

    async fn delete(&self, id: MediaId) -> Result<(), StorageError> {
        let id_bytes = uuid_bytes(id.into_uuid());
        let result = sqlx::query("DELETE FROM media WHERE id = ?1")
            .bind(id_bytes.as_slice())
            .execute(self.pool)
            .await?;
        if result.rows_affected() == 0 {
            Err(StorageError::NotFound)
        } else {
            Ok(())
        }
    }
}

/// SQLite-backed [`MediaBlobRepository`] — stores blob bytes in
/// `media_blobs`. The matching media metadata row is in `media`.
pub struct SqliteMediaBlobRepository<'a> {
    pool: &'a SqlitePool,
}

impl<'a> SqliteMediaBlobRepository<'a> {
    pub(super) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }
}

impl MediaBlobRepository for SqliteMediaBlobRepository<'_> {
    async fn put(&self, media_id: MediaId, data: Bytes) -> Result<(), StorageError> {
        let id_bytes = uuid_bytes(media_id.into_uuid());
        // `INSERT OR REPLACE` keeps the call idempotent across retries: if
        // the metadata row was inserted but the blob write failed previously
        // and the API retried, we want the second blob write to land cleanly
        // without a unique-constraint violation.
        sqlx::query("INSERT OR REPLACE INTO media_blobs (media_id, data) VALUES (?1, ?2)")
            .bind(id_bytes.as_slice())
            .bind(data.as_ref())
            .execute(self.pool)
            .await?;
        Ok(())
    }

    async fn get(&self, media_id: MediaId) -> Result<Bytes, StorageError> {
        let id_bytes = uuid_bytes(media_id.into_uuid());
        let row: Option<(Vec<u8>,)> =
            sqlx::query_as("SELECT data FROM media_blobs WHERE media_id = ?1")
                .bind(id_bytes.as_slice())
                .fetch_optional(self.pool)
                .await?;
        match row {
            Some((data,)) => Ok(Bytes::from(data)),
            None => Err(StorageError::NotFound),
        }
    }

    async fn delete(&self, media_id: MediaId) -> Result<(), StorageError> {
        let id_bytes = uuid_bytes(media_id.into_uuid());
        // The FK from `media_blobs.media_id` to `media.id` cascades on
        // delete of the metadata row, so this path normally only runs in the
        // S3 → DB-fallback scenario where the metadata still exists. Either
        // way the operation is idempotent — a missing row is fine.
        sqlx::query("DELETE FROM media_blobs WHERE media_id = ?1")
            .bind(id_bytes.as_slice())
            .execute(self.pool)
            .await?;
        Ok(())
    }
}
