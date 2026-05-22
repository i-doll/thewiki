//! Storage backend abstraction for the media upload pipeline (#32).
//!
//! Two concrete impls live behind [`MediaBackend`]:
//!
//! - [`DbMediaBackend`] — stores blob bytes in the primary database via the
//!   storage crate's `MediaBlobRepository`. The default; works for small
//!   deploys without any external service.
//! - [`S3MediaBackend`] — uses the `object_store` crate to talk to any
//!   S3-compatible bucket (AWS, R2, MinIO, B2, ...). The bucket is
//!   addressed by `MediaId` (UUIDv7); only the metadata stays in the DB.
//!
//! The trait is intentionally minimal — `put` / `get` / `delete` — so we
//! don't need to thread `object_store`'s richer surface (`list`,
//! `multipart`, ...) through the API layer. It is `dyn`-compatible so the
//! handler can hold an `Arc<dyn MediaBackend>` and not care about the
//! concrete backend.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use object_store::aws::AmazonS3Builder;
use object_store::path::Path as ObjectPath;
// `ObjectStoreExt` extends `Arc<dyn ObjectStore>` with the trait methods
// (`put`, `get`, `delete`) as inherent methods — without it, the compiler
// can't see them through the `Arc<dyn …>` indirection.
use object_store::{ObjectStore, ObjectStoreExt};
use thewiki_core::MediaId;
use thewiki_storage::StorageError;
use thewiki_storage::repo::MediaBlobRepository;

use crate::config::StorageBackend;
use crate::error::ApiError;
use crate::state::AppStorage;

/// Errors a media backend can return.
///
/// The variants converge at the API layer: `NotFound` becomes a 404 and
/// everything else surfaces as a 500. Keeping them distinct here lets the
/// handler distinguish "the blob is gone" from "the bucket is unreachable".
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MediaBackendError {
    /// The blob doesn't exist in the backend.
    #[error("blob not found")]
    NotFound,
    /// The underlying storage rejected the operation.
    #[error("backend error: {0}")]
    Backend(String),
}

impl From<MediaBackendError> for ApiError {
    fn from(err: MediaBackendError) -> Self {
        match err {
            MediaBackendError::NotFound => Self::NotFound,
            MediaBackendError::Backend(msg) => Self::Internal(format!("media backend: {msg}")),
        }
    }
}

impl From<StorageError> for MediaBackendError {
    fn from(err: StorageError) -> Self {
        match err {
            StorageError::NotFound => Self::NotFound,
            other => Self::Backend(other.to_string()),
        }
    }
}

impl From<object_store::Error> for MediaBackendError {
    fn from(err: object_store::Error) -> Self {
        // `object_store::Error` has a `NotFound { .. }` variant. We pattern
        // match on the path-of-not-found rather than the error chain so a
        // missing key surfaces as 404 instead of 500.
        if matches!(err, object_store::Error::NotFound { .. }) {
            Self::NotFound
        } else {
            Self::Backend(err.to_string())
        }
    }
}

/// One blob backend behind the upload pipeline.
///
/// Implementors must be `Send + Sync + 'static` so handler state can hold
/// them in an `Arc<dyn MediaBackend>`.
#[async_trait]
pub trait MediaBackend: Send + Sync + 'static {
    /// Store `data` under the address derived from `media_id`.
    async fn put(&self, media_id: MediaId, data: Bytes) -> Result<(), MediaBackendError>;

    /// Fetch the bytes previously stored under `media_id`.
    async fn get(&self, media_id: MediaId) -> Result<Bytes, MediaBackendError>;

    /// Remove the bytes for `media_id`. Idempotent — deleting a missing
    /// row is OK.
    async fn delete(&self, media_id: MediaId) -> Result<(), MediaBackendError>;

    /// Store thumbnail variant bytes (#33). The DB backend stores variant
    /// bytes in the `media_variants.data` column, so its impl is a no-op;
    /// the S3 backend pushes the bytes to
    /// `<bucket>/media/<media_id>/<variant>.webp`.
    async fn put_variant(
        &self,
        _media_id: MediaId,
        _variant_label: &str,
        _data: Bytes,
    ) -> Result<(), MediaBackendError> {
        Ok(())
    }

    /// Fetch thumbnail variant bytes (#33). The DB backend is unused for
    /// this — the variant rows carry the bytes directly. The S3 backend
    /// fetches from the bucket.
    async fn get_variant(
        &self,
        _media_id: MediaId,
        _variant_label: &str,
    ) -> Result<Bytes, MediaBackendError> {
        Err(MediaBackendError::NotFound)
    }

    /// Delete every variant for `media_id` (#33). The DB backend's row
    /// cascade does this for free; the S3 backend has to walk the prefix.
    async fn delete_variants(&self, _media_id: MediaId) -> Result<(), MediaBackendError> {
        Ok(())
    }

    /// Whether the backend stores variant bytes in the metadata row
    /// (`true` for the in-DB backend) or in a separate bucket location
    /// (`false` for the S3 backend). Used by the upload pipeline to
    /// decide whether to populate `media_variants.data` or push the
    /// bytes to the bucket.
    fn variants_inline_in_db(&self) -> bool {
        false
    }
}

