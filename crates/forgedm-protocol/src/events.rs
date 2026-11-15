//! Server-pushed events (Appendix A.5).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::actions::take_content;
use crate::dto::{AgentStatusInfo, DownloadSummary, ProgressItem};
use crate::enums::ResyncReason;
use crate::error::DownloadError;

/// `ProgressSnapshot` data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressSnapshotData {
    /// Progress rows, coalesced at 4 Hz.
    pub items: Vec<ProgressItem>,
}

/// `DownloadCompleted` data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadCompletedData {
    /// Download id.
    pub id: Uuid,
    /// Final path of the completed file.
    pub final_path: String,
    /// SHA-256 of the file (lowercase hex).
    pub sha256: String,
    /// Whether the final length matched the expectation.
    pub length_verified: bool,
}

/// `DownloadFailed` data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadFailedData {
    /// Download id.
    pub id: Uuid,
    /// Why it failed.
    pub error: DownloadError,
}

/// `DownloadRemoved` data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadRemovedData {
    /// Download id.
    pub id: Uuid,
}

/// `QueueChanged` data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueChangedData {
    /// Queue order, at most 1000 ids.
    pub order: Vec<Uuid>,
}

/// `ResyncRequired` data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResyncRequiredData {
    /// Why the client must resynchronize.
    pub reason: ResyncReason,
}

/// An event payload, one variant per event name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", content = "data")]
pub enum EventBody {
    /// Agent status changed (or the 1 s tick while active).
    AgentStatus(AgentStatusInfo),
    /// A download was inserted.
    DownloadAdded(Box<DownloadSummary>),
    /// A download changed.
    DownloadChanged(Box<DownloadSummary>),
    /// Coalesced progress.
    ProgressSnapshot(ProgressSnapshotData),
    /// A download completed.
    DownloadCompleted(DownloadCompletedData),
    /// A download failed.
    DownloadFailed(DownloadFailedData),
    /// A download was removed.
    DownloadRemoved(DownloadRemovedData),
    /// The queue was reordered.
    QueueChanged(QueueChangedData),
    /// The client must `GetSnapshot` then `Subscribe` again.
    ResyncRequired(ResyncRequiredData),
}

impl EventBody {
    /// The PascalCase wire name of the event.
    #[must_use]
    pub const fn event_name(&self) -> &'static str {
        match self {
            Self::AgentStatus(_) => "AgentStatus",
            Self::DownloadAdded(_) => "DownloadAdded",
            Self::DownloadChanged(_) => "DownloadChanged",
            Self::ProgressSnapshot(_) => "ProgressSnapshot",
            Self::DownloadCompleted(_) => "DownloadCompleted",
            Self::DownloadFailed(_) => "DownloadFailed",
            Self::DownloadRemoved(_) => "DownloadRemoved",
            Self::QueueChanged(_) => "QueueChanged",
            Self::ResyncRequired(_) => "ResyncRequired",
        }
    }

    /// Every event name this build knows.
    pub const NAMES: [&'static str; 9] = [
        "AgentStatus",
        "DownloadAdded",
        "DownloadChanged",
        "ProgressSnapshot",
        "DownloadCompleted",
        "DownloadFailed",
        "DownloadRemoved",
        "QueueChanged",
        "ResyncRequired",
    ];

    /// Parses `data` as the data of the named event. The caller checks the name is in
    /// [`NAMES`](Self::NAMES) first.
    ///
    /// # Errors
    /// A serde error when `data` does not match the event's shape.
    pub fn from_parts(name: &str, data: Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(json!({"event": name, "data": data}))
    }

    /// The event data as a JSON value.
    ///
    /// # Errors
    /// Only if serialization fails, which the event types never do.
    pub fn data_value(&self) -> Result<Value, serde_json::Error> {
        take_content(serde_json::to_value(self)?, "data")
    }
}

/// A sequenced event frame: `{"type":"event","seq":..,"event":..,"data":..}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Strictly increasing per agent instance, below 2^53.
    pub seq: u64,
    /// The event payload.
    pub body: EventBody,
}
