//! Handshake, Ping, GetAgentStatus and ShutdownAgentSafely.

use std::time::{SystemTime, UNIX_EPOCH};

use forgedm_protocol::{
    EmptyResult, HandshakeLimits, HandshakeResult, LIST_PAGE_MAX, MAX_FRAME, PROTOCOL_MINOR,
    PROTOCOL_VERSION, PingResult, ResultBody,
};

use super::AgentHandler;
use crate::app::request_shutdown;
use crate::error::AgentError;

/// Capabilities advertised only once implemented (A.2): replay on `Subscribe` (S0.8), the
/// expected-SHA-256 check (S1.7) and validated resume (S1.9). Segmenting and rate limits come
/// with their slices.
const CAPABILITIES: [&str; 3] = ["subscribe_replay", "checksum_sha256", "resume"];

/// Answers the handshake. The transport already negotiated the protocol version.
pub(super) fn handshake(handler: &AgentHandler) -> Result<ResultBody, AgentError> {
    Ok(ResultBody::Handshake(HandshakeResult {
        protocol_version: PROTOCOL_VERSION,
        protocol_minor: PROTOCOL_MINOR,
        agent_version: env!("CARGO_PKG_VERSION").to_owned(),
        agent_pid: std::process::id(),
        agent_instance_id: handler.hub.instance_id(),
        capabilities: CAPABILITIES.iter().map(|c| (*c).to_owned()).collect(),
        limits: HandshakeLimits {
            max_frame_bytes: MAX_FRAME,
            max_list_page: LIST_PAGE_MAX,
        },
    }))
}

/// Answers Ping with wall-clock time and uptime.
pub(super) fn ping(handler: &AgentHandler) -> Result<ResultBody, AgentError> {
    let agent_time_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0);
    let uptime_ms = u64::try_from(handler.started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(ResultBody::Ping(PingResult {
        agent_time_ms,
        uptime_ms,
    }))
}

/// Answers GetAgentStatus from the hub.
pub(super) async fn status(handler: &AgentHandler) -> Result<ResultBody, AgentError> {
    Ok(ResultBody::GetAgentStatus(handler.hub.status().await?))
}

/// Starts a graceful shutdown. The reply is sent before the agent drains: the transport
/// writes it before it observes the cancellation.
pub(super) async fn shutdown(handler: &AgentHandler) -> Result<ResultBody, AgentError> {
    request_shutdown(&handler.hub, &handler.shutdown).await;
    Ok(ResultBody::ShutdownAgentSafely(EmptyResult {}))
}
