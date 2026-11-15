//! EnqueueUrl and GetDownload against a real agent and the fixture server (PLAN S1.7).
//!
//! Process tests run the real `forgedm-agent.exe` and kill it in a drop guard. Handler tests
//! drive `AgentHandler` in-process with an injected engine.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

mod support;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use forgedm_agent::app;
use forgedm_agent::config::{Config, ProxyMode};
use forgedm_agent::engine_host::{EngineControl, EnqueueSpec};
use forgedm_agent::error::AgentError;
use forgedm_agent::events_hub::{self, HubInit};
use forgedm_agent::handlers::{AgentHandler, ENGINE_REPLY_TIMEOUT};
use forgedm_core::clock::BoxFuture;
use forgedm_core::engine::EngineConfig;
use forgedm_fixture::{FixtureServer, content, verify};
use forgedm_ipc::{Client, ClientConfig, Connection, IpcError, RequestHandler};
use forgedm_platform::peer::PeerIdentity;
use forgedm_platform::sid::Sid;
use forgedm_protocol::{
    ChecksumStatus, ClientKind, DownloadDetail, DownloadIdRequest, DownloadState,
    EnqueueUrlRequest, EnqueueUrlResult, ErrorCode, Event, EventBody, MotwStatus, ProxySource,
    Request, RequestBody, Response, ResultBody, SubscribeRequest,
};
use support::AgentProc;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(60);

fn expected_sha(seed: u64, size: u64) -> String {
    hex::encode(content::sha256(seed, size))
}

fn enqueue_request(url: &str, dir: &Path, name: Option<&str>) -> EnqueueUrlRequest {
    let mut request = EnqueueUrlRequest::new(url);
    request.target_dir = Some(dir.to_str().unwrap().to_owned());
    request.file_name = name.map(str::to_owned);
    request
}

async fn enqueue(client: &mut Client, request: EnqueueUrlRequest) -> EnqueueUrlResult {
    match client.call(RequestBody::EnqueueUrl(request)).await.unwrap() {
        ResultBody::EnqueueUrl(result) => result,
        other => panic!("not an enqueue result: {other:?}"),
    }
}

async fn detail(client: &mut Client, id: Uuid) -> DownloadDetail {
    let body = RequestBody::GetDownload(DownloadIdRequest { download_id: id });
    match client.call(body).await.unwrap() {
        ResultBody::GetDownload(detail) => *detail,
        other => panic!("not a detail: {other:?}"),
    }
}

/// Snapshots, then subscribes from that position (the documented client sequence).
async fn subscribe_from_snapshot(client: &mut Client, instance: Uuid) {
    let ResultBody::GetSnapshot(snapshot) = client
        .call(RequestBody::GetSnapshot(forgedm_protocol::Empty {}))
        .await
        .unwrap()
    else {
        panic!("not a snapshot");
    };
    let subscribe = SubscribeRequest {
        agent_instance_id: instance,
        after_seq: snapshot.seq,
    };
    client
        .call(RequestBody::Subscribe(subscribe))
        .await
        .unwrap();
}

async fn next_event(client: &mut Client) -> Event {
    tokio::time::timeout(WAIT, client.next_event())
        .await
        .expect("an event within the deadline")
        .expect("event")
}