/// In-database blob backend.
///
/// Wraps any [`AppStorage`] handle and dispatches `put`/`get`/`delete` to
/// the storage crate's [`MediaBlobRepository`]. The storage handle is
/// stored in an `Arc` so cloning the backend itself stays cheap.
pub struct DbMediaBackend<S: AppStorage> {
    storage: Arc<S>,
}

impl<S: AppStorage> DbMediaBackend<S> {
    /// Build a DB backend over the supplied storage handle.
    #[must_use]
    pub fn new(storage: Arc<S>) -> Self {
        Self { storage }
    }
}

#[async_trait]
impl<S: AppStorage> MediaBackend for DbMediaBackend<S> {
    async fn put(&self, media_id: MediaId, data: Bytes) -> Result<(), MediaBackendError> {
        self.storage.media_blobs().put(media_id, data).await?;
        Ok(())
    }

    async fn get(&self, media_id: MediaId) -> Result<Bytes, MediaBackendError> {
        self.storage
            .media_blobs()
            .get(media_id)
            .await
            .map_err(MediaBackendError::from)
    }

    async fn delete(&self, media_id: MediaId) -> Result<(), MediaBackendError> {
        self.storage
            .media_blobs()
            .delete(media_id)
            .await
            .map_err(MediaBackendError::from)
    }

    fn variants_inline_in_db(&self) -> bool {
        true
    }
}

/// S3-compatible blob backend, powered by `object_store::aws::AmazonS3`.
///
/// Objects live in the configured bucket under a content-addressed key —
/// `media/<hyphenated-uuid>`. The prefix keeps the bucket tidy so an
/// operator pointing it at a shared bucket can still find every thewiki
/// upload with one `ls`.
pub struct S3MediaBackend {
    store: Arc<dyn ObjectStore>,
}

impl S3MediaBackend {
    /// Build an [`S3MediaBackend`] from a region + bucket (and optional
    /// custom endpoint URL for non-AWS providers).
    ///
    /// Credentials are picked up via the standard AWS SDK chain
    /// (`AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` env vars, instance
    /// metadata, etc.) — `object_store` delegates to `aws-config`.
    ///
    /// # Errors
    ///
    /// Surfaces builder errors as [`MediaBackendError::Backend`].
    pub fn new(
        bucket: &str,
        region: &str,
        endpoint_url: Option<&str>,
    ) -> Result<Self, MediaBackendError> {
        let mut builder = AmazonS3Builder::from_env()
            .with_bucket_name(bucket)
            .with_region(region);
        if let Some(endpoint) = endpoint_url {
            // R2, MinIO, etc. need the endpoint *and* virtual host style
            // disabled (so the bucket is part of the path rather than the
            // sub-domain).
            builder = builder
                .with_endpoint(endpoint)
                .with_virtual_hosted_style_request(false)
                .with_allow_http(endpoint.starts_with("http://"));
        }
        let store = builder
            .build()
            .map_err(|err| MediaBackendError::Backend(err.to_string()))?;
        Ok(Self {
            store: Arc::new(store),
        })
    }

    fn object_path(media_id: MediaId) -> ObjectPath {
        ObjectPath::from(format!("media/{}", media_id.into_uuid()))
    }

