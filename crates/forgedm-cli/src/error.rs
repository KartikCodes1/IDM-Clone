//! CLI error type and the mapping to exit codes (PHASE_0_1_PLAN A.8).

use forgedm_ipc::{BootstrapError, IpcError};
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
/// A watched download ended `failed` or `cancelled` (A.8: 13).
pub const EXIT_DOWNLOAD_ENDED: u8 = 13;
/// Agent identity verification failed (A.8: 14).
pub const EXIT_IDENTITY: u8 = 14;
/// The agent could not be started or did not come up in time (A.8: 15).
pub const EXIT_BOOTSTRAP: u8 = 15;
/// Bad input that clap cannot see, for example an empty URL on stdin (A.8: 2).
pub const EXIT_USAGE: u8 = 2;

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
    /// The agent could not be started, or did not become ready in time.
    #[error("cannot start the agent: {0}")]
    Bootstrap(#[source] BootstrapError),
    /// The watched download ended `failed` or `cancelled`.
    #[error("the download ended: {0}")]
    DownloadEnded(String),
    /// The command line was fine for clap but the input is unusable.
    #[error("{0}")]
    Input(&'static str),
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
            Self::Bootstrap(_) => EXIT_BOOTSTRAP,
            Self::DownloadEnded(_) => EXIT_DOWNLOAD_ENDED,
            Self::Input(_) => EXIT_USAGE,
            Self::ProtocolMismatch => EXIT_PROTOCOL,
            Self::Agent { .. } => EXIT_AGENT_ERROR,
            Self::Ipc(_) | Self::Unexpected | Self::Render(_) => EXIT_INTERNAL,
        }
    }
}

impl From<BootstrapError> for CliError {
    fn from(err: BootstrapError) -> Self {
        match err {
            // A connection problem keeps its own mapping (for example identity failure, 14).
            BootstrapError::Ipc(ipc) => Self::from(ipc),
            other => Self::Bootstrap(other),
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
    fn download_bootstrap_and_input_errors_have_their_own_exit_codes() {
        assert_eq!(CliError::DownloadEnded("failed".into()).exit_code(), 13);
        assert_eq!(CliError::from(BootstrapError::Timeout).exit_code(), 15);
        let gone = BootstrapError::AgentBinaryMissing("x".into());
        assert_eq!(CliError::from(gone).exit_code(), 15);
        let ipc = BootstrapError::Ipc(IpcError::NotRunning);
        assert_eq!(CliError::from(ipc).exit_code(), 10);
        assert_eq!(CliError::Input("empty").exit_code(), 2);
    }

    #[test]
    fn agent_error_names_the_wire_code() {
        let err = CliError::from(IpcError::Server(Box::new(ErrorBody::new(
            ErrorCode::NotImplemented,
        ))));

        assert!(err.to_string().contains("not_implemented"), "{err}");
    }
}
