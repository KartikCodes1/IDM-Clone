//! Shared test support: a fake request handler, frame helpers and a running server.
#![allow(dead_code, clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test support fails by panicking

use std::io;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::Duration;

use forgedm_ipc::{
    Client, ClientConfig, Connection, EventSink, HandlerFuture, IpcError, RequestHandler, Server,
    ServerOptions,
};
use forgedm_platform::peer::PeerIdentity;
use forgedm_platform::sid::Sid;
use forgedm_protocol::{
    ClientKind, DownloadRemovedData, Empty, EmptyResult, ErrorBody, ErrorCode, Event, EventBody,
    HandshakeLimits, HandshakeRequest, HandshakeResult, LIST_PAGE_MAX, ListDownloadsRequest,
    ListDownloadsResult, MAX_FRAME, PingResult, Request, RequestBody, Response, ResultBody,
    ServerMessage, SubscribeResult, parse_server_message,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

static NAME_COUNTER: AtomicU32 = AtomicU32::new(0);
const ERROR_PIPE_BUSY: i32 = 231;
/// A `Subscribe` with this `after_seq` makes the fake handler replay event `after_seq + 1`.
pub const REPLAY_AFTER_SEQ: u64 = 7;

/// A pipe name unique to this process and call.
pub fn unique_pipe_name(tag: &str) -> String {
    let n = NAME_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        r"\\.\pipe\ForgeDM-ipc-test-{}-{tag}-{n}",
        std::process::id()
    )
}

/// The identity of this test process, used as a stand-in peer.
pub fn test_peer() -> PeerIdentity {
    PeerIdentity {
        sid: Sid::current_user().expect("current user"),
        image_path: std::env::current_exe().expect("current exe"),
    }
}

/// A client config that expects this test process as the server.
pub fn client_config(name: &str) -> ClientConfig {
    ClientConfig::new(name, std::env::current_exe().expect("current exe"))
}

pub fn handshake_request(id: u32) -> Request {
    Request::new(
        id,
        RequestBody::Handshake(HandshakeRequest {
            client_kind: ClientKind::Cli,
            client_name: "test".to_owned(),
            client_version: "0.0.0".to_owned(),
            min_protocol: 1,
            max_protocol: 1,
        }),
    )
}

pub fn handshake_frame(id: u32) -> Vec<u8> {
    handshake_request(id).to_frame().expect("frame")
}

pub fn ping_frame(id: u32) -> Vec<u8> {
    Request::new(id, RequestBody::Ping(Empty {}))
        .to_frame()
        .expect("frame")
}

/// A `ListDownloads` request; the fake handler sleeps `offset` ms and answers
/// `total = limit * 1000 + offset`.
pub fn list_request(id: u32, limit: u32, offset: u32) -> Request {
    Request::new(
        id,
        RequestBody::ListDownloads(ListDownloadsRequest {
            states: None,
            limit,
            offset,
        }),
    )
}

pub fn list_frame(id: u32, limit: u32, offset: u32) -> Vec<u8> {
    list_request(id, limit, offset).to_frame().expect("frame")
}

/// Reads one frame body; `None` when the peer closed the stream.
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Option<Vec<u8>> {
    let mut header = [0u8; 4];
    read_exact_or_eof(reader, &mut header).await?;
    let len = u32::from_le_bytes(header);
    assert!(len <= MAX_FRAME, "server sent an oversized frame");
    let mut body = vec![0u8; usize::try_from(len).expect("len")];
    read_exact_or_eof(reader, &mut body).await?;
    Some(body)
}

async fn read_exact_or_eof<R: AsyncRead + Unpin>(reader: &mut R, buf: &mut [u8]) -> Option<()> {
    match reader.read_exact(buf).await {
        Ok(_) => Some(()),
        Err(e) if is_closed(&e) => None,
        Err(e) => panic!("unexpected read error: {e}"),
    }
}

/// Whether an I/O error means "the other side went away".
pub fn is_closed(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::UnexpectedEof
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
    )
}

pub async fn read_message<R: AsyncRead + Unpin>(reader: &mut R) -> Option<ServerMessage> {
    let body = read_frame(reader).await?;
    Some(parse_server_message(&body).expect("server message parses"))
}

/// The error of the next frame, which must be a failure response.
pub async fn read_error<R: AsyncRead + Unpin>(reader: &mut R) -> ErrorBody {
    match read_message(reader).await {
        Some(ServerMessage::Response(Response::Failure { error, .. })) => error,
        other => panic!("expected a failure response, got {other:?}"),
    }
}

/// Asserts the stream is closed (EOF) with no further frames.
pub async fn assert_closed<R: AsyncRead + Unpin>(reader: &mut R) {
    assert!(
        read_frame(reader).await.is_none(),
        "expected the connection to be closed"
    );
}

