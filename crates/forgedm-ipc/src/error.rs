//! Transport errors for the pipe server and client.

use forgedm_platform::PlatformError;
use forgedm_platform::peer::PeerError;
use forgedm_platform::pipe::PipeError;
use forgedm_protocol::{EncodeError, ErrorBody, FrameError, ParseError};

/// Why an IPC operation failed.
///
/// Messages never contain request values, URLs or secrets.
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    /// No agent is listening on the pipe (the pipe does not exist).
    #[error("the agent is not running")]
    NotRunning,
    /// Every pipe instance stayed busy for the whole retry window.
    #[error("the agent is busy: all pipe instances are in use")]
    Busy,
    /// The pipe name is already taken, possibly squatted. The server never starts on it.
    #[error("the pipe name is already in use")]
    NameInUse,
    /// A listening pipe instance could not be created after startup. The server stops and
    /// the agent must exit; the creation is never retried without `first_pipe_instance`.
    #[error("a replacement pipe instance could not be created")]
    Fatal(#[source] PipeError),
    /// The server end of the pipe failed identity verification; nothing was sent.
    #[error("the pipe server failed identity verification")]
    ServerRejected(#[source] PeerError),
    /// An operating-system call in the platform layer failed.
    #[error(transparent)]
    Platform(#[from] PlatformError),
    /// Pipe I/O failed.
    #[error("pipe I/O failed")]
    Io(#[from] std::io::Error),
    /// The request did not complete in time; the connection is no longer usable.
    #[error("the request timed out")]
    Timeout,
    /// The connection is closed (by the peer, or after an earlier failure).
    #[error("the connection is closed")]
    Closed,
    /// The peer sent a frame that violates the framing rules.
    #[error("invalid frame from the peer")]
    Frame(#[from] FrameError),
    /// The peer sent a message that is not a valid response or event.
    #[error("invalid message from the peer")]
    Protocol(#[from] ParseError),
    /// The agent answered with a result that does not belong to the request (protocol bug).
    #[error("unexpected result for the request")]
    UnexpectedResult,
    /// The pipe name is not a local pipe (it must start with `\\.\pipe\`).
    #[error("the pipe name is not a local pipe")]
    InvalidPipeName,
    /// A request could not be encoded.
    #[error("request could not be encoded")]
    Encode(#[from] EncodeError),
    /// A result did not have the shape of the requested action.
    #[error("result does not match the action")]
    Decode(#[from] serde_json::Error),
    /// The agent answered with an error response.
    #[error("the agent returned an error: {}", .0.message)]
    Server(Box<ErrorBody>),
}
