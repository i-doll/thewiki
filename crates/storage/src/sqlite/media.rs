//! SQLite [`MediaRepository`](crate::repo::MediaRepository) and
//! [`MediaBlobRepository`](crate::repo::MediaBlobRepository) impls (#32).

use bytes::Bytes;
use sqlx::SqlitePool;
use thewiki_core::{Media, MediaId};

use crate::codec::media_from_row;
use crate::error::StorageError;
use crate::repo::{
    MediaBlobRepository, MediaRepository, MediaVariant, MediaVariantRepository, PageSlice,
    clamp_limit,
};
use crate::sqlite::codec::{format_ts, map_unique_violation, parse_ts, uuid_bytes};

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

    async fn list_all(
        &self,
        cursor: Option<MediaId>,
        limit: u32,
    ) -> Result<PageSlice<Media>, StorageError> {
        let limit = clamp_limit(limit);
        let rows: Vec<MediaRow> = if let Some(cursor) = cursor {
            let id_bytes = uuid_bytes(cursor.into_uuid());
            sqlx::query_as(
                "SELECT id, content_hash, content_type, byte_size, original_filename,
                        uploaded_by, created_at
                 FROM media WHERE id > ?1 ORDER BY id ASC LIMIT ?2",
            )
            .bind(id_bytes.as_slice())
            .bind(i64::from(limit))
            .fetch_all(self.pool)
            .await?
        } else {
            sqlx::query_as(
                "SELECT id, content_hash, content_type, byte_size, original_filename,
                        uploaded_by, created_at
                 FROM media ORDER BY id ASC LIMIT ?1",
            )
            .bind(i64::from(limit))
            .fetch_all(self.pool)
            .await?
        };
        let items: Vec<Media> = rows
            .into_iter()
            .map(row_to_media)
            .collect::<Result<_, _>>()?;
        // We encode the cursor as the last id's hyphenated UUID string so the
        // wire form lines up with the other cursor-paginated endpoints. The
        // regen-thumbnails CLI doesn't ship the cursor over a wire — it just
        // wants the last id back — so the encoding is opaque to it.
        let next = if items.len() as u32 == limit {
            items
                .last()
                .map(|m| crate::repo::Cursor(m.id.into_uuid().to_string()))
        } else {
            None
        };
        Ok(PageSlice { items, next })
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

/// Raw row shape for the `media_variants` table.
type MediaVariantRow = (
    Vec<u8>,         // media_id
    String,          // variant
    String,          // content_type
    i64,             // byte_size
    i64,             // width
    i64,             // height
    Option<Vec<u8>>, // data
    String,          // created_at
);

fn row_to_variant(row: MediaVariantRow) -> Result<MediaVariant, StorageError> {
    let (media_id, variant, content_type, byte_size, width, height, data, created_at) = row;
    let id_arr: [u8; 16] = media_id
        .as_slice()
        .try_into()
        .map_err(|_| StorageError::invalid_input("media_variants.media_id wrong length"))?;
    let byte_size = u64::try_from(byte_size).map_err(|_| {
        StorageError::invalid_input(format!("variant byte_size out of range: {byte_size}"))
    })?;
    let width = u32::try_from(width)
        .map_err(|_| StorageError::invalid_input(format!("variant width out of range: {width}")))?;
    let height = u32::try_from(height).map_err(|_| {
        StorageError::invalid_input(format!("variant height out of range: {height}"))
    })?;
    Ok(MediaVariant {
        media_id: MediaId::from_uuid(uuid::Uuid::from_bytes(id_arr)),
        variant,
        content_type,
        byte_size,
        width,
        height,
        data: data.map(Bytes::from),
        created_at: parse_ts(&created_at)?,
    })
}

/// SQLite-backed [`MediaVariantRepository`] — stores variant metadata (and,
/// for the DB blob backend, the variant bytes) in `media_variants`.
pub struct SqliteMediaVariantRepository<'a> {
    pool: &'a SqlitePool,
}

impl<'a> SqliteMediaVariantRepository<'a> {
    pub(super) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }
}

impl MediaVariantRepository for SqliteMediaVariantRepository<'_> {
    async fn put(&self, variant: &MediaVariant) -> Result<(), StorageError> {
        let id = uuid_bytes(variant.media_id.into_uuid());
        let created_at = format_ts(variant.created_at)?;
        let byte_size = i64::try_from(variant.byte_size).map_err(|_| {
            StorageError::invalid_input(format!("byte_size {} exceeds i64::MAX", variant.byte_size))
        })?;
        let width = i64::from(variant.width);
        let height = i64::from(variant.height);
        let data_slice: Option<&[u8]> = variant.data.as_ref().map(|b| b.as_ref());
        sqlx::query(
            "INSERT OR REPLACE INTO media_variants
                (media_id, variant, content_type, byte_size, width, height, data, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .bind(id.as_slice())
        .bind(&variant.variant)
        .bind(&variant.content_type)
        .bind(byte_size)
        .bind(width)
        .bind(height)
        .bind(data_slice)
        .bind(&created_at)
        .execute(self.pool)
        .await?;
        Ok(())
    }

    async fn get(
        &self,
        media_id: MediaId,
        variant: &str,
    ) -> Result<Option<MediaVariant>, StorageError> {
        let id = uuid_bytes(media_id.into_uuid());
        let row: Option<MediaVariantRow> = sqlx::query_as(
            "SELECT media_id, variant, content_type, byte_size, width, height, data, created_at
             FROM media_variants WHERE media_id = ?1 AND variant = ?2",
        )
        .bind(id.as_slice())
        .bind(variant)
        .fetch_optional(self.pool)
        .await?;
        row.map(row_to_variant).transpose()
    }

    async fn delete_for_media(&self, media_id: MediaId) -> Result<(), StorageError> {
        let id = uuid_bytes(media_id.into_uuid());
        sqlx::query("DELETE FROM media_variants WHERE media_id = ?1")
            .bind(id.as_slice())
            .execute(self.pool)
            .await?;
        Ok(())
    }
}