// ---- process tests ----------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn enqueue_to_completed_with_ordered_events() {
    let fixture = FixtureServer::start_on(0).await.unwrap();
    let (data, target) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let agent = AgentProc::start(data.path(), &[]);
    let (mut client, hello) = agent.connect().await;
    subscribe_from_snapshot(&mut client, hello.agent_instance_id).await;
    let size = 3_000_000;
    let url = format!("{}/f/5/{size}?token=SECRET9", fixture.base_url());
    let mut request = enqueue_request(&url, target.path(), Some("e2e.bin"));
    request.expected_sha256 = Some(expected_sha(5, size));

    let queued = enqueue(&mut client, request).await;

    assert_eq!(queued.state, DownloadState::Queued);
    assert_eq!(queued.file_name, "e2e.bin");
    let mut events: Vec<Event> = Vec::new();
    loop {
        let event = next_event(&mut client).await;
        let done = matches!(event.body, EventBody::DownloadCompleted(_));
        events.push(event);
        if done {
            break;
        }
    }

    let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
    assert!(
        seqs.windows(2).all(|w| w[0] < w[1]),
        "seq strictly increases: {seqs:?}"
    );
    assert!(
        matches!(events[0].body, EventBody::DownloadAdded(_)),
        "{:?}",
        events[0].body
    );
    let states: Vec<DownloadState> = events
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::DownloadChanged(s) => Some(s.state),
            _ => None,
        })
        .collect();
    assert_eq!(
        states,
        [
            DownloadState::Probing,
            DownloadState::Downloading,
            DownloadState::Verifying,
            DownloadState::Finalizing,
            DownloadState::Completed
        ]
    );
    let last = events.last().unwrap();
    let EventBody::DownloadCompleted(done) = &last.body else {
        panic!("last event is not DownloadCompleted");
    };
    let before_completed = &events[events.len() - 2];
    assert!(
        matches!(&before_completed.body, EventBody::DownloadChanged(s) if s.state == DownloadState::Completed),
        "DownloadCompleted follows the matching DownloadChanged"
    );
    assert_eq!(done.id, queued.download_id);
    assert_eq!(done.sha256, expected_sha(5, size));
    assert!(done.length_verified);
    let mut previous = 0;
    for event in &events {
        if let EventBody::ProgressSnapshot(progress) = &event.body {
            for item in progress.items.iter().filter(|i| i.id == queued.download_id) {
                assert!(item.accepted_bytes >= previous, "bytes never go backwards");
                assert!(item.accepted_bytes <= size, "bytes never exceed the total");
                assert!(item.committed_bytes <= item.accepted_bytes);
                assert!(item.total_bytes.is_none_or(|t| t == size));
                previous = item.accepted_bytes;
            }
        }
    }
    verify::file_matches(Path::new(&done.final_path), 5, size).unwrap();
    assert_eq!(Path::new(&done.final_path).file_name().unwrap(), "e2e.bin");

    let info = detail(&mut client, queued.download_id).await;
    assert_eq!(info.summary.state, DownloadState::Completed);
    assert_eq!(info.summary.accepted_bytes, size);
    assert_eq!(info.summary.committed_bytes, size);
    assert_eq!(info.checksum.status, ChecksumStatus::Match);
    assert_eq!(
        info.checksum.actual.as_deref(),
        Some(expected_sha(5, size).as_str())
    );
    assert_eq!(info.length_verified, Some(true));
    assert!(matches!(
        info.motw_status,
        Some(MotwStatus::Marked | MotwStatus::MarkedDirect)
    ));
    assert!(!info.recent_events.is_empty() && info.recent_events.len() <= 20);
    let shown = format!("{info:?}");
    assert!(
        !shown.contains("SECRET9"),
        "no secret in the detail: {shown}"
    );
    assert!(info.summary.url_display.ends_with(&format!("/f/5/{size}")));
}