    /// Bucket key for a thumbnail variant. Variants share a prefix with
    /// the original media so an operator can browse a given upload's
    /// derived files with one `ls`.
    fn variant_path(media_id: MediaId, label: &str) -> ObjectPath {
        ObjectPath::from(format!("media/{}/{}.webp", media_id.into_uuid(), label))
    }

    /// Bucket key prefix for every variant of `media_id`. Used when we
    /// need to walk and drop every entry on delete.
    fn variant_prefix(media_id: MediaId) -> ObjectPath {
        ObjectPath::from(format!("media/{}", media_id.into_uuid()))
    }
}

#[async_trait]
impl MediaBackend for S3MediaBackend {
    async fn put(&self, media_id: MediaId, data: Bytes) -> Result<(), MediaBackendError> {
        let path = Self::object_path(media_id);
        self.store.put(&path, data.into()).await?;
        Ok(())
    }

    async fn get(&self, media_id: MediaId) -> Result<Bytes, MediaBackendError> {
        let path = Self::object_path(media_id);
        let result = self.store.get(&path).await?;
        let bytes = result.bytes().await?;
        Ok(bytes)
    }

    async fn delete(&self, media_id: MediaId) -> Result<(), MediaBackendError> {
        let path = Self::object_path(media_id);
        // `object_store::ObjectStore::delete` returns `NotFound` for
        // missing keys; flatten that to a successful delete so the API
        // delete-flow stays idempotent (the DB delete may have already
        // succeeded in a prior attempt).
        match self.store.delete(&path).await {
            Ok(()) => Ok(()),
            Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(err) => Err(MediaBackendError::from(err)),
        }
    }

    async fn put_variant(
        &self,
        media_id: MediaId,
        variant_label: &str,
        data: Bytes,
    ) -> Result<(), MediaBackendError> {
        let path = Self::variant_path(media_id, variant_label);
        self.store.put(&path, data.into()).await?;
        Ok(())
    }

    async fn get_variant(
        &self,
        media_id: MediaId,
        variant_label: &str,
    ) -> Result<Bytes, MediaBackendError> {
        let path = Self::variant_path(media_id, variant_label);
        let result = self.store.get(&path).await?;
        let bytes = result.bytes().await?;
        Ok(bytes)
    }

    async fn delete_variants(&self, media_id: MediaId) -> Result<(), MediaBackendError> {
        // Walk the prefix and delete each variant key. The `list` stream
        // yields the original blob too, but we skip it — the caller of
        // `delete_variants` always owns the matching `delete` for the
        // original anyway. We poll the stream with `StreamExt::next` so
        // we don't need to pull in an extra trait crate just for
        // `try_next`.
        use futures_util::StreamExt;
        let prefix = Self::variant_prefix(media_id);
        let mut stream = self.store.list(Some(&prefix));
        while let Some(item) = stream.next().await {
            let meta = item.map_err(MediaBackendError::from)?;
            if meta.location == Self::object_path(media_id) {
                continue;
            }
            if let Err(err) = self.store.delete(&meta.location).await
                && !matches!(err, object_store::Error::NotFound { .. })
            {
                return Err(MediaBackendError::from(err));
            }
        }
        Ok(())
    }
}

/// Build the configured [`MediaBackend`] from [`StorageBackend`] and a
/// storage handle.
///
/// Called at app construction. Returning `Arc<dyn …>` lets the rest of the
/// router stay generic over the concrete backend choice — handlers see only
/// the trait.
///
/// # Errors
///
/// [`MediaBackendError::Backend`] if the S3 builder rejects the supplied
/// configuration. The DB path never fails — it just wraps the storage
/// handle.
pub fn build_media_backend<S: AppStorage>(
    backend_cfg: &StorageBackend,
    storage: Arc<S>,
) -> Result<Arc<dyn MediaBackend>, MediaBackendError> {
    match backend_cfg {
        StorageBackend::Db => Ok(Arc::new(DbMediaBackend::new(storage))),
        StorageBackend::S3 {
            bucket,
            region,
            endpoint_url,
        } => Ok(Arc::new(S3MediaBackend::new(
            bucket,
            region,
            endpoint_url.as_deref(),
        )?)),
    }
}
