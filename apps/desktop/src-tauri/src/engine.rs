//! Starting and stopping the background engine (the agent).

use std::path::Path;

use forgedm_protocol::{AgentStatusInfo, ClientKind, Empty, RequestBody, ResultBody};

use crate::agent_client::{AgentLocator, connect, unexpected_result};
use crate::error::CommandError;

/// Connects to the agent, starting the sibling `forgedm-agent.exe` first when nothing is
/// listening (the `forgedm-ipc` bootstrap verifies the server before sending anything),
/// and returns its status.
///
/// `data_dir` is `None` for the user's default data folder.
///
/// # Errors
/// [`CommandError`]: `agent_unavailable` when the agent is missing or does not come up.
pub async fn start_engine(
    locator: &AgentLocator,
    data_dir: Option<&Path>,
) -> Result<AgentStatusInfo, CommandError> {
    let mut client = forgedm_ipc::ensure_agent(locator.expected_agent_path(), data_dir).await?;
    client
        .handshake(
            ClientKind::Desktop,
            "forgedm-desktop",
            env!("CARGO_PKG_VERSION"),
        )
        .await?;
    match client.call(RequestBody::GetAgentStatus(Empty {})).await? {
        ResultBody::GetAgentStatus(status) => Ok(status),
        _ => Err(unexpected_result()),
    }
}

/// Asks the agent to drain and exit (`ShutdownAgentSafely`).
///
/// # Errors
/// [`CommandError`] (`agent_unavailable` when no agent is running).
pub async fn stop_engine(locator: &AgentLocator) -> Result<(), CommandError> {
    let mut client = connect(locator).await?;
    match client
        .call(RequestBody::ShutdownAgentSafely(Empty {}))
        .await?
    {
        ResultBody::ShutdownAgentSafely(_) => Ok(()),
        _ => Err(unexpected_result()),
    }
}
