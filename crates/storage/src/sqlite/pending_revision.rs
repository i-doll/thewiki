//! SQLite [`PendingRevisionRepository`] impl (#40).
//!
//! The status column is intentionally a free-form `TEXT` with a `CHECK`
//! constraint matching [`PendingRevisionStatus`]; the application is the
//! single writer and only flips `pending → approved | rejected` once,
//! so we don't bother with an enum domain.

use sqlx::SqlitePool;
use thewiki_core::{
    PageId, PendingRevision, PendingRevisionId, PendingRevisionStatus, RevisionId, UserId,
};
use time::OffsetDateTime;

use crate::error::StorageError;
use crate::repo::{
    Cursor, NewPendingRevision, PageSlice, PendingRevisionFilter, PendingRevisionRepository,
    clamp_limit,
};
use crate::sqlite::codec::{decode_uuid, format_ts, hex_decode_id, hex_encode, parse_ts, uuid_bytes};

/// Shape of one `pending_revisions` row returned by the driver.
type Row = (
    Vec<u8>,         // id
    Vec<u8>,         // page_id
    Option<Vec<u8>>, // parent_revision_id
    String,          // body
    Option<Vec<u8>>, // author_id
    Option<String>,  // author_ip
    String,          // comment
    String,          // status
    Option<Vec<u8>>, // reviewer_id
    Option<String>,  // decided_at
    Option<String>,  // rejection_reason
    String,          // created_at
);

fn row_to_pending(row: Row) -> Result<PendingRevision, StorageError> {
    let (
        id,
        page_id,
        parent_revision_id,
        body,
        author_id,
        author_ip,
        comment,
        status,
        reviewer_id,
        decided_at,
        rejection_reason,
        created_at,
    ) = row;
    let status = PendingRevisionStatus::parse(&status).ok_or_else(|| {
        StorageError::invalid_input(format!("unknown pending_revisions.status {status:?}"))
    })?;
    Ok(PendingRevision {
        id: PendingRevisionId::from_uuid(decode_uuid(&id)?),
        page_id: PageId::from_uuid(decode_uuid(&page_id)?),
        parent_revision_id: parent_revision_id
            .as_deref()
            .map(decode_uuid)
            .transpose()?
            .map(RevisionId::from_uuid),
        body,
        author_id: author_id
            .as_deref()
            .map(decode_uuid)
            .transpose()?
            .map(UserId::from_uuid),
        author_ip,
        comment,
        status,
        reviewer_id: reviewer_id
            .as_deref()
            .map(decode_uuid)
            .transpose()?
            .map(UserId::from_uuid),
        decided_at: decided_at.as_deref().map(parse_ts).transpose()?,
        rejection_reason,
        created_at: parse_ts(&created_at)?,
    })
}

/// SQLite-backed pending-revisions repository.
pub struct SqlitePendingRevisionRepository<'a> {
    pool: &'a SqlitePool,
}

impl<'a> SqlitePendingRevisionRepository<'a> {
    pub(super) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }
}

