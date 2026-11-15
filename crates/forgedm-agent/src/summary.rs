//! Conversion from stored download rows to wire summaries (A.4).
//!
//! Only plaintext columns are used: listing never unseals a URL (ARCHITECTURE 7.5).

use forgedm_protocol::{
    DownloadError, DownloadErrorCode, DownloadState, DownloadSummary, ResumeSupport, TransferMode,
};
use forgedm_storage::{DownloadRecord, LastError};

/// The wire summary of a stored download. Live fields (`speed_bps`, `eta_ms`) are absent
/// until an engine reports them.
#[must_use]
pub fn summarize(record: &DownloadRecord) -> DownloadSummary {
    DownloadSummary {
        id: record.id,
        display_name: record.display_name.clone(),
        url_display: record.url_display.clone(),
        state: record.state,
        state_reason: record.state_reason.clone(),
        priority: record.priority,
        source: record.source,
        target_dir: record.target_dir.clone(),
        final_path: record.final_path.clone(),
        total_bytes: record.total_size,
        accepted_bytes: record.bytes_committed,
        committed_bytes: record.bytes_committed,
        speed_bps: None,
        eta_ms: None,
        transfer_mode: record.transfer_mode,
        resume: resume_support(record),
        last_error: record.last_error.as_ref().map(download_error),
        created_at_ms: millis(record.created_at_ms),
        completed_at_ms: record.completed_at_ms.map(millis),
    }
}

/// Resume reliability (ARCHITECTURE 4.5): reliable with a strong validator, best effort
/// without one, not resumable for single-stream transfers, unknown before the probe.
fn resume_support(record: &DownloadRecord) -> ResumeSupport {
    let strong = record.etag_strong == Some(true) || record.last_modified_strong == Some(true);
    match record.transfer_mode {
        None | Some(TransferMode::ZeroLength) => ResumeSupport::Unknown,
        Some(TransferMode::SingleNonresumable | TransferMode::SingleUnknownLength) => {
            ResumeSupport::NotResumable
        }
        Some(TransferMode::Segmented | TransferMode::SingleRanged) if strong => {
            ResumeSupport::Reliable
        }
        Some(
            TransferMode::Segmented
            | TransferMode::SingleRanged
            | TransferMode::SingleRangedUnknownLength,
        ) => ResumeSupport::BestEffort,
    }
}

fn download_error(stored: &LastError) -> DownloadError {
    let code = DownloadErrorCode::ALL
        .iter()
        .copied()
        .find(|candidate| candidate.as_str() == stored.code)
        .unwrap_or_else(|| {
            tracing::warn!(code = %stored.code, "unknown stored error code; using unknown");
            DownloadErrorCode::Unknown
        });
    DownloadError {
        code,
        message: stored.message.clone(),
        retryable: stored.retryable,
        details: None,
    }
}

/// A stored millisecond timestamp as the unsigned wire value (negative becomes 0).
fn millis(value: i64) -> u64 {
    u64::try_from(value).unwrap_or_else(|_| {
        tracing::warn!("negative stored timestamp; using 0");
        0
    })
}

/// Whether a state holds an active slot (A.6): `probing`, `downloading`, `pausing`.
#[must_use]
pub const fn holds_active_slot(state: DownloadState) -> bool {
    matches!(
        state,
        DownloadState::Probing | DownloadState::Downloading | DownloadState::Pausing
    )
}

#[cfg(test)]
mod tests {
    use forgedm_protocol::{ClientKind, Priority};
    use uuid::Uuid;

    use super::*;

    fn record() -> DownloadRecord {
        DownloadRecord {
            id: Uuid::nil(),
            state: DownloadState::Queued,
            state_reason: None,
            source: ClientKind::Cli,
            priority: Priority::Normal,
            queue_position: 0,
            url_display: "https://example.com/a".to_owned(),
            target_dir: r"C:\Downloads".to_owned(),
            requested_name: None,
            display_name: "a.bin".to_owned(),
            file_name: None,
            part_path: None,
            part_file_id: None,
            finalize_target: None,
            final_path: None,
            collision_policy: "rename".to_owned(),
            transfer_mode: None,
            total_size: None,
            mime: None,
            etag: None,
            etag_strong: None,
            last_modified: None,
            last_modified_strong: None,
            checksum_expected: None,
            checksum_actual: None,
            length_verified: None,
            max_connections: 4,
            rate_limit_bps: None,
            bytes_committed: 0,
            retry_count: 0,
            next_retry_at_ms: None,
            motw_status: None,
            last_error: None,
            created_at_ms: 5,
            updated_at_ms: 5,
            completed_at_ms: None,
        }
    }

    #[test]
    fn plain_columns_map_and_resume_is_unknown_before_probe() {
        let summary = summarize(&record());
        assert_eq!(summary.display_name, "a.bin");
        assert_eq!(summary.url_display, "https://example.com/a");
        assert_eq!(summary.resume, ResumeSupport::Unknown);
        assert_eq!(summary.created_at_ms, 5);
        assert_eq!(summary.accepted_bytes, summary.committed_bytes);
    }

    #[test]
    fn resume_support_follows_transfer_mode_and_validator_strength() {
        let mut r = record();
        r.transfer_mode = Some(TransferMode::Segmented);
        assert_eq!(summarize(&r).resume, ResumeSupport::BestEffort);
        r.etag_strong = Some(true);
        assert_eq!(summarize(&r).resume, ResumeSupport::Reliable);
        r.transfer_mode = Some(TransferMode::SingleRangedUnknownLength);
        assert_eq!(summarize(&r).resume, ResumeSupport::BestEffort);
        r.transfer_mode = Some(TransferMode::SingleNonresumable);
        assert_eq!(summarize(&r).resume, ResumeSupport::NotResumable);
    }

    #[test]
    fn unknown_stored_error_code_becomes_the_unknown_variant() {
        let mut r = record();
        r.last_error = Some(LastError {
            code: "disk_full".to_owned(),
            message: "no space".to_owned(),
            retryable: false,
        });
        assert_eq!(
            summarize(&r).last_error.unwrap().code,
            DownloadErrorCode::DiskFull
        );
        r.last_error = Some(LastError {
            code: "from_the_future".to_owned(),
            message: String::new(),
            retryable: true,
        });
        assert_eq!(
            summarize(&r).last_error.unwrap().code,
            DownloadErrorCode::Unknown
        );
    }

    #[test]
    fn negative_timestamps_do_not_wrap() {
        let mut r = record();
        r.created_at_ms = -1;
        assert_eq!(summarize(&r).created_at_ms, 0);
    }

    #[test]
    fn active_slot_states_are_probing_downloading_pausing() {
        assert!(holds_active_slot(DownloadState::Probing));
        assert!(holds_active_slot(DownloadState::Downloading));
        assert!(holds_active_slot(DownloadState::Pausing));
        assert!(!holds_active_slot(DownloadState::Queued));
        assert!(!holds_active_slot(DownloadState::Verifying));
    }
}
