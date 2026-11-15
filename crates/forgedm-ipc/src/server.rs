//! Pipe server (ADR-005, ARCHITECTURE 3.2).
//!
//! # Capacity invariant
//! `PIPE_MAX_INSTANCES = MAX_ACTIVE_CONNECTIONS + 1 = 17`. The accept loop acquires one of
//! 16 semaphore permits **before** awaiting `connect()` on the listening instance, and
//! creates the replacement listening instance **before** the connected handle is handed
//! off or dropped. The instance count therefore never reaches zero while serving, so the
//! name cannot be re-created by a squatter. A creation failure after startup is fatal and
//! stops the server; it is never retried without `first_pipe_instance(true)`.
//!
//! # Per connection
//! The caller's SID is checked (via the pipe's client process id) before any frame is read.
//! Handshake must arrive within 5 s, a frame body within 10 s of its start, and a
//! connection with neither a subscription nor traffic is closed after 60 s. Requests on a
//! connection are processed strictly one at a time, in order.

use std::collections::VecDeque;
use std::future::Future;
use std::os::windows::io::AsHandle;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use forgedm_platform::PlatformError;
use forgedm_platform::peer::{PeerError, PeerIdentity, pipe_client_pid};
use forgedm_platform::pipe::{PipeError, PipeSecurity, create_pipe_instance};
use forgedm_platform::sid::Sid;
use forgedm_protocol::{
    ErrorBody, ErrorCode, FRAME_BODY_TIMEOUT, FrameDecoder, FrameError, HANDSHAKE_TIMEOUT,
    HandshakeRequest, IDLE_TIMEOUT, ParseError, Request, RequestBody, Response, negotiate_version,
    parse_request,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::windows::named_pipe::NamedPipeServer;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep, sleep_until, timeout};
use tokio_util::sync::CancellationToken;

use crate::MAX_ACTIVE_CONNECTIONS;
use crate::error::IpcError;
use crate::identity::verify_pid;
use crate::subscribe::{EventReceiver, EventSink, SubscriptionItem, event_channel};

const READ_CHUNK: usize = 16 * 1024;
/// `ERROR_PIPE_BUSY`: all instances of the pipe exist (the count is at the maximum).
const ERROR_PIPE_BUSY: u32 = 231;
/// How long a busy replacement creation is retried while the connected instance is held.
const REPLACEMENT_BUSY_GRACE: Duration = Duration::from_secs(1);
const REPLACEMENT_RETRY_INTERVAL: Duration = Duration::from_millis(5);
/// `request_id` used in error responses to frames that carry no recoverable id.
const NO_REQUEST_ID: u32 = 0;

/// The future a [`RequestHandler`] returns.
pub type HandlerFuture<'a> = Pin<Box<dyn Future<Output = Response> + Send + 'a>>;

/// Answers requests. The agent implements this; tests use a fake.
///
/// The server has already parsed and validated the request, negotiated the protocol
/// version and enforced "handshake first" before calling `handle`. The `Handshake` request
/// itself is passed through so the handler can build its result; if the handler answers it
/// with a failure, the connection is closed.
pub trait RequestHandler: Send + Sync + 'static {
    /// Handles one request. At most one call per connection is in flight at a time.
    fn handle<'a>(&'a self, conn: &'a mut Connection, request: Request) -> HandlerFuture<'a>;
}

/// Per-connection state visible to the handler.
#[derive(Debug)]
pub struct Connection {
    id: u64,
    peer: PeerIdentity,
    hello: Option<HandshakeRequest>,
    subscription: Option<EventReceiver>,
}

impl Connection {
    /// A connection for `peer`. The server creates these; tests may too.
    #[must_use]
    pub fn new(id: u64, peer: PeerIdentity) -> Self {
        Self {
            id,
            peer,
            hello: None,
            subscription: None,
        }
    }

    /// Server-assigned connection number, unique per server.
    #[must_use]
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The verified peer process.
    #[must_use]
    pub fn peer(&self) -> &PeerIdentity {
        &self.peer
    }

