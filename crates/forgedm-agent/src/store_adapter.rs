//! The core `DownloadStore` port over `forgedm-storage` (ARCHITECTURE 4.1).
//!
//! The expected SHA-256 lives in the `downloads.checksum_expected` column, written in the same
//! transaction as the insert, so the 200-event retention can never prune it. (S1.7 kept it in
//! the `enqueued` event; rows written that way are still read, as a fallback.)
//!
//! The last failure travels as the serialised `DownloadError` in the `detail_json` of the
//! `failed`, `needs_credentials` and `paused` events and is read back from there.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use forgedm_core::error::StoreError;
use forgedm_core::ports::{
    DownloadStore, NewJob, ProbeFacts, StoreEvent, StoreFuture, StoredJob, StoredProgress, unix_ms,
};
use forgedm_core::transfer::segment_table::SegmentSnapshot;
use forgedm_core::url_policy::{SecretUrl, validate_user_url};
use forgedm_protocol::{DownloadError, DownloadState};
use forgedm_storage::{
    DownloadRecord, EventRecord, NewDownload, NewEvent, ProbeResult, SegmentRecord, Storage,
    StorageError,
};
use uuid::Uuid;

/// Event kind that carries the expected checksum.
pub const ENQUEUED_EVENT: &str = "enqueued";
/// Rows fetched per page when scanning for startup work.
const PAGE: u32 = 500;
/// Most events read back when looking for the checksum or the last failure.
const EVENT_SCAN: u32 = 500;
/// Event kinds whose `detail_json` is a serialised [`DownloadError`].
const FAILURE_KINDS: [&str; 5] = [
    "failed",
    "needs_credentials",
    "needs_url_refresh",
    "retry_waiting",
    "paused",
];

/// Maps a storage failure onto the port's error type.
fn map_error(err: StorageError) -> StoreError {
    match err {
        StorageError::NotFound { .. } => StoreError::NotFound,
        StorageError::Conflict {
            expected, actual, ..
        } => StoreError::Conflict { expected, actual },
        StorageError::Crypto(_) => StoreError::SecretsUnavailable,
        other => StoreError::Unavailable(other.to_string()),
    }
}

