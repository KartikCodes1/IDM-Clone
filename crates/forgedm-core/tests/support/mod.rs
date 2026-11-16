//! Shared test support for the S1.12 end-to-end tests: a scripted HTTP server (every request
//! is answered by a closure that sees the request number, the range and all headers) and an
//! engine harness over the in-memory store with a recording sleeper.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code
#![allow(dead_code)] // each test file uses part of this module

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use forgedm_core::client::{ClientOptions, HttpClient, ProxyMode};
use forgedm_core::engine::budgets::FreeSpace;
use forgedm_core::engine::{self, EngineConfig, EngineDeps, EngineHandle};
use forgedm_core::ports::{EngineEvent, NewJob, unix_ms};
use forgedm_core::transfer::resilience::RetryEnv;
use forgedm_core::url_policy::validate_user_url;
use forgedm_fixture::content;
use forgedm_platform::motw::MotwService;
use forgedm_protocol::{ClientKind, DownloadErrorCode, Priority};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;

use crate::testing::{MemoryStore, RecordingSink, RecordingSleeper};

/// Upper bound for waiting on an engine outcome.
pub const WAIT: Duration = Duration::from_secs(90);
/// One MiB.
pub const MIB: u64 = 1024 * 1024;
/// Chunk size the scripted server writes bodies in.
const CHUNK: usize = 64 * 1024;

/// One request as the scripted server saw it.
#[derive(Debug, Clone)]
pub struct Req {
    /// Arrival order, from 0.
    pub index: usize,
    /// HTTP method.
    pub method: String,
    /// Request target.
    pub target: String,
    /// `Range: bytes=start-end` (end is inclusive and absent for open ranges).
    pub range: Option<(u64, Option<u64>)>,
    /// Header names (lowercase) and values.
    pub headers: Vec<(String, String)>,
}

impl Req {
    /// A HEAD or the probing `bytes=0-0` GET.
    pub fn is_probe(&self) -> bool {
        self.method == "HEAD" || self.range == Some((0, Some(0)))
    }

    /// The request is a transfer GET (not part of the probe).
    pub fn is_transfer(&self) -> bool {
        !self.is_probe()
    }

    /// The value of header `name` (lowercase).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// How the scripted server answers one request.
pub enum Reply {
    /// An error (or other) status with extra headers and no body.
    Status(u16, Vec<(&'static str, String)>),
    /// Generated content of `seed`/`total` with range support.
    Content(Content),
    /// Raw bytes, then the connection is closed.
    Raw(Vec<u8>),
}

/// Generated content and how the response misbehaves.
#[derive(Clone)]
pub struct Content {
    /// Content seed.
    pub seed: u64,
    /// Total size.
    pub total: u64,
    /// Strong `ETag` value (without quotes).
    pub etag: &'static str,
    /// Close the connection after this many body bytes.
    pub drop_after: Option<u64>,
    /// Stop sending after this many body bytes but keep the connection open.
    pub stall_after: Option<u64>,
}

impl Content {
    /// Well-behaved content.
    pub fn new(seed: u64, total: u64) -> Self {
        Self {
            seed,
            total,
            etag: "v1",
            drop_after: None,
            stall_after: None,
        }
    }
}

type Handler = dyn Fn(&Req) -> Reply + Send + Sync;

/// A scripted HTTP/1.1 server on a loopback port. Every response closes its connection.
pub struct Scripted {
    /// The bound address.
    pub addr: SocketAddr,
    log: Arc<Mutex<Vec<Req>>>,
}

impl Scripted {
    /// Starts a server that answers every request with `handler`.
    pub async fn start(handler: impl Fn(&Req) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        let handler: Arc<Handler> = Arc::new(handler);
        let counter = Arc::new(AtomicU64::new(0));
        let shared = Arc::clone(&log);
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let (handler, log, counter) = (handler.clone(), shared.clone(), counter.clone());
                tokio::spawn(async move {
                    let _ = serve(stream, handler, log, counter).await;
                });
            }
        });
        Self { addr, log }
    }

    /// Every request seen so far.
    pub fn requests(&self) -> Vec<Req> {
        self.log.lock().unwrap().clone()
    }

    /// The transfer GETs seen so far.
    pub fn transfers(&self) -> Vec<Req> {
        self.requests()
            .into_iter()
            .filter(Req::is_transfer)
            .collect()
    }

