//! libsql [`MediaRepository`](crate::repo::MediaRepository) and
//! [`MediaBlobRepository`](crate::repo::MediaBlobRepository) impls (#32).

use bytes::Bytes;
use libsql::{Connection, Value};
use thewiki_core::{Media, MediaId};

use crate::codec::media_from_row;
use crate::error::StorageError;
use crate::libsql::codec::{format_ts, into_db, map_unique_violation, opt_text, uuid_bytes};
use crate::repo::{MediaBlobRepository, MediaRepository};

/// libsql-backed media metadata repository.
pub struct LibsqlMediaRepository<'a> {
    conn: &'a Connection,
}

impl<'a> LibsqlMediaRepository<'a> {
    pub(super) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}

impl MediaRepository for LibsqlMediaRepository<'_> {
    async fn create(&self, media: &Media) -> Result<(), StorageError> {
        let id = uuid_bytes(media.id.into_uuid());
        let uploader = uuid_bytes(media.uploaded_by.into_uuid());
        let created_at = format_ts(media.created_at)?;
        let byte_size = i64::try_from(media.byte_size).map_err(|_| {
            StorageError::invalid_input(format!("byte_size {} exceeds i64::MAX", media.byte_size))
        })?;

        let binds: Vec<Value> = vec![
            Value::Blob(id.to_vec()),
            Value::Blob(media.content_hash.to_vec()),
            Value::Text(media.content_type.clone()),
            Value::Integer(byte_size),
            opt_text(media.original_filename.as_deref()),
            Value::Blob(uploader.to_vec()),
            Value::Text(created_at),
        ];

        match self
            .conn
            .execute(
                "INSERT INTO media
                    (id, content_hash, content_type, byte_size, original_filename,
                     uploaded_by, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                binds,
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(err) => Err(map_unique_violation(
                err,
                "media content_hash already stored",
            )),
        }
    }

    async fn get_by_id(&self, id: MediaId) -> Result<Media, StorageError> {
        let id_bytes = uuid_bytes(id.into_uuid());
        let mut rows = into_db(
            self.conn
                .query(
                    "SELECT id, content_hash, content_type, byte_size, original_filename,
                            uploaded_by, created_at
                     FROM media WHERE id = ?1",
                    vec![Value::Blob(id_bytes.to_vec())],
                )
                .await,
        )?;
        let Some(row) = into_db(rows.next().await)? else {
            return Err(StorageError::NotFound);
        };
        decode_media(&row)
    }

    async fn get_by_content_hash(
        &self,
        content_hash: &[u8; 32],
    ) -> Result<Option<Media>, StorageError> {
        let mut rows = into_db(
            self.conn
                .query(
                    "SELECT id, content_hash, content_type, byte_size, original_filename,
                            uploaded_by, created_at
                     FROM media WHERE content_hash = ?1",
                    vec![Value::Blob(content_hash.to_vec())],
                )
                .await,
        )?;
        let row = into_db(rows.next().await)?;
        row.as_ref().map(decode_media).transpose()
    }

    async fn delete(&self, id: MediaId) -> Result<(), StorageError> {
        let id_bytes = uuid_bytes(id.into_uuid());
        let affected = into_db(
            self.conn
                .execute(
                    "DELETE FROM media WHERE id = ?1",
                    vec![Value::Blob(id_bytes.to_vec())],
                )
                .await,
        )?;
        if affected == 0 {
            Err(StorageError::NotFound)
        } else {
            Ok(())
        }
    }
}

fn decode_media(row: &libsql::Row) -> Result<Media, StorageError> {
    let id: Vec<u8> = into_db(row.get::<Vec<u8>>(0))?;
    let content_hash: Vec<u8> = into_db(row.get::<Vec<u8>>(1))?;
    let content_type: String = into_db(row.get::<String>(2))?;
    let byte_size: i64 = into_db(row.get::<i64>(3))?;
    let original_filename: Option<String> = into_db(row.get::<Option<String>>(4))?;
    let uploaded_by: Vec<u8> = into_db(row.get::<Vec<u8>>(5))?;
    let created_at: String = into_db(row.get::<String>(6))?;
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

/// libsql-backed [`MediaBlobRepository`] — stores blob bytes in
/// `media_blobs`.
pub struct LibsqlMediaBlobRepository<'a> {
    conn: &'a Connection,
}

impl<'a> LibsqlMediaBlobRepository<'a> {
    pub(super) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}

impl MediaBlobRepository for LibsqlMediaBlobRepository<'_> {
    async fn put(&self, media_id: MediaId, data: Bytes) -> Result<(), StorageError> {
        let id_bytes = uuid_bytes(media_id.into_uuid());
        into_db(
            self.conn
                .execute(
                    "INSERT OR REPLACE INTO media_blobs (media_id, data) VALUES (?1, ?2)",
                    vec![Value::Blob(id_bytes.to_vec()), Value::Blob(data.to_vec())],
                )
                .await,
        )?;
        Ok(())
    }

    async fn get(&self, media_id: MediaId) -> Result<Bytes, StorageError> {
        let id_bytes = uuid_bytes(media_id.into_uuid());
        let mut rows = into_db(
            self.conn
                .query(
                    "SELECT data FROM media_blobs WHERE media_id = ?1",
                    vec![Value::Blob(id_bytes.to_vec())],
                )
                .await,
        )?;
        let Some(row) = into_db(rows.next().await)? else {
            return Err(StorageError::NotFound);
        };
        let data: Vec<u8> = into_db(row.get::<Vec<u8>>(0))?;
        Ok(Bytes::from(data))
    }

    async fn delete(&self, media_id: MediaId) -> Result<(), StorageError> {
        let id_bytes = uuid_bytes(media_id.into_uuid());
        into_db(
            self.conn
                .execute(
                    "DELETE FROM media_blobs WHERE media_id = ?1",
                    vec![Value::Blob(id_bytes.to_vec())],
                )
                .await,
        )?;
        Ok(())
    }
}
