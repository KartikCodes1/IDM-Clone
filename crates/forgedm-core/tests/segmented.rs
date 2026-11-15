//! Segmented transfer through the real engine (PLAN S1.11): real HTTP against the fixture
//! server, real files, real writer thread. Only persistence is the in-memory fake.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

#[path = "../src/testing.rs"]
#[allow(dead_code)] // each test file uses part of the shared fakes
mod testing;

use std::net::SocketAddr;
use std::os::windows::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use forgedm_core::client::{HttpClient, ProxyMode};
use forgedm_core::engine::{self, Control, EngineConfig, EngineDeps, EngineHandle};
use forgedm_core::ports::{EngineEvent, NewJob, unix_ms};
use forgedm_core::transfer::prealloc::{
    clear_sparse, is_sparse, prepare_part, prepare_part_on_volume,
};
use forgedm_core::transfer::segment_table::{MIN_SPLIT, SPLIT_ALIGN, SegmentSnapshot};
use forgedm_core::url_policy::validate_user_url;
use forgedm_core::writer::{Sink, WRITE_COALESCE, WRITER_QUEUE, Writer};
use forgedm_fixture::{FixtureServer, LoggedRequest, content, verify};
use forgedm_platform::fs::{VolumeInfo, allocated_size, volume_info};
use forgedm_platform::motw::MotwService;
use forgedm_protocol::{ClientKind, DownloadErrorCode, DownloadState, Priority, TransferMode};
use testing::{MemoryStore, RecordingSink};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(90);
const MIB: u64 = 1024 * 1024;
const POLL: Duration = Duration::from_millis(20);

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
        let engine = Self::engine_over(&store, &sink);
        Self {
            server,
            store,
            sink,
            engine,
            dir: tempfile::tempdir().unwrap(),
        }
    }

    /// A new engine over the same store and sink: an "agent restart".
    fn engine_over(store: &Arc<MemoryStore>, sink: &Arc<RecordingSink>) -> EngineHandle {
        engine::start(
            EngineConfig::default(),
            EngineDeps {
                store: store.clone(),
                sink: sink.clone(),
                client: HttpClient::new(ProxyMode::None).unwrap(),
                motw: Arc::new(MotwService::start().unwrap()),
            },
        )
    }

    fn target(&self) -> PathBuf {
        std::fs::canonicalize(self.dir.path()).unwrap()
    }

    fn url(&self, path_and_query: &str) -> String {
        format!("{}{path_and_query}", self.server.base_url())
    }

    async fn enqueue_url(&self, url: &str, name: &str, connections: u32) -> Uuid {
        let job = NewJob {
            id: Uuid::now_v7(),
            source: ClientKind::Cli,
            priority: Priority::Normal,
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

    async fn run(&self, query: &str, name: &str, connections: u32) -> (Uuid, EngineEvent) {
        let id = self.enqueue_url(&self.url(query), name, connections).await;
        (id, self.sink.wait_terminal(id, WAIT).await)
    }

    fn log(&self, key: &str) -> Vec<LoggedRequest> {
        self.server.log().for_test(key)
    }

    fn part_path(&self, id: Uuid) -> PathBuf {
        self.store.row(id).unwrap().job.part_path.expect("a .part")
    }

    /// Polls until `check` holds or the deadline passes.
    async fn wait_until(&self, what: &str, mut check: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + WAIT;
        while !check() {
            assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
            tokio::time::sleep(POLL).await;
        }
    }
}

fn completed(event: &EngineEvent) -> (PathBuf, String) {
    match event {
        EngineEvent::Completed {
            final_path, sha256, ..
        } => (final_path.clone(), sha256.clone()),
        other => panic!("expected completion, got {other:?}"),
    }
}

fn failure(event: &EngineEvent) -> (DownloadErrorCode, Option<String>) {
    match event {
        EngineEvent::Failed { error, .. } => (
            error.code,
            error.details.as_ref().and_then(|d| d.reason.clone()),
        ),
        other => panic!("expected a failure, got {other:?}"),
    }
}

/// `(start, end_excl)` of a logged `Range: bytes=a-b`.
fn window(entry: &LoggedRequest) -> Option<(u64, u64)> {
    let spec = entry.range.as_deref()?.strip_prefix("bytes=")?;
    let (a, b) = spec.split_once('-')?;
    Some((a.parse().ok()?, b.parse::<u64>().ok()? + 1))
}

/// The transfer requests (everything but the probe's `bytes=0-0`), in arrival order.
fn transfers(log: &[LoggedRequest]) -> Vec<&LoggedRequest> {
    log.iter()
        .filter(|e| {
            e.method == "GET" && e.range.is_some() && e.range.as_deref() != Some("bytes=0-0")
        })
        .collect()
}

/// Checks that bytes `[start, end)` of `path` are the generator's bytes.
fn range_matches(path: &Path, seed: u64, start: u64, end: u64) {
    let file = std::fs::File::open(path).unwrap();
    let mut pos = start;
    let mut actual = vec![0u8; 64 * 1024];
    let mut expected = vec![0u8; 64 * 1024];
    while pos < end {
        let n = usize::try_from((end - pos).min(64 * 1024)).unwrap();
        let mut got = 0;
        while got < n {
            let read = file
                .seek_read(&mut actual[got..n], pos + got as u64)
                .unwrap();
            assert!(
                read > 0,
                "file ends at {} but {end} was committed",
                pos + got as u64
            );
            got += read;
        }
        content::fill(seed, pos, &mut expected[..n]);
        assert_eq!(
            actual[..n],
            expected[..n],
            "mismatch in [{pos}, {})",
            pos + n as u64
        );
        pos += n as u64;
    }
}

fn expected_sha(seed: u64, size: u64) -> String {
    hex::encode(content::sha256(seed, size))
}

fn segments_of(h: &Harness, id: Uuid) -> Vec<SegmentSnapshot> {
    h.store.row(id).unwrap().segments
}

#[tokio::test(flavor = "multi_thread")]
async fn workers_1_4_8_identical_sha_and_length() {
    let h = Harness::new().await;
    let size = 64 * MIB + 12_345;
    for connections in [1u32, 4, 8] {
        let name = format!("w{connections}.bin");

        let (id, event) = h
            .run(
                &format!("/f/41/{size}?t=w{connections}"),
                &name,
                connections,
            )
            .await;

        let (path, sha) = completed(&event);
        assert_eq!(sha, expected_sha(41, size), "{connections} workers");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), size);
        verify::file_matches(&path, 41, size).unwrap();
        let row = h.store.row(id).unwrap();
        assert_eq!(row.probe, Some((TransferMode::Segmented, Some(size))));
        assert_eq!(
            row.segments,
            [SegmentSnapshot {
                seg_no: 0,
                start: 0,
                end: size,
                committed: size
            }],
            "adjacent complete segments merge in the final checkpoint"
        );
        let first_plan = &row.segment_checkpoints[0];
        let expected_plan = usize::try_from(connections.min(4)).unwrap();
        assert_eq!(first_plan.len(), expected_plan, "the initial plan");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn logged_ranges_disjoint_and_cover() {
    let h = Harness::new().await;
    let size = 32 * MIB + 1000;
    let (id, event) = h.run(&format!("/f/42/{size}?t=cover"), "c.bin", 4).await;
    completed(&event);

    let log = h.log("cover");
    let mut windows: Vec<(u64, u64)> = transfers(&log).iter().filter_map(|e| window(e)).collect();
    windows.sort_unstable();
    assert!(windows.len() >= 4, "{windows:?}");
    assert_eq!(windows[0].0, 0, "{windows:?}");
    for pair in windows.windows(2) {
        assert_ne!(pair[0].0, pair[1].0, "two requests start at {}", pair[0].0);
        assert!(
            pair[1].0 <= pair[0].1,
            "gap between {:?} and {:?}",
            pair[0],
            pair[1]
        );
    }
    assert!(windows.iter().all(|(s, e)| s < e && *e <= size));
    assert_eq!(windows.iter().map(|w| w.1).max(), Some(size));
    // Without a split the windows are exactly the plan: disjoint and tiling.
    if windows.len() == 4 {
        for pair in windows.windows(2) {
            assert_eq!(pair[0].1, pair[1].0, "{windows:?}");
        }
    }
    assert!(h.server.log().served_bytes() >= size);
    assert_eq!(h.store.state(id), Some(DownloadState::Completed));
}

#[tokio::test(flavor = "multi_thread")]
async fn connections_really_parallel_distinct_ports() {
    let h = Harness::new().await;
    let size = 32 * MIB;

    let (_, event) = h
        .run(&format!("/f/43/{size}?slow=8000000&t=par"), "par.bin", 4)
        .await;

    completed(&event);
    let log = h.log("par");
    let ports: std::collections::BTreeSet<u16> =
        transfers(&log).iter().map(|e| e.peer_port).collect();
    assert!(ports.len() >= 4, "distinct client ports {ports:?}");
    let conns: std::collections::BTreeSet<u64> =
        transfers(&log).iter().map(|e| e.conn_id).collect();
    assert!(conns.len() >= 4, "distinct connections {conns:?}");
    assert!(
        h.server.log().max_concurrency() >= 3,
        "bodies must overlap, saw {}",
        h.server.log().max_concurrency()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn steals_from_slow_range() {
    let h = Harness::new().await;
    let size = 64 * MIB;
    let slow_from = 48 * MIB;
    let id = h
        .enqueue_url(
            &h.url(&format!(
                "/f/44/{size}?slow=4000000&slow_from={slow_from}&t=steal"
            )),
            "steal.bin",
            4,
        )
        .await;

    // Progress polling: the three fast segments finish, their workers go idle and must take
    // half of the slow one, so a request appears that starts inside it.
    let log_handle = h.server.log();
    h.wait_until("a request inside the slow segment", || {
        log_handle
            .for_test("steal")
            .iter()
            .filter_map(window)
            .any(|(start, _)| start > slow_from)
    })
    .await;
    let stolen = h
        .log("steal")
        .iter()
        .filter_map(window)
        .map(|w| w.0)
        .filter(|s| *s > slow_from)
        .min()
        .unwrap();
    assert!(stolen >= slow_from + MIN_SPLIT, "{stolen}");
    assert_eq!(stolen % SPLIT_ALIGN, 0, "split points are 64 KiB aligned");

    let (path, sha) = completed(&h.sink.wait_terminal(id, WAIT).await);
    verify::file_matches(&path, 44, size).unwrap();
    assert_eq!(sha, expected_sha(44, size));
}

#[tokio::test(flavor = "multi_thread")]
async fn small_file_single_connection() {
    let h = Harness::new().await;
    for (seed, size, connections, mode) in [
        (45u64, 6 * MIB, 8u32, TransferMode::SingleRanged),
        (46, 8 * MIB - 1, 8, TransferMode::SingleRanged),
        (47, 8 * MIB, 1, TransferMode::Segmented),
    ] {
        let key = format!("small{seed}");
        let (id, event) = h
            .run(
                &format!("/f/{seed}/{size}?t={key}"),
                &format!("{key}.bin"),
                connections,
            )
            .await;

        let (path, _) = completed(&event);
        verify::file_matches(&path, seed, size).unwrap();
        assert_eq!(h.store.row(id).unwrap().probe, Some((mode, Some(size))));
        let log = h.log(&key);
        assert_eq!(
            transfers(&log).len(),
            1,
            "one connection for {size}: {log:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn short_206_continues() {
    let h = Harness::new().await;
    let size = 16 * MIB + 7;

    let (_, event) = h
        .run(&format!("/f/48/{size}?short206=3&t=short"), "short.bin", 4)
        .await;

    let (path, sha) = completed(&event);
    verify::file_matches(&path, 48, size).unwrap();
    assert_eq!(sha, expected_sha(48, size));
    let log = h.log("short");
    assert!(
        transfers(&log).len() >= 4 + 4 * 3,
        "each short 206 is followed by a request for the rest: {}",
        transfers(&log).len()
    );
}

/// Every committed range of the last checkpoint matches the generator.
fn assert_committed_ranges_valid(h: &Harness, id: Uuid, seed: u64) {
    let part = h.part_path(id);
    for seg in segments_of(h, id) {
        range_matches(&part, seed, seg.start, seg.committed);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ranges_ignored_mid_download_no_misplaced_writes() {
    let h = Harness::new().await;
    let size = 32 * MIB;

    // With a strong validator `If-Range` turns the ignored range into a changed resource.
    let (id, event) = h
        .run(
            &format!("/f/49/{size}?range_200_after=3&slow=6000000&t=r200"),
            "r200.bin",
            4,
        )
        .await;
    assert_eq!(failure(&event).0, DownloadErrorCode::RemoteChanged);
    assert_eq!(h.store.state(id), Some(DownloadState::Failed));
    assert_committed_ranges_valid(&h, id, 49);

    // Without a validator there is no `If-Range`: the 200 must still never be written at
    // another offset.
    let (id, event) = h
        .run(
            &format!("/f/50/{size}?etag=none&range_200_after=3&slow=6000000&t=r200b"),
            "r200b.bin",
            4,
        )
        .await;
    let (code, reason) = failure(&event);
    assert_eq!(code, DownloadErrorCode::RangeProtocolViolation);
    assert_eq!(reason.as_deref(), Some("ranges_no_longer_supported"));
    assert_committed_ranges_valid(&h, id, 50);
}

/// A server that probes correctly but answers every other range with a wrong start.
async fn serve_bad_ranges(total: u64) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                let range = head.lines().find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("range: bytes=")
                        .map(str::to_owned)
                });
                let reply: Vec<u8> = match range.as_deref().and_then(|r| r.split_once('-')) {
                    None => b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
                    Some(("0", "0")) => format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-0/{total}\r\n\
                         Content-Length: 1\r\nETag: \"bad\"\r\nConnection: close\r\n\r\n\u{0}"
                    )
                    .into_bytes(),
                    Some((a, b)) => {
                        let (a, b): (u64, u64) = (a.parse().unwrap(), b.parse().unwrap());
                        let mut out = format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {}-{b}/{total}\r\n\
                             Content-Length: {}\r\nETag: \"bad\"\r\nConnection: close\r\n\r\n",
                            a + 1,
                            b - a
                        )
                        .into_bytes();
                        out.extend(std::iter::repeat_n(0xEEu8, 64 * 1024));
                        out
                    }
                };
                let _ = stream.write_all(&reply).await;
            });
        }
    });
    addr
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_content_range_writes_nothing() {
    let h = Harness::new().await;
    let total = 16 * MIB;
    let addr = serve_bad_ranges(total).await;

    let id = h
        .enqueue_url(&format!("http://{addr}/bad.bin"), "bad.bin", 4)
        .await;
    let event = h.sink.wait_terminal(id, WAIT).await;

    let (code, reason) = failure(&event);
    assert_eq!(code, DownloadErrorCode::RangeProtocolViolation);
    assert_eq!(reason.as_deref(), Some("wrong_start"));
    let bytes = std::fs::read(h.part_path(id)).unwrap();
    assert!(
        bytes.iter().all(|b| *b == 0),
        "no byte of the refused body reached the file"
    );
    let committed = h.store.row(id).unwrap().checkpoints.last().copied();
    assert_eq!(committed.map(|c| c.0), Some(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_connections_2_8_1_preserve_progress() {
    let h = Harness::new().await;
    let size = 40 * MIB;
    let id = h
        .enqueue_url(
            &h.url(&format!("/f/51/{size}?slow=4000000&t=rt")),
            "rt.bin",
            2,
        )
        .await;
    let accepted = || h.engine.live(id).map_or(0, |s| s.accepted);
    let connections = || h.engine.live(id).map_or(0, |s| s.active_connections);
    let mut last = 0;
    let mut watch = |now: u64| {
        assert!(
            now >= last,
            "accepted bytes went backwards: {last} -> {now}"
        );
        last = now;
    };

    h.wait_until("2 connections running", || connections() == 2)
        .await;
    h.wait_until("4 MiB accepted", || {
        watch(accepted());
        accepted() >= 4 * MIB
    })
    .await;
    assert_eq!(h.engine.set_max_connections(id, 8).await.unwrap(), 8);
    h.wait_until("more than 2 connections after raising to 8", || {
        watch(accepted());
        connections() > 2
    })
    .await;
    h.wait_until("16 MiB accepted", || {
        watch(accepted());
        accepted() >= 16 * MIB
    })
    .await;
    assert_eq!(h.engine.set_max_connections(id, 1).await.unwrap(), 1);
    h.wait_until("one connection after lowering to 1", || {
        watch(accepted());
        connections() == 1
    })
    .await;

    let (path, sha) = completed(&h.sink.wait_terminal(id, WAIT).await);
    verify::file_matches(&path, 51, size).unwrap();
    assert_eq!(sha, expected_sha(51, size));
    let checkpoints = h.store.row(id).unwrap().checkpoints;
    assert!(
        checkpoints.windows(2).all(|w| w[0].0 <= w[1].0),
        "committed bytes never decrease: {checkpoints:?}"
    );
}

/// Asserts that every request after `before` starts at or beyond the committed offset of the
/// segment (of the paused table) that contains it.
fn assert_resumed_from_committed(log: &[LoggedRequest], before: usize, segs: &[SegmentSnapshot]) {
    let resumed: Vec<(u64, u64)> = transfers(&log[before..])
        .iter()
        .filter_map(|e| window(e))
        .collect();
    assert!(!resumed.is_empty());
    for (start, _) in resumed {
        let seg = segs
            .iter()
            .find(|s| s.start <= start && start < s.end)
            .unwrap();
        assert!(
            start >= seg.committed,
            "request at {start} re-fetches committed bytes of {seg:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_and_resume_continue_every_segment_from_committed() {
    let h = Harness::new().await;
    let size = 48 * MIB;
    let id = h
        .enqueue_url(
            &h.url(&format!("/f/52/{size}?slow=2000000&t=pr")),
            "pr.bin",
            4,
        )
        .await;
    h.wait_until("a checkpoint with data", || {
        h.store
            .row(id)
            .is_some_and(|r| r.checkpoints.last().is_some_and(|c| c.0 >= 4 * MIB))
    })
    .await;

    let paused = h
        .engine
        .control(
            id,
            Control::Pause {
                discard_progress: false,
            },
        )
        .await
        .unwrap();

    assert_eq!(paused, DownloadState::Paused);
    let segs = segments_of(&h, id);
    assert!(
        segs.iter().filter(|s| s.committed > s.start).count() >= 2,
        "several segments made progress: {segs:?}"
    );
    assert_committed_ranges_valid(&h, id, 52);
    let before = h.log("pr").len();
    h.engine.control(id, Control::Resume).await.unwrap();
    let (path, sha) = completed(&h.sink.wait_terminal(id, WAIT).await);

    verify::file_matches(&path, 52, size).unwrap();
    assert_eq!(sha, expected_sha(52, size));
    assert_resumed_from_committed(&h.log("pr"), before, &segs);
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_then_restart_resumes_segments_from_committed() {
    let h = Harness::new().await;
    let size = 48 * MIB;
    let id = h
        .enqueue_url(
            &h.url(&format!("/f/53/{size}?slow=2000000&t=rs")),
            "rs.bin",
            4,
        )
        .await;
    h.wait_until("a checkpoint with data", || {
        h.store
            .row(id)
            .is_some_and(|r| r.checkpoints.last().is_some_and(|c| c.0 >= 4 * MIB))
    })
    .await;

    h.engine.shutdown().await;

    assert_eq!(h.store.state(id), Some(DownloadState::Interrupted));
    let segs = segments_of(&h, id);
    assert_committed_ranges_valid(&h, id, 53);
    let before = h.log("rs").len();
    let restarted = Harness::engine_over(&h.store, &h.sink);
    assert_eq!(restarted.recover().await.unwrap(), 1);
    let (path, sha) = completed(&h.sink.wait_terminal(id, WAIT).await);

    verify::file_matches(&path, 53, size).unwrap();
    assert_eq!(sha, expected_sha(53, size));
    assert_resumed_from_committed(&h.log("rs"), before, &segs);
}

#[tokio::test(flavor = "multi_thread")]
async fn restart_keeps_the_stored_connection_cap() {
    let h = Harness::new().await;
    let size = 48 * MIB;
    let id = h
        .enqueue_url(
            &h.url(&format!("/f/54/{size}?slow=2000000&t=cap")),
            "cap.bin",
            2,
        )
        .await;
    h.wait_until("a checkpoint with data", || {
        h.store
            .row(id)
            .is_some_and(|r| r.checkpoints.last().is_some_and(|c| c.0 >= 2 * MIB))
    })
    .await;
    h.engine.shutdown().await;
    assert_eq!(h.store.row(id).unwrap().job.max_connections, 2);
    let before = h.log("cap").len();

    let restarted = Harness::engine_over(&h.store, &h.sink);
    assert_eq!(restarted.recover().await.unwrap(), 1);
    let (path, _) = completed(&h.sink.wait_terminal(id, WAIT).await);

    verify::file_matches(&path, 54, size).unwrap();
    let after = h.log("cap").len() - before;
    assert!(
        after <= 2,
        "a cap of 2 must survive the restart, but {after} requests were made"
    );
}

/// A sink that blocks every write until released, counting the writes it started.
struct GatedSink {
    gate: std::sync::mpsc::Receiver<()>,
    started: Arc<AtomicU64>,
}

impl Sink for GatedSink {
    fn write_at(&mut self, buf: &[u8], _offset: u64) -> std::io::Result<usize> {
        self.started.fetch_add(1, Ordering::SeqCst);
        let _ = self.gate.recv();
        Ok(buf.len())
    }
    fn sync_data(&mut self) -> std::io::Result<()> {
        Ok(())
    }
    fn sync_all(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn writer_backpressure_bounded_queue() {
    let (open, gate) = std::sync::mpsc::channel();
    let started = Arc::new(AtomicU64::new(0));
    let sink = GatedSink {
        gate,
        started: Arc::clone(&started),
    };
    let written = Arc::new(AtomicU64::new(0));
    let writer = Writer::spawn("backpressure".to_owned(), sink, Arc::clone(&written)).unwrap();
    let handle = writer.handle();
    let piece = Bytes::from(vec![7u8; WRITE_COALESCE]);

    // The disk is stuck on the first write: at most one piece is in the thread and
    // WRITER_QUEUE wait in the queue; the next send must wait.
    let mut accepted = 0usize;
    for index in 0..(WRITER_QUEUE + 8) {
        let offset = (index * WRITE_COALESCE) as u64;
        let sent = tokio::time::timeout(
            Duration::from_millis(150),
            handle.send(offset, piece.clone()),
        )
        .await;
        if sent.is_err() {
            break;
        }
        accepted += 1;
    }
    assert!(
        (WRITER_QUEUE..=WRITER_QUEUE + 1).contains(&accepted),
        "accepted {accepted} pieces while the disk was stuck"
    );
    assert!(started.load(Ordering::SeqCst) <= 1);

    // Releasing the disk drains everything, in order, and nothing was lost.
    for _ in 0..(WRITER_QUEUE + 8) {
        let _ = open.send(());
    }
    let frontier = writer.close().await.unwrap();
    assert_eq!(frontier, (accepted * WRITE_COALESCE) as u64);
}

fn sparse_volume() -> Option<VolumeInfo> {
    let probe = tempfile::tempfile().unwrap();
    volume_info(&probe)
        .ok()
        .filter(|v| v.supports_sparse && v.fs_name.eq_ignore_ascii_case("NTFS"))
}

#[test]
#[allow(clippy::print_stderr)] // the runtime skip message is the point
fn sparse_set_then_cleared() {
    if sparse_volume().is_none() {
        eprintln!(
            "SKIPPED sparse_set_then_cleared: the temp volume is not NTFS with sparse support"
        );
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sparse.part");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();

    assert!(prepare_part_on_volume(&file, 64 * MIB).unwrap());
    assert!(is_sparse(&file).unwrap(), "set at creation");
    file.seek_write(&[1, 2, 3, 4], 60 * MIB).unwrap();
    assert_eq!(file.metadata().unwrap().len(), 60 * MIB + 4);
    assert!(
        allocated_size(&path).unwrap() < MIB,
        "the hole before 60 MiB takes no space"
    );
    clear_sparse(&file);

    assert!(!is_sparse(&file).unwrap(), "cleared at finalize");
}

#[test]
fn fat32_over_4gib_fails_early_file_system_limit() {
    let file = tempfile::tempfile().unwrap();
    let fat32 = |_: &std::fs::File| {
        Ok(VolumeInfo {
            fs_name: "FAT32".to_owned(),
            supports_sparse: false,
            supports_streams: false,
        })
    };

    let refused = prepare_part(&file, 4 * 1024 * MIB, fat32);

    let stop = refused.unwrap_err();
    assert!(matches!(
        stop,
        forgedm_core::error::TaskStop::Fail(f) if f.code == DownloadErrorCode::FileSystemLimit
    ));
    assert_eq!(file.metadata().unwrap().len(), 0, "nothing was written");
    assert!(prepare_part(&file, 4 * 1024 * MIB - 1, fat32).is_ok());
}
