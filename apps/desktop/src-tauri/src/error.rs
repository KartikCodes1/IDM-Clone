//! The error type every Tauri command returns to the webview.
//!
//! The webview only ever sees a coarse `kind` and a fixed, user-facing `message`. Paths,
//! OS error text and agent internals are withheld from the webview (THREAT_MODEL UI-01).
//! Detail is emitted as `tracing` events; Phase 0 installs no subscriber for the desktop
//! crate (no new dependency allowed), so those events are currently discarded.

use forgedm_ipc::IpcError;
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
            IpcError::Server(_) => Self::new(
                CommandErrorKind::AgentError,
                "The background engine could not complete the request.",
            ),
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
}
