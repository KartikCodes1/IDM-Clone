//! Plain data types exchanged with the repositories.

use std::fmt;

use forgedm_protocol::{ClientKind, DownloadState, MotwStatus, Priority, TransferMode};
use uuid::Uuid;

/// A download to insert. It always starts `queued`.
#[derive(Clone)]
pub struct NewDownload {
    /// UUIDv7 id.
    pub id: Uuid,
    /// Handshake client kind that created it.
    pub source: ClientKind,
    /// Scheduling priority.
    pub priority: Priority,
    /// Queue position (gaps of 1024).
    pub queue_position: i64,
    /// The plaintext URL; sealed before it reaches the database.
    pub original_url: String,
    /// Redacted display form; stored in plaintext.
    pub url_display: String,
    /// Destination directory.
    pub target_dir: String,
    /// File name the user asked for.
    pub requested_name: Option<String>,
    /// Name shown in lists.
    pub display_name: String,
    /// Collision policy (default `rename`).
    pub collision_policy: String,
    /// Per-download connection cap, `1..=16`.
    pub max_connections: u32,
    /// Creation time, ms since the epoch.
    pub created_at_ms: i64,
}

impl fmt::Debug for NewDownload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewDownload")
            .field("id", &self.id)
            .field("original_url", &"[redacted]")
            .field("url_display", &self.url_display)
            .finish_non_exhaustive()
    }
}

/// The last recorded error of a download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastError {
    /// `DownloadErrorCode` wire string.
    pub code: String,
    /// Redacted message.
    pub message: String,
    /// Whether the error is retryable.
    pub retryable: bool,
}

/// A download row without its sealed columns. Loading never unseals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadRecord {
    /// Id.
    pub id: Uuid,
    /// Current state (validated on load).
    pub state: DownloadState,
    /// Open-set reason string.
    pub state_reason: Option<String>,
    /// Client kind that created it.
    pub source: ClientKind,
    /// Priority.
    pub priority: Priority,
    /// Queue position.
    pub queue_position: i64,
    /// Redacted URL.
    pub url_display: String,
    /// Destination directory.
    pub target_dir: String,
    /// Requested file name.
    pub requested_name: Option<String>,
    /// Display name.
    pub display_name: String,
    /// Resolved file name.
    pub file_name: Option<String>,
    /// Staging `.part` path.
    pub part_path: Option<String>,
    /// NTFS file id of the `.part` file.
    pub part_file_id: Option<Vec<u8>>,
    /// Chosen final path while finalizing.
    pub finalize_target: Option<String>,
    /// Final path once completed.
    pub final_path: Option<String>,
    /// Collision policy.
    pub collision_policy: String,
    /// Transfer mode once probed.
    pub transfer_mode: Option<TransferMode>,
    /// Total size when known.
    pub total_size: Option<u64>,
    /// MIME type.
    pub mime: Option<String>,
    /// ETag value.
    pub etag: Option<String>,
    /// Whether the ETag is strong.
    pub etag_strong: Option<bool>,
    /// Last-Modified value.
    pub last_modified: Option<String>,
    /// Whether Last-Modified is a strong validator.
    pub last_modified_strong: Option<bool>,
    /// Expected SHA-256, lowercase hex.
    pub checksum_expected: Option<String>,
    /// Computed SHA-256, hex.
    pub checksum_actual: Option<String>,
    /// Whether the final length was verified.
    pub length_verified: Option<bool>,
    /// Per-download connection cap.
    pub max_connections: u32,
    /// Per-download rate limit, bytes per second.
    pub rate_limit_bps: Option<u64>,
    /// Bytes durably committed.
    pub bytes_committed: u64,
    /// Retry counter.
    pub retry_count: u32,
    /// When the next retry is due.
    pub next_retry_at_ms: Option<i64>,
    /// Mark-of-the-Web outcome.
    pub motw_status: Option<MotwStatus>,
    /// Last error.
    pub last_error: Option<LastError>,
    /// Creation time.
    pub created_at_ms: i64,
    /// Last update time.
    pub updated_at_ms: i64,
    /// Completion time.
    pub completed_at_ms: Option<i64>,
}

/// Probe-derived columns written by `Downloads::save_probe`.
///
/// `final_url`, `file_name`, `part_path` and `part_file_id` keep their stored value when
/// `None`; every other field overwrites the column.
#[derive(Clone, Default)]
pub struct ProbeResult {
    /// Chosen transfer mode.
    pub transfer_mode: Option<TransferMode>,
    /// Total size when known.
    pub total_size: Option<u64>,
    /// MIME type.
    pub mime: Option<String>,
    /// Final URL after redirects; sealed before storage.
    pub final_url: Option<String>,
    /// Resolved file name.
    pub file_name: Option<String>,
    /// Staging `.part` path.
    pub part_path: Option<String>,
    /// NTFS file id of the `.part` file.
    pub part_file_id: Option<Vec<u8>>,
    /// ETag value.
    pub etag: Option<String>,
    /// Whether the ETag is strong.
    pub etag_strong: Option<bool>,
    /// Last-Modified value.
    pub last_modified: Option<String>,
    /// Whether Last-Modified is strong.
    pub last_modified_strong: Option<bool>,
}

impl fmt::Debug for ProbeResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProbeResult")
            .field("transfer_mode", &self.transfer_mode)
            .field("total_size", &self.total_size)
            .field("final_url", &self.final_url.as_ref().map(|_| "[redacted]"))
            .finish_non_exhaustive()
    }
}

/// An event to append to `download_events`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEvent {
    /// Event time, ms since the epoch.
    pub at_ms: i64,
    /// Event kind.
    pub kind: String,
    /// Optional error or reason code.
    pub code: Option<String>,
    /// Optional JSON detail, at most 4 KiB, already redacted.
    pub detail_json: Option<String>,
}

impl NewEvent {
    /// An event with no code or detail.
    #[must_use]
    pub fn new(at_ms: i64, kind: impl Into<String>) -> Self {
        Self {
            at_ms,
            kind: kind.into(),
            code: None,
            detail_json: None,
        }
    }
}

/// A stored event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRecord {
    /// Row id (monotonic).
    pub id: i64,
    /// Event time.
    pub at_ms: i64,
    /// Event kind.
    pub kind: String,
    /// State before, for transitions.
    pub from_state: Option<DownloadState>,
    /// State after, for transitions.
    pub to_state: Option<DownloadState>,
    /// Optional code.
    pub code: Option<String>,
    /// Optional JSON detail.
    pub detail_json: Option<String>,
}

/// One persisted segment. `end` is exclusive; `None` is an unknown-length single stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentRecord {
    /// Segment number within the download.
    pub seg_no: u32,
    /// First byte.
    pub start: u64,
    /// One past the last byte.
    pub end: Option<u64>,
    /// Next byte to fetch (everything below is committed).
    pub committed: u64,
    /// Retry counter.
    pub retry_count: u32,
    /// Last update time.
    pub updated_at_ms: i64,
}
