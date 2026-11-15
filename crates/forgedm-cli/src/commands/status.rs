//! `forgedm status`: report the agent's state; never starts it.

use forgedm_protocol::{Empty, RequestBody, ResultBody};

use crate::client::Target;
use crate::error::CliError;
use crate::output;

/// Connects, asks `GetAgentStatus` and prints it.
pub async fn run(target: &Target) -> Result<(), CliError> {
    let (mut client, hello) = target.connect().await?;
    match client.call(RequestBody::GetAgentStatus(Empty {})).await? {
        ResultBody::GetAgentStatus(info) => {
            output::status(&hello, &info);
            Ok(())
        }
        _ => Err(CliError::Unexpected),
    }
}
