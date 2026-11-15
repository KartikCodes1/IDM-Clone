//! Request payloads (Appendix A.2, A.3). The agent is strict: every payload rejects
//! unknown fields. Defaults are always serialized so golden files are canonical.

use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use crate::enums::{ClientKind, DownloadState, Priority};

/// Distinguishes an absent field (`None`) from an explicit `null` (`Some(None)`).
pub(crate) fn present_or_null<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// The empty payload `{}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Empty {}

/// `Handshake` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandshakeRequest {
    /// `cli` or `desktop` (`native_host` is reserved for Phase 3).
    pub client_kind: ClientKind,
    /// At most 64 characters.
    pub client_name: String,
    /// At most 32 characters.
    pub client_version: String,
    /// Lowest protocol major the client speaks.
    pub min_protocol: u32,
    /// Highest protocol major the client speaks.
    pub max_protocol: u32,
}

/// `Subscribe` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscribeRequest {
    /// The agent instance the snapshot came from.
    pub agent_instance_id: Uuid,
    /// Replay events after this sequence number.
    pub after_seq: u64,
}

/// `ListDownloads` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListDownloadsRequest {
    /// Restrict to these states.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub states: Option<Vec<DownloadState>>,
    /// Page size, `1..=500`.
    pub limit: u32,
    /// Rows to skip.
    #[serde(default)]
    pub offset: u32,
}

/// `EnqueueUrl` payload. `Debug` redacts the URL, which may carry credentials.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnqueueUrlRequest {
    /// Source URL, at most 8192 bytes.
    pub url: String,
    /// Absolute destination directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_dir: Option<String>,
    /// Requested file name, at most 255 UTF-16 units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
    /// 64 hex characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_sha256: Option<String>,
    /// `1..=16`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_connections: Option<u32>,
    /// Absent: default; `null`: unlimited; number: at least 1024.
    #[serde(
        default,
        deserialize_with = "present_or_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub rate_limit_bps: Option<Option<u64>>,
    /// Scheduling priority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<Priority>,
    /// Enqueue in `paused`.
    #[serde(default)]
    pub start_paused: bool,
}

impl std::fmt::Debug for EnqueueUrlRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnqueueUrlRequest")
            .field("url", &"<redacted>")
            .field("target_dir", &self.target_dir)
            .field("file_name", &self.file_name)
            .field("expected_sha256", &self.expected_sha256)
            .field("max_connections", &self.max_connections)
            .field("rate_limit_bps", &self.rate_limit_bps)
            .field("priority", &self.priority)
            .field("start_paused", &self.start_paused)
            .finish()
    }
}

impl EnqueueUrlRequest {
    /// A request for `url` with every option at its default.
    #[must_use]
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            target_dir: None,
            file_name: None,
            expected_sha256: None,
            max_connections: None,
            rate_limit_bps: None,
            priority: None,
            start_paused: false,
        }
    }

    /// `expected_sha256` lowercased (the normalized form the agent stores).
    #[must_use]
    pub fn normalized_sha256(&self) -> Option<String> {
        self.expected_sha256.as_deref().map(str::to_ascii_lowercase)
    }
}

/// Payload of `GetDownload`, `Resume` and `Cancel`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadIdRequest {
    /// Target download.
    pub download_id: Uuid,
}

/// `Pause` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PauseRequest {
    /// Target download.
    pub download_id: Uuid,
    /// Allow pausing a non-resumable download by resetting its progress.
    #[serde(default)]
    pub discard_progress: bool,
}

/// `Retry` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryRequest {
    /// Target download.
    pub download_id: Uuid,
    /// Reset segments, truncate the part file and clear validators.
    #[serde(default)]
    pub restart_from_zero: bool,
}

/// `Remove` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoveRequest {
    /// Target download.
    pub download_id: Uuid,
    /// Also delete the recorded `.part` file.
    #[serde(default)]
    pub delete_partial_file: bool,
}

/// `PauseAll` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PauseAllRequest {
    /// Also pause non-resumable downloads, resetting their progress.
    #[serde(default)]
    pub discard_progress: bool,
}

/// `UpdatePriority` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdatePriorityRequest {
    /// Target download.
    pub download_id: Uuid,
    /// New priority.
    pub priority: Priority,
}

/// Wire form of [`LimitScope`]: `"global"` or `{"download_id": id}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum ScopeRepr {
    Global(GlobalTag),
    Download(DownloadScope),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum GlobalTag {
    #[serde(rename = "global")]
    Global,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DownloadScope {
    download_id: Uuid,
}

/// Which limits an `UpdateLimits` call changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "ScopeRepr", into = "ScopeRepr")]
pub enum LimitScope {
    /// Agent-wide limits.
    Global,
    /// One download's limits.
    Download {
        /// Target download.
        download_id: Uuid,
    },
}

impl From<ScopeRepr> for LimitScope {
    fn from(repr: ScopeRepr) -> Self {
        match repr {
            ScopeRepr::Global(GlobalTag::Global) => Self::Global,
            ScopeRepr::Download(DownloadScope { download_id }) => Self::Download { download_id },
        }
    }
}

impl From<LimitScope> for ScopeRepr {
    fn from(scope: LimitScope) -> Self {
        match scope {
            LimitScope::Global => Self::Global(GlobalTag::Global),
            LimitScope::Download { download_id } => Self::Download(DownloadScope { download_id }),
        }
    }
}

/// `UpdateLimits` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateLimitsRequest {
    /// Global or per-download.
    pub scope: LimitScope,
    /// `1..=16`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_connections: Option<u32>,
    /// Absent: unchanged; `null`: unlimited; number: at least 1024.
    #[serde(
        default,
        deserialize_with = "present_or_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub rate_limit_bps: Option<Option<u64>>,
    /// `1..=10`, global scope only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_active_downloads: Option<u32>,
}

impl UpdateLimitsRequest {
    /// A request for `scope` that changes nothing yet.
    #[must_use]
    pub fn new(scope: LimitScope) -> Self {
        Self {
            scope,
            max_connections: None,
            rate_limit_bps: None,
            max_active_downloads: None,
        }
    }
}
