//! PauseAll, ResumeAll, UpdateLimits and UpdatePriority. S1.11 serves `UpdateLimits` for
//! `max_connections` of one download; the rest is S1.13 and answers `not_implemented`.

use forgedm_protocol::{
    EffectiveLimits, Empty, LimitScope, PauseAllRequest, ResultBody, UpdateLimitsRequest,
    UpdateLimitsResult, UpdatePriorityRequest,
};

use super::AgentHandler;
use crate::error::AgentError;

/// S1.13: Pause applied to every eligible download.
pub(super) fn pause_all(_request: &PauseAllRequest) -> Result<ResultBody, AgentError> {
    Err(AgentError::NotImplemented)
}

/// S1.13: Resume applied to every paused download.
pub(super) fn resume_all(_request: &Empty) -> Result<ResultBody, AgentError> {
    Err(AgentError::NotImplemented)
}

/// S1.11: the connection cap of one download, applied live (a running segmented download
/// opens or retires connections within a quarter of a second). The rate limit, the active
/// download cap and global limits are S1.13 and refused as unsupported, never ignored.
pub(super) async fn update_limits(
    handler: &AgentHandler,
    request: UpdateLimitsRequest,
) -> Result<ResultBody, AgentError> {
    let engine = handler.engine()?;
    if request.rate_limit_bps.is_some() {
        return Err(AgentError::Unsupported("rate_limit_bps"));
    }
    if request.max_active_downloads.is_some() {
        return Err(AgentError::Unsupported("max_active_downloads"));
    }
    let LimitScope::Download { download_id } = request.scope else {
        return Err(AgentError::Unsupported("global limits"));
    };
    let mut effective = EffectiveLimits::default();
    if let Some(value) = request.max_connections {
        effective.max_connections = Some(engine.set_max_connections(download_id, value).await?);
    }
    Ok(ResultBody::UpdateLimits(UpdateLimitsResult { effective }))
}

/// S1.13: queue priority.
pub(super) fn update_priority(_request: &UpdatePriorityRequest) -> Result<ResultBody, AgentError> {
    Err(AgentError::NotImplemented)
}
