//! `forgedm agent stop`: ask the agent to shut down safely.

use forgedm_protocol::{Empty, RequestBody, ResultBody};

use crate::cli::AgentCommand;
use crate::client::Target;
use crate::error::CliError;
use crate::output;

/// Runs one `agent` subcommand.
pub async fn run(target: &Target, action: AgentCommand) -> Result<(), CliError> {
    match action {
        AgentCommand::Stop => stop(target).await,
    }
}

/// Sends `ShutdownAgentSafely`; the agent answers before it drains.
async fn stop(target: &Target) -> Result<(), CliError> {
    let (mut client, _hello) = target.connect().await?;
    match client
        .call(RequestBody::ShutdownAgentSafely(Empty {}))
        .await?
    {
        ResultBody::ShutdownAgentSafely(_) => {
            output::line("agent is shutting down");
            Ok(())
        }
        _ => Err(CliError::Unexpected),
    }
}