fn new_event(event: &StoreEvent) -> NewEvent {
    NewEvent {
        at_ms: event.at_ms,
        kind: event.kind.clone(),
        code: event.code.clone(),
        detail_json: event.detail.clone(),
    }
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// The expected checksum recorded with the `enqueued` event (the S1.7 location; only read
/// as a fallback for rows that predate the column).
#[must_use]
pub fn expected_sha_from_events(events: &[EventRecord]) -> Option<String> {
    events
        .iter()
        .find(|e| e.kind == ENQUEUED_EVENT)
        .and_then(|e| e.detail_json.as_deref())
        .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
        .and_then(|value| {
            value
                .get("expected_sha256")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        })
}

/// The expected checksum of a stored download: the column when set, else (rows written
/// before the column was used) the `enqueued` event, failing closed on an unreadable detail.
///
/// # Errors
/// [`StoreError::Unavailable`] when the column is empty and the legacy event detail exists
/// but cannot be read.
pub fn expected_sha_of(
    column: Option<&str>,
    events: &[EventRecord],
) -> Result<Option<String>, StoreError> {
    if let Some(sha) = column {
        return Ok(Some(sha.to_owned()));
    }
    let Some(json) = events
        .iter()
        .find(|e| e.kind == ENQUEUED_EVENT)
        .and_then(|e| e.detail_json.as_deref())
    else {
        return Ok(None);
    };
    let value: serde_json::Value = serde_json::from_str(json).map_err(|_| {
        tracing::error!("the enqueued event detail is unreadable");
        StoreError::Unavailable("the checksum expectation is unreadable".to_owned())
    })?;
    Ok(value
        .get("expected_sha256")
        .and_then(|v| v.as_str())
        .map(str::to_owned))
}

/// The newest recorded failure, from the event log (newest first).
#[must_use]
pub fn failure_from_events(events: &[EventRecord]) -> Option<DownloadError> {
    events
        .iter()
        .filter(|e| FAILURE_KINDS.contains(&e.kind.as_str()))
        .find_map(|e| {
            e.detail_json
                .as_deref()
                .and_then(|json| serde_json::from_str::<DownloadError>(json).ok())
        })
}

/// [`DownloadStore`] over the SQLite storage.
#[derive(Debug, Clone)]
pub struct StorageStore {
    storage: Storage,
    count: Arc<AtomicUsize>,
}

impl StorageStore {
    /// A store over `storage`; `existing` is how many downloads are already stored.
    #[must_use]
    pub fn new(storage: Storage, existing: usize) -> Self {
        Self {
            storage,
            count: Arc::new(AtomicUsize::new(existing)),
        }
    }

    async fn events_of(&self, id: Uuid) -> Result<Vec<EventRecord>, StoreError> {
        self.storage
            .events()
            .list(id, EVENT_SCAN)
            .await
            .map_err(map_error)
    }

    async fn job_from(&self, record: DownloadRecord) -> Result<StoredJob, StoreError> {
        let id = record.id;
        let sealed = self
            .storage
            .downloads()
            .original_url(id)
            .await
            .map_err(map_error)?;
        let url: SecretUrl =
            validate_user_url(sealed.expose()).map_err(|_| StoreError::SecretsUnavailable)?;
        // The column is authoritative; the event log is only consulted for legacy rows.
        let expected_sha256 = match record.checksum_expected.as_deref() {
            Some(sha) => Some(sha.to_owned()),
            None => expected_sha_of(None, &self.events_of(id).await?)?,
        };
        let progress = StoredProgress {
            total: record.total_size,
            committed: record.bytes_committed,
            etag: record.etag.clone(),
            etag_strong: record.etag_strong,
            last_modified: record.last_modified.clone(),
            last_modified_strong: record.last_modified_strong,
        };
        Ok(StoredJob {
            id,
            state: record.state,
            state_reason: record.state_reason,
            url,
            target_dir: PathBuf::from(record.target_dir),
            requested_name: record.requested_name,
            display_name: record.display_name,
            file_name: record.file_name,
            expected_sha256,
            part_path: record.part_path.map(PathBuf::from),
            part_file_id: record.part_file_id,
            transfer_mode: record.transfer_mode,
            progress,
            max_connections: record.max_connections,
        })
    }

    async fn insert_row(&self, new: &NewJob) -> Result<(), StoreError> {
        let row = NewDownload {
            id: new.id,
            source: new.source,
            priority: new.priority,
            queue_position: new.created_at_ms,
            original_url: new.url.as_url().as_str().to_owned(),
            url_display: new.url.redacted().to_string(),
            target_dir: path_text(&new.target_dir),
            requested_name: new.requested_name.clone(),
            display_name: new.display_name.clone(),
            collision_policy: "rename".to_owned(),
            max_connections: new.max_connections,
            created_at_ms: new.created_at_ms,
        };
        self.storage
            .downloads()
            .insert_with_checksum(&row, new.expected_sha256.as_deref())
            .await
            .map_err(map_error)?;
        // History only: the checksum itself is already in its column.
        let event = NewEvent::new(new.created_at_ms, ENQUEUED_EVENT);
        if let Err(err) = self.storage.events().append(new.id, &event).await {
            tracing::warn!(error = %err, "could not record the enqueued event");
        }
        Ok(())
    }
}

fn segment(committed: u64, total: Option<u64>) -> SegmentRecord {
    SegmentRecord {
        seg_no: 0,
        start: 0,
        end: total,
        committed,
        retry_count: 0,
        updated_at_ms: unix_ms(),
    }
}

impl DownloadStore for StorageStore {
    fn insert(&self, new: NewJob) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.insert_row(&new).await?;
            self.count.fetch_add(1, Ordering::AcqRel);
            Ok(())
        })
    }

    fn load(&self, id: Uuid) -> StoreFuture<'_, Option<StoredJob>> {
        Box::pin(async move {
            let Some(record) = self.storage.downloads().get(id).await.map_err(map_error)? else {
                return Ok(None);
            };
            self.job_from(record).await.map(Some)
        })
    }

    fn startup_work(&self) -> StoreFuture<'_, Vec<StoredJob>> {
        Box::pin(async move {
            let wanted = [
                DownloadState::Queued,
                DownloadState::Interrupted,
                DownloadState::RetryWaiting,
            ];
            let mut records = Vec::new();
            loop {
                let offset = u32::try_from(records.len()).unwrap_or(u32::MAX);
                let page = self
                    .storage
                    .downloads()
                    .list(Some(&wanted), PAGE, offset)
                    .await
                    .map_err(map_error)?;
                let last = page.len() < usize::try_from(PAGE).unwrap_or(usize::MAX);
                records.extend(page);
                if last {
                    break;
                }
            }
            // `list` is newest first; work runs oldest first.
            records.reverse();
            let mut jobs = Vec::with_capacity(records.len());
            for record in records {
                let id = record.id;
                match self.job_from(record).await {
                    Ok(job) => jobs.push(job),
                    // One unreadable download must not stop the rest from resuming.
                    Err(err) => {
                        tracing::error!(%id, error = %err, "skipping an unreadable download")
                    }
                }
            }
            Ok(jobs)
        })
    }

    fn count(&self) -> StoreFuture<'_, usize> {
        Box::pin(async move { Ok(self.count.load(Ordering::Acquire)) })
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
            self.storage
                .downloads()
                .transition(id, from, to, reason.as_deref(), &new_event(&event))
                .await
                .map_err(map_error)
        })
    }

    fn save_probe(&self, id: Uuid, facts: ProbeFacts) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let probe = ProbeResult {
                transfer_mode: Some(facts.transfer_mode),
                total_size: facts.total,
                mime: facts.mime,
                final_url: Some(facts.final_url.as_url().as_str().to_owned()),
                file_name: Some(facts.file_name),
                part_path: Some(path_text(&facts.part_path)),
                part_file_id: facts.part_file_id,
                etag: facts.etag,
                etag_strong: facts.etag_strong,
                last_modified: facts.last_modified,
                last_modified_strong: facts.last_modified_strong,
            };
            self.storage
                .downloads()
                .save_probe(id, &probe, unix_ms())
                .await
                .map_err(map_error)
        })
    }

    fn checkpoint(&self, id: Uuid, committed: u64, total: Option<u64>) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.storage
                .segments()
                .save_checkpoint(id, committed, &[segment(committed, total)])
                .await
                .map_err(map_error)
        })
    }

    fn checkpoint_segments(&self, id: Uuid, segments: Vec<SegmentSnapshot>) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let now = unix_ms();
            let records: Vec<SegmentRecord> = segments
                .iter()
                .map(|s| SegmentRecord {
                    seg_no: s.seg_no,
                    start: s.start,
                    end: Some(s.end),
                    committed: s.committed,
                    retry_count: 0,
                    updated_at_ms: now,
                })
                .collect();
            let committed: u64 = records.iter().map(|r| r.committed - r.start).sum();
            self.storage
                .segments()
                .save_checkpoint(id, committed, &records)
                .await
                .map_err(map_error)
        })
    }

    fn load_segments(&self, id: Uuid) -> StoreFuture<'_, Vec<SegmentSnapshot>> {
        Box::pin(async move {
            let records = self.storage.segments().load(id).await.map_err(map_error)?;
            // A record without an end is an open-ended single stream, never a segmented plan.
            Ok(records
                .into_iter()
                .filter_map(|r| {
                    r.end.map(|end| SegmentSnapshot {
                        seg_no: r.seg_no,
                        start: r.start,
                        end,
                        committed: r.committed,
                    })
                })
                .collect())
        })
    }

    fn reset_progress(&self, id: Uuid) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.storage
                .downloads()
                .reset_for_restart(id, unix_ms())
                .await
                .map_err(map_error)
        })
    }

    fn remove(&self, id: Uuid) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let existed = self
                .storage
                .downloads()
                .delete(id)
                .await
                .map_err(map_error)?;
            if !existed {
                return Err(StoreError::NotFound);
            }
            // Saturating: the counter only gates the queue cap.
            let mut current = self.count.load(Ordering::Acquire);
            while current > 0 {
                match self.count.compare_exchange_weak(
                    current,
                    current - 1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => break,
                    Err(actual) => current = actual,
                }
            }
            Ok(())
        })
    }

    fn set_max_connections(&self, id: Uuid, value: u32) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.storage
                .downloads()
                .set_max_connections(id, value, unix_ms())
                .await
                .map_err(map_error)
        })
    }

    fn schedule_retry(&self, id: Uuid, at_ms: i64) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.storage
                .downloads()
                .set_next_retry_at(id, at_ms, unix_ms())
                .await
                .map_err(map_error)
        })
    }

    fn next_retry_at(&self, id: Uuid) -> StoreFuture<'_, Option<i64>> {
        Box::pin(async move {
            let record = self.storage.downloads().get(id).await.map_err(map_error)?;
            Ok(record.and_then(|r| r.next_retry_at_ms))
        })
    }

    fn set_finalize_target(&self, id: Uuid, target: &Path) -> StoreFuture<'_, ()> {
        let target = path_text(target);
        Box::pin(async move {
            self.storage
                .downloads()
                .set_finalize_target(id, &target, unix_ms())
                .await
                .map_err(map_error)
        })
    }

    fn complete(
        &self,
        id: Uuid,
        final_path: &Path,
        sha256: &str,
        length_verified: bool,
    ) -> StoreFuture<'_, ()> {
        let (final_path, sha256) = (path_text(final_path), sha256.to_owned());
        Box::pin(async move {
            self.storage
                .downloads()
                .complete(id, &final_path, &sha256, length_verified, unix_ms())
                .await
                .map_err(map_error)
        })
    }

    fn append_event(&self, id: Uuid, event: StoreEvent) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.storage
                .events()
                .append(id, &new_event(&event))
                .await
                .map_err(map_error)
        })
    }
}