    /// The handshake payload; present while (and after) the handshake request is handled.
    #[must_use]
    pub fn hello(&self) -> Option<&HandshakeRequest> {
        self.hello.as_ref()
    }

    /// Starts (or replaces) this connection's subscription and returns the hub's sink.
    /// Events pushed into the sink are written after the response to the current request.
    pub fn subscribe(&mut self) -> EventSink {
        let (sink, receiver) = event_channel();
        self.subscription = Some(receiver);
        sink
    }

    /// Ends the subscription; the hub's sink starts reporting `Closed`.
    pub fn unsubscribe(&mut self) {
        self.subscription = None;
    }

    /// Whether a subscription is active.
    #[must_use]
    pub fn is_subscribed(&self) -> bool {
        self.subscription.is_some()
    }
}

/// Connection timeouts. Defaults are the canonical constants (PHASE_0_1_PLAN A.9); only
/// tests shorten them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    /// Handshake must arrive this soon after the connection opens.
    pub handshake: Duration,
    /// A frame must complete this soon after its first byte.
    pub frame_body: Duration,
    /// A connection with no subscription and no traffic is closed after this.
    pub idle: Duration,
    /// A single write to the client may take at most this long (stuck-reader guard).
    pub write: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            handshake: HANDSHAKE_TIMEOUT,
            frame_body: FRAME_BODY_TIMEOUT,
            idle: IDLE_TIMEOUT,
            write: FRAME_BODY_TIMEOUT,
        }
    }
}

/// Server settings.
#[derive(Debug, Clone, Copy, Default)]
pub struct ServerOptions {
    /// Connection timeouts.
    pub timeouts: Timeouts,
}

/// Why a connection ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// The client closed its end.
    PeerClosed,
    /// The server is shutting down.
    Cancelled,
    /// No handshake in time.
    HandshakeTimeout,
    /// A frame did not complete in time.
    FrameTimeout,
    /// No traffic and no subscription for the idle period.
    IdleTimeout,
    /// Malformed or oversized frame, bad first request, or an unsupported version.
    ProtocolViolation,
    /// The handler refused the handshake.
    HandshakeRejected,
    /// The caller is not the expected user.
    PeerRejected,
    /// Reading or writing the pipe failed (including a stuck reader).
    Io,
}

/// A bound pipe server. [`bind`](Self::bind) claims the name; [`serve`](Self::serve) runs it.
pub struct Server {
    listener: NamedPipeServer,
    name: String,
    security: PipeSecurity,
    expected_sid: Sid,
    handler: Arc<dyn RequestHandler>,
    options: ServerOptions,
}

impl Server {
    /// Claims `name` by creating the first instance with `first_pipe_instance(true)`.
    ///
    /// Must run inside a tokio runtime. No connection is accepted until
    /// [`serve`](Self::serve), so the caller can finish startup first.
    ///
    /// # Errors
    /// [`IpcError::NameInUse`] when the name exists (possibly squatted; a `forgedm::security`
    /// event is logged and the caller must not serve), [`IpcError::Platform`] otherwise.
    pub fn bind(
        name: &str,
        handler: Arc<dyn RequestHandler>,
        options: ServerOptions,
    ) -> Result<Self, IpcError> {
        let security = PipeSecurity::for_current_user()?;
        let expected_sid = Sid::current_user()?;
        let listener = match create_pipe_instance(name, true, &security) {
            Ok(listener) => listener,
            Err(PipeError::NameInUse) => {
                tracing::error!(
                    target: "forgedm::security",
                    pipe = name,
                    "pipe name is already in use; refusing to serve"
                );
                return Err(IpcError::NameInUse);
            }
            Err(PipeError::Platform(err)) => return Err(IpcError::Platform(err)),
        };
        Ok(Self {
            listener,
            name: name.to_owned(),
            security,
            expected_sid,
            handler,
            options,
        })
    }

