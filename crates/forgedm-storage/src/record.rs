//! Row mapping for `downloads`: raw primitives, then validation into [`DownloadRecord`].

use forgedm_protocol::{
    ClientKind, DownloadErrorCode, DownloadState, MotwStatus, Priority, TransferMode,
};
use sqlx::FromRow;
use uuid::Uuid;

use crate::convert::{parse_enum, parse_enum_opt, to_u32, to_u64};
use crate::error::StorageError;
use crate::models::{DownloadRecord, LastError};

/// The column list of a `DownloadRecord` select (never the sealed columns). A macro so
/// callers can `concat!` it into `&'static str` SQL.
macro_rules! download_columns {
    () => {
        "id, state, state_reason, source, priority, queue_position, url_display, target_dir, \
         requested_name, display_name, file_name, part_path, part_file_id, finalize_target, \
         final_path, collision_policy, transfer_mode, total_size, mime, etag, etag_strong, \
         last_modified, last_modified_strong, checksum_expected, checksum_actual, \
         length_verified, max_connections, rate_limit_bps, bytes_committed, retry_count, \
         next_retry_at_ms, motw_status, last_error_code, last_error_message, \
         last_error_retryable, created_at_ms, updated_at_ms, completed_at_ms"
    };
}
pub(crate) use download_columns;

/// Unvalidated column values as SQLite returns them.
#[derive(Debug, FromRow)]
pub(crate) struct RawDownload {
    id: String,
    state: String,
    state_reason: Option<String>,
    source: String,
    priority: i64,
    queue_position: i64,
    url_display: String,
    target_dir: String,
    requested_name: Option<String>,
    display_name: String,
    file_name: Option<String>,
    part_path: Option<String>,
    part_file_id: Option<Vec<u8>>,
    finalize_target: Option<String>,
    final_path: Option<String>,
    collision_policy: String,
    transfer_mode: Option<String>,
    total_size: Option<i64>,
    mime: Option<String>,
    etag: Option<String>,
    etag_strong: Option<bool>,
    last_modified: Option<String>,
    last_modified_strong: Option<bool>,
    checksum_expected: Option<String>,
    checksum_actual: Option<String>,
    length_verified: Option<bool>,
    max_connections: i64,
    rate_limit_bps: Option<i64>,
    bytes_committed: i64,
    retry_count: i64,
    next_retry_at_ms: Option<i64>,
    motw_status: Option<String>,
    last_error_code: Option<String>,
    last_error_message: Option<String>,
    last_error_retryable: Option<bool>,
    created_at_ms: i64,
    updated_at_ms: i64,
    completed_at_ms: Option<i64>,
}

/// Priority stored as 2/1/0 (Appendix A.4).
pub(crate) const fn priority_to_db(priority: Priority) -> i64 {
    match priority {
        Priority::High => 2,
        Priority::Normal => 1,
        Priority::Low => 0,
    }
}

fn priority_from_db(value: i64) -> Result<Priority, StorageError> {
    match value {
        2 => Ok(Priority::High),
        1 => Ok(Priority::Normal),
        0 => Ok(Priority::Low),
        _ => Err(StorageError::InvalidValue { field: "priority" }),
    }
}

/// Parse a stored state string.
pub(crate) fn parse_state(value: &str) -> Result<DownloadState, StorageError> {
    parse_enum("state", value, DownloadState::ALL, DownloadState::as_str)
}

fn last_error(raw: &RawDownload) -> Result<Option<LastError>, StorageError> {
    let Some(code) = raw.last_error_code.as_deref() else {
        return Ok(None);
    };
    parse_enum(
        "last_error_code",
        code,
        DownloadErrorCode::ALL,
        DownloadErrorCode::as_str,
    )?;
    Ok(Some(LastError {
        code: code.to_owned(),
        message: raw.last_error_message.clone().unwrap_or_default(),
        retryable: raw.last_error_retryable.unwrap_or(false),
    }))
}

impl TryFrom<RawDownload> for DownloadRecord {
    type Error = StorageError;

    fn try_from(raw: RawDownload) -> Result<Self, StorageError> {
        let id =
            Uuid::parse_str(&raw.id).map_err(|_| StorageError::InvalidValue { field: "id" })?;
        let last_error = last_error(&raw)?;
        Ok(Self {
            id,
            state: parse_state(&raw.state)?,
            source: parse_enum("source", &raw.source, ClientKind::ALL, ClientKind::as_str)?,
            priority: priority_from_db(raw.priority)?,
            transfer_mode: parse_enum_opt(
                "transfer_mode",
                raw.transfer_mode.as_deref(),
                TransferMode::ALL,
                TransferMode::as_str,
            )?,
            motw_status: parse_enum_opt(
                "motw_status",
                raw.motw_status.as_deref(),
                MotwStatus::ALL,
                MotwStatus::as_str,
            )?,
            total_size: raw
                .total_size
                .map(|v| to_u64("total_size", v))
                .transpose()?,
            max_connections: to_u32("max_connections", raw.max_connections)?,
            rate_limit_bps: raw
                .rate_limit_bps
                .map(|v| to_u64("rate_limit_bps", v))
                .transpose()?,
            bytes_committed: to_u64("bytes_committed", raw.bytes_committed)?,
            retry_count: to_u32("retry_count", raw.retry_count)?,
            last_error,
            state_reason: raw.state_reason,
            queue_position: raw.queue_position,
            url_display: raw.url_display,
            target_dir: raw.target_dir,
            requested_name: raw.requested_name,
            display_name: raw.display_name,
            file_name: raw.file_name,
            part_path: raw.part_path,
            part_file_id: raw.part_file_id,
            finalize_target: raw.finalize_target,
            final_path: raw.final_path,
            collision_policy: raw.collision_policy,
            mime: raw.mime,
            etag: raw.etag,
            etag_strong: raw.etag_strong,
            last_modified: raw.last_modified,
            last_modified_strong: raw.last_modified_strong,
            checksum_expected: raw.checksum_expected,
            checksum_actual: raw.checksum_actual,
            length_verified: raw.length_verified,
            next_retry_at_ms: raw.next_retry_at_ms,
            created_at_ms: raw.created_at_ms,
            updated_at_ms: raw.updated_at_ms,
            completed_at_ms: raw.completed_at_ms,
        })
    }
}
