//! GetSnapshot, ListDownloads, GetDownload, Subscribe and Unsubscribe (A.3, ADR-019).

use forgedm_ipc::Connection;
use forgedm_protocol::{
    DownloadIdRequest, EmptyResult, ListDownloadsRequest, ResultBody, SubscribeRequest,
};

use super::AgentHandler;
use crate::error::AgentError;
use crate::events_hub::SubscribeOutcome;

/// The snapshot and the `seq` it is consistent with.
pub(super) async fn snapshot(handler: &AgentHandler) -> Result<ResultBody, AgentError> {
    Ok(ResultBody::GetSnapshot(Box::new(
        handler.hub.snapshot().await?,
    )))
}

/// One page of downloads, newest first. `limit` was validated by the transport (1..=500).
pub(super) async fn list(
    handler: &AgentHandler,
    request: ListDownloadsRequest,
) -> Result<ResultBody, AgentError> {
    let page = handler
        .hub
        .list(request.states, request.limit, request.offset)
        .await?;
    Ok(ResultBody::ListDownloads(Box::new(page)))
}

/// Registers the connection's event queue with the hub and replays `(after_seq, current]`.
///
/// On any failure the connection is left without a subscription, so it is subject to the
/// idle timeout again and the client can resynchronise on the same connection.
pub(super) async fn subscribe(
    handler: &AgentHandler,
    conn: &mut Connection,
    request: SubscribeRequest,
) -> Result<ResultBody, AgentError> {
    let sink = conn.subscribe();
    match handler.hub.subscribe(request, sink).await {
        Ok(SubscribeOutcome::Subscribed(result)) => Ok(ResultBody::Subscribe(result)),
        Ok(SubscribeOutcome::Resync(reason)) => {
            conn.unsubscribe();
            Err(AgentError::ResyncRequired(reason.as_str()))
        }
        Err(gone) => {
            conn.unsubscribe();
            Err(gone.into())
        }
    }
}

/// Ends the connection's subscription (a no-op when there is none).
pub(super) fn unsubscribe(conn: &mut Connection) -> Result<ResultBody, AgentError> {
    conn.unsubscribe();
    Ok(ResultBody::Unsubscribe(EmptyResult {}))
}

/// The full view of one download, with live numbers while it runs. An unknown id is
/// `not_found`.
pub(super) async fn get_download(
    handler: &AgentHandler,
    request: DownloadIdRequest,
) -> Result<ResultBody, AgentError> {
    let detail = handler.engine()?.detail(request.download_id).await?;
    Ok(ResultBody::GetDownload(Box::new(detail)))
}