    /// `http://127.0.0.1:<port><path>`.
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }
}

async fn read_head(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).await? == 0 {
            break;
        }
        head.push(byte[0]);
        if head.len() > 64 * 1024 {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&head).into_owned())
}

fn parse(head: &str, index: usize) -> Req {
    let mut lines = head.lines();
    let first = lines.next().unwrap_or_default();
    let mut parts = first.split(' ');
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or_default().to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let range = headers
        .iter()
        .find(|(n, _)| n == "range")
        .and_then(|(_, v)| v.strip_prefix("bytes="))
        .and_then(|v| v.split_once('-'))
        .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok())));
    Req {
        index,
        method,
        target,
        range,
        headers,
    }
}

async fn serve(
    mut stream: TcpStream,
    handler: Arc<Handler>,
    log: Arc<Mutex<Vec<Req>>>,
    counter: Arc<AtomicU64>,
) -> std::io::Result<()> {
    let head = read_head(&mut stream).await?;
    let index = usize::try_from(counter.fetch_add(1, Ordering::SeqCst)).unwrap();
    let req = parse(&head, index);
    log.lock().unwrap().push(req.clone());
    match handler(&req) {
        Reply::Status(code, headers) => {
            let mut out =
                format!("HTTP/1.1 {code} Scripted\r\nContent-Length: 0\r\nConnection: close\r\n");
            for (name, value) in headers {
                out.push_str(&format!("{name}: {value}\r\n"));
            }
            out.push_str("\r\n");
            stream.write_all(out.as_bytes()).await?;
        }
        Reply::Raw(bytes) => stream.write_all(&bytes).await?,
        Reply::Content(spec) => serve_content(&mut stream, &req, &spec).await?,
    }
    stream.shutdown().await
}

async fn serve_content(stream: &mut TcpStream, req: &Req, spec: &Content) -> std::io::Result<()> {
    let (status, first, last) = match req.range {
        Some((a, b)) if a < spec.total => (206, a, b.unwrap_or(u64::MAX).min(spec.total - 1)),
        _ => (200, 0, spec.total.saturating_sub(1)),
    };
    let len = if spec.total == 0 { 0 } else { last - first + 1 };
    let mut head = format!(
        "HTTP/1.1 {status} OK\r\nContent-Length: {len}\r\nETag: \"{}\"\r\nAccept-Ranges: bytes\r\nConnection: close\r\n",
        spec.etag
    );
    if status == 206 {
        head.push_str(&format!(
            "Content-Range: bytes {first}-{last}/{}\r\n",
            spec.total
        ));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).await?;
    if req.method == "HEAD" {
        return Ok(());
    }
    let limit = spec.drop_after.or(spec.stall_after).unwrap_or(len).min(len);
    let mut sent = 0u64;
    while sent < limit {
        let n = usize::try_from((limit - sent).min(CHUNK as u64)).unwrap();
        let mut buf = vec![0u8; n];
        content::fill(spec.seed, first + sent, &mut buf);
        stream.write_all(&buf).await?;
        sent += n as u64;
    }
    if spec.stall_after.is_some_and(|s| s < len) {
        tokio::time::sleep(Duration::from_secs(120)).await;
    }
    Ok(())
}

/// Everything a retry test needs: the engine over fakes, with a recording sleeper.
pub struct Harness {
    /// The persistence fake.
    pub store: Arc<MemoryStore>,
    /// Recorded events.
    pub sink: Arc<RecordingSink>,
    /// The engine.
    pub engine: EngineHandle,
    /// The sleeper every retry wait goes through.
    pub sleeper: Arc<RecordingSleeper>,
    /// The download directory.
    pub dir: tempfile::TempDir,
}

impl Harness {
    /// Waits complete at once.
    pub fn new() -> Self {
        Self::with(RecordingSleeper::new(), ClientOptions::new(ProxyMode::None))
    }

    /// Waits never complete (only cancellation ends them).
    pub fn blocking() -> Self {
        Self::with(
            RecordingSleeper::blocking(),
            ClientOptions::new(ProxyMode::None),
        )
    }

    /// Custom client options (short timeouts, fixed resolutions).
    pub fn with_client(options: ClientOptions) -> Self {
        Self::with(RecordingSleeper::new(), options)
    }

    /// Custom engine tunables (active downloads, connection budgets).
    pub fn configured(config: EngineConfig) -> Self {
        Self::build(
            RecordingSleeper::new(),
            ClientOptions::new(ProxyMode::None),
            config,
            None,
        )
    }

