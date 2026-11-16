//! Wire views of a stored download: the summary with live numbers overlaid, and the full
//! `DownloadDetail` of `GetDownload` (A.4).
//!
//! Everything shown here is redacted: URLs are rendered without query or userinfo, and event
//! messages are built from enum values only, never from stored detail text.

use forgedm_core::ports::ProgressSample;
use forgedm_core::url_policy::validate_user_url;
use forgedm_protocol::{
    ChecksumInfo, ChecksumStatus, DownloadDetail, DownloadErrorCode, DownloadState,
    DownloadSummary, MotwStatus, RECENT_EVENTS_MAX, RecentEvent, SegmentInfo, ValidatorStrength,
    Validators,
};
use forgedm_storage::{DownloadRecord, EventRecord, SegmentRecord, Storage, StorageError};
use serde_json::Value;
use uuid::Uuid;

use crate::error::AgentError;
use crate::store_adapter::{expected_sha_from_events, failure_from_events};
use crate::summary::{holds_active_slot, summarize};

/// Event kind written when the MOTW outcome is known.
const MOTW_EVENT: &str = "motw";
/// Event kind written on a checksum mismatch.
const CHECKSUM_EVENT: &str = "checksum";
/// Most events read for one detail view.
const EVENT_SCAN: u32 = 500;

/// The summary of `record`, with the last failure restored from its events when the record
/// carries none, and live numbers laid over while it runs.
#[must_use]
pub fn summary_of(
    record: &DownloadRecord,
    events: &[EventRecord],
    live: Option<&ProgressSample>,
) -> DownloadSummary {
    let mut summary = summarize(record);
    if summary.last_error.is_none() && wants_last_error(record.state) {
        summary.last_error = failure_from_events(events);
    }
    if let Some(sample) = live.filter(|_| holds_active_slot(record.state)) {
        summary.accepted_bytes = sample.accepted;
        summary.committed_bytes = sample.committed;
        summary.speed_bps = sample.speed_bps;
        summary.eta_ms = sample.eta_ms;
        summary.total_bytes = sample.total.or(summary.total_bytes);
    }
    summary
}

const fn wants_last_error(state: DownloadState) -> bool {
    matches!(
        state,
        DownloadState::Failed
            | DownloadState::NeedsCredentials
            | DownloadState::NeedsUrlRefresh
            | DownloadState::RetryWaiting
            | DownloadState::Paused
    )
}

/// Loads `id` and builds its summary (the engine host's source for change events).
///
/// # Errors
/// [`StorageError`] on a database failure.
pub async fn load_summary(
    storage: &Storage,
    id: Uuid,
    live: Option<&ProgressSample>,
) -> Result<Option<DownloadSummary>, StorageError> {
    let Some(record) = storage.downloads().get(id).await? else {
        return Ok(None);
    };
    let events = if wants_last_error(record.state) {
        storage.events().list(id, EVENT_SCAN).await?
    } else {
        Vec::new()
    };
    Ok(Some(summary_of(&record, &events, live)))
}

fn strength(value: Option<bool>, present: bool) -> ValidatorStrength {
    match (present, value) {
        (true, Some(true)) => ValidatorStrength::Strong,
        (true, _) => ValidatorStrength::Weak,
        (false, _) => ValidatorStrength::None,
    }
}

/// The digest from the `checksum` mismatch event, when there was one.
fn mismatch_actual(events: &[EventRecord]) -> Option<String> {
    events
        .iter()
        .find(|e| e.kind == CHECKSUM_EVENT)
        .and_then(|e| e.detail_json.as_deref())
        .and_then(|json| serde_json::from_str::<Value>(json).ok())
        .and_then(|v| v.get("actual").and_then(Value::as_str).map(str::to_owned))
}

fn checksum_info(record: &DownloadRecord, events: &[EventRecord]) -> ChecksumInfo {
    let expected = record
        .checksum_expected
        .clone()
        .or_else(|| expected_sha_from_events(events));
    let actual = record
        .checksum_actual
        .clone()
        .or_else(|| mismatch_actual(events));
    let status = match (&expected, &actual, record.state) {
        (None, _, _) => ChecksumStatus::None,
        (Some(_), Some(actual), DownloadState::Completed) if expected.as_ref() == Some(actual) => {
            ChecksumStatus::Match
        }
        (Some(_), Some(_), _) if mismatch_actual(events).is_some() => ChecksumStatus::Mismatch,
        (Some(_), _, _) => ChecksumStatus::Pending,
    };
    ChecksumInfo {
        expected,
        actual,
        status,
    }
}

