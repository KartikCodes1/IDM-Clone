//! The error type every Tauri command returns to the webview.
//!
//! The webview only ever sees a coarse `kind` and a fixed, user-facing `message`. Paths,
//! OS error text and agent internals are withheld from the webview (THREAT_MODEL UI-01).
//! Detail is emitted as `tracing` events; Phase 0 installs no subscriber for the desktop
//! crate (no new dependency allowed), so those events are currently discarded.

use forgedm_ipc::{BootstrapError, IpcError};
use forgedm_protocol::ErrorCode;
use serde::Serialize;

/// Coarse failure class, serialized as snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandErrorKind {
    /// The agent is not running (or not installed next to the desktop executable).
    AgentUnavailable,
    /// Something answered on the pipe but failed identity verification; nothing was sent.
    AgentIdentity,
    /// The agent does not speak protocol v1.
    ProtocolMismatch,
    /// The agent answered with an error response.
    AgentError,
    /// The webview sent something the app refuses before contacting the agent (a malformed
    /// id, a download that is not in a state the action applies to).
    InvalidRequest,
    /// Anything else: timeouts, transport faults, malformed answers, local faults.
    Internal,
}

/// What the webview receives when a command fails.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, thiserror::Error)]
#[error("{message}")]
pub struct CommandError {
    /// Failure class.
    pub kind: CommandErrorKind,
    /// Fixed, user-facing text.
    pub message: String,
}

