//! The loopback listener, connection loop and request routing.

use crate::body::FixtureBody;
use crate::flags::{self, Flags};
use crate::log::{LoggedRequest, RequestLog};
use crate::response::{self, Target};
use http::{Method, Request, Response, StatusCode, header};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;
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
        let task = tokio::spawn(accept_loop(listener, log.clone()));
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

const ACCEPT_BACKOFF: Duration = Duration::from_millis(50);

async fn accept_loop(listener: TcpListener, log: RequestLog) {
    let next_conn = AtomicU64::new(0);
    // Dropping the set (when this task is aborted) aborts every connection task.
    let mut connections = JoinSet::new();
    loop {
        let (stream, _peer) = match listener.accept().await {
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
        let conn_id = next_conn.fetch_add(1, Ordering::Relaxed);
        let log = log.clone();
        connections.spawn(async move {
            let service = service_fn(move |req: Request<hyper::body::Incoming>| {
                let log = log.clone();
                async move { Ok::<_, Infallible>(respond(&req, conn_id, &log)) }
            });
            // A connection error (peer reset, deliberate drop_after) only ends that connection.
            let _ = http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
        while connections.try_join_next().is_some() {}
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn header_text<B>(req: &Request<B>, name: header::HeaderName) -> Option<String> {
    req.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// Why a request is refused before any content is produced.
#[derive(Debug, thiserror::Error)]
enum Rejection {
    #[error("not found")]
    NotFound,
    #[error("seed and size must be u64")]
    BadPath,
    #[error("{0}")]
    BadFlag(#[from] flags::FlagError),
    #[error("method not allowed")]
    MethodNotAllowed,
}

impl Rejection {
    fn into_response(self) -> Response<FixtureBody> {
        let status = match self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::BadPath | Self::BadFlag(_) => StatusCode::BAD_REQUEST,
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
        };
        response::error_response(status, &self.to_string())
    }
}

/// Splits `/f/{seed}/{size}` into its numbers.
fn parse_path(path: &str) -> Result<(u64, u64), Rejection> {
    let mut parts = path.strip_prefix("/f/").unwrap_or("").split('/');
    let (Some(seed), Some(size), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(Rejection::NotFound);
    };
    match (seed.parse::<u64>(), size.parse::<u64>()) {
        (Ok(seed), Ok(size)) => Ok((seed, size)),
        _ => Err(Rejection::BadPath),
    }
}

/// Logs the request and produces its response.
fn respond<B>(req: &Request<B>, conn_id: u64, log: &RequestLog) -> Response<FixtureBody> {
    let query = req.uri().query().unwrap_or("");
    let pairs = flags::parse_query(query);
    log.record(LoggedRequest {
        seq: 0,
        method: req.method().to_string(),
        target: req
            .uri()
            .path_and_query()
            .map_or_else(|| req.uri().path().to_owned(), ToString::to_string),
        range: header_text(req, header::RANGE),
        if_range: header_text(req, header::IF_RANGE),
        conn_id,
        test_key: Flags::test_key_of(&pairs),
        at_unix_ms: unix_ms(),
    });
    route(req, &pairs).unwrap_or_else(Rejection::into_response)
}

fn route<B>(req: &Request<B>, pairs: &[(&str, &str)]) -> Result<Response<FixtureBody>, Rejection> {
    let (seed, size) = parse_path(req.uri().path())?;
    let flags = Flags::from_pairs(pairs)?;
    let is_head = match *req.method() {
        Method::GET => false,
        Method::HEAD if flags.head => true,
        _ => return Err(Rejection::MethodNotAllowed),
    };
    let range = header_text(req, header::RANGE);
    let if_range = header_text(req, header::IF_RANGE);
    let target = Target {
        is_head,
        seed,
        size,
        range: range.as_deref(),
        if_range: if_range.as_deref(),
    };
    Ok(response::build(&flags, &target))
}
