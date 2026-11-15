//! The 19 actions: typed request payloads and typed results (Appendix A.3).
//!
//! Both enums use adjacent tagging (`action` + `payload` / `result`) only as an internal
//! dispatch device; the envelope fields are handled in [`crate::envelope`].

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::dto::{AgentStatusInfo, DownloadDetail};
use crate::enums::Action;
use crate::payloads::{
    DownloadIdRequest, Empty, EnqueueUrlRequest, HandshakeRequest, ListDownloadsRequest,
    PauseAllRequest, PauseRequest, RemoveRequest, RetryRequest, SubscribeRequest,
    UpdateLimitsRequest, UpdatePriorityRequest,
};
use crate::results::{
    EmptyResult, EnqueueUrlResult, HandshakeResult, ListDownloadsResult, PauseAllResult,
    PingResult, ResumeAllResult, SnapshotResult, StateResult, SubscribeResult, UpdateLimitsResult,
};

/// Extracts the content half of a serialized adjacently tagged enum.
pub(crate) fn take_content(value: Value, content_key: &str) -> Result<Value, serde_json::Error> {
    match value {
        Value::Object(mut map) => map
            .remove(content_key)
            .ok_or_else(|| serde::de::Error::custom("tagged enum has no content")),
        _ => Err(serde::de::Error::custom("tagged enum is not an object")),
    }
}

/// A request payload, one variant per action. Payloads reject unknown fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", content = "payload")]
pub enum RequestBody {
    /// First frame on every connection.
    Handshake(HandshakeRequest),
    /// Liveness check.
    Ping(Empty),
    /// Agent status.
    GetAgentStatus(Empty),
    /// Consistent snapshot of the download list.
    GetSnapshot(Empty),
    /// Start receiving events.
    Subscribe(SubscribeRequest),
    /// Stop receiving events.
    Unsubscribe(Empty),
    /// Paged download list.
    ListDownloads(ListDownloadsRequest),
    /// Drain and exit.
    ShutdownAgentSafely(Empty),
    /// Add a download.
    EnqueueUrl(EnqueueUrlRequest),
    /// One download's detail.
    GetDownload(DownloadIdRequest),
    /// Pause a download.
    Pause(PauseRequest),
    /// Resume a download.
    Resume(DownloadIdRequest),
    /// Cancel a download.
    Cancel(DownloadIdRequest),
    /// Retry a failed or cancelled download.
    Retry(RetryRequest),
    /// Remove a download.
    Remove(RemoveRequest),
    /// Pause every eligible download.
    PauseAll(PauseAllRequest),
    /// Resume every paused download.
    ResumeAll(Empty),
    /// Change connection and rate limits.
    UpdateLimits(UpdateLimitsRequest),
    /// Change a download's priority.
    UpdatePriority(UpdatePriorityRequest),
}

impl RequestBody {
    /// The action this payload belongs to.
    #[must_use]
    pub const fn action(&self) -> Action {
        match self {
            Self::Handshake(_) => Action::Handshake,
            Self::Ping(_) => Action::Ping,
            Self::GetAgentStatus(_) => Action::GetAgentStatus,
            Self::GetSnapshot(_) => Action::GetSnapshot,
            Self::Subscribe(_) => Action::Subscribe,
            Self::Unsubscribe(_) => Action::Unsubscribe,
            Self::ListDownloads(_) => Action::ListDownloads,
            Self::ShutdownAgentSafely(_) => Action::ShutdownAgentSafely,
            Self::EnqueueUrl(_) => Action::EnqueueUrl,
            Self::GetDownload(_) => Action::GetDownload,
            Self::Pause(_) => Action::Pause,
            Self::Resume(_) => Action::Resume,
            Self::Cancel(_) => Action::Cancel,
            Self::Retry(_) => Action::Retry,
            Self::Remove(_) => Action::Remove,
            Self::PauseAll(_) => Action::PauseAll,
            Self::ResumeAll(_) => Action::ResumeAll,
            Self::UpdateLimits(_) => Action::UpdateLimits,
            Self::UpdatePriority(_) => Action::UpdatePriority,
        }
    }