    /// Accepts connections until `shutdown` is cancelled or a replacement instance cannot be
    /// created. Open connections are told to stop and are awaited before returning.
    ///
    /// Handlers are not cancelled: a handler that never finishes keeps `serve` from
    /// returning. The caller must bound the call, for example with `tokio::time::timeout`;
    /// dropping the future aborts every connection task.
    ///
    /// # Errors
    /// [`IpcError::Fatal`] when a listening instance could not be created after startup.
    pub async fn serve(mut self, shutdown: CancellationToken) -> Result<(), IpcError> {
        let permits = Arc::new(Semaphore::new(MAX_ACTIVE_CONNECTIONS));
        let connections = shutdown.child_token();
        let mut tasks = JoinSet::new();
        let outcome = self.accept_loop(&permits, &connections, &mut tasks).await;
        connections.cancel();
        while let Some(joined) = tasks.join_next().await {
            log_join(joined);
        }
        outcome
    }

    async fn accept_loop(
        &mut self,
        permits: &Arc<Semaphore>,
        cancel: &CancellationToken,
        tasks: &mut JoinSet<()>,
    ) -> Result<(), IpcError> {
        let ids = AtomicU64::new(1);
        loop {
            while let Some(joined) = tasks.try_join_next() {
                log_join(joined);
            }
            // Permit first, then connect: at most 16 connected handles exist.
            let permit = tokio::select! {
                biased;
                () = cancel.cancelled() => return Ok(()),
                permit = permits.clone().acquire_owned() => {
                    permit.map_err(|_| IpcError::Closed)?
                }
            };
            let connected = tokio::select! {
                biased;
                () = cancel.cancelled() => return Ok(()),
                result = self.listener.connect() => result,
            };
            // The replacement exists before the connected handle goes anywhere.
            let next = self.create_replacement().await?;
            let accepted = std::mem::replace(&mut self.listener, next);
            match connected {
                Ok(()) => {
                    let id = ids.fetch_add(1, Ordering::Relaxed);
                    tasks.spawn(self.connection_task(accepted, permit, id, cancel.clone()));
                }
                Err(err) => {
                    // The client vanished before we accepted it; the instance is unusable.
                    tracing::debug!(error = %err, "dropping a pipe instance that failed to connect");
                    drop(accepted);
                    drop(permit);
                }
            }
        }
    }

    /// Creates the next listening instance (`first_pipe_instance(false)`).
    ///
    /// A handle that was just dropped is closed lazily by the I/O driver, so for a moment the
    /// instance count can still read 17 and creation reports `ERROR_PIPE_BUSY`. That one
    /// error is retried for at most [`REPLACEMENT_BUSY_GRACE`]; this is safe because the
    /// connected instance is still held, so the name cannot have vanished and cannot be
    /// re-created by anyone else. Every other error, and a persistent busy, is fatal.
    async fn create_replacement(&self) -> Result<NamedPipeServer, IpcError> {
        let give_up_at = Instant::now() + REPLACEMENT_BUSY_GRACE;
        loop {
            match create_pipe_instance(&self.name, false, &self.security) {
                Ok(instance) => return Ok(instance),
                Err(PipeError::Platform(PlatformError::Win32 { code, .. }))
                    if code == ERROR_PIPE_BUSY && Instant::now() < give_up_at =>
                {
                    sleep(REPLACEMENT_RETRY_INTERVAL).await;
                }
                Err(err) => {
                    tracing::error!(
                        target: "forgedm::security",
                        error = %err,
                        "listening pipe instance could not be created; the server is stopping"
                    );
                    return Err(IpcError::Fatal(err));
                }
            }
        }
    }

