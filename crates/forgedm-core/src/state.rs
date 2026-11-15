//! Download state machine: the pure transition table of PHASE_0_1_PLAN Appendix A.6.
//!
//! [`transition`] takes the current state and a [`Command`] and returns either the
//! resulting [`Transition`] or a typed [`TransitionError`]. It performs no I/O; the
//! engine applies the result as a compare-and-set on the `from` state. [`on_startup`]
//! is the startup reconcile mapping (ARCHITECTURE 6.3 step 1).
//!
//! A repeated Pause, Resume, Cancel or Retry is an idempotent no-op that reports the
//! unchanged state (`changed == false`). "Active" for the Resume/Retry no-op covers
//! every state in which the download is already moving: probing, downloading, pausing,
//! verifying and finalizing (plus queued).

use forgedm_protocol::{DownloadState, ErrorCode};

/// An input to the state machine: a user command or an engine event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// User Pause. `resumable` is whether the server supports resuming this download;
    /// `discard_progress` is the user's decision for a non-resumable one.
    Pause {
        /// The download can continue from its committed bytes.
        resumable: bool,
        /// The user accepted resetting progress at Resume.
        discard_progress: bool,
    },
    /// User Resume (retry now from `retry_waiting`).
    Resume,
    /// User Cancel (the `.part` file is kept).
    Cancel,
    /// User Retry of a failed, cancelled or blocked download.
    Retry,
    /// Admission: `queued` -> `probing`.
    Admit,
    /// Admission of a download that is already fully committed: `queued` -> `verifying`.
    AdmitCommitted,
    /// The probe accepted the identity: `probing` -> `downloading`.
    ProbeSucceeded,
    /// The probe found a zero-length body: `probing` -> `verifying`.
    ZeroLength,
    /// All bytes are committed: `downloading` -> `verifying`.
    TransferComplete,
    /// A retryable failure with no worker active: -> `retry_waiting`.
    WaitRetry,
    /// A fatal failure, or an exhausted retry budget: -> `failed`.
    Fail,
    /// The server asked for credentials: -> `needs_credentials`.
    NeedCredentials,
    /// The URL expired: -> `needs_url_refresh`.
    NeedUrlRefresh,
    /// Disk full or destination unavailable, after quiesce: `downloading` -> `paused`.
    PauseForDisk,
    /// Workers quiesced after a pause request: `pausing` -> `paused`.
    Quiesced,
    /// The retry timer fired: `retry_waiting` -> `queued`.
    TimerDue,
    /// Shutdown or crash reconcile: -> `interrupted`.
    Interrupt,
    /// Startup auto-resume: `interrupted` -> `queued`.
    AutoResume,
    /// Length and hash checks passed: `verifying` -> `finalizing`.
    Verified,
    /// The no-replace rename and MOTW finished: `finalizing` -> `completed`.
    Complete,
}

/// The result of a legal [`Command`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    /// The resulting state (the current state for a no-op).
    pub to: DownloadState,
    /// Whether the state changed. A no-op needs no database write.
    pub changed: bool,
    /// Set when pausing an active non-resumable download with `discard_progress`: the
    /// progress is reset at Resume.
    pub discard_progress: bool,
}

/// Why a [`Command`] was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TransitionError {
    /// The command is not allowed in the current state (`invalid_state`).
    #[error("{command:?} is not allowed while the download is {}", .current.as_str())]
    InvalidState {
        /// The state the download was in.
        current: DownloadState,
        /// The refused command.
        command: Command,
    },
    /// Pause on an active non-resumable download without `discard_progress`.
    #[error("the download cannot resume; pause needs discard_progress")]
    NotResumable,
}

impl TransitionError {
    /// The wire error code for this refusal.
    #[must_use]
    pub const fn error_code(&self) -> ErrorCode {
        match self {
            Self::InvalidState { .. } => ErrorCode::InvalidState,
            Self::NotResumable => ErrorCode::NotResumable,
        }
    }
}

/// What to do with a download whose `.part` file turned out to be invalid (PCR-01).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartFileInvalidOutcome {
    /// Before `finalizing`: reset the segments and queue the download again.
    ResetAndRequeue,
    /// In `finalizing`: fail with `part_file_invalid`; the user's Retry restarts from 0.
    FailPartFileInvalid,
}

const fn moved(to: DownloadState) -> Transition {
    Transition {
        to,
        changed: true,
        discard_progress: false,
    }
}

const fn unchanged(state: DownloadState) -> Transition {
    Transition {
        to: state,
        changed: false,
        discard_progress: false,
    }
}

fn invalid(current: DownloadState, command: Command) -> TransitionError {
    TransitionError::InvalidState { current, command }
}

/// Applies `cmd` to a download in state `from`.
///
/// # Errors
///
/// [`TransitionError::InvalidState`] when the table has no edge for the pair, and
/// [`TransitionError::NotResumable`] for Pause on an active non-resumable download
/// without `discard_progress`.
pub fn transition(from: DownloadState, cmd: Command) -> Result<Transition, TransitionError> {
    match cmd {
        Command::Pause {
            resumable,
            discard_progress,
        } => pause(from, resumable, discard_progress),
        Command::Resume => resume(from),
        Command::Cancel => cancel(from),
        Command::Retry => retry(from),
        event => engine_event(from, event)
            .map(moved)
            .ok_or_else(|| invalid(from, event)),
    }
}

