//! Pipe client (ARCHITECTURE 3.5).
//!
//! [`Client::connect`] opens the pipe with tokio's defaults (SQOS identification kept),
//! retries `ERROR_PIPE_BUSY`, and verifies the server's SID and full canonical image path
//! **before any byte is written**; any verification failure aborts with nothing sent.

use std::os::windows::io::AsHandle;
use std::path::{Path, PathBuf};
use std::time::Duration;

use forgedm_platform::peer::PeerError;
use forgedm_platform::peer::pipe_server_pid;
use forgedm_platform::sid::Sid;
use forgedm_protocol::{
    ClientKind, Event, FrameDecoder, HandshakeRequest, HandshakeResult, Request, RequestBody,
    Response, ResultBody, ServerMessage, parse_server_message,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
use tokio::time::{Instant, sleep, timeout};

use crate::error::IpcError;
use crate::identity::verify_pid;
use crate::{CLIENT_REQUEST_TIMEOUT, PIPE_BUSY_RETRY_INTERVAL, PIPE_BUSY_RETRY_TOTAL};

/// `ERROR_PIPE_BUSY`: every instance of the pipe is connected.
const ERROR_PIPE_BUSY: i32 = 231;
/// Only local pipes are opened; a `\\host\pipe\...` name would reach out over SMB.
const LOCAL_PIPE_PREFIX: &str = r"\\.\pipe\";
const READ_CHUNK: usize = 16 * 1024;

/// What a client needs to find and trust the agent.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Full pipe name, `\\.\pipe\ForgeDM-<SID>[-<hash>]`.
    pub pipe_name: String,
    /// The agent executable the server process must be running from. Compared as a full
    /// canonical path, case-insensitively; there is no "any image" mode.
    pub expected_agent_path: PathBuf,
    /// Per-request timeout (default [`CLIENT_REQUEST_TIMEOUT`]).
    pub request_timeout: Duration,
}

impl ClientConfig {
    /// A config with the default request timeout.
    #[must_use]
    pub fn new(pipe_name: impl Into<String>, expected_agent_path: impl Into<PathBuf>) -> Self {
        Self {
            pipe_name: pipe_name.into(),
            expected_agent_path: expected_agent_path.into(),
            request_timeout: CLIENT_REQUEST_TIMEOUT,
        }
    }
}

/// A connected, verified client. One request at a time (`&mut self`).
#[derive(Debug)]
pub struct Client<S = NamedPipeClient> {
    stream: S,
    decoder: FrameDecoder,
    events: std::collections::VecDeque<Event>,
    next_request_id: u32,
    request_timeout: Duration,
    broken: bool,
}

