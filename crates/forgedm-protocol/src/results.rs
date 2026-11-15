//! Action results (Appendix A.2, A.3). Clients are tolerant: unknown result fields are
//! ignored, so a newer agent may add fields within v1.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::dto::{AgentStatusInfo, DownloadSummary};
use crate::enums::DownloadState;

/// Result of actions that return `{}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmptyResult {}

/// Limits advertised in the handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandshakeLimits {
    /// Largest frame body the agent accepts.
    pub max_frame_bytes: u32,
    /// Largest `ListDownloads` page.
    pub max_list_page: u32,
}

/// `Handshake` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandshakeResult {
    /// Negotiated major version (always 1).
    pub protocol_version: u32,
    /// Agent's minor version within the major.
    pub protocol_minor: u32,
    /// Agent crate version.
    pub agent_version: String,
    /// Agent process id.
    pub agent_pid: u32,
    /// UUIDv7 generated at every agent start.
    pub agent_instance_id: Uuid,
    /// Implemented optional features (`segmented`, `resume`, `rate_limit`,
    /// `checksum_sha256`, `subscribe_replay`); unknown names are ignored.
    pub capabilities: Vec<String>,
    /// Server limits.
    pub limits: HandshakeLimits,
}

/// `Ping` result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PingResult {
    /// Agent wall clock, epoch milliseconds.
    pub agent_time_ms: u64,
    /// Milliseconds since the agent started.
    pub uptime_ms: u64,
}

/// `GetSnapshot` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotResult {
    /// The agent instance this snapshot belongs to.
    pub agent_instance_id: Uuid,
    /// Event sequence number the snapshot is consistent with.
    pub seq: u64,
    /// Agent status.
    pub agent_status: AgentStatusInfo,
    /// At most 1000 downloads, newest first.
    pub downloads: Vec<DownloadSummary>,
    /// More downloads exist than were included.
    pub truncated: bool,
}

/// `Subscribe` result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscribeResult {
    /// The hub's current sequence number.
    pub current_seq: u64,
}

/// `ListDownloads` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListDownloadsResult {
    /// The requested page.
    pub items: Vec<DownloadSummary>,
    /// Total rows matching the filter.
    pub total: u32,
}

/// `EnqueueUrl` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnqueueUrlResult {
    /// The new download's id.
    pub download_id: Uuid,
    /// Its initial state.
    pub state: DownloadState,
    /// The sanitized file name chosen.
    pub file_name: String,
}

/// Result of `Pause`, `Resume`, `Cancel` and `Retry`: the resulting state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateResult {
    /// State after the command.
    pub state: DownloadState,
}

/// `PauseAll` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PauseAllResult {
    /// Downloads that were paused.
    pub paused: Vec<Uuid>,
    /// Active non-resumable downloads that were skipped.
    pub skipped_not_resumable: Vec<Uuid>,
}

/// `ResumeAll` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeAllResult {
    /// Downloads that were resumed.
    pub resumed: Vec<Uuid>,
}

/// The limits in force after `UpdateLimits`. Absent fields are unlimited or not
/// applicable to the scope.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveLimits {
    /// Connection cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_connections: Option<u32>,
    /// Rate cap; absent means unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_bps: Option<u64>,
    /// Active download cap (global scope only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_active_downloads: Option<u32>,
}

/// `UpdateLimits` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateLimitsResult {
    /// The limits now in force.
    pub effective: EffectiveLimits,
}