pub async fn write_all<W: AsyncWrite + Unpin>(writer: &mut W, bytes: &[u8]) {
    writer.write_all(bytes).await.expect("write");
    writer.flush().await.expect("flush");
}

/// Opens the pipe without any identity verification (server-side tests only).
pub async fn raw_connect(name: &str) -> NamedPipeClient {
    for _ in 0..100 {
        match ClientOptions::new().open(name) {
            Ok(c) => return c,
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => panic!("raw connect failed: {e}"),
        }
    }
    panic!("pipe stayed busy");
}

/// The fake request handler: the real agent is a later slice.
#[derive(Default)]
pub struct TestHandler {
    /// Requests that reached the handler.
    pub handled: AtomicUsize,
    in_flight: AtomicUsize,
    /// Highest number of requests ever processed at once.
    pub max_in_flight: AtomicUsize,
    /// `request_id` in the order requests started.
    pub started: Mutex<Vec<u32>>,
    /// Sinks created by `Subscribe`, for the test to push events into.
    pub sinks: Mutex<Vec<EventSink>>,
}

impl TestHandler {
    fn begin(&self, request_id: u32) {
        self.handled.fetch_add(1, Ordering::SeqCst);
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(now, Ordering::SeqCst);
        self.started.lock().expect("lock").push(request_id);
    }

    fn end(&self) {
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
    }

    async fn answer(&self, conn: &mut Connection, request: Request) -> Response {
        let id = request.request_id;
        let result = match request.body {
            RequestBody::Handshake(_) => ResultBody::Handshake(HandshakeResult {
                protocol_version: 1,
                protocol_minor: 0,
                agent_version: "test".to_owned(),
                agent_pid: std::process::id(),
                agent_instance_id: Uuid::now_v7(),
                capabilities: Vec::new(),
                limits: HandshakeLimits {
                    max_frame_bytes: MAX_FRAME,
                    max_list_page: LIST_PAGE_MAX,
                },
            }),
            RequestBody::Ping(_) => ResultBody::Ping(PingResult {
                agent_time_ms: 1,
                uptime_ms: 2,
            }),
            RequestBody::ListDownloads(p) => {
                tokio::time::sleep(Duration::from_millis(u64::from(p.offset))).await;
                ResultBody::ListDownloads(Box::new(ListDownloadsResult {
                    items: Vec::new(),
                    total: p.limit * 1000 + p.offset,
                }))
            }
            RequestBody::Subscribe(p) => {
                let mut sink = conn.subscribe();
                if p.after_seq == REPLAY_AFTER_SEQ {
                    let replay = Event {
                        seq: REPLAY_AFTER_SEQ + 1,
                        body: EventBody::DownloadRemoved(DownloadRemovedData { id: Uuid::nil() }),
                    };
                    sink.try_send(replay).expect("replay fits");
                }
                self.sinks.lock().expect("lock").push(sink);
                ResultBody::Subscribe(SubscribeResult { current_seq: 0 })
            }
            RequestBody::Unsubscribe(_) => {
                conn.unsubscribe();
                ResultBody::Unsubscribe(EmptyResult {})
            }
            _ => return Response::failure(id, ErrorBody::new(ErrorCode::NotImplemented)),
        };
        Response::success(id, &result).expect("serializable result")
    }
}

impl RequestHandler for TestHandler {
    fn handle<'a>(&'a self, conn: &'a mut Connection, request: Request) -> HandlerFuture<'a> {
        Box::pin(async move {
            self.begin(request.request_id);
            let response = self.answer(conn, request).await;
            self.end();
            response
        })
    }
}

/// A server bound to a unique pipe, serving with a [`TestHandler`].
pub struct RunningServer {
    pub name: String,
    pub handler: Arc<TestHandler>,
    pub shutdown: CancellationToken,
    pub task: JoinHandle<Result<(), IpcError>>,
}

impl RunningServer {
    pub fn start(tag: &str) -> Self {
        Self::start_with(tag, ServerOptions::default())
    }

    pub fn start_with(tag: &str, options: ServerOptions) -> Self {
        let name = unique_pipe_name(tag);
        let handler = Arc::new(TestHandler::default());
        let server = Server::bind(&name, handler.clone(), options).expect("bind");
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(server.serve(shutdown.clone()));
        Self {
            name,
            handler,
            shutdown,
            task,
        }
    }

    pub async fn client(&self) -> Client {
        Client::connect(client_config(&self.name))
            .await
            .expect("client connects")
    }

    pub async fn stop(self) -> Result<(), IpcError> {
        self.shutdown.cancel();
        self.task.await.expect("server task")
    }
}