impl CommandError {
    /// Builds an error from a kind and a message.
    #[must_use]
    pub fn new(kind: CommandErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl From<IpcError> for CommandError {
    fn from(err: IpcError) -> Self {
        // Detail goes to a tracing event (no sink installed yet); the webview gets a fixed
        // sentence per class.
        tracing::warn!(error = %err, "agent call failed");
        match err {
            IpcError::NotRunning => Self::new(
                CommandErrorKind::AgentUnavailable,
                "The background engine is not running.",
            ),
            IpcError::ServerRejected(_) => Self::new(
                CommandErrorKind::AgentIdentity,
                "The program answering on the engine pipe could not be verified.",
            ),
            IpcError::Server(body) if body.code == ErrorCode::UnsupportedProtocolVersion => {
                Self::new(
                    CommandErrorKind::ProtocolMismatch,
                    "The background engine uses an incompatible protocol version.",
                )
            }
            IpcError::Server(body) => {
                Self::new(CommandErrorKind::AgentError, server_message(body.code))
            }
            IpcError::Busy => Self::new(
                CommandErrorKind::Internal,
                "The background engine is busy. Try again in a moment.",
            ),
            _ => Self::new(
                CommandErrorKind::Internal,
                "Communication with the background engine failed.",
            ),
        }
    }
}

/// Fixed, user-facing text per agent error code. The agent's own message is never relayed,
/// so nothing a request carried can come back to the webview.
fn server_message(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::NotFound => "That download no longer exists.",
        ErrorCode::InvalidState => {
            "That action is not available while the download is in its current state."
        }
        ErrorCode::NotResumable => {
            "The server does not support resuming, so this download cannot be paused without starting over. Cancel it instead."
        }
        ErrorCode::InvalidUrl => {
            "The link was rejected. Check that it is a valid http or https address."
        }
        ErrorCode::InvalidDestination => {
            "That folder cannot be used. Choose an existing folder you can write to."
        }
        ErrorCode::InvalidFileName => "The file name was rejected.",
        ErrorCode::InvalidChecksum => {
            "The SHA-256 value is not valid. It must be 64 hexadecimal characters."
        }
        ErrorCode::LimitExceeded => {
            "A limit was reached. Remove or finish some downloads and try again."
        }
        ErrorCode::Busy => "The background engine is busy. Try again in a moment.",
        ErrorCode::ShuttingDown => "The background engine is shutting down.",
        ErrorCode::StorageUnavailable => "The engine's download database is unavailable.",
        ErrorCode::InvalidPayload => "The engine did not accept that request.",
        _ => "The background engine could not complete the request.",
    }
}

impl From<BootstrapError> for CommandError {
    fn from(err: BootstrapError) -> Self {
        match err {
            BootstrapError::Ipc(inner) => Self::from(inner),
            BootstrapError::AgentBinaryMissing(_) => {
                tracing::warn!("agent executable missing");
                Self::new(
                    CommandErrorKind::AgentUnavailable,
                    "The background engine is not installed next to the app.",
                )
            }
            BootstrapError::Spawn(_)
            | BootstrapError::AgentExited { .. }
            | BootstrapError::Timeout => {
                tracing::warn!(error = %err, "the agent could not be started");
                Self::new(
                    CommandErrorKind::AgentUnavailable,
                    "The background engine could not be started.",
                )
            }
            other => {
                tracing::warn!(error = %other, "agent bootstrap fault");
                Self::new(
                    CommandErrorKind::Internal,
                    "The background engine could not be located.",
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_error_serializes_kind_message() {
        let err = CommandError::new(
            CommandErrorKind::AgentUnavailable,
            "the engine is not running",
        );
        let json = serde_json::to_value(&err).expect("serializes");
        assert_eq!(
            json,
            serde_json::json!({"kind": "agent_unavailable", "message": "the engine is not running"})
        );
        let kinds = [
            (CommandErrorKind::AgentUnavailable, "agent_unavailable"),
            (CommandErrorKind::AgentIdentity, "agent_identity"),
            (CommandErrorKind::ProtocolMismatch, "protocol_mismatch"),
            (CommandErrorKind::AgentError, "agent_error"),
            (CommandErrorKind::Internal, "internal"),
        ];
        for (kind, wire) in kinds {
            assert_eq!(serde_json::to_value(kind).expect("serializes"), wire);
        }
    }

    #[test]
    fn ipc_errors_map_to_kinds_without_leaking_detail() {
        let missing = CommandError::from(IpcError::NotRunning);
        assert_eq!(missing.kind, CommandErrorKind::AgentUnavailable);
        let io = CommandError::from(IpcError::Io(std::io::Error::other(r"C:\secret\path")));
        assert_eq!(io.kind, CommandErrorKind::Internal);
        assert!(!io.message.contains("secret"));
        assert_eq!(
            CommandError::from(IpcError::Closed).kind,
            CommandErrorKind::Internal
        );
    }

    fn server_error(code: ErrorCode) -> CommandError {
        CommandError::from(IpcError::Server(Box::new(
            forgedm_protocol::ErrorBody::new(code),
        )))
    }

    #[test]
    fn get_download_maps_not_found() {
        let err = server_error(ErrorCode::NotFound);
        assert_eq!(err.kind, CommandErrorKind::AgentError);
        assert!(err.message.contains("no longer exists"), "{}", err.message);
    }

    #[test]
    fn every_server_code_has_a_friendly_nonempty_message() {
        let codes = [
            ErrorCode::InvalidState,
            ErrorCode::NotResumable,
            ErrorCode::InvalidUrl,
            ErrorCode::InvalidDestination,
            ErrorCode::InvalidFileName,
            ErrorCode::InvalidChecksum,
            ErrorCode::LimitExceeded,
            ErrorCode::Busy,
            ErrorCode::ShuttingDown,
            ErrorCode::StorageUnavailable,
            ErrorCode::InvalidPayload,
            ErrorCode::Internal,
            ErrorCode::Unknown,
        ];
        for code in codes {
            let err = server_error(code);
            assert_eq!(err.kind, CommandErrorKind::AgentError, "{code:?}");
            assert!(err.message.ends_with('.'), "{code:?}: {}", err.message);
            assert!(!err.message.contains('_'), "{code:?}: {}", err.message);
        }
        assert!(server_error(ErrorCode::InvalidUrl).message.contains("http"));
        assert!(
            server_error(ErrorCode::NotResumable)
                .message
                .contains("resum")
        );
    }

    #[test]
    fn bootstrap_failures_map_without_leaking_paths() {
        use forgedm_ipc::BootstrapError;
        let missing = CommandError::from(BootstrapError::AgentBinaryMissing(
            r"C:\secretorgedm-agent.exe".into(),
        ));
        assert_eq!(missing.kind, CommandErrorKind::AgentUnavailable);
        let relative = CommandError::from(BootstrapError::AgentPathNotAbsolute("a.exe".into()));
        assert_eq!(relative.kind, CommandErrorKind::Internal);
        for err in [
            CommandError::from(BootstrapError::Timeout),
            CommandError::from(BootstrapError::AgentExited { code: Some(2) }),
            CommandError::from(BootstrapError::Spawn(std::io::Error::other(r"C:\secret"))),
        ] {
            assert_eq!(err.kind, CommandErrorKind::AgentUnavailable);
            assert!(!err.message.contains("secret"));
        }
        let via_ipc = CommandError::from(BootstrapError::Ipc(IpcError::NotRunning));
        assert_eq!(via_ipc.kind, CommandErrorKind::AgentUnavailable);
    }

    #[test]
    fn invalid_request_kind_serializes_snake_case() {
        let value = serde_json::to_value(CommandErrorKind::InvalidRequest).expect("serializes");
        assert_eq!(value, "invalid_request");
    }
}