    fn connection_task(
        &self,
        stream: NamedPipeServer,
        permit: OwnedSemaphorePermit,
        id: u64,
        cancel: CancellationToken,
    ) -> impl Future<Output = ()> + Send + use<> {
        let handler = self.handler.clone();
        let expected_sid = self.expected_sid.clone();
        let timeouts = self.options.timeouts;
        async move {
            match check_caller(&stream, &expected_sid) {
                Ok(peer) => {
                    let conn = Connection::new(id, peer);
                    let reason =
                        serve_connection(stream, conn, handler.as_ref(), &timeouts, &cancel).await;
                    tracing::debug!(connection = id, ?reason, "connection closed");
                }
                Err(err) => {
                    tracing::warn!(
                        target: "forgedm::security",
                        connection = id,
                        reason = %err,
                        "pipe caller rejected; closing without reading"
                    );
                    drop(stream);
                }
            }
            // The handle is closed above, so the instance slot is free before the permit.
            drop(permit);
        }
    }
}

/// Resolves the client process of `stream` and requires it to run as `expected_sid`.
fn check_caller(stream: &NamedPipeServer, expected_sid: &Sid) -> Result<PeerIdentity, PeerError> {
    let pid = pipe_client_pid(stream.as_handle())?;
    verify_pid(pid, expected_sid, None)
}

/// What to do after handling one input.
enum Flow {
    Continue,
    Close(CloseReason),
}

/// One client conversation over `stream`. Returns why it ended; the stream is dropped
/// (closed) before this returns.
///
/// The caller must already have verified `conn.peer()`. Public so timing behaviour can be
/// tested over in-memory streams.
#[doc(hidden)]
pub async fn serve_connection<S>(
    stream: S,
    conn: Connection,
    handler: &dyn RequestHandler,
    timeouts: &Timeouts,
    cancel: &CancellationToken,
) -> CloseReason
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let now = Instant::now();
    let session = Session {
        stream,
        conn,
        handler,
        timeouts,
        decoder: FrameDecoder::new(),
        pending: VecDeque::new(),
        frame_error: None,
        started: now,
        last_activity: now,
        frame_started: None,
        handshaken: false,
    };
    session.run(cancel).await
}

