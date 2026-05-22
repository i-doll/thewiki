//! Async indexer worker.
//!
//! The HTTP request path is async; the Tantivy writer is sync and emphatically
//! single-owner. We bridge the two with a single-consumer worker:
//!
//! ```text
//!   handlers ── send(IndexJob) ──▶ mpsc::Sender ── recv ──▶ worker task
//!                                                                │
//!                                                                ▼
//!                                                          IndexWriter
//!                                                                │
//!                                                                ▼
//!                                                       .last_indexed marker
//! ```
//!
//! Every public hook on the API side returns immediately; the worker absorbs
//! jobs, batches them into commits (every 200 ms or every 100 jobs, whichever
//! first), and survives errors by logging + backing off rather than panicking.
//!
//! The mpsc channel is bounded so a sudden burst of edits cannot grow memory
//! without bound. On full, the API side falls through to a non-blocking
//! `try_send` and logs a structured event; the dropped job will be re-applied
//! on the next `thewiki reindex` (the index lags storage; storage is
//! authoritative).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tantivy::{IndexWriter, TantivyDocument};
use thewiki_core::PageId;
use tokio::sync::mpsc;
use tokio::time::{Instant, MissedTickBehavior};
use tracing::{debug, error, info, warn};

use crate::error::SearchError;
use crate::index::{PageDoc, SearchIndex};

/// Default commit cadence — the index becomes consistent at most this far
/// behind the latest accepted job.
pub const DEFAULT_COMMIT_INTERVAL: Duration = Duration::from_millis(200);

/// Default per-commit job-count threshold. A burst of 100 edits commits
/// immediately instead of waiting for the timer.
pub const DEFAULT_COMMIT_BATCH: usize = 100;

/// Default mpsc capacity. Generous enough that a burst doesn't drop edits on
/// the floor, small enough that a stuck worker is observable in metrics.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 1024;

/// A single unit of work the worker can apply.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum IndexJob {
    /// Insert or replace a page document.
    Upsert(PageDoc),
    /// Delete a page from the index.
    Delete(PageId),
    /// Drop every document and start over. Used by `thewiki reindex` and by
    /// the startup crash-recovery path when `.last_indexed` is missing.
    /// The actual rebuild content is delivered as a follow-up series of
    /// `Upsert` jobs by whoever sent the `Rebuild` (typically the CLI).
    Rebuild,
}

/// Clonable, send-anywhere handle the API layer holds.
///
/// Constructing one without an underlying worker is fine — every method
/// becomes a no-op log line — which keeps tests that don't care about
/// search from having to spin up the worker.
#[derive(Debug, Clone)]
pub struct IndexerHandle {
    sender: Option<mpsc::Sender<IndexJob>>,
}

impl IndexerHandle {
    /// Construct a disabled handle. All jobs are silently dropped (logged at
    /// `debug!`). Use in tests that don't exercise search.
    #[must_use]
    pub fn disabled() -> Self {
        Self { sender: None }
    }

    /// `true` if the handle is connected to a running worker.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.sender.is_some()
    }

    /// Enqueue a job, returning `false` when the channel was full or the
    /// worker has shut down. Non-blocking — the API handler never waits on
    /// the indexer.
    pub fn try_send(&self, job: IndexJob) -> bool {
        let Some(sender) = self.sender.as_ref() else {
            debug!("indexer disabled, dropping job: {job:?}");
            return false;
        };
        match sender.try_send(job) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(job)) => {
                warn!(
                    "indexer channel full, dropping job: {job:?} — \
                     the index will catch up on the next `thewiki reindex`"
                );
                false
            }
            Err(mpsc::error::TrySendError::Closed(job)) => {
                warn!("indexer channel closed, dropping job: {job:?}");
                false
            }
        }
    }

    /// Convenience: schedule an upsert.
    pub fn upsert(&self, doc: PageDoc) -> bool {
        self.try_send(IndexJob::Upsert(doc))
    }

    /// Convenience: schedule a delete.
    pub fn delete(&self, page_id: PageId) -> bool {
        self.try_send(IndexJob::Delete(page_id))
    }
}

