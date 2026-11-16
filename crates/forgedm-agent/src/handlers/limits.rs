//! PauseAll, ResumeAll, UpdateLimits and UpdatePriority (PLAN A.3, A.6; S1.11 and S1.13).
//!
//! `UpdateLimits` with a download scope sets that download's connection cap and rate limit
//! live (for the agent session). With the global scope it changes how many downloads run at
//! once, the global rate limit and the default connections per download; these are stored in
//! `settings` and survive a restart. Range validation happens at the pipe (A.3); the engine
//! clamps again.

use forgedm_protocol::{
    EffectiveLimits, Empty, EmptyResult, LimitScope, PauseAllRequest, ResultBody,
    UpdateLimitsRequest, UpdateLimitsResult, UpdatePriorityRequest,
};

use super::AgentHandler;
use crate::error::AgentError;
use crate::limits_store::GlobalPatch;

/// Pause applied to every eligible download; active non-resumable ones are skipped unless
/// `discard_progress`.
pub(super) async fn pause_all(
    handler: &AgentHandler,
    request: PauseAllRequest,
) -> Result<ResultBody, AgentError> {
    let result = handler
        .engine()?
        .pause_all(request.discard_progress)
        .await?;
    Ok(ResultBody::PauseAll(result))
}

/// Resume applied to every paused download.
pub(super) async fn resume_all(
    handler: &AgentHandler,
    _request: Empty,
) -> Result<ResultBody, AgentError> {
    Ok(ResultBody::ResumeAll(handler.engine()?.resume_all().await?))
}

/// The connection cap, rate limit and (global only) active download cap, applied live.
pub(super) async fn update_limits(
    handler: &AgentHandler,
    request: UpdateLimitsRequest,
) -> Result<ResultBody, AgentError> {
    let engine = handler.engine()?;
    let effective = match request.scope {
        LimitScope::Global => {
            let patch = GlobalPatch {
                max_connections: request.max_connections,
                rate_limit_bps: request.rate_limit_bps,
                max_active_downloads: request.max_active_downloads,
            };
            engine.update_global_limits(patch).await?
        }
        LimitScope::Download { download_id } => {
            if request.max_active_downloads.is_some() {
                return Err(AgentError::Unsupported("max_active_downloads"));
            }
            let mut effective = EffectiveLimits::default();
            if let Some(value) = request.max_connections {
                effective.max_connections =
                    Some(engine.set_max_connections(download_id, value).await?);
            }
            if let Some(limit) = request.rate_limit_bps {
                effective.rate_limit_bps = engine.set_rate_limit(download_id, limit).await?;
            }
            effective
        }
    };
    Ok(ResultBody::UpdateLimits(UpdateLimitsResult { effective }))
}

/// Changes the queue priority; a queued download is reordered at once.
pub(super) async fn update_priority(
    handler: &AgentHandler,
    request: UpdatePriorityRequest,
) -> Result<ResultBody, AgentError> {
    handler
        .engine()?
        .set_priority(request.download_id, request.priority)
        .await?;
    Ok(ResultBody::UpdatePriority(EmptyResult {}))
}