struct Session<'a, S> {
    stream: S,
    conn: Connection,
    handler: &'a dyn RequestHandler,
    timeouts: &'a Timeouts,
    decoder: FrameDecoder,
    pending: VecDeque<Vec<u8>>,
    frame_error: Option<FrameError>,
    started: Instant,
    last_activity: Instant,
    frame_started: Option<Instant>,
    handshaken: bool,
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send> Session<'_, S> {
    async fn run(mut self, cancel: &CancellationToken) -> CloseReason {
        let mut buf = vec![0u8; READ_CHUNK];
        loop {
            while let Some(body) = self.pending.pop_front() {
                self.last_activity = Instant::now();
                if let Flow::Close(reason) = self.handle_frame(&body).await {
                    return reason;
                }
                self.last_activity = Instant::now();
            }
            if let Some(err) = self.frame_error.take() {
                self.reply_and_close(NO_REQUEST_ID, ErrorBody::new(err.error_code()))
                    .await;
                return CloseReason::ProtocolViolation;
            }
            if let Flow::Close(reason) = self.wait(&mut buf, cancel).await {
                return reason;
            }
        }
    }

    /// The nearest deadline that applies right now, and what it means.
    fn next_deadline(&self) -> Option<(Instant, CloseReason)> {
        let frame = self
            .frame_started
            .map(|t| (t + self.timeouts.frame_body, CloseReason::FrameTimeout));
        let connection = if !self.handshaken {
            Some((
                self.started + self.timeouts.handshake,
                CloseReason::HandshakeTimeout,
            ))
        } else if self.conn.subscription.is_none() {
            Some((
                self.last_activity + self.timeouts.idle,
                CloseReason::IdleTimeout,
            ))
        } else {
            None
        };
        [frame, connection]
            .into_iter()
            .flatten()
            .min_by_key(|(at, _)| *at)
    }

    async fn wait(&mut self, buf: &mut [u8], cancel: &CancellationToken) -> Flow {
        let deadline = self.next_deadline();
        tokio::select! {
            biased;
            () = cancel.cancelled() => Flow::Close(CloseReason::Cancelled),
            () = sleep_until_or_never(deadline.map(|(at, _)| at)) => {
                let reason = deadline.map_or(CloseReason::IdleTimeout, |(_, r)| r);
                self.on_timeout(reason).await
            }
            item = next_item(&mut self.conn.subscription) => self.forward(item).await,
            read = self.stream.read(buf) => self.on_read(read, buf).await,
        }
    }

    async fn on_timeout(&mut self, reason: CloseReason) -> Flow {
        let code = match reason {
            CloseReason::HandshakeTimeout => Some(ErrorCode::HandshakeRequired),
            CloseReason::FrameTimeout => Some(ErrorCode::MalformedFrame),
            _ => None,
        };
        if let Some(code) = code {
            self.reply_and_close(NO_REQUEST_ID, ErrorBody::new(code))
                .await;
        }
        Flow::Close(reason)
    }

    async fn forward(&mut self, item: SubscriptionItem) -> Flow {
        let (event, ends) = match item {
            SubscriptionItem::Event(event) => (Some(event), false),
            SubscriptionItem::Resync(event) => (Some(event), true),
            SubscriptionItem::Closed => (None, true),
        };
        if ends {
            self.conn.subscription = None;
            // The client gets a full idle period to resynchronise on this connection.
            self.last_activity = Instant::now();
        }
        let Some(event) = event else {
            return Flow::Continue;
        };
        match event.to_frame() {
            Ok(frame) if self.write(&frame).await => Flow::Continue,
            Ok(_) => Flow::Close(CloseReason::Io),
            // An event that cannot be framed is skipped; the client resyncs on a gap.
            Err(err) => {
                tracing::error!(error = %err, "event could not be framed; skipped");
                Flow::Continue
            }
        }
    }

    async fn on_read(&mut self, read: std::io::Result<usize>, buf: &[u8]) -> Flow {
        let n = match read {
            Ok(0) => return Flow::Close(CloseReason::PeerClosed),
            Ok(n) => n,
            Err(err) if is_disconnect(&err) => return Flow::Close(CloseReason::PeerClosed),
            Err(err) => {
                tracing::warn!(kind = ?err.kind(), "pipe read failed; closing the connection");
                return Flow::Close(CloseReason::Io);
            }
        };
        let Some(chunk) = buf.get(..n) else {
            return Flow::Close(CloseReason::Io);
        };
        if let Err(err) = self.decoder.feed(chunk) {
            self.frame_error = Some(err);
        }
        let mut completed = false;
        while let Some(body) = self.decoder.next_frame() {
            completed = true;
            self.pending.push_back(body);
        }
        let partial = self.decoder.buffered_bytes() > 0;
        self.frame_started = match (partial, completed, self.frame_started) {
            (false, _, _) => None,
            (true, true, _) | (true, false, None) => Some(Instant::now()),
            (true, false, Some(started)) => Some(started),
        };
        Flow::Continue
    }

    async fn handle_frame(&mut self, body: &[u8]) -> Flow {
        let request = match parse_request(body) {
            Ok(request) => request,
            Err(err) => return self.reject_parse(&err).await,
        };
        let is_handshake = matches!(request.body, RequestBody::Handshake(_));
        if !self.handshaken && !is_handshake {
            self.reply_and_close(
                request.request_id,
                ErrorBody::new(ErrorCode::HandshakeRequired),
            )
            .await;
            return Flow::Close(CloseReason::ProtocolViolation);
        }
        if let Err(err) = request.validate() {
            return self.reject(request.request_id, err.to_error_body()).await;
        }
        match (&request.body, self.handshaken) {
            (RequestBody::Handshake(hello), false) => {
                let hello = hello.clone();
                self.handshake(request, hello).await
            }
            (RequestBody::Handshake(_), true) => {
                let error = ErrorBody::invalid_payload(None, "already_handshaken");
                self.reject(request.request_id, error).await
            }
            _ => self.dispatch(request).await,
        }
    }

    async fn handshake(&mut self, request: Request, hello: HandshakeRequest) -> Flow {
        if let Err(err) = negotiate_version(hello.min_protocol, hello.max_protocol) {
            self.reply_and_close(request.request_id, err.to_error_body())
                .await;
            return Flow::Close(CloseReason::ProtocolViolation);
        }
        self.conn.hello = Some(hello);
        let reply = self.handler.handle(&mut self.conn, request).await;
        let accepted = matches!(reply, Response::Success { .. });
        if !self.send(&reply).await {
            return Flow::Close(CloseReason::Io);
        }
        if accepted {
            self.handshaken = true;
            Flow::Continue
        } else {
            Flow::Close(CloseReason::HandshakeRejected)
        }
    }

    async fn dispatch(&mut self, request: Request) -> Flow {
        let reply = self.handler.handle(&mut self.conn, request).await;
        if self.send(&reply).await {
            Flow::Continue
        } else {
            Flow::Close(CloseReason::Io)
        }
    }

    /// Answers a request that failed to parse.
    async fn reject_parse(&mut self, err: &ParseError) -> Flow {
        let request_id = err.request_id().unwrap_or(NO_REQUEST_ID);
        let body = if self.handshaken {
            err.to_error_body()
        } else {
            match err {
                ParseError::Malformed | ParseError::UnsupportedVersion { .. } => {
                    err.to_error_body()
                }
                _ => ErrorBody::new(ErrorCode::HandshakeRequired),
            }
        };
        if !self.handshaken || err.closes_connection() {
            self.reply_and_close(request_id, body).await;
            return Flow::Close(CloseReason::ProtocolViolation);
        }
        self.reject(request_id, body).await
    }

    /// Sends a non-closing error response (or closes if the handshake has not happened).
    async fn reject(&mut self, request_id: u32, error: ErrorBody) -> Flow {
        if !self.handshaken {
            self.reply_and_close(request_id, error).await;
            return Flow::Close(CloseReason::ProtocolViolation);
        }
        if self.send(&Response::failure(request_id, error)).await {
            Flow::Continue
        } else {
            Flow::Close(CloseReason::Io)
        }
    }

    /// Best-effort error response before closing.
    async fn reply_and_close(&mut self, request_id: u32, error: ErrorBody) {
        let _delivered = self.send(&Response::failure(request_id, error)).await;
    }

    /// Encodes and writes a response; an unencodable one becomes `internal`.
    async fn send(&mut self, response: &Response) -> bool {
        let frame = response.to_frame().or_else(|_| {
            Response::failure(response.request_id(), ErrorBody::new(ErrorCode::Internal)).to_frame()
        });
        match frame {
            Ok(frame) => self.write(&frame).await,
            Err(_) => false,
        }
    }

    async fn write(&mut self, frame: &[u8]) -> bool {
        let stream = &mut self.stream;
        let write = async {
            stream.write_all(frame).await?;
            stream.flush().await
        };
        match timeout(self.timeouts.write, write).await {
            Ok(Ok(())) => true,
            Ok(Err(err)) => {
                tracing::warn!(kind = ?err.kind(), "pipe write failed");
                false
            }
            Err(_elapsed) => {
                tracing::warn!("pipe write timed out (stuck reader)");
                false
            }
        }
    }
}

/// Reports a connection task that panicked or was cancelled; the permit is already released.
fn log_join(joined: Result<(), tokio::task::JoinError>) {
    if let Err(err) = joined {
        tracing::error!(error = %err, "a connection task ended abnormally");
    }
}

async fn sleep_until_or_never(at: Option<Instant>) {
    match at {
        Some(at) => sleep_until(at).await,
        None => std::future::pending().await,
    }
}

async fn next_item(subscription: &mut Option<EventReceiver>) -> SubscriptionItem {
    match subscription {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

fn is_disconnect(err: &std::io::Error) -> bool {
    use std::io::ErrorKind::{BrokenPipe, ConnectionAborted, ConnectionReset, UnexpectedEof};
    matches!(
        err.kind(),
        BrokenPipe | ConnectionAborted | ConnectionReset | UnexpectedEof
    )
}
