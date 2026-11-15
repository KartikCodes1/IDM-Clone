//! Engine error types and the mapping from low-level failures to download error codes
//! (PHASE_0_1_PLAN A.7, ARCHITECTURE 4.10).
//!
//! Messages are generic: they never contain URLs, paths or other request values.

use std::io;

use forgedm_platform::PlatformError;
use forgedm_platform::fs::{IoClass, classify_io};
use forgedm_protocol::{DownloadError, DownloadErrorCode, DownloadErrorDetails, DownloadState};
use thiserror::Error;

use crate::retry::TransferErrorKind;

/// Why a download ended in `failed` (or must be paused or blocked).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// Machine code (A.7).
    pub code: DownloadErrorCode,
    /// Code parameters (`http_status{status}`, `range_protocol_violation{reason}`).
    pub details: Option<DownloadErrorDetails>,
}

impl Failure {
    /// A failure with `code` and no parameters.
    #[must_use]
    pub const fn new(code: DownloadErrorCode) -> Self {
        Self {
            code,
            details: None,
        }
    }

    /// `http_status{status}`.
    #[must_use]
    pub fn http_status(status: u16) -> Self {
        Self {
            code: DownloadErrorCode::HttpStatus,
            details: Some(DownloadErrorDetails {
                status: Some(status),
                ..DownloadErrorDetails::default()
            }),
        }
    }

    /// `range_protocol_violation{reason}`.
    #[must_use]
    pub fn protocol_violation(reason: &str) -> Self {
        Self {
            code: DownloadErrorCode::RangeProtocolViolation,
            details: Some(DownloadErrorDetails {
                reason: Some(reason.to_owned()),
                ..DownloadErrorDetails::default()
            }),
        }
    }

    /// Whether a later retry by the user (or, from S1.12, automatically) can succeed.
    #[must_use]
    pub const fn retryable(&self) -> bool {
        matches!(
            self.code,
            DownloadErrorCode::NetworkTimeout
                | DownloadErrorCode::NetworkConnect
                | DownloadErrorCode::NetworkReset
                | DownloadErrorCode::BodyIncomplete
                | DownloadErrorCode::DnsFailure
                | DownloadErrorCode::RateLimited
                | DownloadErrorCode::FinalizeLocked
                | DownloadErrorCode::DiskFull
                | DownloadErrorCode::DestinationUnavailable
        )
    }

    /// The wire form: generic message, retryability and parameters.
    #[must_use]
    pub fn to_download_error(&self) -> DownloadError {
        DownloadError {
            code: self.code,
            message: message_for(self.code).to_owned(),
            retryable: self.retryable(),
            details: self.details.clone(),
        }
    }
}

/// Generic user-facing text for a failure code. Never includes URLs or paths.
#[must_use]
pub const fn message_for(code: DownloadErrorCode) -> &'static str {
    use DownloadErrorCode as C;
    match code {
        C::NetworkTimeout => "the connection timed out",
        C::NetworkConnect => "could not connect to the server",
        C::NetworkReset => "the connection was reset",
        C::BodyIncomplete => "the server closed the connection before the file was complete",
        C::DnsFailure => "the host name could not be resolved",
        C::RateLimited => "the server is rate limiting requests",
        C::HttpStatus => "the server answered with an error status",
        C::AuthRequired => "the server requires credentials",
        C::UrlExpired => "the download link has expired",
        C::TlsFailure => "the secure connection could not be verified",
        C::TooManyRedirects => "too many redirects",
        C::RedirectLoop => "redirect loop",
        C::RedirectDowngrade => "the server redirected from https to http",
        C::RedirectBlocked => "a redirect to a blocked destination was refused",
        C::RangeProtocolViolation => "the server broke the HTTP range protocol",
        C::UnsupportedContentEncoding => "the server sent a content-encoded response",
        C::RemoteChanged => "the remote file changed",
        C::SizeMismatch => "the downloaded size does not match the expected size",
        C::ChecksumMismatch => "the SHA-256 checksum does not match",
        C::DiskFull => "the disk is full",
        C::DestinationUnavailable => "the destination folder is not available",
        C::DiskIo => "a disk error occurred",
        C::PathTooLong => "the file path is too long",
        C::FileSystemLimit => "the file system cannot hold this file",
        C::SecretsUnavailable => "the stored URL cannot be read",
        C::BlockedByPolicy => "the file was blocked by security policy",
        C::BlockedByAntivirus => "the file was blocked by antivirus software",
        C::Internal | C::Unknown => "an internal error occurred",
        C::PathEscape => "the destination resolved outside the chosen folder",
        C::FinalizeLocked => "the finished file could not be moved into place",
        C::RetryBudgetExhausted => "too many consecutive failures",
        C::PartFileInvalid => "the partial file is missing or was changed",
    }
}

