//! Agent error types.
//!
//! [`StartupError`] decides the process exit code (A.8). [`AgentError`] is what a request
//! handler fails with; [`AgentError::to_wire`] is the only place internal failures become
//! wire error codes. Internal details are logged with a correlation id, never sent.

use forgedm_core::error::EngineError;
use forgedm_ipc::IpcError;
use forgedm_platform::PlatformError;
use forgedm_protocol::{
    ErrorBody, ErrorCode, ErrorDetails, InvalidDestinationReason, InvalidUrlReason,
};
use forgedm_storage::StorageError;
use uuid::Uuid;

use crate::config::ConfigError;
use crate::data_dir::DataDirError;
use crate::events_hub::HubError;

/// Exit codes of `forgedm-agent.exe` (PHASE_0_1_PLAN A.8).
pub mod exit {
    /// Clean shutdown.
    pub const OK: u8 = 0;
    /// Unexpected internal failure.
    pub const INTERNAL: u8 = 1;
    /// Bad command line, including a relative `--data-dir`.
    pub const USAGE: u8 = 2;
    /// Another agent holds the instance lock.
    pub const ALREADY_RUNNING: u8 = 3;
    /// The pipe name is squatted or the pipe could not be secured.
    pub const PIPE_SECURITY: u8 = 4;
    /// The database could not be opened, migrated or is corrupt.
    pub const STORAGE: u8 = 5;
    /// The data directory is unusable.
    pub const DATA_DIR: u8 = 6;
}