fn pause(
    from: DownloadState,
    resumable: bool,
    discard_progress: bool,
) -> Result<Transition, TransitionError> {
    use DownloadState as S;
    let cmd = Command::Pause {
        resumable,
        discard_progress,
    };
    match from {
        S::Queued | S::RetryWaiting | S::Interrupted => Ok(moved(S::Paused)),
        S::Probing | S::Downloading if resumable => Ok(moved(S::Pausing)),
        S::Probing | S::Downloading if discard_progress => Ok(Transition {
            to: S::Pausing,
            changed: true,
            discard_progress: true,
        }),
        S::Probing | S::Downloading => Err(TransitionError::NotResumable),
        S::Paused | S::Pausing => Ok(unchanged(from)),
        S::Verifying
        | S::Finalizing
        | S::Completed
        | S::Failed
        | S::Cancelled
        | S::NeedsCredentials
        | S::NeedsUrlRefresh
        | S::Unknown => Err(invalid(from, cmd)),
    }
}

fn resume(from: DownloadState) -> Result<Transition, TransitionError> {
    use DownloadState as S;
    match from {
        S::Paused | S::Interrupted | S::RetryWaiting => Ok(moved(S::Queued)),
        S::Queued | S::Probing | S::Downloading | S::Pausing | S::Verifying | S::Finalizing => {
            Ok(unchanged(from))
        }
        S::Completed
        | S::Failed
        | S::Cancelled
        | S::NeedsCredentials
        | S::NeedsUrlRefresh
        | S::Unknown => Err(invalid(from, Command::Resume)),
    }
}

fn cancel(from: DownloadState) -> Result<Transition, TransitionError> {
    use DownloadState as S;
    match from {
        S::Queued
        | S::Probing
        | S::Downloading
        | S::Pausing
        | S::Paused
        | S::RetryWaiting
        | S::Verifying
        | S::NeedsCredentials
        | S::NeedsUrlRefresh
        | S::Interrupted => Ok(moved(S::Cancelled)),
        S::Cancelled => Ok(unchanged(from)),
        S::Finalizing | S::Completed | S::Failed | S::Unknown => {
            Err(invalid(from, Command::Cancel))
        }
    }
}

fn retry(from: DownloadState) -> Result<Transition, TransitionError> {
    use DownloadState as S;
    match from {
        S::Failed | S::Cancelled | S::NeedsCredentials | S::NeedsUrlRefresh | S::Interrupted => {
            Ok(moved(S::Queued))
        }
        S::Queued | S::Probing | S::Downloading | S::Pausing | S::Verifying | S::Finalizing => {
            Ok(unchanged(from))
        }
        S::Paused | S::RetryWaiting | S::Completed | S::Unknown => {
            Err(invalid(from, Command::Retry))
        }
    }
}

/// The state-changing edges triggered by engine events (everything but the four user
/// commands). `None` means the table has no such edge.
fn engine_event(from: DownloadState, cmd: Command) -> Option<DownloadState> {
    use Command as C;
    use DownloadState as S;
    let to = match (from, cmd) {
        (S::Queued, C::Admit) => S::Probing,
        (S::Queued, C::AdmitCommitted) => S::Verifying,
        (S::Probing, C::ProbeSucceeded) => S::Downloading,
        (S::Probing, C::ZeroLength) | (S::Downloading, C::TransferComplete) => S::Verifying,
        (S::Probing | S::Downloading, C::WaitRetry) => S::RetryWaiting,
        (S::Probing | S::Downloading | S::RetryWaiting | S::Verifying | S::Finalizing, C::Fail) => {
            S::Failed
        }
        (S::Probing | S::Downloading, C::NeedCredentials) => S::NeedsCredentials,
        (S::Probing | S::Downloading, C::NeedUrlRefresh) => S::NeedsUrlRefresh,
        (S::Downloading, C::PauseForDisk) | (S::Pausing, C::Quiesced) => S::Paused,
        (S::RetryWaiting, C::TimerDue) | (S::Interrupted, C::AutoResume) => S::Queued,
        (S::Probing | S::Downloading | S::Pausing | S::Verifying, C::Interrupt) => S::Interrupted,
        (S::Verifying, C::Verified) => S::Finalizing,
        (S::Finalizing, C::Complete) => S::Completed,
        _ => return None,
    };
    Some(to)
}

/// The state a download is moved to when the agent starts, before any request is served
/// (ARCHITECTURE 6.3 step 1).
///
/// `probing`, `downloading` and `verifying` become `interrupted`; `pausing` becomes
/// `paused`. `finalizing` is returned unchanged: it is reconciled against the part file
/// (see [`part_file_invalid_outcome`]). Every other state, including `retry_waiting`, is
/// kept.
#[must_use]
pub const fn on_startup(state: DownloadState) -> DownloadState {
    use DownloadState as S;
    match state {
        S::Probing | S::Downloading | S::Verifying => S::Interrupted,
        S::Pausing => S::Paused,
        S::Queued
        | S::Paused
        | S::RetryWaiting
        | S::Finalizing
        | S::Completed
        | S::Failed
        | S::Cancelled
        | S::Interrupted
        | S::NeedsCredentials
        | S::NeedsUrlRefresh
        | S::Unknown => state,
    }
}

/// What an invalid `.part` file means for a download in `state` (PCR-01).
#[must_use]
pub const fn part_file_invalid_outcome(state: DownloadState) -> PartFileInvalidOutcome {
    match state {
        DownloadState::Finalizing => PartFileInvalidOutcome::FailPartFileInvalid,
        _ => PartFileInvalidOutcome::ResetAndRequeue,
    }
}