    /// Custom tunables and an injected free-space reading.
    pub fn with_free_space(config: EngineConfig, space: Arc<dyn FreeSpace>) -> Self {
        Self::build(
            RecordingSleeper::new(),
            ClientOptions::new(ProxyMode::None),
            config,
            Some(space),
        )
    }

    fn with(sleeper: RecordingSleeper, options: ClientOptions) -> Self {
        Self::build(sleeper, options, EngineConfig::default(), None)
    }

    fn build(
        sleeper: RecordingSleeper,
        options: ClientOptions,
        config: EngineConfig,
        space: Option<Arc<dyn FreeSpace>>,
    ) -> Self {
        let store = Arc::new(MemoryStore::new());
        let sink = Arc::new(RecordingSink::new());
        let sleeper = Arc::new(sleeper);
        let deps = EngineDeps {
            store: store.clone(),
            sink: sink.clone(),
            client: HttpClient::with_options(&options).unwrap(),
            motw: Arc::new(MotwService::start().unwrap()),
        };
        let retry = RetryEnv {
            sleeper: sleeper.clone(),
            jitter_seed: Some(42),
        };
        let engine = match space {
            Some(space) => engine::start_with_space(config, deps, retry, space),
            None => engine::start_with(config, deps, retry),
        };
        Self {
            store,
            sink,
            engine,
            sleeper,
            dir: tempfile::tempdir().unwrap(),
        }
    }

    /// The canonical download directory.
    pub fn target(&self) -> PathBuf {
        std::fs::canonicalize(self.dir.path()).unwrap()
    }

    /// Enqueues `url` as `name` with a cap of `connections`.
    pub async fn enqueue(&self, url: &str, name: &str, connections: u32) -> Uuid {
        self.enqueue_with(url, name, connections, Priority::Normal)
            .await
    }

    /// Enqueues `url` as `name` with a cap of `connections` and a priority.
    pub async fn enqueue_with(
        &self,
        url: &str,
        name: &str,
        connections: u32,
        priority: Priority,
    ) -> Uuid {
        let job = NewJob {
            id: Uuid::now_v7(),
            source: ClientKind::Cli,
            priority,
            display_name: name.to_owned(),
            url: validate_user_url(url).unwrap(),
            target_dir: self.target(),
            requested_name: Some(name.to_owned()),
            expected_sha256: None,
            max_connections: connections,
            created_at_ms: unix_ms(),
        };
        let id = job.id;
        self.engine.enqueue(job).await.unwrap();
        id
    }

    /// Enqueues `url` without a requested file name (the name comes from the response or URL).
    pub async fn enqueue_unnamed(&self, url: &str) -> Uuid {
        let job = NewJob {
            id: Uuid::now_v7(),
            source: ClientKind::Cli,
            priority: Priority::Normal,
            display_name: "download".to_owned(),
            url: validate_user_url(url).unwrap(),
            target_dir: self.target(),
            requested_name: None,
            expected_sha256: None,
            max_connections: 4,
            created_at_ms: unix_ms(),
        };
        let id = job.id;
        self.engine.enqueue(job).await.unwrap();
        id
    }

    /// Enqueues and waits for `completed` or `failed`.
    pub async fn run(&self, url: &str, name: &str, connections: u32) -> (Uuid, EngineEvent) {
        let id = self.enqueue(url, name, connections).await;
        (id, self.sink.wait_terminal(id, WAIT).await)
    }

    /// The final path and checksum of a completion.
    pub fn completed(event: &EngineEvent) -> (PathBuf, String) {
        match event {
            EngineEvent::Completed {
                final_path, sha256, ..
            } => (final_path.clone(), sha256.clone()),
            other => panic!("expected completion, got {other:?}"),
        }
    }

    /// The code and details of a failure.
    pub fn failure(event: &EngineEvent) -> forgedm_protocol::DownloadError {
        match event {
            EngineEvent::Failed { error, .. } => error.clone(),
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    /// The failure code only.
    pub fn failure_code(event: &EngineEvent) -> DownloadErrorCode {
        Self::failure(event).code
    }
}

/// Lowercase hex SHA-256 of generated content.
pub fn expected_sha(seed: u64, size: u64) -> String {
    hex::encode(content::sha256(seed, size))
}