/// Why the agent cannot start (or had to stop on its own).
#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    /// The command line is invalid.
    #[error("usage: {0}")]
    Usage(#[from] ConfigError),
    /// The data directory cannot be used.
    #[error("data directory: {0}")]
    DataDir(#[from] DataDirError),
    /// The default data root or the user SID could not be resolved.
    #[error("cannot resolve the data root: {0}")]
    Resolve(#[source] PlatformError),
    /// Another agent holds `run\agent.lock`.
    #[error("another agent is already running for this data directory")]
    AlreadyRunning,
    /// The lock file could not be created or locked for another reason.
    #[error("instance lock failed: {0}")]
    Lock(#[source] std::io::Error),
    /// Log files could not be set up.
    #[error("logging setup failed: {0}")]
    Logging(String),
    /// The pipe name is taken, the DACL could not be built, or a replacement instance failed.
    #[error("pipe security failure: {0}")]
    PipeSecurity(#[source] IpcError),
    /// The database failed to open, migrate or validate.
    #[error("storage failure: {0}")]
    Storage(#[from] StorageError),
    /// A runtime facility failed (for example the tokio runtime could not be built).
    #[error("internal error: {0}")]
    Internal(String),
}

impl StartupError {
    /// The process exit code for this failure.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => exit::USAGE,
            Self::DataDir(_) => exit::DATA_DIR,
            Self::AlreadyRunning => exit::ALREADY_RUNNING,
            Self::PipeSecurity(_) => exit::PIPE_SECURITY,
            Self::Storage(_) => exit::STORAGE,
            Self::Resolve(_) | Self::Lock(_) | Self::Logging(_) | Self::Internal(_) => {
                exit::INTERNAL
            }
        }
    }
}

/// Why a request failed inside the agent.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// The action belongs to a later slice.
    #[error("action not implemented in this build")]
    NotImplemented,
    /// The agent is shutting down and refuses new mutating work.
    #[error("agent is shutting down")]
    ShuttingDown,
    /// No reply from the state hub or the engine in time, or the engine channel is full.
    #[error("agent is busy")]
    Busy,
    /// The queue holds the maximum number of downloads.
    #[error("the download queue is full")]
    LimitExceeded,
    /// No such download.
    #[error("download not found")]
    NotFound,
    /// The command is not allowed in the download's current state.
    #[error("command not allowed while the download is {}", .0.as_str())]
    InvalidState(forgedm_protocol::DownloadState),
    /// Pause of an active non-resumable download without `discard_progress`.
    #[error("the download cannot resume")]
    NotResumable,
    /// The URL was rejected.
    #[error("url rejected: {}", .0.as_str())]
    InvalidUrl(InvalidUrlReason),
    /// The destination directory was rejected.
    #[error("destination rejected: {}", .0.as_str())]
    InvalidDestination(InvalidDestinationReason),
    /// The requested file name is unusable.
    #[error("file name rejected")]
    InvalidFileName,
    /// The expected checksum is not 64 hex characters.
    #[error("checksum rejected")]
    InvalidChecksum,
    /// A field is not supported yet (it belongs to a later slice).
    #[error("{0} is not supported yet")]
    Unsupported(&'static str),
    /// The client's subscription position cannot be served; it must resynchronise.
    #[error("resynchronisation required: {0}")]
    ResyncRequired(&'static str),
    /// The state hub stopped.
    #[error("state hub is gone")]
    HubGone(#[from] HubError),
    /// A database failure.
    #[error("storage error")]
    Storage(#[from] StorageError),
    /// The engine's persistence failed.
    #[error("engine storage error")]
    Engine(#[from] forgedm_core::error::StoreError),
    /// A response could not be built.
    #[error("response encoding failed: {0}")]
    Encode(#[from] serde_json::Error),
}

impl From<EngineError> for AgentError {
    fn from(err: EngineError) -> Self {
        match err {
            EngineError::Busy => Self::Busy,
            EngineError::ShuttingDown => Self::ShuttingDown,
            EngineError::LimitExceeded => Self::LimitExceeded,
            EngineError::NotFound => Self::NotFound,
            EngineError::InvalidState(state) => Self::InvalidState(state),
            EngineError::NotResumable => Self::NotResumable,
            EngineError::Store(store) => Self::Engine(store),
        }
    }
}

impl AgentError {
    /// The error body sent to the client. Internal causes are logged here with a correlation
    /// id (also returned in `details.reason`) and never put on the wire.
    #[must_use]
    pub fn to_wire(&self) -> ErrorBody {
        match self {
            Self::NotImplemented => ErrorBody::new(ErrorCode::NotImplemented),
            Self::ShuttingDown => ErrorBody::new(ErrorCode::ShuttingDown),
            Self::Busy => ErrorBody::new(ErrorCode::Busy),
            Self::LimitExceeded => ErrorBody::new(ErrorCode::LimitExceeded),
            Self::NotFound => ErrorBody::new(ErrorCode::NotFound),
            Self::InvalidState(state) => ErrorBody::invalid_state(*state),
            Self::NotResumable => ErrorBody::new(ErrorCode::NotResumable),
            Self::InvalidUrl(reason) => ErrorBody::invalid_url(*reason),
            Self::InvalidDestination(reason) => ErrorBody::invalid_destination(*reason),
            Self::InvalidFileName => ErrorBody::new(ErrorCode::InvalidFileName),
            Self::InvalidChecksum => ErrorBody::new(ErrorCode::InvalidChecksum),
            Self::Unsupported(field) => {
                ErrorBody::new(ErrorCode::NotImplemented).with_details(ErrorDetails {
                    field: Some((*field).to_owned()),
                    reason: Some("not_supported_yet".to_owned()),
                    ..ErrorDetails::default()
                })
            }
            Self::ResyncRequired(reason) => {
                ErrorBody::new(ErrorCode::ResyncRequired).with_details(ErrorDetails {
                    reason: Some((*reason).to_owned()),
                    ..ErrorDetails::default()
                })
            }
            Self::Storage(err) => storage_to_wire(err),
            Self::HubGone(_) | Self::Encode(_) => self.internal(),
            Self::Engine(err) => engine_store_to_wire(err),
        }
    }

    fn internal(&self) -> ErrorBody {
        let correlation = Uuid::now_v7();
        tracing::error!(%correlation, error = %self, "internal error while handling a request");
        ErrorBody::new(ErrorCode::Internal).with_details(ErrorDetails {
            reason: Some(format!("correlation_id={correlation}")),
            ..ErrorDetails::default()
        })
    }
}

fn engine_store_to_wire(err: &forgedm_core::error::StoreError) -> ErrorBody {
    use forgedm_core::error::StoreError;
    match err {
        StoreError::NotFound => ErrorBody::new(ErrorCode::NotFound),
        StoreError::Unavailable(_) => {
            tracing::error!(error = %err, "storage unavailable while handling a request");
            ErrorBody::new(ErrorCode::StorageUnavailable)
        }
        StoreError::Conflict { .. } | StoreError::SecretsUnavailable => {
            let correlation = Uuid::now_v7();
            tracing::error!(%correlation, error = %err, "engine storage error");
            ErrorBody::new(ErrorCode::Internal).with_details(ErrorDetails {
                reason: Some(format!("correlation_id={correlation}")),
                ..ErrorDetails::default()
            })
        }
    }
}

fn storage_to_wire(err: &StorageError) -> ErrorBody {
    match err {
        StorageError::NotFound { .. } => ErrorBody::new(ErrorCode::NotFound),
        StorageError::Busy(_) | StorageError::Database(_) | StorageError::Corrupt(_) => {
            tracing::error!(error = %err, "storage unavailable while handling a request");
            ErrorBody::new(ErrorCode::StorageUnavailable)
        }
        other => {
            let correlation = Uuid::now_v7();
            tracing::error!(%correlation, error = %other, "storage error while handling a request");
            ErrorBody::new(ErrorCode::Internal).with_details(ErrorDetails {
                reason: Some(format!("correlation_id={correlation}")),
                ..ErrorDetails::default()
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_errors_map_to_the_appendix_a8_exit_codes() {
        let usage = StartupError::Usage(ConfigError::RelativeDataDir);
        assert_eq!(usage.exit_code(), 2);
        assert_eq!(StartupError::AlreadyRunning.exit_code(), 3);
        assert_eq!(
            StartupError::PipeSecurity(IpcError::NameInUse).exit_code(),
            4
        );
        let storage = StartupError::Storage(StorageError::InvalidValue { field: "x" });
        assert_eq!(storage.exit_code(), 5);
        let data_dir = DataDirError::ReparsePoint { path: "x".into() };
        assert_eq!(StartupError::DataDir(data_dir).exit_code(), 6);
        assert_eq!(StartupError::Internal("x".into()).exit_code(), 1);
        assert_eq!(StartupError::Logging("x".into()).exit_code(), 1);
    }

    #[test]
    fn request_errors_map_to_wire_codes_and_retryability() {
        let cases = [
            (AgentError::NotImplemented, ErrorCode::NotImplemented, false),
            (AgentError::ShuttingDown, ErrorCode::ShuttingDown, true),
            (AgentError::Busy, ErrorCode::Busy, true),
            (
                AgentError::ResyncRequired("evicted"),
                ErrorCode::ResyncRequired,
                true,
            ),
            (
                AgentError::HubGone(HubError::Gone),
                ErrorCode::Internal,
                false,
            ),
        ];
        for (error, code, retryable) in cases {
            let wire = error.to_wire();
            assert_eq!(wire.code, code, "{error}");
            assert_eq!(wire.retryable, retryable, "{error}");
        }
    }

    #[test]
    fn control_errors_map_to_their_wire_codes() {
        use forgedm_protocol::DownloadState;
        assert_eq!(AgentError::NotFound.to_wire().code, ErrorCode::NotFound);
        let wire = AgentError::InvalidState(DownloadState::Completed).to_wire();
        assert_eq!(wire.code, ErrorCode::InvalidState);
        assert_eq!(
            wire.details.unwrap().current_state,
            Some(DownloadState::Completed)
        );
        let refused = AgentError::NotResumable.to_wire();
        assert_eq!(refused.code, ErrorCode::NotResumable);
        assert!(!refused.retryable);
    }

    #[test]
    fn internal_errors_carry_a_correlation_id_but_no_cause_text() {
        let wire = AgentError::HubGone(HubError::Gone).to_wire();
        let reason = wire.details.unwrap().reason.unwrap();
        assert!(reason.starts_with("correlation_id="), "{reason}");
        assert!(!wire.message.contains("hub"), "{}", wire.message);
    }

    #[test]
    fn storage_not_found_and_unavailable_are_distinguished() {
        let id = Uuid::nil();
        let missing = AgentError::Storage(StorageError::NotFound { id }).to_wire();
        assert_eq!(missing.code, ErrorCode::NotFound);
        let invalid = AgentError::Storage(StorageError::InvalidValue { field: "x" }).to_wire();
        assert_eq!(invalid.code, ErrorCode::Internal);
    }

    #[test]
    fn resync_reason_is_exposed_in_details() {
        let wire = AgentError::ResyncRequired("evicted").to_wire();
        assert_eq!(wire.details.unwrap().reason.as_deref(), Some("evicted"));
    }
}