    /// Strictly parses `payload` as the payload of `action`.
    ///
    /// # Errors
    /// A serde error naming an unknown or missing field, or a type mismatch.
    pub fn from_parts(action: Action, payload: Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(json!({"action": action.as_str(), "payload": payload}))
    }

    /// The payload as a JSON value.
    ///
    /// # Errors
    /// Only if serialization fails, which the payload types never do.
    pub fn payload_value(&self) -> Result<Value, serde_json::Error> {
        take_content(serde_json::to_value(self)?, "payload")
    }
}

/// A typed result, one variant per action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", content = "result")]
pub enum ResultBody {
    /// Handshake result.
    Handshake(HandshakeResult),
    /// Ping result.
    Ping(PingResult),
    /// Agent status.
    GetAgentStatus(AgentStatusInfo),
    /// Snapshot.
    GetSnapshot(Box<SnapshotResult>),
    /// Subscription accepted.
    Subscribe(SubscribeResult),
    /// Unsubscribed.
    Unsubscribe(EmptyResult),
    /// A page of downloads.
    ListDownloads(Box<ListDownloadsResult>),
    /// Shutdown accepted.
    ShutdownAgentSafely(EmptyResult),
    /// Download created.
    EnqueueUrl(EnqueueUrlResult),
    /// Download detail.
    GetDownload(Box<DownloadDetail>),
    /// State after `Pause`.
    Pause(StateResult),
    /// State after `Resume`.
    Resume(StateResult),
    /// State after `Cancel`.
    Cancel(StateResult),
    /// State after `Retry`.
    Retry(StateResult),
    /// Download removed.
    Remove(EmptyResult),
    /// Outcome of `PauseAll`.
    PauseAll(PauseAllResult),
    /// Outcome of `ResumeAll`.
    ResumeAll(ResumeAllResult),
    /// Limits now in force.
    UpdateLimits(UpdateLimitsResult),
    /// Priority changed.
    UpdatePriority(EmptyResult),
}

impl ResultBody {
    /// The action this result answers.
    #[must_use]
    pub const fn action(&self) -> Action {
        match self {
            Self::Handshake(_) => Action::Handshake,
            Self::Ping(_) => Action::Ping,
            Self::GetAgentStatus(_) => Action::GetAgentStatus,
            Self::GetSnapshot(_) => Action::GetSnapshot,
            Self::Subscribe(_) => Action::Subscribe,
            Self::Unsubscribe(_) => Action::Unsubscribe,
            Self::ListDownloads(_) => Action::ListDownloads,
            Self::ShutdownAgentSafely(_) => Action::ShutdownAgentSafely,
            Self::EnqueueUrl(_) => Action::EnqueueUrl,
            Self::GetDownload(_) => Action::GetDownload,
            Self::Pause(_) => Action::Pause,
            Self::Resume(_) => Action::Resume,
            Self::Cancel(_) => Action::Cancel,
            Self::Retry(_) => Action::Retry,
            Self::Remove(_) => Action::Remove,
            Self::PauseAll(_) => Action::PauseAll,
            Self::ResumeAll(_) => Action::ResumeAll,
            Self::UpdateLimits(_) => Action::UpdateLimits,
            Self::UpdatePriority(_) => Action::UpdatePriority,
        }
    }

    /// Tolerantly parses `result` as the result of `action`.
    ///
    /// # Errors
    /// A serde error when a required field is missing or has the wrong type.
    pub fn from_value(action: Action, result: Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(json!({"action": action.as_str(), "result": result}))
    }

    /// The result as a JSON value.
    ///
    /// # Errors
    /// Only if serialization fails, which the result types never do.
    pub fn to_value(&self) -> Result<Value, serde_json::Error> {
        take_content(serde_json::to_value(self)?, "result")
    }
}