fn motw_status(record: &DownloadRecord, events: &[EventRecord]) -> Option<MotwStatus> {
    record.motw_status.or_else(|| {
        events
            .iter()
            .find(|e| e.kind == MOTW_EVENT)
            .and_then(|e| e.code.as_deref())
            .and_then(|code| MotwStatus::ALL.iter().copied().find(|s| s.as_str() == code))
    })
}

/// Whether a worker is transferring `segment` right now: it has bytes left and one of the
/// engine's live `[start, end)` ranges (the unfetched part of every owned segment) overlaps
/// it. The download's state alone says nothing about this.
fn is_transferring(segment: &SegmentRecord, live_ranges: &[(u64, u64)]) -> bool {
    let end = segment.end.unwrap_or(u64::MAX);
    segment.committed < end
        && live_ranges
            .iter()
            .any(|&(from, to)| from < end && to > segment.start)
}

fn segment_info(segment: &SegmentRecord, active: bool) -> SegmentInfo {
    SegmentInfo {
        seg_no: segment.seg_no,
        start: segment.start,
        end: segment.end.and_then(|e| e.checked_sub(1)),
        committed: segment.committed.saturating_sub(segment.start),
        active,
        speed_bps: None,
    }
}

/// A redacted description of an event, built from enum values only.
fn event_message(event: &EventRecord) -> String {
    match (event.from_state, event.to_state) {
        (Some(from), Some(to)) => format!("{} -> {}", from.as_str(), to.as_str()),
        _ => match event.code.as_deref().filter(|c| is_known_code(c)) {
            Some(code) => format!("{}: {code}", event.kind),
            None => event.kind.clone(),
        },
    }
}

/// Codes are safe to show; anything else in the `code` column is withheld.
fn is_known_code(code: &str) -> bool {
    DownloadErrorCode::ALL.iter().any(|c| c.as_str() == code)
        || MotwStatus::ALL.iter().any(|s| s.as_str() == code)
        || matches!(
            code,
            "user" | "crash" | "agent_shutdown" | "interrupted" | "mismatch" | "metadata_corrupt"
        )
}

fn recent_events(events: &[EventRecord]) -> Vec<RecentEvent> {
    let mut recent: Vec<RecentEvent> = events
        .iter()
        .take(RECENT_EVENTS_MAX)
        .map(|e| RecentEvent {
            at_ms: u64::try_from(e.at_ms).unwrap_or(0),
            kind: e.kind.clone(),
            message: event_message(e),
        })
        .collect();
    recent.reverse();
    recent
}