#[cfg(test)]
mod tests {
    use forgedm_protocol::DownloadErrorCode;

    use super::*;

    fn event(kind: &str, detail: Option<&str>) -> EventRecord {
        EventRecord {
            id: 1,
            at_ms: 0,
            kind: kind.to_owned(),
            from_state: None,
            to_state: None,
            code: None,
            detail_json: detail.map(str::to_owned),
        }
    }

    #[test]
    fn the_expected_checksum_is_read_from_the_enqueued_event() {
        let sha = "a".repeat(64);
        let detail = format!(r#"{{"expected_sha256":"{sha}"}}"#);
        let events = vec![event("state", None), event(ENQUEUED_EVENT, Some(&detail))];
        assert_eq!(expected_sha_from_events(&events), Some(sha));
        assert_eq!(
            expected_sha_from_events(&[event(ENQUEUED_EVENT, None)]),
            None
        );
        assert_eq!(expected_sha_from_events(&[]), None);
    }

    #[test]
    fn the_column_wins_and_the_event_is_only_a_legacy_fallback() {
        let column = "b".repeat(64);
        let legacy = "c".repeat(64);
        let detail = format!(r#"{{"expected_sha256":"{legacy}"}}"#);
        let events = [event(ENQUEUED_EVENT, Some(&detail))];

        assert_eq!(
            expected_sha_of(Some(&column), &events).unwrap(),
            Some(column),
            "the column is authoritative"
        );
        assert_eq!(expected_sha_of(None, &events).unwrap(), Some(legacy));
        assert_eq!(
            expected_sha_of(None, &[]).unwrap(),
            None,
            "pruned: no event, no checksum"
        );
        assert!(expected_sha_of(None, &[event(ENQUEUED_EVENT, Some("not json"))]).is_err());
    }

    #[test]
    fn the_last_failure_is_the_newest_failure_event() {
        let newer = r#"{"code":"checksum_mismatch","message":"m","retryable":false}"#;
        let older = r#"{"code":"disk_io","message":"m","retryable":false}"#;
        let events = vec![
            event("state", None),
            event("failed", Some(newer)),
            event("failed", Some(older)),
        ];
        let error = failure_from_events(&events).unwrap();
        assert_eq!(error.code, DownloadErrorCode::ChecksumMismatch);
        assert!(failure_from_events(&[event("failed", Some("not json"))]).is_none());
    }
}
