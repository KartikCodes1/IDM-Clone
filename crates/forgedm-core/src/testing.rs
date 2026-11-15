//! In-memory fakes for the engine ports (feature `test-support`).
//!
//! [`MemoryStore`] implements [`DownloadStore`] over a map with the same compare-and-set
//! behaviour as the real store, and [`RecordingSink`] records every [`EngineEvent`]. This
//! file only names `forgedm_core::` paths, so integration tests can also include it with
//! `#[path]` without enabling the feature.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test support fails by panicking

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use forgedm_core::clock::BoxFuture;
use forgedm_core::error::StoreError;
use forgedm_core::ports::{
    DownloadStore, EngineEvent, EngineEventSink, NewJob, ProbeFacts, StoreEvent, StoreFuture,
    StoredJob, StoredProgress, unix_ms,
};
use forgedm_core::transfer::segment_table::SegmentSnapshot;
use forgedm_protocol::{DownloadState, TransferMode};
use tokio::sync::Notify;
use uuid::Uuid;

/// What the store kept for one download.
#[derive(Debug, Clone)]
pub struct Row {
    /// The job as the engine sees it.
    pub job: StoredJob,
    /// Transfer mode and total from the probe, once saved.
    pub probe: Option<(TransferMode, Option<u64>)>,
    /// The file name chosen at probe time.
    pub file_name: Option<String>,
    /// Every checkpoint written, as `(committed, total)`.
    pub checkpoints: Vec<(u64, Option<u64>)>,
    /// The segment table last checkpointed (segmented downloads).
    pub segments: Vec<SegmentSnapshot>,
    /// Every segment table checkpointed, oldest first.
    pub segment_checkpoints: Vec<Vec<SegmentSnapshot>>,
    /// Every event, oldest first.
    pub events: Vec<StoreEvent>,
    /// Every rename candidate persisted.
    pub finalize_targets: Vec<PathBuf>,
    /// `(final path, sha256, length verified)` once completed.
    pub completion: Option<(PathBuf, String, bool)>,
    /// Times progress was reset.
    pub resets: u32,
    /// Insertion order.
    pub created_at_ms: i64,
}

/// An in-memory [`DownloadStore`].
#[derive(Default)]
pub struct MemoryStore {
    rows: Mutex<HashMap<Uuid, Row>>,
}