/// Why a download task stopped before reaching `completed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStop {
    /// Ends in `failed`.
    Fail(Failure),
    /// 401 or 407: ends in `needs_credentials`.
    NeedsCredentials,
    /// Disk full or destination gone: ends in `paused` with this reason (`DiskFull` or
    /// `DestinationUnavailable`).
    PauseFor(DownloadErrorCode),
    /// Stopped on request: see [`StopReason`](crate::engine::StopReason).
    Stopped(crate::engine::StopReason),
}

impl From<Failure> for TaskStop {
    fn from(failure: Failure) -> Self {
        Self::Fail(failure)
    }
}

/// Maps a transport failure to the failure recorded when no automatic retry exists yet
/// (automatic retries arrive with S1.12).
#[must_use]
pub fn failure_from_transfer(kind: TransferErrorKind) -> TaskStop {
    use DownloadErrorCode as C;
    use TransferErrorKind as K;
    let code = match kind {
        K::ConnectTimeout | K::ReadTimeout => C::NetworkTimeout,
        K::ConnectRefused => C::NetworkConnect,
        K::ConnectionReset => C::NetworkReset,
        K::PrematureEof => C::BodyIncomplete,
        K::DnsTemporary | K::DnsFailure => C::DnsFailure,
        K::Tls => C::TlsFailure,
        K::DiskFull => return TaskStop::PauseFor(C::DiskFull),
        K::DeviceGone => return TaskStop::PauseFor(C::DestinationUnavailable),
        K::LocalIo => C::DiskIo,
        K::TooManyRedirects => C::TooManyRedirects,
        K::RedirectLoop => C::RedirectLoop,
        K::RedirectDowngrade => C::RedirectDowngrade,
        K::RedirectBlocked => C::RedirectBlocked,
        K::MalformedFraming => C::RangeProtocolViolation,
    };
    TaskStop::Fail(Failure::new(code))
}

/// Maps a local I/O failure by its OS code, never by message text.
#[must_use]
pub fn stop_from_io(err: &io::Error) -> TaskStop {
    stop_from_class(classify_io(err))
}

/// Maps an [`IoClass`] to the resulting stop.
#[must_use]
pub fn stop_from_class(class: IoClass) -> TaskStop {
    match class {
        IoClass::DiskFull => TaskStop::PauseFor(DownloadErrorCode::DiskFull),
        IoClass::DeviceGone => TaskStop::PauseFor(DownloadErrorCode::DestinationUnavailable),
        IoClass::AccessDenied | IoClass::Sharing | IoClass::Other => {
            TaskStop::Fail(Failure::new(DownloadErrorCode::DiskIo))
        }
    }
}

/// Maps a platform failure (file creation, finalize, MOTW) to the resulting stop.
#[must_use]
pub fn stop_from_platform(err: &PlatformError) -> TaskStop {
    use DownloadErrorCode as C;
    match err {
        PlatformError::DiskFull => TaskStop::PauseFor(C::DiskFull),
        PlatformError::DestinationUnavailable => TaskStop::PauseFor(C::DestinationUnavailable),
        PlatformError::PathTooLong => TaskStop::Fail(Failure::new(C::PathTooLong)),
        PlatformError::FileSystemLimit(_) => TaskStop::Fail(Failure::new(C::FileSystemLimit)),
        PlatformError::PathEscape => TaskStop::Fail(Failure::new(C::PathEscape)),
        PlatformError::SharingViolation => TaskStop::Fail(Failure::new(C::FinalizeLocked)),
        PlatformError::MotwBlockedByPolicy => TaskStop::Fail(Failure::new(C::BlockedByPolicy)),
        PlatformError::MotwBlockedByAntivirus => {
            TaskStop::Fail(Failure::new(C::BlockedByAntivirus))
        }
        PlatformError::Io(io) => stop_from_io(io),
        PlatformError::Win32 { .. }
        | PlatformError::InvalidInput(_)
        | PlatformError::AlreadyExists
        | PlatformError::Unsupported(_)
        | PlatformError::MotwFailed { .. } => TaskStop::Fail(Failure::new(C::DiskIo)),
    }
}

