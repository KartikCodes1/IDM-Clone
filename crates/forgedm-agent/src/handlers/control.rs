//! Pause, Resume, Cancel, Retry and Remove. Owned by S1.9; `not_implemented` in this slice.

use forgedm_protocol::{DownloadIdRequest, PauseRequest, RemoveRequest, ResultBody, RetryRequest};

use crate::error::AgentError;

/// S1.9: queued -> paused, active -> pausing -> paused, per A.6.
pub(super) fn pause(_request: &PauseRequest) -> Result<ResultBody, AgentError> {
    Err(AgentError::NotImplemented)
}

/// S1.9: paused, interrupted or retry_waiting -> queued.
pub(super) fn resume(_request: &DownloadIdRequest) -> Result<ResultBody, AgentError> {
    Err(AgentError::NotImplemented)
}

/// S1.9: any non-terminal state except finalizing -> cancelled.
pub(super) fn cancel(_request: &DownloadIdRequest) -> Result<ResultBody, AgentError> {
    Err(AgentError::NotImplemented)
}

/// S1.9: failed, cancelled or needs_* -> queued.
pub(super) fn retry(_request: &RetryRequest) -> Result<ResultBody, AgentError> {
    Err(AgentError::NotImplemented)
}

/// S1.9: quiesce, then delete the row (and optionally the recorded `.part`).
pub(super) fn remove(_request: &RemoveRequest) -> Result<ResultBody, AgentError> {
    Err(AgentError::NotImplemented)
}
