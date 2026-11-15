//! The single-stream vertical slice through the real engine (PLAN S1.7): real HTTP against
//! the fixture server, real files, real Zone.Identifier streams. Only persistence is the
//! in-memory fake.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

#[path = "../src/testing.rs"]
mod testing;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use forgedm_core::client::{HttpClient, ProxyMode};
use forgedm_core::engine::{self, EngineConfig, EngineDeps, EngineHandle};
use forgedm_core::ports::{EngineEvent, NewJob, StoredJob, unix_ms};
use forgedm_core::url_policy::validate_user_url;
use forgedm_core::writer::{Sink, Writer};
use forgedm_fixture::{FixtureServer, content, verify};
use forgedm_platform::motw::MotwService;
use forgedm_protocol::{ClientKind, DownloadErrorCode, DownloadState, Priority, TransferMode};
use testing::{MemoryStore, RecordingSink};
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(60);

struct Harness {
    server: FixtureServer,
    store: Arc<MemoryStore>,
    sink: Arc<RecordingSink>,
    engine: EngineHandle,
    dir: tempfile::TempDir,
}

impl Harness {
    async fn new() -> Self {
        let server = FixtureServer::start_on(0).await.unwrap();
        let store = Arc::new(MemoryStore::new());
        let sink = Arc::new(RecordingSink::new());
        let engine = engine::start(
            EngineConfig::default(),
            EngineDeps {
                store: store.clone(),
                sink: sink.clone(),
                client: HttpClient::new(ProxyMode::None).unwrap(),
                motw: Arc::new(MotwService::start().unwrap()),
            },
        );
        Self {
            server,
            store,
            sink,
            engine,
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn target(&self) -> PathBuf {
        std::fs::canonicalize(self.dir.path()).unwrap()
    }

    fn url(&self, path_and_query: &str) -> String {
        format!("{}{path_and_query}", self.server.base_url())
    }

    fn new_job(&self, url: &str, name: Option<&str>, sha: Option<&str>) -> NewJob {
        let url = validate_user_url(url).unwrap();
        NewJob {
            id: Uuid::now_v7(),
            source: ClientKind::Cli,
            priority: Priority::Normal,
            display_name: name.unwrap_or("download").to_owned(),
            url,
            target_dir: self.target(),
            requested_name: name.map(str::to_owned),
            expected_sha256: sha.map(str::to_owned),
            max_connections: 4,
            created_at_ms: unix_ms(),
        }
    }

    async fn enqueue(&self, url: &str, name: Option<&str>, sha: Option<&str>) -> Uuid {
        let job = self.new_job(url, name, sha);
        let id = job.id;
        self.engine.enqueue(job).await.unwrap();
        id
    }

    async fn run(&self, url: &str) -> (Uuid, EngineEvent) {
        let id = self.enqueue(url, None, None).await;
        let event = self.sink.wait_terminal(id, WAIT).await;
        (id, event)
    }

    fn files_in_target(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.target())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

fn expected_sha(seed: u64, size: u64) -> String {
    hex::encode(content::sha256(seed, size))
}

fn failed_code(event: &EngineEvent) -> DownloadErrorCode {
    match event {
        EngineEvent::Failed { error, .. } => error.code,
        other => panic!("expected a failure, got {other:?}"),
    }
}

fn completed_path(event: &EngineEvent) -> PathBuf {
    match event {
        EngineEvent::Completed { final_path, .. } => final_path.clone(),
        other => panic!("expected completion, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn single_stream_matches_generator() {
    let h = Harness::new().await;
    for (seed, size) in [
        (11u64, 0u64),
        (12, 1),
        (13, 64 * 1024 - 1),
        (14, 10 * 1024 * 1024 + 3),
    ] {
        let (id, event) = h.run(&h.url(&format!("/f/{seed}/{size}"))).await;

        let EngineEvent::Completed {
            sha256,
            length_verified,
            final_path,
            ..
        } = event
        else {
            panic!("size {size}: {event:?}");
        };
        verify::file_matches(&final_path, seed, size).unwrap();
        assert_eq!(sha256, expected_sha(seed, size), "size {size}");
        assert!(length_verified, "size {size}");
        assert_eq!(h.store.state(id), Some(DownloadState::Completed));
        let row = h.store.row(id).unwrap();
        // S1.9: the probe finds the fixture range-capable, so a non-empty file is a ranged
        // transfer (the plain non-resumable path is covered with `ranges=0` below). S1.11:
        // from 8 MiB on it is segmented; below, a single ranged stream.
        let expected_mode = match size {
            0 => TransferMode::ZeroLength,
            s if s >= 8 * 1024 * 1024 => TransferMode::Segmented,
            _ => TransferMode::SingleRanged,
        };
        assert_eq!(row.probe, Some((expected_mode, Some(size))));
        assert!(
            !row.finalize_targets.is_empty(),
            "candidate persisted before the move"
        );
        assert!(
            h.files_in_target()
                .iter()
                .all(|n| !n.ends_with(".forgedm.part")),
            "the staging file is gone after completion"
        );
        if size > 0 {
            let (committed, total) = *row.checkpoints.last().unwrap();
            assert_eq!((committed, total), (size, Some(size)));
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_length_completes_length_unverified() {
    let h = Harness::new().await;

    // `ranges=0`: with ranges a 206 `Content-Range` would reveal the total even without a
    // Content-Length, which (correctly, since S1.9) makes the length verifiable.
    let (id, event) = h.run(&h.url("/f/21/300000?cl=0&ranges=0")).await;

    let EngineEvent::Completed {
        length_verified,
        final_path,
        sha256,
        ..
    } = event
    else {
        panic!("{event:?}");
    };
    assert!(
        !length_verified,
        "no declared length: nothing proves completeness"
    );
    verify::file_matches(&final_path, 21, 300_000).unwrap();
    assert_eq!(sha256, expected_sha(21, 300_000));
    assert_eq!(
        h.store.row(id).unwrap().probe,
        Some((TransferMode::SingleUnknownLength, None))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn drop_after_with_cl_is_incomplete_not_completed() {
    let h = Harness::new().await;

    let (id, event) = h.run(&h.url("/f/22/400000?drop_after=100000")).await;

    assert_eq!(failed_code(&event), DownloadErrorCode::BodyIncomplete);
    assert_eq!(h.store.state(id), Some(DownloadState::Failed));
    let files = h.files_in_target();
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(
        files[0].ends_with(".forgedm.part"),
        "the .part is kept: {files:?}"
    );
    assert!(h.store.row(id).unwrap().completion.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn gzip_response_rejected() {
    let h = Harness::new().await;

    let (id, event) = h.run(&h.url("/f/23/5000?encoding=gzip")).await;

    assert_eq!(
        failed_code(&event),
        DownloadErrorCode::UnsupportedContentEncoding
    );
    assert_eq!(h.store.state(id), Some(DownloadState::Failed));
    assert!(
        h.files_in_target().is_empty(),
        "nothing is written for a content-coded body"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn identity_header_sent() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // A server that ignores ranges: it answers the HEAD with headers only and every GET
    // with the whole 2-byte body, and records each request head it sees.
    let heads = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&heads);
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let seen = Arc::clone(&seen);
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    head.extend_from_slice(&buf[..n]);
                }
                let text = String::from_utf8_lossy(&head).to_ascii_lowercase();
                let body = if text.starts_with("head ") { "" } else { "hi" };
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{body}"
                );
                seen.lock().unwrap().push(text);
                let _ = stream.write_all(reply.as_bytes()).await;
            });
        }
    });
    let h = Harness::new().await;

    let (_id, event) = h.run(&format!("http://{addr}/x.bin")).await;

    let heads = heads.lock().unwrap().clone();
    assert!(
        matches!(event, EngineEvent::Completed { .. }),
        "{event:?} after {heads:?}"
    );
    assert!(
        heads.len() >= 3,
        "HEAD, probe GET and transfer GET: {heads:?}"
    );
    assert!(heads[0].starts_with("head /x.bin http/1.1"), "{}", heads[0]);
    for head in &heads {
        assert!(head.contains("accept-encoding: identity\r\n"), "{head}");
        assert!(head.contains("accept: */*\r\n"), "{head}");
        assert!(
            head.contains(&format!(
                "user-agent: forgedm/{}\r\n",
                env!("CARGO_PKG_VERSION")
            )),
            "{head}"
        );
        assert!(!head.contains("\r\nreferer:"), "{head}");
    }
    assert!(
        heads[1].contains("\r\nrange: bytes=0-0\r\n"),
        "the probe asks for the first byte: {}",
        heads[1]
    );
    let transfer = heads.last().unwrap();
    assert!(
        !transfer.contains("\r\nrange:"),
        "no Range on the plain GET of a server that ignores ranges: {transfer}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn redirect_chain_followed_with_pure_policy() {
    let h = Harness::new().await;

    let (_id, event) = h.run(&h.url("/f/24/70000?redirect=chain:3&t=chain")).await;

    let path = completed_path(&event);
    verify::file_matches(&path, 24, 70_000).unwrap();
    let hops = h.server.log().for_test("chain").len();
    // The HEAD, the probe's GET and the transfer's GET each follow the three redirects.
    assert_eq!(
        hops,
        3 * 4,
        "three redirects then the content, for each of three requests"
    );

    let (_id, looped) = h.run(&h.url("/f/24/70000?redirect=loop")).await;
    assert_eq!(failed_code(&looped), DownloadErrorCode::RedirectLoop);
    let (_id, endless) = h.run(&h.url("/f/24/70000?redirect=chain:30")).await;
    assert_eq!(failed_code(&endless), DownloadErrorCode::TooManyRedirects);
}

#[tokio::test(flavor = "multi_thread")]
async fn sha_match_completes_mismatch_fails_keeps_part() {
    let h = Harness::new().await;
    let good = expected_sha(31, 200_000);

    let id = h
        .enqueue(&h.url("/f/31/200000"), Some("ok.bin"), Some(&good))
        .await;
    let event = h.sink.wait_terminal(id, WAIT).await;
    assert!(matches!(event, EngineEvent::Completed { .. }), "{event:?}");

    let wrong = "0".repeat(64);
    let id = h
        .enqueue(&h.url("/f/31/200000"), Some("bad.bin"), Some(&wrong))
        .await;
    let event = h.sink.wait_terminal(id, WAIT).await;

    assert_eq!(failed_code(&event), DownloadErrorCode::ChecksumMismatch);
    assert_eq!(h.store.state(id), Some(DownloadState::Failed));
    let files = h.files_in_target();
    assert!(files.contains(&"ok.bin".to_owned()), "{files:?}");
    assert!(!files.contains(&"bad.bin".to_owned()), "{files:?}");
    let kept: Vec<&String> = files.iter().filter(|n| n.starts_with("bad.bin.")).collect();
    assert_eq!(kept.len(), 1, "the .part is kept: {files:?}");
    let part = h.target().join(kept[0]);
    assert_eq!(std::fs::metadata(part).unwrap().len(), 200_000);
    let events = h.store.row(id).unwrap().events;
    let mismatch = events
        .iter()
        .find(|e| e.kind == "checksum")
        .expect("mismatch event");
    assert!(
        mismatch.detail.as_deref().unwrap().contains(&good),
        "actual digest recorded"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn existing_target_never_overwritten() {
    let h = Harness::new().await;
    let existing = h.target().join("report.bin");
    std::fs::write(&existing, b"precious").unwrap();

    let id = h
        .enqueue(&h.url("/f/41/5000"), Some("report.bin"), None)
        .await;
    let event = h.sink.wait_terminal(id, WAIT).await;

    let path = completed_path(&event);
    assert_eq!(path.file_name().unwrap(), "report (1).bin");
    assert_eq!(std::fs::read(&existing).unwrap(), b"precious");
    verify::file_matches(&path, 41, 5000).unwrap();
    let targets = h.store.row(id).unwrap().finalize_targets;
    assert_eq!(
        targets.len(),
        2,
        "both candidates persisted before their attempts: {targets:?}"
    );
}

/// Records which thread performs every file operation, and writes at most 3 bytes per call.
struct ShortSink {
    data: Arc<std::sync::Mutex<Vec<u8>>>,
    threads: Arc<std::sync::Mutex<Vec<Option<String>>>>,
    calls: Arc<AtomicU64>,
}

impl Sink for ShortSink {
    fn write_at(&mut self, buf: &[u8], offset: u64) -> std::io::Result<usize> {
        self.threads
            .lock()
            .unwrap()
            .push(std::thread::current().name().map(str::to_owned));
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let n = buf.len().min(3);
        let mut data = self.data.lock().unwrap();
        let end = usize::try_from(offset).unwrap() + n;
        if data.len() < end {
            data.resize(end, 0);
        }
        data[usize::try_from(offset).unwrap()..end].copy_from_slice(&buf[..n]);
        Ok(n)
    }

    fn sync_data(&mut self) -> std::io::Result<()> {
        self.threads
            .lock()
            .unwrap()
            .push(std::thread::current().name().map(str::to_owned));
        Ok(())
    }

    fn sync_all(&mut self) -> std::io::Result<()> {
        self.threads
            .lock()
            .unwrap()
            .push(std::thread::current().name().map(str::to_owned));
        Ok(())
    }
}

type Shared<T> = Arc<std::sync::Mutex<T>>;

/// A [`ShortSink`] and the shared state its probes read.
struct ShortProbe {
    sink: ShortSink,
    data: Shared<Vec<u8>>,
    threads: Shared<Vec<Option<String>>>,
    calls: Arc<AtomicU64>,
}

fn short_sink() -> ShortProbe {
    let data = Arc::new(std::sync::Mutex::new(Vec::new()));
    let threads = Arc::new(std::sync::Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicU64::new(0));
    let sink = ShortSink {
        data: Arc::clone(&data),
        threads: Arc::clone(&threads),
        calls: Arc::clone(&calls),
    };
    ShortProbe {
        sink,
        data,
        threads,
        calls,
    }
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| u8::try_from(i % 251).unwrap()).collect()
}

#[tokio::test]
async fn writer_loops_short_writes() {
    let ShortProbe {
        sink, data, calls, ..
    } = short_sink();
    let written = Arc::new(AtomicU64::new(0));
    let mut writer =
        Writer::spawn("forgedm-writer-test".to_owned(), sink, Arc::clone(&written)).unwrap();
    let expected = pattern(1000);

    writer.write(0, &expected[..400]).await.unwrap();
    writer.write(400, &expected[400..]).await.unwrap();
    let frontier = writer.flush().await.unwrap();
    let closed = writer.close().await.unwrap();

    assert_eq!(frontier, 1000);
    assert_eq!(closed, 1000);
    assert_eq!(
        *data.lock().unwrap(),
        expected,
        "every byte lands despite 3-byte writes"
    );
    assert!(
        calls.load(std::sync::atomic::Ordering::SeqCst) >= 334,
        "short writes were looped"
    );
}

#[tokio::test]
async fn writes_happen_on_writer_thread_only() {
    let ShortProbe { sink, threads, .. } = short_sink();
    let mut writer = Writer::spawn(
        "forgedm-writer-abcd1234".to_owned(),
        sink,
        Arc::new(AtomicU64::new(0)),
    )
    .unwrap();
    // More than one coalesced piece, so several Data messages cross the channel.
    let big = pattern(600 * 1024);

    writer.write(0, &big).await.unwrap();
    writer.flush().await.unwrap();
    writer.close().await.unwrap();

    let threads = threads.lock().unwrap();
    assert!(!threads.is_empty());
    assert!(
        threads
            .iter()
            .all(|t| t.as_deref() == Some("forgedm-writer-abcd1234")),
        "file operations ran on {threads:?}"
    );
    assert_ne!(
        std::thread::current().name(),
        Some("forgedm-writer-abcd1234")
    );
}

#[tokio::test]
async fn a_non_contiguous_write_is_refused() {
    let ShortProbe { sink, .. } = short_sink();
    let mut writer = Writer::spawn(
        "forgedm-writer-x".to_owned(),
        sink,
        Arc::new(AtomicU64::new(0)),
    )
    .unwrap();

    writer.write(0, b"abc").await.unwrap();
    let result = writer.write(10, b"def").await;

    assert!(matches!(
        result,
        Err(forgedm_core::writer::WriterError::NotContiguous { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_stops_promptly_keeps_part() {
    let h = Harness::new().await;
    let id = h
        .enqueue(&h.url("/f/51/20000000?slow=400000"), Some("slow.bin"), None)
        .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if h.engine.live(id).is_some_and(|p| p.accepted > 0) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "download never started"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let asked = tokio::time::Instant::now();
    assert!(h.engine.cancel(id));
    h.store
        .wait_state(
            id,
            |s| s == DownloadState::Cancelled,
            Duration::from_secs(5),
        )
        .await;

    assert!(
        asked.elapsed() < Duration::from_secs(3),
        "cancel took {:?}",
        asked.elapsed()
    );
    let files = h.files_in_target();
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(files[0].starts_with("slow.bin.") && files[0].ends_with(".forgedm.part"));
    let len = std::fs::metadata(h.target().join(&files[0])).unwrap().len();
    assert!(len > 0 && len < 20_000_000, "partial data kept: {len}");
    assert!(h.store.row(id).unwrap().completion.is_none());
    assert!(
        !h.sink
            .events()
            .iter()
            .any(|e| matches!(e, EngineEvent::Failed { id: i, .. } if *i == id)),
        "a cancel is not a failure"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn untrusted_cd_safe_name_inside_target() {
    let h = Harness::new().await;
    let header = "attachment%3B%20filename%3D%22..%5C..%5Cevil%3A%3Fname.exe%22";

    let (_id, event) = h.run(&h.url(&format!("/f/61/3000?cd={header}"))).await;

    let path = completed_path(&event);
    assert_eq!(
        path.parent().unwrap(),
        h.target(),
        "created directly inside the target"
    );
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(name, "evilname.exe");
    assert!(path.exists());
    let outside = h.target().parent().unwrap().join("evil.exe");
    assert!(!outside.exists());
    verify::file_matches(&path, 61, 3000).unwrap();
}

fn zone_stream(path: &Path) -> String {
    std::fs::read_to_string(format!("{}:Zone.Identifier", path.display())).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn motw_applied_to_completed_file() {
    let h = Harness::new().await;

    let (id, event) = h.run(&h.url("/f/71/90000?token=SECRET123&t=motw")).await;

    let path = completed_path(&event);
    let zone = zone_stream(&path);
    assert!(zone.contains("ZoneId=3"), "{zone}");
    assert!(
        zone.contains(&format!("HostUrl={}/f/71/90000", h.server.base_url())),
        "{zone}"
    );
    assert!(
        !zone.contains("SECRET123") && !zone.contains('?'),
        "no query in the zone: {zone}"
    );
    let events = h.store.row(id).unwrap().events;
    let motw = events
        .iter()
        .find(|e| e.kind == "motw")
        .expect("motw event");
    assert!(
        matches!(motw.code.as_deref(), Some("marked" | "marked_direct")),
        "{:?}",
        motw.code
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn error_statuses_end_in_failed_or_needs_credentials() {
    let h = Harness::new().await;

    let (id, event) = h.run(&h.url("/f/81/100?status=404")).await;
    assert_eq!(failed_code(&event), DownloadErrorCode::HttpStatus);
    assert_eq!(h.store.state(id), Some(DownloadState::Failed));

    let id = h.enqueue(&h.url("/f/81/100?auth=basic"), None, None).await;
    h.store
        .wait_state(id, |s| s == DownloadState::NeedsCredentials, WAIT)
        .await;
    assert!(h.files_in_target().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn recover_restarts_an_interrupted_download_from_zero() {
    let h = Harness::new().await;
    // A previous run staged a partial file, then the agent was killed.
    let first = h
        .enqueue(
            &h.url("/f/91/5000000?slow=300000&ranges=0"),
            Some("big.bin"),
            None,
        )
        .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while h.engine.live(first).is_none_or(|p| p.accepted == 0) {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(h.engine.cancel(first));
    h.store
        .wait_state(
            first,
            |s| s == DownloadState::Cancelled,
            Duration::from_secs(5),
        )
        .await;
    let stale = h.store.row(first).unwrap().job;
    let part = stale.part_path.clone().unwrap();
    assert!(part.exists());

    // Stage an `interrupted` row that records that part file, as a crash would leave it.
    let id = Uuid::now_v7();
    h.store.seed(StoredJob {
        id,
        state: DownloadState::Interrupted,
        state_reason: Some("crash".to_owned()),
        url: validate_user_url(&h.url("/f/92/70000")).unwrap(),
        part_path: stale.part_path.clone(),
        part_file_id: stale.part_file_id.clone(),
        requested_name: Some("again.bin".to_owned()),
        display_name: "again.bin".to_owned(),
        file_name: None,
        ..stale
    });

    let queued = h.engine.recover().await.unwrap();

    assert_eq!(queued, 1);
    let event = h.sink.wait_terminal(id, WAIT).await;
    verify::file_matches(&completed_path(&event), 92, 70_000).unwrap();
    assert!(
        !part.exists(),
        "the stale staging file we created was removed"
    );
    let row = h.store.row(id).unwrap();
    assert_eq!(row.resets, 1);
    assert!(row.events.iter().any(|e| e.kind == "restart_from_zero"));
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_interrupts_running_downloads() {
    let h = Harness::new().await;
    let id = h
        .enqueue(&h.url("/f/95/20000000?slow=300000"), Some("s.bin"), None)
        .await;
    while h.engine.live(id).is_none_or(|p| p.accepted == 0) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    h.engine.shutdown().await;

    assert_eq!(h.store.state(id), Some(DownloadState::Interrupted));
    assert_eq!(
        h.store.row(id).unwrap().job.state_reason.as_deref(),
        Some("agent_shutdown")
    );
    let after = h
        .engine
        .enqueue(h.new_job(&h.url("/f/1/1"), None, None))
        .await;
    assert!(after.is_err(), "a stopped engine accepts no work");
}
