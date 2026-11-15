//! Data transfer objects shared by results and events (Appendix A.4).
//!
//! These appear in agent output, which clients parse tolerantly: no
//! `deny_unknown_fields`, so a newer agent may add fields. Optional fields are omitted
//! when absent.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::enums::{
    AgentState, ChecksumStatus, ClientKind, DownloadState, MotwStatus, Priority, ProxySource,
    ResumeSupport, TransferMode, ValidatorStrength,
};
use crate::error::DownloadError;

/// `AgentStatusInfo`: the agent's own status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentStatusInfo {
    /// Agent crate version.
    pub agent_version: String,
    /// Agent process id.
    pub agent_pid: u32,
    /// UUIDv7 generated at every agent start.
    pub agent_instance_id: Uuid,
    /// Lifecycle state.
    pub state: AgentState,
    /// Milliseconds since the agent started.
    pub uptime_ms: u64,
    /// Downloads holding an active slot.
    pub active: u32,
    /// Downloads waiting for a slot.
    pub queued: u32,
    /// Sum of current download speeds.
    pub total_speed_bps: u64,
    /// Where proxy settings came from.
    pub proxy_source: ProxySource,
    /// Absolute data directory.
    pub data_dir: String,
}

/// A download's row in lists, snapshots and change events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadSummary {
    /// Download id (UUIDv7).
    pub id: Uuid,
    /// File name shown in the UI.
    pub display_name: String,
    /// Redacted URL (no userinfo, query or fragment).
    pub url_display: String,
    /// Current state.
    pub state: DownloadState,
    /// Open-set reason (`user`, `disk_full`, `destination_unavailable`, `agent_shutdown`,
    /// `crash`, `metadata_corrupt`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_reason: Option<String>,
    /// Scheduling priority.
    pub priority: Priority,
    /// Which kind of client enqueued it.
    pub source: ClientKind,
    /// Absolute destination directory.
    pub target_dir: String,
    /// Final path once completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_path: Option<String>,
    /// Total size when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    /// Bytes received from the network and accepted by the writer.
    pub accepted_bytes: u64,
    /// Bytes durably committed to the checkpoint.
    pub committed_bytes: u64,
    /// Smoothed speed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_bps: Option<u64>,
    /// Estimated time remaining.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_ms: Option<u64>,
    /// Transfer mode once probed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transfer_mode: Option<TransferMode>,
    /// Resume reliability.
    pub resume: ResumeSupport,
    /// The most recent failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<DownloadError>,
    /// Creation time, epoch milliseconds.
    pub created_at_ms: u64,
    /// Completion time, epoch milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<u64>,
}

/// HTTP validator strengths recorded for a download.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Validators {
    /// Strength of the `ETag`.
    pub etag: ValidatorStrength,
    /// Strength of `Last-Modified`.
    pub last_modified: ValidatorStrength,
}

/// Checksum expectation and result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChecksumInfo {
    /// Expected SHA-256 (lowercase hex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    /// Computed SHA-256 (lowercase hex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
    /// Verification status.
    pub status: ChecksumStatus,
}

/// One byte range of a segmented download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentInfo {
    /// Segment number.
    pub seg_no: u32,
    /// First byte (inclusive).
    pub start: u64,
    /// Last byte (inclusive) when the length is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<u64>,
    /// Bytes committed within the segment.
    pub committed: u64,
    /// Whether a worker is transferring the segment.
    pub active: bool,
    /// Segment speed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_bps: Option<u64>,
}

/// A redacted entry of a download's recent event log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentEvent {
    /// Event time, epoch milliseconds.
    pub at_ms: u64,
    /// Short machine kind.
    pub kind: String,
    /// Redacted description.
    pub message: String,
}

/// `DownloadDetail`: a [`DownloadSummary`] plus transfer internals.
///
/// The summary is flattened so the wire object is one flat map. This type is only ever
/// parsed from agent output (tolerant), never from client requests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadDetail {
    /// The summary fields.
    #[serde(flatten)]
    pub summary: DownloadSummary,
    /// Redacted URL after redirects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_url_display: Option<String>,
    /// MIME type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    /// Validator strengths.
    pub validators: Validators,
    /// Checksum state.
    pub checksum: ChecksumInfo,
    /// Whether the final length matched the expectation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length_verified: Option<bool>,
    /// Segment plan.
    pub segments: Vec<SegmentInfo>,
    /// Consecutive retry count.
    pub retry_count: u32,
    /// When the next automatic retry fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_retry_at_ms: Option<u64>,
    /// Mark-of-the-Web status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motw_status: Option<MotwStatus>,
    /// At most 20 redacted recent events.
    pub recent_events: Vec<RecentEvent>,
}

/// One row of a `ProgressSnapshot` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressItem {
    /// Download id.
    pub id: Uuid,
    /// Accepted bytes.
    pub accepted_bytes: u64,
    /// Committed bytes.
    pub committed_bytes: u64,
    /// Total size when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    /// Smoothed speed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_bps: Option<u64>,
    /// Estimated time remaining.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_ms: Option<u64>,
    /// Open connections.
    pub active_connections: u32,
}