/// Owning indexer task. Spawned by [`Indexer::spawn`]; the returned
/// [`IndexerHandle`] is what the API layer wires through `AppState`.
pub struct Indexer {
    index: Arc<SearchIndex>,
    commit_interval: Duration,
    commit_batch: usize,
}

impl Indexer {
    /// Construct a new indexer driving `index`.
    #[must_use]
    pub fn new(index: Arc<SearchIndex>) -> Self {
        Self {
            index,
            commit_interval: DEFAULT_COMMIT_INTERVAL,
            commit_batch: DEFAULT_COMMIT_BATCH,
        }
    }

    /// Override the commit cadence. Lower latency vs. higher write
    /// amplification — the default is the sweet spot for a small public
    /// wiki.
    #[must_use]
    pub fn with_commit_interval(mut self, interval: Duration) -> Self {
        self.commit_interval = interval;
        self
    }

    /// Override the per-commit job-count threshold.
    #[must_use]
    pub fn with_commit_batch(mut self, batch: usize) -> Self {
        self.commit_batch = batch.max(1);
        self
    }

    /// Spawn the worker and return its handle.
    ///
    /// The worker runs until every handle clone is dropped, at which point
    /// it issues a final commit and exits. The task is `'static` so it can
    /// outlive the `Indexer` value (which is only the spawn-time builder).
    pub fn spawn(self) -> IndexerHandle {
        self.spawn_with_capacity(DEFAULT_CHANNEL_CAPACITY)
    }

    /// `spawn` with an explicit channel capacity. Useful in tests that want
    /// to force a backpressure event.
    pub fn spawn_with_capacity(self, capacity: usize) -> IndexerHandle {
        let (tx, rx) = mpsc::channel::<IndexJob>(capacity);
        let index = Arc::clone(&self.index);
        let commit_interval = self.commit_interval;
        let commit_batch = self.commit_batch;
        tokio::spawn(async move {
            if let Err(err) = run_worker(index, rx, commit_interval, commit_batch).await {
                error!(error = %err, "indexer worker exited with error");
            }
        });
        IndexerHandle { sender: Some(tx) }
    }
}

