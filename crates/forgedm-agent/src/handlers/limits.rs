//! PauseAll, ResumeAll, UpdateLimits and UpdatePriority. Owned by S1.11 and S1.13;
//! `not_implemented` in this slice.

use forgedm_protocol::{
    Empty, PauseAllRequest, ResultBody, UpdateLimitsRequest, UpdatePriorityRequest,
};

use crate::error::AgentError;

/// S1.13: Pause applied to every eligible download.
pub(super) fn pause_all(_request: &PauseAllRequest) -> Result<ResultBody, AgentError> {
    Err(AgentError::NotImplemented)
}

/// S1.13: Resume applied to every paused download.
pub(super) fn resume_all(_request: &Empty) -> Result<ResultBody, AgentError> {
    Err(AgentError::NotImplemented)
}

/// S1.11 (`max_connections`) and S1.13 (rate limit, active-download cap).
pub(super) fn update_limits(_request: &UpdateLimitsRequest) -> Result<ResultBody, AgentError> {
    Err(AgentError::NotImplemented)
}

/// S1.13: queue priority.
pub(super) fn update_priority(_request: &UpdatePriorityRequest) -> Result<ResultBody, AgentError> {
    Err(AgentError::NotImplemented)
}