impl PendingRevisionRepository for SqlitePendingRevisionRepository<'_> {
    async fn create(&self, new: NewPendingRevision) -> Result<PendingRevision, StorageError> {
        let id = PendingRevisionId::new();
        let created_at = OffsetDateTime::now_utc();
        let id_bytes = uuid_bytes(id.into_uuid());
        let page_bytes = uuid_bytes(new.page_id.into_uuid());
        let parent_bytes = new
            .parent_revision_id
            .map(|p| uuid_bytes(p.into_uuid()));
        let author_bytes = new.author_id.map(|a| uuid_bytes(a.into_uuid()));
        let created_at_str = format_ts(created_at)?;
        let status = PendingRevisionStatus::Pending.as_str();

        sqlx::query(
            "INSERT INTO pending_revisions \
                (id, page_id, parent_revision_id, body, author_id, author_ip, \
                 comment, status, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )
        .bind(id_bytes.as_slice())
        .bind(page_bytes.as_slice())
        .bind(parent_bytes.as_ref().map(|b| b.as_slice()))
        .bind(&new.body)
        .bind(author_bytes.as_ref().map(|b| b.as_slice()))
        .bind(new.author_ip.as_deref())
        .bind(&new.comment)
        .bind(status)
        .bind(&created_at_str)
        .execute(self.pool)
        .await?;

        Ok(PendingRevision {
            id,
            page_id: new.page_id,
            parent_revision_id: new.parent_revision_id,
            body: new.body,
            author_id: new.author_id,
            author_ip: new.author_ip,
            comment: new.comment,
            status: PendingRevisionStatus::Pending,
            reviewer_id: None,
            decided_at: None,
            rejection_reason: None,
            created_at,
        })
    }

    async fn get_by_id(&self, id: PendingRevisionId) -> Result<PendingRevision, StorageError> {
        let id_bytes = uuid_bytes(id.into_uuid());
        let row: Option<Row> = sqlx::query_as(
            "SELECT id, page_id, parent_revision_id, body, author_id, author_ip, \
                    comment, status, reviewer_id, decided_at, rejection_reason, created_at \
             FROM pending_revisions WHERE id = ?1",
        )
        .bind(id_bytes.as_slice())
        .fetch_optional(self.pool)
        .await?;
        match row {
            Some(r) => row_to_pending(r),
            None => Err(StorageError::NotFound),
        }
    }

    async fn list(
        &self,
        filter: PendingRevisionFilter,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> Result<PageSlice<PendingRevision>, StorageError> {
        let limit = clamp_limit(limit);
        let take = i64::from(limit) + 1;
        let cursor_pair = cursor.map(|c| decode_cursor(&c)).transpose()?;
        let status_filter = filter.status.map(|s| s.as_str().to_owned());

        // Expand the four (status × cursor) combinations into concrete
        // queries so each bind set has a fixed type and SQLite doesn't
        // hit a datatype-mismatch coercion when the dynamic builder
        // appends placeholders.
        let rows: Vec<Row> = match (status_filter.as_deref(), cursor_pair.as_ref()) {
            (None, None) => sqlx::query_as(
                "SELECT id, page_id, parent_revision_id, body, author_id, author_ip, \
                        comment, status, reviewer_id, decided_at, rejection_reason, created_at \
                 FROM pending_revisions \
                 ORDER BY created_at DESC, id DESC LIMIT ?1",
            )
            .bind(take)
            .fetch_all(self.pool)
            .await?,
            (Some(s), None) => sqlx::query_as(
                "SELECT id, page_id, parent_revision_id, body, author_id, author_ip, \
                        comment, status, reviewer_id, decided_at, rejection_reason, created_at \
                 FROM pending_revisions WHERE status = ?1 \
                 ORDER BY created_at DESC, id DESC LIMIT ?2",
            )
            .bind(s)
            .bind(take)
            .fetch_all(self.pool)
            .await?,
            (None, Some((ts, id_hex))) => {
                let id_bytes = hex_decode_id(id_hex)?;
                sqlx::query_as(
                    "SELECT id, page_id, parent_revision_id, body, author_id, author_ip, \
                            comment, status, reviewer_id, decided_at, rejection_reason, created_at \
                     FROM pending_revisions \
                     WHERE (created_at < ?1 OR (created_at = ?1 AND id < ?2)) \
                     ORDER BY created_at DESC, id DESC LIMIT ?3",
                )
                .bind(ts)
                .bind(id_bytes.as_slice())
                .bind(take)
                .fetch_all(self.pool)
                .await?
            }
            (Some(s), Some((ts, id_hex))) => {
                let id_bytes = hex_decode_id(id_hex)?;
                sqlx::query_as(
                    "SELECT id, page_id, parent_revision_id, body, author_id, author_ip, \
                            comment, status, reviewer_id, decided_at, rejection_reason, created_at \
                     FROM pending_revisions WHERE status = ?1 \
                     AND (created_at < ?2 OR (created_at = ?2 AND id < ?3)) \
                     ORDER BY created_at DESC, id DESC LIMIT ?4",
                )
                .bind(s)
                .bind(ts)
                .bind(id_bytes.as_slice())
                .bind(take)
                .fetch_all(self.pool)
                .await?
            }
        };
        finalise_page(rows, limit)
    }

    async fn count(&self, filter: PendingRevisionFilter) -> Result<u64, StorageError> {
        let status_filter = filter.status.map(|s| s.as_str().to_owned());
        let row: (i64,) = if let Some(s) = status_filter.as_ref() {
            sqlx::query_as("SELECT COUNT(*) FROM pending_revisions WHERE status = ?1")
                .bind(s)
                .fetch_one(self.pool)
                .await?
        } else {
            sqlx::query_as("SELECT COUNT(*) FROM pending_revisions")
                .fetch_one(self.pool)
                .await?
        };
        // The COUNT(*) cannot legitimately be negative.
        #[allow(clippy::cast_sign_loss, reason = "COUNT(*) is non-negative")]
        Ok(row.0.max(0) as u64)
    }

    async fn approve(
        &self,
        id: PendingRevisionId,
        reviewer_id: UserId,
        decided_at: OffsetDateTime,
    ) -> Result<PendingRevision, StorageError> {
        decide(self.pool, id, reviewer_id, decided_at, DecisionKind::Approve).await
    }

    async fn reject(
        &self,
        id: PendingRevisionId,
        reviewer_id: UserId,
        reason: &str,
        decided_at: OffsetDateTime,
    ) -> Result<PendingRevision, StorageError> {
        decide(
            self.pool,
            id,
            reviewer_id,
            decided_at,
            DecisionKind::Reject(reason),
        )
        .await
    }
}

enum DecisionKind<'a> {
    Approve,
    Reject(&'a str),
}

async fn decide(
    pool: &SqlitePool,
    id: PendingRevisionId,
    reviewer_id: UserId,
    decided_at: OffsetDateTime,
    kind: DecisionKind<'_>,
) -> Result<PendingRevision, StorageError> {
    let id_bytes = uuid_bytes(id.into_uuid());
    let reviewer_bytes = uuid_bytes(reviewer_id.into_uuid());
    let decided_at_str = format_ts(decided_at)?;
    let pending = PendingRevisionStatus::Pending.as_str();

    // Transactional read-modify-write so a racing reviewer can't approve and
    // reject the same row in the same instant; the `WHERE status = 'pending'`
    // clause is the conflict guard.
    let mut tx = pool.begin().await?;
    let exists: Option<(String,)> =
        sqlx::query_as("SELECT status FROM pending_revisions WHERE id = ?1")
            .bind(id_bytes.as_slice())
            .fetch_optional(&mut *tx)
            .await?;
    let current_status = match exists {
        Some((s,)) => s,
        None => return Err(StorageError::NotFound),
    };
    if current_status != pending {
        return Err(StorageError::Conflict(format!(
            "pending revision already {current_status}"
        )));
    }
    let new_status = match kind {
        DecisionKind::Approve => PendingRevisionStatus::Approved,
        DecisionKind::Reject(_) => PendingRevisionStatus::Rejected,
    };
    let reason = match kind {
        DecisionKind::Approve => None,
        DecisionKind::Reject(r) => Some(r),
    };
    sqlx::query(
        "UPDATE pending_revisions \
            SET status = ?1, reviewer_id = ?2, decided_at = ?3, rejection_reason = ?4 \
            WHERE id = ?5 AND status = ?6",
    )
    .bind(new_status.as_str())
    .bind(reviewer_bytes.as_slice())
    .bind(&decided_at_str)
    .bind(reason)
    .bind(id_bytes.as_slice())
    .bind(pending)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    // Re-read the row so the caller sees a single authoritative view.
    let row: Option<Row> = sqlx::query_as(
        "SELECT id, page_id, parent_revision_id, body, author_id, author_ip, \
                comment, status, reviewer_id, decided_at, rejection_reason, created_at \
         FROM pending_revisions WHERE id = ?1",
    )
    .bind(id_bytes.as_slice())
    .fetch_optional(pool)
    .await?;
    match row {
        Some(r) => row_to_pending(r),
        None => Err(StorageError::NotFound),
    }
}

fn encode_cursor(created_at: &str, id: &[u8]) -> Cursor {
    Cursor(format!("{}|{}", created_at, hex_encode(id)))
}

fn decode_cursor(c: &Cursor) -> Result<(String, String), StorageError> {
    let (ts, id_hex) = c.0.split_once('|').ok_or_else(|| {
        StorageError::invalid_input("pending-revisions cursor must be `<timestamp>|<hex-id>`")
    })?;
    let _ = parse_ts(ts)?; // validate the timestamp format
    Ok((ts.to_string(), id_hex.to_string()))
}

fn finalise_page(
    mut rows: Vec<Row>,
    limit: u32,
) -> Result<PageSlice<PendingRevision>, StorageError> {
    let limit_usize = limit as usize;
    let has_more = rows.len() > limit_usize;
    if has_more {
        rows.truncate(limit_usize);
    }
    let next = if has_more {
        rows.last().map(|last| encode_cursor(&last.11, &last.0))
    } else {
        None
    };
    let items = rows
        .into_iter()
        .map(row_to_pending)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PageSlice { items, next })
}