impl MemoryStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn rows(&self) -> MutexGuard<'_, HashMap<Uuid, Row>> {
        self.rows.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A copy of one row.
    #[must_use]
    pub fn row(&self, id: Uuid) -> Option<Row> {
        self.rows().get(&id).cloned()
    }

    /// The stored state of `id`.
    #[must_use]
    pub fn state(&self, id: Uuid) -> Option<DownloadState> {
        self.rows().get(&id).map(|r| r.job.state)
    }

    /// Inserts a row directly in any state (to stage recovery scenarios).
    pub fn seed(&self, job: StoredJob) {
        let row = Row {
            job: job.clone(),
            probe: None,
            file_name: job.file_name.clone(),
            checkpoints: Vec::new(),
            segments: Vec::new(),
            segment_checkpoints: Vec::new(),
            events: Vec::new(),
            finalize_targets: Vec::new(),
            completion: None,
            resets: 0,
            created_at_ms: unix_ms(),
        };
        self.rows().insert(job.id, row);
    }

    /// Waits until `id` is in a state accepted by `done`, up to `limit`.
    ///
    /// # Panics
    /// When the deadline passes first (a test failure).
    pub async fn wait_state(
        &self,
        id: Uuid,
        done: impl Fn(DownloadState) -> bool,
        limit: Duration,
    ) -> DownloadState {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            if let Some(state) = self.state(id).filter(|s| done(*s)) {
                return state;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out; state is {:?}",
                self.state(id)
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

impl DownloadStore for MemoryStore {
    fn insert(&self, new: NewJob) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let job = StoredJob {
                id: new.id,
                state: DownloadState::Queued,
                state_reason: None,
                url: new.url,
                target_dir: new.target_dir,
                requested_name: new.requested_name,
                display_name: new.display_name,
                file_name: None,
                expected_sha256: new.expected_sha256,
                part_path: None,
                part_file_id: None,
                transfer_mode: None,
                progress: StoredProgress::default(),
                max_connections: new.max_connections,
            };
            self.seed(job);
            Ok(())
        })
    }

    fn load(&self, id: Uuid) -> StoreFuture<'_, Option<StoredJob>> {
        Box::pin(async move { Ok(self.row(id).map(|r| r.job)) })
    }

    fn startup_work(&self) -> StoreFuture<'_, Vec<StoredJob>> {
        Box::pin(async move {
            let mut rows: Vec<Row> = self.rows().values().cloned().collect();
            rows.sort_by_key(|r| r.created_at_ms);
            Ok(rows
                .into_iter()
                .map(|r| r.job)
                .filter(|j| matches!(j.state, DownloadState::Queued | DownloadState::Interrupted))
                .collect())
        })
    }

    fn count(&self) -> StoreFuture<'_, usize> {
        Box::pin(async move { Ok(self.rows().len()) })
    }

    fn transition(
        &self,
        id: Uuid,
        from: DownloadState,
        to: DownloadState,
        reason: Option<String>,
        event: StoreEvent,
    ) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut rows = self.rows();
            let row = rows.get_mut(&id).ok_or(StoreError::NotFound)?;
            if row.job.state != from {
                return Err(StoreError::Conflict {
                    expected: from,
                    actual: Some(row.job.state),
                });
            }
            row.job.state = to;
            row.job.state_reason = reason;
            row.events.push(event);
            Ok(())
        })
    }

    fn save_probe(&self, id: Uuid, facts: ProbeFacts) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut rows = self.rows();
            let row = rows.get_mut(&id).ok_or(StoreError::NotFound)?;
            row.probe = Some((facts.transfer_mode, facts.total));
            row.file_name = Some(facts.file_name.clone());
            row.job.file_name = Some(facts.file_name);
            row.job.transfer_mode = Some(facts.transfer_mode);
            row.job.part_path = Some(facts.part_path);
            row.job.part_file_id = facts.part_file_id;
            row.job.progress = StoredProgress {
                total: facts.total,
                committed: 0,
                etag: facts.etag,
                etag_strong: facts.etag_strong,
                last_modified: facts.last_modified,
                last_modified_strong: facts.last_modified_strong,
            };
            Ok(())
        })
    }

    fn checkpoint(&self, id: Uuid, committed: u64, total: Option<u64>) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut rows = self.rows();
            let row = rows.get_mut(&id).ok_or(StoreError::NotFound)?;
            row.checkpoints.push((committed, total));
            row.job.progress.committed = committed;
            Ok(())
        })
    }

    fn checkpoint_segments(&self, id: Uuid, segments: Vec<SegmentSnapshot>) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut rows = self.rows();
            let row = rows.get_mut(&id).ok_or(StoreError::NotFound)?;
            let committed: u64 = segments.iter().map(|s| s.committed - s.start).sum();
            row.checkpoints.push((committed, row.job.progress.total));
            row.job.progress.committed = committed;
            row.segment_checkpoints.push(segments.clone());
            row.segments = segments;
            Ok(())
        })
    }

    fn load_segments(&self, id: Uuid) -> StoreFuture<'_, Vec<SegmentSnapshot>> {
        Box::pin(async move {
            let rows = self.rows();
            Ok(rows
                .get(&id)
                .map(|r| r.segments.clone())
                .unwrap_or_default())
        })
    }

    fn reset_progress(&self, id: Uuid) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut rows = self.rows();
            let row = rows.get_mut(&id).ok_or(StoreError::NotFound)?;
            row.checkpoints.clear();
            row.segments.clear();
            row.segment_checkpoints.clear();
            row.resets += 1;
            row.job.progress = StoredProgress::default();
            row.job.part_path = None;
            row.job.part_file_id = None;
            row.job.transfer_mode = None;
            Ok(())
        })
    }

    fn remove(&self, id: Uuid) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.rows()
                .remove(&id)
                .map(|_| ())
                .ok_or(StoreError::NotFound)
        })
    }

    fn set_finalize_target(&self, id: Uuid, target: &std::path::Path) -> StoreFuture<'_, ()> {
        let target = target.to_path_buf();
        Box::pin(async move {
            let mut rows = self.rows();
            let row = rows.get_mut(&id).ok_or(StoreError::NotFound)?;
            if row.job.state != DownloadState::Finalizing {
                return Err(StoreError::Conflict {
                    expected: DownloadState::Finalizing,
                    actual: Some(row.job.state),
                });
            }
            row.finalize_targets.push(target);
            Ok(())
        })
    }

    fn complete(
        &self,
        id: Uuid,
        final_path: &std::path::Path,
        sha256: &str,
        length_verified: bool,
    ) -> StoreFuture<'_, ()> {
        let (final_path, sha256) = (final_path.to_path_buf(), sha256.to_owned());
        Box::pin(async move {
            let mut rows = self.rows();
            let row = rows.get_mut(&id).ok_or(StoreError::NotFound)?;
            if row.job.state != DownloadState::Finalizing {
                return Err(StoreError::Conflict {
                    expected: DownloadState::Finalizing,
                    actual: Some(row.job.state),
                });
            }
            row.job.state = DownloadState::Completed;
            row.completion = Some((final_path, sha256, length_verified));
            Ok(())
        })
    }

    fn append_event(&self, id: Uuid, event: StoreEvent) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut rows = self.rows();
            let row = rows.get_mut(&id).ok_or(StoreError::NotFound)?;
            row.events.push(event);
            Ok(())
        })
    }
}

/// Records every engine event.
#[derive(Default)]
pub struct RecordingSink {
    events: Mutex<Vec<EngineEvent>>,
    changed: Notify,
}

impl RecordingSink {
    /// An empty sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything recorded so far, oldest first.
    #[must_use]
    pub fn events(&self) -> Vec<EngineEvent> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn push(&self, event: EngineEvent) {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(event);
        self.changed.notify_waiters();
    }

    /// Waits for the first event of download `id` that is `Completed` or `Failed`.
    ///
    /// # Panics
    /// When none arrives within `limit` (a test failure).
    pub async fn wait_terminal(&self, id: Uuid, limit: Duration) -> EngineEvent {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            let found = self.events().into_iter().find(|e| {
                matches!(e, EngineEvent::Completed { id: i, .. } | EngineEvent::Failed { id: i, .. } if *i == id)
            });
            if let Some(event) = found {
                return event;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "no terminal event within {limit:?}"
            );
            let _ = tokio::time::timeout(Duration::from_millis(50), self.changed.notified()).await;
        }
    }
}

impl EngineEventSink for RecordingSink {
    fn emit(&self, event: EngineEvent) -> BoxFuture<'_, ()> {
        Box::pin(async move { self.push(event) })
    }

    fn try_emit(&self, event: EngineEvent) -> bool {
        self.push(event);
        true
    }
}