/// Builds the `GetDownload` view of `id`.
///
/// # Errors
/// [`AgentError::Storage`] (`not_found` for an unknown id).
pub async fn build_detail(
    storage: &Storage,
    id: Uuid,
    live: Option<&ProgressSample>,
) -> Result<DownloadDetail, AgentError> {
    let record = storage
        .downloads()
        .get(id)
        .await?
        .ok_or(StorageError::NotFound { id })?;
    let events = storage.events().list(id, EVENT_SCAN).await?;
    let segments = match storage.segments().load(id).await {
        Ok(segments) => segments,
        Err(err) => {
            tracing::warn!(%id, error = %err, "segments unreadable; showing none");
            Vec::new()
        }
    };
    let final_url_display = match storage.downloads().final_url(id).await {
        Ok(url) => url
            .and_then(|u| validate_user_url(u.expose()).ok())
            .map(|u| u.redacted().to_string()),
        Err(err) => {
            tracing::warn!(%id, error = %err, "final URL unreadable; showing none");
            None
        }
    };
    let transferring = holds_active_slot(record.state);
    let ranges = live.map_or(&[][..], |sample| sample.active_ranges.as_slice());
    Ok(DownloadDetail {
        summary: summary_of(&record, &events, live),
        final_url_display,
        mime: record.mime.clone(),
        validators: Validators {
            etag: strength(record.etag_strong, record.etag.is_some()),
            last_modified: strength(record.last_modified_strong, record.last_modified.is_some()),
        },
        checksum: checksum_info(&record, &events),
        length_verified: record.length_verified,
        segments: segments
            .iter()
            .map(|s| segment_info(s, transferring && is_transferring(s, ranges)))
            .collect(),
        retry_count: record.retry_count,
        next_retry_at_ms: record
            .next_retry_at_ms
            .filter(|_| record.state == DownloadState::RetryWaiting)
            .and_then(|t| u64::try_from(t).ok()),
        motw_status: motw_status(&record, &events),
        recent_events: recent_events(&events),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: &str, code: Option<&str>) -> EventRecord {
        EventRecord {
            id: 1,
            at_ms: 5,
            kind: kind.to_owned(),
            from_state: None,
            to_state: None,
            code: code.map(str::to_owned),
            detail_json: None,
        }
    }

    #[test]
    fn event_messages_use_states_and_known_codes_only() {
        let mut moved = event("state", None);
        moved.from_state = Some(DownloadState::Queued);
        moved.to_state = Some(DownloadState::Probing);
        assert_eq!(event_message(&moved), "queued -> probing");
        assert_eq!(
            event_message(&event("failed", Some("checksum_mismatch"))),
            "failed: checksum_mismatch"
        );
        let leaked = event("failed", Some("https://secret.example/x?token=1"));
        assert_eq!(event_message(&leaked), "failed");
    }

    #[test]
    fn recent_events_are_chronological_and_capped() {
        let events: Vec<EventRecord> = (0..30)
            .map(|n| {
                let mut e = event("state", None);
                e.at_ms = 100 - n;
                e
            })
            .collect();
        let recent = recent_events(&events);
        assert_eq!(recent.len(), RECENT_EVENTS_MAX);
        assert!(recent.windows(2).all(|w| w[0].at_ms <= w[1].at_ms));
    }

    #[test]
    fn validator_strength_distinguishes_absent_weak_and_strong() {
        assert_eq!(strength(None, false), ValidatorStrength::None);
        assert_eq!(strength(Some(false), true), ValidatorStrength::Weak);
        assert_eq!(strength(Some(true), true), ValidatorStrength::Strong);
    }

    fn segment(start: u64, end: u64, committed: u64) -> SegmentRecord {
        SegmentRecord {
            seg_no: 0,
            start,
            end: Some(end),
            committed,
            retry_count: 0,
            updated_at_ms: 0,
        }
    }

    #[test]
    fn only_segments_a_worker_is_transferring_are_active() {
        const M: u64 = 1024 * 1024;
        let done = segment(0, 10 * M, 10 * M);
        let busy = segment(10 * M, 20 * M, 12 * M);
        let idle = segment(20 * M, 30 * M, 20 * M);
        // One worker is fetching the unfinished tail of the middle segment.
        let ranges = [(14 * M, 20 * M)];

        assert!(!is_transferring(&done, &ranges), "finished");
        assert!(is_transferring(&busy, &ranges), "a worker is on it");
        assert!(
            !is_transferring(&idle, &ranges),
            "unfinished but nobody on it"
        );
        assert!(!is_transferring(&busy, &[]), "no worker, not active");
    }

    #[test]
    fn a_finished_segment_is_not_active_even_if_a_stale_range_overlaps() {
        let done = segment(0, 100, 100);
        assert!(!is_transferring(&done, &[(0, 100)]));
        let open_ended = SegmentRecord {
            end: None,
            ..segment(0, 0, 40)
        };
        assert!(is_transferring(&open_ended, &[(40, u64::MAX)]));
    }

    #[test]
    fn segment_end_is_inclusive_on_the_wire() {
        let segment = SegmentRecord {
            seg_no: 0,
            start: 0,
            end: Some(100),
            committed: 40,
            retry_count: 0,
            updated_at_ms: 0,
        };
        let info = segment_info(&segment, true);
        assert_eq!((info.end, info.committed), (Some(99), 40));
    }
}