/// Maps a store failure raised while a download runs: unreadable secrets fail the download
/// as `secrets_unavailable`; anything else is an `internal` failure (the cause is logged,
/// never shown).
#[must_use]
pub fn stop_from_store(err: &StoreError) -> TaskStop {
    match err {
        StoreError::SecretsUnavailable => {
            TaskStop::Fail(Failure::new(DownloadErrorCode::SecretsUnavailable))
        }
        other => {
            tracing::error!(error = %other, "storage failed while a download was running");
            TaskStop::Fail(Failure::new(DownloadErrorCode::Internal))
        }
    }
}

/// A persistence failure reported through the [`DownloadStore`](crate::ports::DownloadStore)
/// port. The adapter maps its own errors onto these.
#[derive(Debug, Error)]
pub enum StoreError {
    /// No such download.
    #[error("download not found")]
    NotFound,
    /// The compare-and-set lost: the stored state is not the expected one.
    #[error("download is not in state {expected:?} (actual: {actual:?})")]
    Conflict {
        /// State the caller expected.
        expected: DownloadState,
        /// State actually stored, when known.
        actual: Option<DownloadState>,
    },
    /// The sealed URL cannot be read.
    #[error("stored secrets are unavailable")]
    SecretsUnavailable,
    /// Any other storage failure.
    #[error("storage unavailable: {0}")]
    Unavailable(String),
}

/// What the engine API (`EngineHandle`) can fail with.
#[derive(Debug, Error)]
pub enum EngineError {
    /// The engine channel is full or the engine did not answer in time (`busy`).
    #[error("the engine is busy")]
    Busy,
    /// The engine has stopped.
    #[error("the engine is shutting down")]
    ShuttingDown,
    /// The queue holds the maximum number of downloads (`limit_exceeded`).
    #[error("the download queue is full")]
    LimitExceeded,
    /// No such download (`not_found`).
    #[error("download not found")]
    NotFound,
    /// The command is not allowed in the download's current state (`invalid_state`).
    #[error("the command is not allowed while the download is {}", .0.as_str())]
    InvalidState(DownloadState),
    /// Pause of an active non-resumable download without `discard_progress`.
    #[error("the download cannot resume; pause needs discard_progress")]
    NotResumable,
    /// A persistence failure.
    #[error(transparent)]
    Store(#[from] StoreError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_full_and_device_gone_pause_instead_of_failing() {
        assert_eq!(
            failure_from_transfer(TransferErrorKind::DiskFull),
            TaskStop::PauseFor(DownloadErrorCode::DiskFull)
        );
        assert_eq!(
            failure_from_transfer(TransferErrorKind::DeviceGone),
            TaskStop::PauseFor(DownloadErrorCode::DestinationUnavailable)
        );
        assert_eq!(
            stop_from_platform(&PlatformError::DiskFull),
            TaskStop::PauseFor(DownloadErrorCode::DiskFull)
        );
    }

    #[test]
    fn premature_eof_is_body_incomplete_and_retryable() {
        let TaskStop::Fail(failure) = failure_from_transfer(TransferErrorKind::PrematureEof) else {
            unreachable!("not a failure");
        };
        assert_eq!(failure.code, DownloadErrorCode::BodyIncomplete);
        assert!(failure.to_download_error().retryable);
    }

    #[test]
    fn io_errors_are_classified_by_os_code() {
        let full = io::Error::from_raw_os_error(112);
        assert_eq!(
            stop_from_io(&full),
            TaskStop::PauseFor(DownloadErrorCode::DiskFull)
        );
        let other = io::Error::from(io::ErrorKind::PermissionDenied);
        assert_eq!(
            stop_from_io(&other),
            TaskStop::Fail(Failure::new(DownloadErrorCode::DiskIo))
        );
    }

    #[test]
    fn http_status_failure_carries_the_status_and_a_generic_message() {
        let error = Failure::http_status(404).to_download_error();
        assert_eq!(error.details.and_then(|d| d.status), Some(404));
        assert!(!error.message.contains("404"));
    }
}
