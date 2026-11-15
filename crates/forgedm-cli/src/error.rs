//! CLI error type and the mapping to exit codes (PHASE_0_1_PLAN A.8).

use forgedm_ipc::IpcError;
use forgedm_protocol::ErrorCode;
use thiserror::Error;

/// Internal error (A.8: 1).
pub const EXIT_INTERNAL: u8 = 1;
/// Agent not running and not started (A.8: 10).
pub const EXIT_NOT_RUNNING: u8 = 10;
/// The agent returned an error (A.8: 11).
pub const EXIT_AGENT_ERROR: u8 = 11;
/// Protocol version mismatch (A.8: 12).
pub const EXIT_PROTOCOL: u8 = 12;
/// Agent identity verification failed (A.8: 14).
pub const EXIT_IDENTITY: u8 = 14;

/// Everything that can make a command fail.
#[derive(Debug, Error)]
pub enum CliError {
    /// No agent is listening for this data directory.
    #[error("the agent is not running")]
    NotRunning,
    /// The pipe server is not the expected agent of this user.
    #[error(
        "the process behind the pipe is not the expected ForgeDM agent; refusing to talk to it"
    )]
    IdentityFailed(#[source] IpcError),
    /// The agent does not speak this client's protocol version.
    #[error("protocol version mismatch with the agent")]
    ProtocolMismatch,
    /// The agent answered with an error response.
    #[error("the agent returned an error ({code}): {message}")]
    Agent {
        /// Wire error code, snake_case.
        code: String,
        /// The agent's generic message.
        message: String,
    },
    /// Any other transport or platform failure.
    #[error("cannot talk to the agent: {0}")]
    Ipc(#[source] IpcError),
    /// The agent sent a result that does not belong to the request.
    #[error("the agent sent an unexpected result")]
    Unexpected,
    /// Rendering output failed.
    #[error("cannot render the output: {0}")]
    Render(#[from] serde_json::Error),
}

impl CliError {
    /// The process exit code for this error.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::NotRunning => EXIT_NOT_RUNNING,
            Self::IdentityFailed(_) => EXIT_IDENTITY,
            Self::ProtocolMismatch => EXIT_PROTOCOL,
            Self::Agent { .. } => EXIT_AGENT_ERROR,
            Self::Ipc(_) | Self::Unexpected | Self::Render(_) => EXIT_INTERNAL,
        }
    }
}

impl From<IpcError> for CliError {
    fn from(err: IpcError) -> Self {
        match err {
            IpcError::NotRunning => Self::NotRunning,
            IpcError::ServerRejected(_) => Self::IdentityFailed(err),
            IpcError::UnexpectedResult => Self::Unexpected,
            IpcError::Server(body) if body.code == ErrorCode::UnsupportedProtocolVersion => {
                Self::ProtocolMismatch
            }
            IpcError::Server(body) => Self::Agent {
                code: serde_json::to_value(body.code)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "unknown".to_owned()),
                message: body.message,
            },
            other => Self::Ipc(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forgedm_protocol::ErrorBody;

    #[test]
    fn exit_codes_follow_appendix_a8() {
        let rejected = IpcError::Server(Box::new(ErrorBody::unsupported_protocol_version()));
        let other = IpcError::Server(Box::new(ErrorBody::new(ErrorCode::NotImplemented)));

        assert_eq!(CliError::from(IpcError::NotRunning).exit_code(), 10);
        assert_eq!(CliError::from(rejected).exit_code(), 12);
        assert_eq!(CliError::from(other).exit_code(), 11);
        assert_eq!(CliError::from(IpcError::Timeout).exit_code(), 1);
    }

    #[test]
    fn agent_error_names_the_wire_code() {
        let err = CliError::from(IpcError::Server(Box::new(ErrorBody::new(
            ErrorCode::NotImplemented,
        ))));

        assert!(err.to_string().contains("not_implemented"), "{err}");
    }
}
