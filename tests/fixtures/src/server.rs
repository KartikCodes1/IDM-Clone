//! The loopback listener, connection loop and request routing.

use crate::body::FixtureBody;
use crate::flags::{self, Flags};
use crate::log::{InFlight, LoggedRequest, RequestLog};
use crate::raw;
use crate::response::{self, error_response};
use crate::scenario::{self, Ctx, Resolved};
use crate::state::Shared;
use http::{Method, Request, Response, StatusCode, header};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};

/// Errors starting the fixture server.
#[derive(Debug, thiserror::Error)]
pub enum FixtureError {
    /// The loopback socket could not be bound.
    #[error("cannot bind 127.0.0.1:{port}: {source}")]
    Bind {
        /// Requested port (0 = any).
        port: u16,
        /// Underlying error.
        source: std::io::Error,
    },
    /// The bound address could not be read.
    #[error("cannot read the bound address: {0}")]
    LocalAddr(std::io::Error),
}

/// A running fixture server. Dropping it stops the listener and all its connections.
pub struct FixtureServer {
    addr: SocketAddr,
    log: RequestLog,
    task: JoinHandle<()>,
}

impl FixtureServer {
    /// Starts on an ephemeral loopback port and detaches: the server lives as long as the
    /// tokio runtime. Returns the base URL (`http://127.0.0.1:PORT`) and the request log.
    ///
    /// # Errors
    /// [`FixtureError`] when the socket cannot be bound.
    pub async fn start() -> Result<(String, RequestLog), FixtureError> {
        let server = Self::start_on(0).await?;
        let out = (server.base_url(), server.log());
        // Detach on purpose: the accept task ends with the runtime.
        std::mem::forget(server);
        Ok(out)
    }

    /// Starts on `127.0.0.1:port` (0 = ephemeral) and returns a handle that owns the server.
    ///
    /// # Errors
    /// [`FixtureError`] when the socket cannot be bound.
    pub async fn start_on(port: u16) -> Result<Self, FixtureError> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
            .await
            .map_err(|source| FixtureError::Bind { port, source })?;
        let addr = listener.local_addr().map_err(FixtureError::LocalAddr)?;
        let log = RequestLog::default();
        let shared = Shared::new(log.clone(), addr.port());
        let task = tokio::spawn(accept_loop(listener, shared));
        Ok(Self { addr, log, task })
    }

    /// The bound socket address (always `127.0.0.1`).
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// `http://127.0.0.1:PORT`.
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// A handle to the request log.
    #[must_use]
    pub fn log(&self) -> RequestLog {
        self.log.clone()
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Identity of one accepted connection.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Conn {
    /// Server-assigned connection id.
    pub id: u64,
    /// Remote TCP port.
    pub peer_port: u16,
}

const ACCEPT_BACKOFF: Duration = Duration::from_millis(50);

async fn accept_loop(listener: TcpListener, shared: Shared) {
    let next_conn = AtomicU64::new(0);
    // Dropping the set (when this task is aborted) aborts every connection task.
    let mut connections = JoinSet::new();
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(e) => {
                // Per-connection failures are retried at once; anything else (for example
                // descriptor exhaustion) backs off so the loop cannot spin on a persistent error.
                if !matches!(
                    e.kind(),
                    io::ErrorKind::ConnectionAborted | io::ErrorKind::ConnectionReset
                ) {
                    tokio::time::sleep(ACCEPT_BACKOFF).await;
                }
                continue;
            }
        };
        let conn = Conn {
            id: next_conn.fetch_add(1, Ordering::Relaxed),
            peer_port: peer.port(),
        };
        connections.spawn(serve_connection(stream, shared.clone(), conn));
        while connections.try_join_next().is_some() {}
    }
}