#[tokio::test(flavor = "multi_thread")]
async fn get_download_true_bytes_midway() {
    let fixture = FixtureServer::start_on(0).await.unwrap();
    let (data, target) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let agent = AgentProc::start(data.path(), &[]);
    let (mut client, _hello) = agent.connect().await;
    let size = 30_000_000;
    let url = format!("{}/f/6/{size}?slow=500000", fixture.base_url());
    let queued = enqueue(
        &mut client,
        enqueue_request(&url, target.path(), Some("mid.bin")),
    )
    .await;

    let started = Instant::now();
    let first = loop {
        let info = detail(&mut client, queued.download_id).await;
        if info.summary.state == DownloadState::Downloading && info.summary.accepted_bytes > 0 {
            break info;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "never reached downloading"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    tokio::time::sleep(Duration::from_millis(600)).await;
    let second = detail(&mut client, queued.download_id).await;

    assert_eq!(first.summary.total_bytes, Some(size));
    assert!(
        second.summary.accepted_bytes > first.summary.accepted_bytes,
        "bytes advance"
    );
    assert!(second.summary.accepted_bytes < size, "midway, not done");
    assert!(second.summary.committed_bytes <= second.summary.accepted_bytes);
    let part_len = std::fs::read_dir(target.path())
        .unwrap()
        .map(|e| e.unwrap())
        .find(|e| e.file_name().to_string_lossy().ends_with(".forgedm.part"))
        .map(|e| e.metadata().unwrap().len())
        .expect("the staging file exists midway");
    assert!(
        part_len <= second.summary.accepted_bytes,
        "bytes on disk ({part_len}) never exceed accepted bytes ({})",
        second.summary.accepted_bytes
    );
    assert!(
        second.summary.speed_bps.is_some(),
        "a live speed is reported while downloading"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn nonresumable_kill_restart_redownloads_from_zero() {
    let fixture = FixtureServer::start_on(0).await.unwrap();
    let (data, target) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let size = 700_000;
    // Fast to 300 kB, then a crawl: the first run is killed in the crawl, the second
    // finishes after about three seconds. `ranges=0`: the server ignores ranges, so the
    // download is not resumable and must restart from zero (a range-capable one resumes).
    let url = format!(
        "{}/f/7/{size}?slow=100000&slow_from=300000&ranges=0&t=restart",
        fixture.base_url()
    );
    let mut agent = AgentProc::start(data.path(), &[]);
    let (mut client, _hello) = agent.connect().await;
    let queued = enqueue(
        &mut client,
        enqueue_request(&url, target.path(), Some("again.bin")),
    )
    .await;
    let started = Instant::now();
    loop {
        let info = detail(&mut client, queued.download_id).await;
        if info.summary.accepted_bytes >= 300_000 {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(30));
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    drop(client);

    agent.kill();
    let restarted = AgentProc::start(data.path(), &[]);
    let (mut client, _hello) = restarted.connect().await;

    let started = Instant::now();
    let finished = loop {
        let info = detail(&mut client, queued.download_id).await;
        if info.summary.state == DownloadState::Completed {
            break info;
        }
        assert!(
            started.elapsed() < WAIT,
            "stuck in {:?}",
            info.summary.state
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };

    let path = finished.summary.final_path.clone().expect("final path");
    verify::file_matches(Path::new(&path), 7, size).unwrap();
    assert!(
        finished
            .recent_events
            .iter()
            .any(|e| e.kind == "restart_from_zero"),
        "the restart is recorded: {:?}",
        finished.recent_events
    );
    let log = fixture.log().for_test("restart");
    // Each run probes (HEAD, then GET bytes=0-0) and then makes one plain GET.
    let requests: Vec<_> = log
        .iter()
        .filter(|r| r.method == "GET" && r.range.as_deref() != Some("bytes=0-0"))
        .collect();
    assert_eq!(requests.len(), 2, "one transfer request per run: {log:?}");
    assert!(
        requests.iter().all(|r| r.range.is_none()),
        "never a ranged transfer request"
    );
    let leftovers: Vec<String> = std::fs::read_dir(target.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        leftovers,
        ["again.bin"],
        "the stale staging file was removed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn progress_events_at_most_5_per_sec() {
    let fixture = FixtureServer::start_on(0).await.unwrap();
    let (data, target) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let agent = AgentProc::start(data.path(), &[]);
    let (mut client, hello) = agent.connect().await;
    subscribe_from_snapshot(&mut client, hello.agent_instance_id).await;
    let urls = [
        format!("{}/f/8/2500000?slow=800000", fixture.base_url()),
        format!("{}/f/9/2500000?slow=800000", fixture.base_url()),
    ];
    let mut ids = Vec::new();
    for (n, url) in urls.iter().enumerate() {
        let name = format!("p{n}.bin");
        ids.push(
            enqueue(
                &mut client,
                enqueue_request(url, target.path(), Some(&name)),
            )
            .await
            .download_id,
        );
    }

    let mut arrivals: Vec<Instant> = Vec::new();
    let mut completed = 0;
    while completed < ids.len() {
        let event = next_event(&mut client).await;
        match event.body {
            EventBody::ProgressSnapshot(_) => arrivals.push(Instant::now()),
            EventBody::DownloadCompleted(_) => completed += 1,
            _ => {}
        }
    }

    assert!(
        arrivals.len() >= 4,
        "the run was long enough to see progress: {}",
        arrivals.len()
    );
    let window = Duration::from_millis(1100);
    for (i, start) in arrivals.iter().enumerate() {
        let in_window = arrivals[i..]
            .iter()
            .take_while(|t| t.duration_since(*start) <= window)
            .count();
        assert!(
            in_window <= 5,
            "{in_window} progress events within {window:?}"
        );
    }
}

// ---- in-process handler tests -----------------------------------------------------------

fn connection() -> Connection {
    let peer = PeerIdentity {
        sid: Sid::current_user().unwrap(),
        image_path: std::env::current_exe().unwrap(),
    };
    Connection::new(1, peer)
}

fn test_hub() -> forgedm_agent::events_hub::HubHandle {
    events_hub::spawn(
        HubInit {
            instance_id: Uuid::now_v7(),
            proxy_source: ProxySource::None,
            data_dir: r"C:\data".to_owned(),
            started: std::time::Instant::now(),
            pid: std::process::id(),
        },
        Vec::new(),
    )
}

/// An engine that must never be reached: every validation failure happens before it.
struct Unreachable;

impl EngineControl for Unreachable {
    fn enqueue(&self, _spec: EnqueueSpec) -> BoxFuture<'_, Result<EnqueueUrlResult, AgentError>> {
        Box::pin(async { panic!("the engine was reached with an invalid request") })
    }

    fn detail(&self, _id: Uuid) -> BoxFuture<'_, Result<DownloadDetail, AgentError>> {
        Box::pin(async { panic!("the engine was reached") })
    }
}

/// An engine whose channel is full.
struct FullEngine;

impl EngineControl for FullEngine {
    fn enqueue(&self, _spec: EnqueueSpec) -> BoxFuture<'_, Result<EnqueueUrlResult, AgentError>> {
        Box::pin(async { Err(AgentError::Busy) })
    }

    fn detail(&self, _id: Uuid) -> BoxFuture<'_, Result<DownloadDetail, AgentError>> {
        Box::pin(async { Err(AgentError::Busy) })
    }
}

/// An engine that never answers.
struct SilentEngine;

impl EngineControl for SilentEngine {
    fn enqueue(&self, _spec: EnqueueSpec) -> BoxFuture<'_, Result<EnqueueUrlResult, AgentError>> {
        Box::pin(std::future::pending())
    }

    fn detail(&self, _id: Uuid) -> BoxFuture<'_, Result<DownloadDetail, AgentError>> {
        Box::pin(std::future::pending())
    }
}

async fn ask(engine: impl EngineControl, request: EnqueueUrlRequest) -> Response {
    let handler = AgentHandler::new(
        test_hub(),
        CancellationToken::new(),
        std::time::Instant::now(),
    )
    .with_engine(Arc::new(engine));
    let mut conn = connection();
    handler
        .handle(&mut conn, Request::new(3, RequestBody::EnqueueUrl(request)))
        .await
}

fn error_of(response: &Response) -> (ErrorCode, Option<String>) {
    let error = response.error().expect("a failure response");
    (
        error.code,
        error.details.as_ref().and_then(|d| d.reason.clone()),
    )
}

#[tokio::test]
async fn invalid_urls_rejected_with_codes() {
    let dir = TempDir::new().unwrap();
    let too_long = format!("https://example.com/{}", "a".repeat(9000));
    let cases = [
        ("not a url", "parse"),
        ("ftp://example.com/f.bin", "scheme"),
        ("https://user:secret@example.com/f.bin", "userinfo"),
        ("https://example.com/f\u{7}.bin", "control_chars"),
        (too_long.as_str(), "too_long"),
    ];
    for (url, reason) in cases {
        let response = ask(Unreachable, enqueue_request(url, dir.path(), None)).await;

        let (code, detail) = error_of(&response);

        assert_eq!(code, ErrorCode::InvalidUrl, "{reason}");
        assert_eq!(detail.as_deref(), Some(reason));
        let wire = format!("{:?}", response.error().unwrap());
        assert!(
            !wire.contains("secret"),
            "the error never echoes the URL: {wire}"
        );
    }
}

#[tokio::test]
async fn invalid_destinations_rejected_with_codes() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("a-file.txt");
    std::fs::write(&file, b"x").unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let junction = dir.path().join("junction");
    let linked = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&junction)
        .arg(&real)
        .output()
        .unwrap();
    assert!(
        linked.status.success(),
        "cannot create a junction: {linked:?}"
    );
    let missing = dir.path().join("missing");
    let cases: Vec<(String, &str)> = vec![
        (r"relative\dir".to_owned(), "not_absolute"),
        (missing.to_str().unwrap().to_owned(), "not_found"),
        (file.to_str().unwrap().to_owned(), "not_directory"),
        (r"\\.\C:\forgedm-device".to_owned(), "device_path"),
        (
            r"\\?\GLOBALROOT\Device\HarddiskVolume1".to_owned(),
            "device_path",
        ),
        (r"\\server\share\dir".to_owned(), "unc_unsupported"),
        (junction.to_str().unwrap().to_owned(), "reparse_point"),
    ];
    for (target, reason) in cases {
        let mut request = EnqueueUrlRequest::new("https://example.com/f.bin");
        request.target_dir = Some(target.clone());

        let response = ask(Unreachable, request).await;

        let (code, detail) = error_of(&response);
        assert_eq!(code, ErrorCode::InvalidDestination, "{target}");
        assert_eq!(detail.as_deref(), Some(reason), "{target}");
    }
}

