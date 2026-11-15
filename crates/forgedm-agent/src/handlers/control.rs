//! Pause, Resume, Cancel, Retry and Remove (PLAN A.3 and A.6; S1.9).
//!
//! Each call goes to the engine, which serializes commands per download, judges them with the
//! pure transition table and answers with the resulting state. Repeated commands are
//! idempotent no-ops; a repeated `Remove` is `not_found`, which clients treat as success.

use forgedm_core::engine::Control;
use forgedm_protocol::{
    DownloadIdRequest, EmptyResult, PauseRequest, RemoveRequest, ResultBody, RetryRequest,
    StateResult,
};

use super::AgentHandler;
use crate::error::AgentError;

/// queued -> paused; active -> pausing -> paused; an active non-resumable download needs
/// `discard_progress`.
pub(super) async fn pause(
    handler: &AgentHandler,
    request: PauseRequest,
) -> Result<ResultBody, AgentError> {
    let control = Control::Pause {
        discard_progress: request.discard_progress,
    };
    let state = handler
        .engine()?
        .control(request.download_id, control)
        .await?;
    Ok(ResultBody::Pause(StateResult { state }))
}

/// paused, interrupted or retry_waiting -> queued.
pub(super) async fn resume(
    handler: &AgentHandler,
    request: DownloadIdRequest,
) -> Result<ResultBody, AgentError> {
    let state = handler
        .engine()?
        .control(request.download_id, Control::Resume)
        .await?;
    Ok(ResultBody::Resume(StateResult { state }))
}

/// Any non-terminal state except finalizing -> cancelled (the `.part` is kept).
pub(super) async fn cancel(
    handler: &AgentHandler,
    request: DownloadIdRequest,
) -> Result<ResultBody, AgentError> {
    let state = handler
        .engine()?
        .control(request.download_id, Control::Cancel)
        .await?;
    Ok(ResultBody::Cancel(StateResult { state }))
}

/// failed, cancelled or needs_* -> queued; `restart_from_zero` forgets progress first.
pub(super) async fn retry(
    handler: &AgentHandler,
    request: RetryRequest,
) -> Result<ResultBody, AgentError> {
    let control = Control::Retry {
        restart_from_zero: request.restart_from_zero,
    };
    let state = handler
        .engine()?
        .control(request.download_id, control)
        .await?;
    Ok(ResultBody::Retry(StateResult { state }))
}

/// Quiesce, then delete the row (and, on request, the recorded `.part`).
pub(super) async fn remove(
    handler: &AgentHandler,
    request: RemoveRequest,
) -> Result<ResultBody, AgentError> {
    handler
        .engine()?
        .remove(request.download_id, request.delete_partial_file)
        .await?;
    Ok(ResultBody::Remove(EmptyResult {}))
}
