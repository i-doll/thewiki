//! Postgres [`MediaRepository`](crate::repo::MediaRepository) and
//! [`MediaBlobRepository`](crate::repo::MediaBlobRepository) impls (#32).

use bytes::Bytes;
use sqlx::PgPool;
use thewiki_core::{Media, MediaId, UserId};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::StorageError;
use crate::postgres::codec::map_unique_violation;
use crate::repo::{MediaBlobRepository, MediaRepository};

/// Native row shape: Postgres binds `BYTEA`/`UUID`/`TIMESTAMPTZ` directly.
type MediaRow = (
    Uuid,           // id
    Vec<u8>,        // content_hash
    String,         // content_type
    i64,            // byte_size
    Option<String>, // original_filename
    Uuid,           // uploaded_by
    OffsetDateTime, // created_at
);

fn row_to_media(row: MediaRow) -> Result<Media, StorageError> {
    let (id, content_hash, content_type, byte_size, original_filename, uploaded_by, created_at) =
        row;
    let content_hash: [u8; 32] = content_hash.as_slice().try_into().map_err(|_| {
        StorageError::invalid_input(format!(
            "stored content_hash has wrong byte length: expected 32, got {}",
            content_hash.len()
        ))
    })?;
    let byte_size = u64::try_from(byte_size)
        .map_err(|_| StorageError::invalid_input(format!("byte_size out of range: {byte_size}")))?;
    Ok(Media {
        id: MediaId::from_uuid(id),
        content_hash,
        content_type,
        byte_size,
        original_filename,
        uploaded_by: UserId::from_uuid(uploaded_by),
        created_at,
    })
}

/// Postgres-backed media metadata repository.
pub struct PostgresMediaRepository<'a> {
    pool: &'a PgPool,
}

impl<'a> PostgresMediaRepository<'a> {
    pub(super) fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }
}

impl MediaRepository for PostgresMediaRepository<'_> {
    async fn create(&self, media: &Media) -> Result<(), StorageError> {
        let byte_size = i64::try_from(media.byte_size).map_err(|_| {
            StorageError::invalid_input(format!("byte_size {} exceeds i64::MAX", media.byte_size))
        })?;

        let result = sqlx::query(
            "INSERT INTO media
                (id, content_hash, content_type, byte_size, original_filename,
                 uploaded_by, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(media.id.into_uuid())
        .bind(media.content_hash.as_slice())
        .bind(&media.content_type)
        .bind(byte_size)
        .bind(&media.original_filename)
        .bind(media.uploaded_by.into_uuid())
        .bind(media.created_at)
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
        let row: Option<MediaRow> = sqlx::query_as(
            "SELECT id, content_hash, content_type, byte_size, original_filename,
                    uploaded_by, created_at
             FROM media WHERE id = $1",
        )
        .bind(id.into_uuid())
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
             FROM media WHERE content_hash = $1",
        )
        .bind(content_hash.as_slice())
        .fetch_optional(self.pool)
        .await?;

        row.map(row_to_media).transpose()
    }

    async fn delete(&self, id: MediaId) -> Result<(), StorageError> {
        let result = sqlx::query("DELETE FROM media WHERE id = $1")
            .bind(id.into_uuid())
            .execute(self.pool)
            .await?;
        if result.rows_affected() == 0 {
            Err(StorageError::NotFound)
        } else {
            Ok(())
        }
    }
}

/// Postgres-backed blob repository, backing the in-DB media backend.
pub struct PostgresMediaBlobRepository<'a> {
    pool: &'a PgPool,
}

impl<'a> PostgresMediaBlobRepository<'a> {
    pub(super) fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }
}

impl MediaBlobRepository for PostgresMediaBlobRepository<'_> {
    async fn put(&self, media_id: MediaId, data: Bytes) -> Result<(), StorageError> {
        // `INSERT … ON CONFLICT DO UPDATE` keeps the call idempotent on
        // retry — same semantics as SQLite's `INSERT OR REPLACE`.
        sqlx::query(
            "INSERT INTO media_blobs (media_id, data) VALUES ($1, $2)
             ON CONFLICT (media_id) DO UPDATE SET data = EXCLUDED.data",
        )
        .bind(media_id.into_uuid())
        .bind(data.as_ref())
        .execute(self.pool)
        .await?;
        Ok(())
    }

    async fn get(&self, media_id: MediaId) -> Result<Bytes, StorageError> {
        let row: Option<(Vec<u8>,)> =
            sqlx::query_as("SELECT data FROM media_blobs WHERE media_id = $1")
                .bind(media_id.into_uuid())
                .fetch_optional(self.pool)
                .await?;
        match row {
            Some((data,)) => Ok(Bytes::from(data)),
            None => Err(StorageError::NotFound),
        }
    }

    async fn delete(&self, media_id: MediaId) -> Result<(), StorageError> {
        sqlx::query("DELETE FROM media_blobs WHERE media_id = $1")
            .bind(media_id.into_uuid())
            .execute(self.pool)
            .await?;
        Ok(())
    }
}