#[tokio::test(start_paused = true)]
async fn engine_busy_maps_to_busy() {
    let dir = TempDir::new().unwrap();
    let request = || enqueue_request("https://example.com/f.bin", dir.path(), None);

    let full = ask(FullEngine, request()).await;
    let started = tokio::time::Instant::now();
    let silent = ask(SilentEngine, request()).await;

    assert_eq!(error_of(&full).0, ErrorCode::Busy);
    assert!(full.error().unwrap().retryable);
    assert_eq!(error_of(&silent).0, ErrorCode::Busy);
    assert!(
        started.elapsed() >= ENGINE_REPLY_TIMEOUT,
        "a silent engine is answered with busy only after {ENGINE_REPLY_TIMEOUT:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn enqueue_over_queue_cap_limit_exceeded() {
    assert_eq!(
        forgedm_core::engine::QUEUE_MAX,
        10_000,
        "the default cap is A.9 QUEUE_MAX"
    );
    let fixture = FixtureServer::start_on(0).await.unwrap();
    let (data, target) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let config = Config::new(
        data.path().to_path_buf(),
        ProxyMode::None,
        "warn".to_owned(),
    );
    let prepared = app::prepare(config).expect("prepare");
    let token = CancellationToken::new();
    let cap = EngineConfig {
        queue_max: 2,
        ..EngineConfig::default()
    };
    let agent = app::start_with(prepared, token.clone(), cap)
        .await
        .expect("start");
    let mut client = Client::connect(ClientConfig::new(
        agent.pipe_name(),
        std::env::current_exe().unwrap(),
    ))
    .await
    .unwrap();
    client
        .handshake(ClientKind::Cli, "cap-test", "0")
        .await
        .unwrap();
    let url = |n: u32| format!("{}/f/{n}/1000", fixture.base_url());

    enqueue(
        &mut client,
        enqueue_request(&url(1), target.path(), Some("one.bin")),
    )
    .await;
    enqueue(
        &mut client,
        enqueue_request(&url(2), target.path(), Some("two.bin")),
    )
    .await;
    let third = client
        .call(RequestBody::EnqueueUrl(enqueue_request(
            &url(3),
            target.path(),
            Some("three.bin"),
        )))
        .await;

    match third {
        Err(IpcError::Server(error)) => {
            assert_eq!(error.code, ErrorCode::LimitExceeded);
            assert!(!error.retryable);
        }
        other => panic!("expected limit_exceeded, got {other:?}"),
    }
    token.cancel();
    agent.wait().await.expect("clean stop");
}

#[tokio::test]
async fn get_download_unknown_id_is_not_found() {
    let response = {
        let handler = AgentHandler::new(
            test_hub(),
            CancellationToken::new(),
            std::time::Instant::now(),
        )
        .with_engine(Arc::new(NotFoundEngine));
        let mut conn = connection();
        let body = RequestBody::GetDownload(DownloadIdRequest {
            download_id: Uuid::now_v7(),
        });
        handler.handle(&mut conn, Request::new(4, body)).await
    };

    assert_eq!(error_of(&response).0, ErrorCode::NotFound);
}

/// An engine that knows no download.
struct NotFoundEngine;

impl EngineControl for NotFoundEngine {
    fn enqueue(&self, _spec: EnqueueSpec) -> BoxFuture<'_, Result<EnqueueUrlResult, AgentError>> {
        Box::pin(async { Err(AgentError::Busy) })
    }

    fn detail(&self, id: Uuid) -> BoxFuture<'_, Result<DownloadDetail, AgentError>> {
        Box::pin(async move {
            Err(AgentError::Storage(
                forgedm_storage::StorageError::NotFound { id },
            ))
        })
    }
}