impl Client<NamedPipeClient> {
    /// Connects to the agent and verifies it before sending anything.
    ///
    /// Must run inside a tokio runtime.
    ///
    /// # Errors
    /// [`IpcError::NotRunning`] when the pipe does not exist, [`IpcError::Busy`] after 5 s of
    /// `ERROR_PIPE_BUSY`, [`IpcError::ServerRejected`] when the server's SID or image path
    /// does not match (fail closed, zero bytes sent), otherwise I/O or platform errors.
    pub async fn connect(config: ClientConfig) -> Result<Self, IpcError> {
        if !config.pipe_name.starts_with(LOCAL_PIPE_PREFIX) {
            return Err(IpcError::InvalidPipeName);
        }
        let stream = open_with_retry(&config.pipe_name).await?;
        verify_server(&stream, &config.expected_agent_path)?;
        Ok(Self::from_verified_stream(stream, config.request_timeout))
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> Client<S> {
    /// Wraps a stream whose peer the caller has already verified.
    ///
    /// This performs **no** identity check; it exists so the request machinery can be
    /// driven over in-memory streams. Use [`Client::connect`] for real pipes.
    #[doc(hidden)]
    pub fn from_verified_stream(stream: S, request_timeout: Duration) -> Self {
        Self {
            stream,
            decoder: FrameDecoder::new(),
            events: std::collections::VecDeque::new(),
            next_request_id: 1,
            request_timeout,
            broken: false,
        }
    }

    /// Performs the mandatory first request.
    ///
    /// # Errors
    /// Any [`IpcError`]; an unsupported protocol range is [`IpcError::Server`].
    pub async fn handshake(
        &mut self,
        client_kind: ClientKind,
        client_name: &str,
        client_version: &str,
    ) -> Result<HandshakeResult, IpcError> {
        let body = RequestBody::Handshake(HandshakeRequest {
            client_kind,
            client_name: client_name.to_owned(),
            client_version: client_version.to_owned(),
            min_protocol: 1,
            max_protocol: 1,
        });
        match self.call(body).await? {
            ResultBody::Handshake(result) => Ok(result),
            _ => {
                self.broken = true;
                Err(IpcError::UnexpectedResult)
            }
        }
    }

    /// Sends a request and returns its typed result.
    ///
    /// # Errors
    /// [`IpcError::Server`] for an error response; [`IpcError::Timeout`] after the request
    /// timeout (the connection is then unusable); other [`IpcError`]s for transport faults.
    pub async fn call(&mut self, body: RequestBody) -> Result<ResultBody, IpcError> {
        let action = body.action();
        match self.request(body).await? {
            Response::Success { result, .. } => Ok(ResultBody::from_value(action, result)?),
            Response::Failure { error, .. } => Err(IpcError::Server(Box::new(error))),
        }
    }

    /// Sends a request and returns the raw response with the matching `request_id`.
    /// Events that arrive first are queued for [`next_event`](Self::next_event).
    ///
    /// # Errors
    /// As [`call`](Self::call), except error responses are returned as `Ok`.
    pub async fn request(&mut self, body: RequestBody) -> Result<Response, IpcError> {
        if self.broken {
            return Err(IpcError::Closed);
        }
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
        let frame = Request::new(request_id, body).to_frame()?;
        let outcome = timeout(self.request_timeout, self.exchange(request_id, &frame)).await;
        match outcome {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(err)) => {
                self.broken = true;
                Err(err)
            }
            Err(_elapsed) => {
                self.broken = true;
                Err(IpcError::Timeout)
            }
        }
    }

    /// Waits for the next event (no timeout; wrap it with one if needed).
    ///
    /// # Errors
    /// [`IpcError::Closed`] when the connection ends, or a protocol error.
    pub async fn next_event(&mut self) -> Result<Event, IpcError> {
        if self.broken {
            return Err(IpcError::Closed);
        }
        if let Some(event) = self.events.pop_front() {
            return Ok(event);
        }
        loop {
            match self.read_message().await {
                Ok(ServerMessage::Event(event)) => return Ok(event),
                Ok(ServerMessage::Response(_) | ServerMessage::UnknownEvent { .. }) => {}
                Err(err) => {
                    self.broken = true;
                    return Err(err);
                }
            }
        }
    }

    async fn exchange(&mut self, request_id: u32, frame: &[u8]) -> Result<Response, IpcError> {
        self.stream.write_all(frame).await.map_err(map_io)?;
        self.stream.flush().await.map_err(map_io)?;
        loop {
            match self.read_message().await? {
                ServerMessage::Response(r) if r.request_id() == request_id => return Ok(r),
                ServerMessage::Event(event) => self.events.push_back(event),
                ServerMessage::Response(other) => {
                    tracing::warn!(
                        request_id = other.request_id(),
                        "ignoring a response that matches no in-flight request"
                    );
                }
                // An event this build does not know.
                ServerMessage::UnknownEvent { .. } => {}
            }
        }
    }

    async fn read_message(&mut self) -> Result<ServerMessage, IpcError> {
        let mut buf = vec![0u8; READ_CHUNK];
        loop {
            if let Some(body) = self.decoder.next_frame() {
                return Ok(parse_server_message(&body)?);
            }
            let n = self.stream.read(&mut buf).await.map_err(map_io)?;
            let chunk = buf.get(..n).filter(|c| !c.is_empty());
            let Some(chunk) = chunk else {
                return Err(IpcError::Closed);
            };
            self.decoder.feed(chunk)?;
        }
    }
}

fn map_io(err: std::io::Error) -> IpcError {
    use std::io::ErrorKind::{BrokenPipe, ConnectionAborted, ConnectionReset, UnexpectedEof};
    if matches!(
        err.kind(),
        BrokenPipe | ConnectionAborted | ConnectionReset | UnexpectedEof
    ) {
        IpcError::Closed
    } else {
        IpcError::Io(err)
    }
}

/// Opens the pipe, retrying `ERROR_PIPE_BUSY` every 50 ms for up to 5 s.
async fn open_with_retry(name: &str) -> Result<NamedPipeClient, IpcError> {
    let started = Instant::now();
    loop {
        match ClientOptions::new().open(name) {
            Ok(client) => return Ok(client),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(IpcError::NotRunning);
            }
            Err(err) if err.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                if started.elapsed() >= PIPE_BUSY_RETRY_TOTAL {
                    return Err(IpcError::Busy);
                }
                sleep(PIPE_BUSY_RETRY_INTERVAL).await;
            }
            Err(err) => return Err(IpcError::Io(err)),
        }
    }
}

/// Checks the server process behind `stream` before anything is written.
fn verify_server(stream: &NamedPipeClient, expected_image: &Path) -> Result<(), IpcError> {
    let check = || -> Result<(), PeerError> {
        let pid = pipe_server_pid(stream.as_handle())?;
        let me = Sid::current_user()?;
        verify_pid(pid, &me, Some(expected_image)).map(|_| ())
    };
    check().map_err(|err| {
        tracing::warn!(
            target: "forgedm::security",
            reason = %err,
            "pipe server failed identity verification; nothing was sent"
        );
        IpcError::ServerRejected(err)
    })
}