/// Routes the connection to the raw socket path when its first request asks for a raw mode,
/// otherwise to hyper.
async fn serve_connection(stream: TcpStream, shared: Shared, conn: Conn) {
    if raw::wants_raw(&stream).await {
        raw::serve(stream, &shared, conn).await;
        return;
    }
    let service = service_fn(move |req: Request<hyper::body::Incoming>| {
        let shared = shared.clone();
        async move { Ok::<_, Infallible>(respond(&req, conn, &shared)) }
    });
    // A connection error (peer reset, deliberate drop_after) only ends that connection.
    let _ = http1::Builder::new()
        .serve_connection(TokioIo::new(stream), service)
        .await;
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

pub(crate) fn header_text<B>(req: &Request<B>, name: header::HeaderName) -> Option<String> {
    req.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// Why a request is refused before any content is produced.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Rejection {
    #[error("not found")]
    NotFound,
    #[error("seed and size must be u64")]
    BadPath,
    #[error("{0}")]
    BadFlag(#[from] flags::FlagError),
    #[error("method not allowed")]
    MethodNotAllowed,
    #[error("raw and lie_cl flags need the first request of a fresh connection")]
    RawNeedsFreshConnection,
    #[error("lie_cl cannot be combined with the {0} flag")]
    UnsupportedWithLieCl(&'static str),
}

impl Rejection {
    pub(crate) fn status(&self) -> StatusCode {
        match self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::BadPath
            | Self::BadFlag(_)
            | Self::RawNeedsFreshConnection
            | Self::UnsupportedWithLieCl(_) => StatusCode::BAD_REQUEST,
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
        }
    }

    fn into_response(self) -> Response<FixtureBody> {
        error_response(self.status(), &self.to_string())
    }
}

/// Splits `/f/{seed}/{size}` into its numbers.
pub(crate) fn parse_path(path: &str) -> Result<(u64, u64), Rejection> {
    let mut parts = path.strip_prefix("/f/").unwrap_or("").split('/');
    let (Some(seed), Some(size), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(Rejection::NotFound);
    };
    match (seed.parse::<u64>(), size.parse::<u64>()) {
        (Ok(seed), Ok(size)) => Ok((seed, size)),
        _ => Err(Rejection::BadPath),
    }
}

/// Appends the request to the log (header values are never stored, only presence).
pub(crate) fn log_request<B>(
    req: &Request<B>,
    conn: Conn,
    shared: &Shared,
    pairs: &[(&str, &str)],
) {
    shared.log.record(LoggedRequest {
        seq: 0,
        method: req.method().to_string(),
        target: req
            .uri()
            .path_and_query()
            .map_or_else(|| req.uri().path().to_owned(), ToString::to_string),
        range: header_text(req, header::RANGE),
        if_range: header_text(req, header::IF_RANGE),
        conn_id: conn.id,
        test_key: Flags::test_key_of(pairs),
        at_unix_ms: unix_ms(),
        peer_port: conn.peer_port,
        has_authorization: req.headers().contains_key(header::AUTHORIZATION),
        has_cookie: req.headers().contains_key(header::COOKIE),
    });
}

/// True when the request carries `Authorization: Basic ...` (the credentials are not checked).
fn has_basic_auth<B>(req: &Request<B>) -> bool {
    header_text(req, header::AUTHORIZATION).is_some_and(|v| {
        v.get(..6)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("basic "))
    })
}

/// Logs the request and produces its response.
fn respond<B>(req: &Request<B>, conn: Conn, shared: &Shared) -> Response<FixtureBody> {
    if req.uri().path() == LOG_EXPORT_PATH {
        return export_log(shared);
    }
    let pairs = flags::parse_query(req.uri().query().unwrap_or(""));
    log_request(req, conn, shared, &pairs);
    route(req, &pairs, shared).unwrap_or_else(Rejection::into_response)
}

/// Path of the request-log export (not itself logged).
pub const LOG_EXPORT_PATH: &str = "/__log";

/// `GET /__log`: the request log and the served byte total as JSON.
fn export_log(shared: &Shared) -> Response<FixtureBody> {
    let mut resp = Response::new(FixtureBody::from_bytes(bytes::Bytes::from(
        shared.log.to_json(),
    )));
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    resp
}

fn route<B>(
    req: &Request<B>,
    pairs: &[(&str, &str)],
    shared: &Shared,
) -> Result<Response<FixtureBody>, Rejection> {
    let (seed, size) = parse_path(req.uri().path())?;
    let flags = Flags::from_pairs(pairs)?;
    let is_head = match *req.method() {
        Method::GET => false,
        Method::HEAD if flags.head => true,
        _ => return Err(Rejection::MethodNotAllowed),
    };
    if flags.raw.is_some() || flags.lie_cl.is_some() {
        return Err(Rejection::RawNeedsFreshConnection);
    }
    let Some(slot) = shared.log.try_enter(flags.max_conns) else {
        return Ok(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "max_conns exceeded",
        ));
    };
    let ctx = Ctx {
        path: req.uri().path(),
        query: req.uri().query().unwrap_or(""),
        size,
        has_basic_auth: has_basic_auth(req),
        shared,
    };
    let resp = scenario::early_response(&flags, &ctx).unwrap_or_else(|| {
        let resolved = Resolved::new(
            &flags,
            shared,
            ctx.path,
            header_text(req, header::RANGE),
            header_text(req, header::IF_RANGE),
        );
        response::build(&flags, &resolved.target(is_head, seed, size))
    });
    Ok(attach(resp, slot, shared))
}

/// Keeps `slot` held until the response body is finished or dropped.
fn attach(resp: Response<FixtureBody>, slot: InFlight, shared: &Shared) -> Response<FixtureBody> {
    let (parts, body) = resp.into_parts();
    let body = body
        .with_guard(slot)
        .with_served(shared.log.served_counter());
    Response::from_parts(parts, body)
}