/// Inner worker loop. Pulled into a function so the spawn site stays small.
///
/// Algorithm:
///
/// 1. Wait for the next job or the commit timer, whichever fires first.
/// 2. Drain anything else immediately ready on the channel (cheap; one
///    syscall) to amortise commit work across bursts.
/// 3. If the batch is empty, sleep until the next timer tick.
/// 4. Otherwise apply every job to the writer.
/// 5. If `commit_batch` jobs accumulated since the last commit OR the
///    commit timer fired, commit.
/// 6. On success, write the `.last_indexed` marker. On failure, log + back
///    off — we deliberately do not panic, the worker stays alive.
async fn run_worker(
    index: Arc<SearchIndex>,
    mut rx: mpsc::Receiver<IndexJob>,
    commit_interval: Duration,
    commit_batch: usize,
) -> Result<(), SearchError> {
    // The writer is sync. We park it inside an `Option` so a Rebuild can
    // swap it out (delete_all_documents then a fresh writer is the canonical
    // wipe-and-rebuild dance in Tantivy).
    let writer_index = Arc::clone(&index);
    let mut writer: IndexWriter<TantivyDocument> =
        tokio::task::spawn_blocking(move || writer_index.new_writer())
            .await
            .map_err(|e| SearchError::schema(format!("writer spawn_blocking: {e}")))??;

    let mut ticker = tokio::time::interval_at(Instant::now() + commit_interval, commit_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    let mut pending_since_commit: usize = 0;
    info!(
        commit_interval_ms = commit_interval.as_millis() as u64,
        commit_batch, "search indexer worker started"
    );

    loop {
        tokio::select! {
            // Channel close: drain whatever is left, commit, exit.
            maybe_job = rx.recv() => {
                let Some(job) = maybe_job else {
                    debug!("indexer channel closed, committing and shutting down");
                    if pending_since_commit > 0 {
                        commit_and_mark(&index, &mut writer).await;
                    }
                    break;
                };
                apply_job(&index, &mut writer, job).await;
                pending_since_commit += 1;

                // Opportunistic batch drain so a burst doesn't trigger N
                // separate commits.
                while pending_since_commit < commit_batch {
                    match rx.try_recv() {
                        Ok(job) => {
                            apply_job(&index, &mut writer, job).await;
                            pending_since_commit += 1;
                        }
                        Err(_) => break,
                    }
                }

                if pending_since_commit >= commit_batch {
                    commit_and_mark(&index, &mut writer).await;
                    pending_since_commit = 0;
                }
            }
            _ = ticker.tick() => {
                if pending_since_commit > 0 {
                    commit_and_mark(&index, &mut writer).await;
                    pending_since_commit = 0;
                }
            }
        }
    }
    Ok(())
}

/// Apply a single job. Errors are logged — the worker never bubbles them up
/// because the job is already off the wire and storage is authoritative.
async fn apply_job(
    index: &Arc<SearchIndex>,
    writer: &mut IndexWriter<TantivyDocument>,
    job: IndexJob,
) {
    match job {
        IndexJob::Upsert(doc) => {
            if let Err(err) = index.upsert_on(writer, &doc) {
                error!(error = %err, page_id = %doc.page_id, "indexer upsert failed");
            }
        }
        IndexJob::Delete(page_id) => {
            if let Err(err) = index.delete_on(writer, page_id) {
                error!(error = %err, %page_id, "indexer delete failed");
            }
        }
        IndexJob::Rebuild => {
            // `delete_all_documents` is idempotent; the follow-up Upsert
            // jobs replay the authoritative state. We commit immediately
            // so the wipe is durable before content lands.
            if let Err(err) = writer.delete_all_documents() {
                error!(error = %err, "indexer rebuild wipe failed");
            } else if let Err(err) = writer.commit() {
                error!(error = %err, "indexer rebuild commit failed");
            } else if let Err(err) = index.write_last_indexed_marker() {
                error!(error = %err, "indexer rebuild marker write failed");
            } else {
                info!("indexer wiped index for rebuild");
            }
        }
    }
}

/// Commit + drop the marker. Failures are logged but never crash the worker.
async fn commit_and_mark(index: &Arc<SearchIndex>, writer: &mut IndexWriter<TantivyDocument>) {
    match writer.commit() {
        Ok(opstamp) => {
            debug!(opstamp, "indexer commit");
            if let Err(err) = index.write_last_indexed_marker() {
                warn!(error = %err, "failed to write .last_indexed marker (commit succeeded)");
            }
        }
        Err(err) => {
            error!(error = %err, "indexer commit failed; will retry on next tick");
            // Give the next attempt a moment so we don't busy-loop on a
            // persistent error (e.g. disk full).
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// One-shot convenience used by `thewiki reindex`: open the index at `path`,
/// rebuild it from the supplied iterator of [`PageDoc`]s, then close.
///
/// Synchronous-friendly — uses a fresh single-threaded writer rather than
/// going through the worker channel. Returns the number of documents
/// indexed so the CLI can print progress.
///
/// # Errors
///
/// Bubbles every Tantivy or I/O error up to the caller; partial state on
/// disk is fine because the wipe happens inside a single commit.
pub fn rebuild_into(
    path: &Path,
    docs: impl IntoIterator<Item = PageDoc>,
) -> Result<u64, SearchError> {
    let index = SearchIndex::open(path)?;
    let mut writer = index.new_writer()?;
    writer.delete_all_documents()?;
    let mut count: u64 = 0;
    for doc in docs {
        index.upsert_on(&writer, &doc)?;
        count = count.saturating_add(1);
    }
    writer.commit()?;
    index.write_last_indexed_marker()?;
    Ok(count)
}
