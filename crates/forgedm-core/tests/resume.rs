//! Probe, validated resume and the control commands through the real engine (PLAN S1.9):
//! real HTTP against the fixture server and real files; only persistence is the in-memory
//! fake. The kill-and-restart proof with real processes is `forgedm-cli`'s `e2e_first_proof`.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

#[path = "../src/testing.rs"]
#[allow(dead_code)] // each test file uses part of the shared fakes
mod testing;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use forgedm_core::client::{HttpClient, ProxyMode};
use forgedm_core::engine::{self, Control, EngineConfig, EngineDeps, EngineHandle};
use forgedm_core::error::EngineError;
use forgedm_core::ports::{EngineEvent, NewJob, StoredJob, StoredProgress, unix_ms};
use forgedm_core::transfer::encode_file_id;
use forgedm_core::url_policy::validate_user_url;
use forgedm_fixture::{FixtureServer, LoggedRequest, content, verify};
use forgedm_platform::fs::{create_part_file, file_id};
use forgedm_platform::motw::MotwService;
use forgedm_protocol::{ClientKind, DownloadErrorCode, DownloadState, Priority, TransferMode};
use testing::{MemoryStore, RecordingSink};
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(60);
/// The fixture's seed change once an `etag=mutable` resource has mutated.
const MUTATED_SEED_XOR: u64 = 0x9E37_79B9_7F4A_7C15;

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

    async fn enqueue(&self, url: &str, name: &str, sha: Option<&str>) -> Uuid {
        let job = NewJob {
            id: Uuid::now_v7(),
            source: ClientKind::Cli,
            priority: Priority::Normal,
            display_name: name.to_owned(),
            url: validate_user_url(url).unwrap(),
            target_dir: self.target(),
            requested_name: Some(name.to_owned()),
            expected_sha256: sha.map(str::to_owned),
            max_connections: 4,
            created_at_ms: unix_ms(),
        };
        let id = job.id;
        self.engine.enqueue(job).await.unwrap();
        id
    }

    /// Waits for the first checkpoint of at least `min` bytes and returns it.
    async fn wait_committed(&self, id: Uuid, min: u64) -> u64 {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let last = self
                .store
                .row(id)
                .and_then(|r| r.checkpoints.last().copied());
            if let Some((committed, _)) = last.filter(|(c, _)| *c >= min) {
                return committed;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "no checkpoint of {min} bytes"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn committed(&self, id: Uuid) -> u64 {
        self.store.row(id).unwrap().checkpoints.last().unwrap().0
    }

    fn part_path(&self, id: Uuid) -> PathBuf {
        self.store
            .row(id)
            .unwrap()
            .job
            .part_path
            .expect("a recorded .part")
    }

    fn log(&self, key: &str) -> Vec<LoggedRequest> {
        self.server.log().for_test(key)
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

fn completed(event: &EngineEvent) -> (PathBuf, String) {
    match event {
        EngineEvent::Completed {
            final_path, sha256, ..
        } => (final_path.clone(), sha256.clone()),
        other => panic!("expected completion, got {other:?}"),
    }
}

fn pause() -> Control {
    Control::Pause {
        discard_progress: false,
    }
}

fn gets_with_range(log: &[LoggedRequest]) -> Vec<&LoggedRequest> {
    log.iter()
        .filter(|e| e.method == "GET" && e.range.as_deref() != Some("bytes=0-0"))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_flush_commit_resume_uses_committed_offset_and_strong_if_range() {
    let h = Harness::new().await;
    let size = 2_000_000u64;
    let id = h
        .enqueue(
            &h.url(&format!("/f/31/{size}?slow=500000&t=pause1")),
            "p.bin",
            None,
        )
        .await;
    h.wait_committed(id, 1).await;

    let state = h.engine.control(id, pause()).await.unwrap();

    assert_eq!(
        state,
        DownloadState::Paused,
        "the reply is the settled state"
    );
    assert_eq!(h.store.state(id), Some(DownloadState::Paused));
    let committed = h.committed(id);
    assert!(committed > 0 && committed < size, "committed {committed}");
    verify::prefix_matches(&h.part_path(id), 31, committed).unwrap();

    assert_eq!(
        h.engine.control(id, Control::Resume).await.unwrap(),
        DownloadState::Queued
    );
    let (path, sha) = completed(&h.sink.wait_terminal(id, WAIT).await);

    verify::file_matches(&path, 31, size).unwrap();
    assert_eq!(sha, expected_sha(31, size));
    let log = h.log("pause1");
    let ranged = gets_with_range(&log);
    assert_eq!(
        ranged.len(),
        2,
        "one transfer before the pause, one after: {log:?}"
    );
    let resumed = ranged[1];
    assert_eq!(
        resumed.range.as_deref(),
        Some(format!("bytes={committed}-{}", size - 1).as_str())
    );
    assert_eq!(
        resumed.if_range.as_deref(),
        Some(format!("\"31-{size}\"").as_str()),
        "the strong ETag is sent as If-Range"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn resumed_download_checksum_actual_matches_generator() {
    let h = Harness::new().await;
    let size = 1_500_000u64;
    let sha = expected_sha(32, size);
    let id = h
        .enqueue(
            &h.url(&format!("/f/32/{size}?slow=500000")),
            "c.bin",
            Some(&sha),
        )
        .await;
    h.wait_committed(id, 1).await;
    h.engine.control(id, pause()).await.unwrap();
    h.engine.control(id, Control::Resume).await.unwrap();

    let (path, actual) = completed(&h.sink.wait_terminal(id, WAIT).await);

    assert_eq!(actual, sha, "the digest streamed from the finished file");
    verify::file_matches(&path, 32, size).unwrap();
    let completion = h.store.row(id).unwrap().completion.unwrap();
    assert_eq!((completion.1.as_str(), completion.2), (sha.as_str(), true));
}

#[tokio::test(flavor = "multi_thread")]
async fn weak_or_no_validator_restarts_from_zero() {
    for (n, etag) in ["weak", "none"].into_iter().enumerate() {
        let h = Harness::new().await;
        let size = 1_500_000u64;
        let key = format!("restart_{etag}");
        let url = h.url(&format!(
            "/f/{}/{size}?slow=500000&etag={etag}&t={key}",
            50 + n
        ));
        let id = h.enqueue(&url, "w.bin", None).await;
        h.wait_committed(id, 1).await;
        h.engine.control(id, pause()).await.unwrap();

        h.engine.control(id, Control::Resume).await.unwrap();
        let (path, _) = completed(&h.sink.wait_terminal(id, WAIT).await);

        verify::file_matches(&path, 50 + u64::try_from(n).unwrap(), size).unwrap();
        let row = h.store.row(id).unwrap();
        assert!(row.resets >= 1, "{etag}: progress was reset");
        assert!(
            row.events.iter().any(|e| e.kind == "restart_from_zero"),
            "{etag}: the restart is recorded"
        );
        let log = h.log(&key);
        let last = *gets_with_range(&log).last().unwrap();
        assert_eq!(
            last.range.as_deref(),
            Some(format!("bytes=0-{}", size - 1).as_str()),
            "{etag}: the transfer starts over"
        );
        assert_eq!(last.if_range, None, "{etag}: nothing strong to send");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn n200_on_resume_never_written_at_offset() {
    let h = Harness::new().await;
    let size = 1_500_000u64;
    // The third ranged request (the resume) is answered 200 with the whole body.
    let id = h
        .enqueue(
            &h.url(&format!(
                "/f/33/{size}?slow=500000&range_200_after=2&t=r200"
            )),
            "r.bin",
            None,
        )
        .await;
    h.wait_committed(id, 1).await;
    h.engine.control(id, pause()).await.unwrap();
    let committed = h.committed(id);

    h.engine.control(id, Control::Resume).await.unwrap();
    let event = h.sink.wait_terminal(id, WAIT).await;

    assert_eq!(failed_code(&event), DownloadErrorCode::RemoteChanged);
    let part = h.part_path(id);
    assert_eq!(
        std::fs::metadata(&part).unwrap().len(),
        committed,
        "the 200 body was never written at the resume offset"
    );
    verify::prefix_matches(&part, 33, committed).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn resume_rejected_when_etag_changed_between_runs() {
    let h = Harness::new().await;
    let size = 1_500_000u64;
    // HEAD, probe GET and the first transfer are requests 0..=2; the resume is the first
    // request served mutated (new ETag, new content).
    let url = h.url(&format!(
        "/f/34/{size}?slow=500000&etag=mutable&mutate_after=3&t=mut"
    ));
    let id = h.enqueue(&url, "m.bin", None).await;
    h.wait_committed(id, 1).await;
    h.engine.control(id, pause()).await.unwrap();

    h.engine.control(id, Control::Resume).await.unwrap();
    let event = h.sink.wait_terminal(id, WAIT).await;

    assert_eq!(failed_code(&event), DownloadErrorCode::RemoteChanged);
    assert_eq!(h.store.state(id), Some(DownloadState::Failed));

    let state = h
        .engine
        .control(
            id,
            Control::Retry {
                restart_from_zero: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(state, DownloadState::Queued);
    h.store
        .wait_state(id, |s| s == DownloadState::Completed, WAIT)
        .await;

    let row = h.store.row(id).unwrap();
    assert!(row.resets >= 1);
    let (path, _, _) = row.completion.unwrap();
    verify::file_matches(&path, 34 ^ MUTATED_SEED_XOR, size).unwrap();
    let log = h.log("mut");
    let last = *gets_with_range(&log).last().unwrap();
    assert_eq!(
        last.range.as_deref(),
        Some(format!("bytes=0-{}", size - 1).as_str()),
        "the retry starts from zero"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn n416_at_end_after_crash_is_complete() {
    let h = Harness::new().await;
    let (seed, size) = (35u64, 200_000u64);
    // A crash left every byte on disk and committed, but the download unsettled.
    let part = h.target().join(format!(
        "crash.bin.{}.forgedm.part",
        &Uuid::now_v7().simple().to_string()[24..]
    ));
    let file = create_part_file(&part).unwrap();
    let mut bytes = vec![0u8; usize::try_from(size).unwrap()];
    content::fill(seed, 0, &mut bytes);
    std::os::windows::fs::FileExt::seek_write(&file, &bytes, 0).unwrap();
    let id = Uuid::now_v7();
    h.store.seed(StoredJob {
        id,
        state: DownloadState::Interrupted,
        state_reason: Some("crash".to_owned()),
        url: validate_user_url(&h.url(&format!("/f/{seed}/{size}?t=end416"))).unwrap(),
        target_dir: h.target(),
        requested_name: Some("crash.bin".to_owned()),
        display_name: "crash.bin".to_owned(),
        file_name: Some("crash.bin".to_owned()),
        expected_sha256: None,
        part_path: Some(part.clone()),
        part_file_id: Some(encode_file_id(&file_id(&file).unwrap())),
        transfer_mode: Some(TransferMode::SingleRanged),
        max_connections: 4,
        progress: StoredProgress {
            total: Some(size),
            committed: size,
            etag: Some(format!("\"{seed}-{size}\"")),
            etag_strong: Some(true),
            last_modified: None,
            last_modified_strong: None,
        },
    });
    drop(file);

    assert_eq!(h.engine.recover().await.unwrap(), 1);
    let (path, sha) = completed(&h.sink.wait_terminal(id, WAIT).await);

    verify::file_matches(&path, seed, size).unwrap();
    assert_eq!(sha, expected_sha(seed, size));
    let log = h.log("end416");
    let ranged = gets_with_range(&log);
    assert_eq!(ranged.len(), 1, "{log:?}");
    assert_eq!(
        ranged[0].range.as_deref(),
        Some(format!("bytes={size}-{size}").as_str()),
        "the server is asked for the byte after the end and answers 416"
    );
    assert!(
        h.server.log().served_bytes() < 64,
        "no file byte was transferred, only the short 416 message: {}",
        h.server.log().served_bytes()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ignored_range_marks_nonresumable_single_stream() {
    let h = Harness::new().await;
    let size = 900_000u64;

    let id = h
        .enqueue(
            &h.url(&format!("/f/36/{size}?ranges=0&t=noranges")),
            "n.bin",
            None,
        )
        .await;
    let (path, _) = completed(&h.sink.wait_terminal(id, WAIT).await);

    verify::file_matches(&path, 36, size).unwrap();
    let row = h.store.row(id).unwrap();
    assert_eq!(
        row.probe,
        Some((TransferMode::SingleNonresumable, Some(size)))
    );
    let log = h.log("noranges");
    assert_eq!(
        log[1].range.as_deref(),
        Some("bytes=0-0"),
        "the probe asked for a range"
    );
    assert!(
        log[2..].iter().all(|e| e.range.is_none()),
        "the transfer is a plain GET: {log:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn non_range_server_sequential_no_gaps_overlap() {
    let h = Harness::new().await;
    let size = 3_000_003u64;

    let id = h
        .enqueue(
            &h.url(&format!("/f/37/{size}?ranges=0&cl=0&t=seq")),
            "s.bin",
            None,
        )
        .await;
    let (path, sha) = completed(&h.sink.wait_terminal(id, WAIT).await);

    // `verify::file_matches` fails on a gap, an overlap or a stray byte at any offset.
    verify::file_matches(&path, 37, size).unwrap();
    assert_eq!(sha, expected_sha(37, size));
    let log = h.log("seq");
    let transfers: Vec<_> = log
        .iter()
        .filter(|e| e.method == "GET" && e.range.is_none())
        .collect();
    assert_eq!(transfers.len(), 1, "one sequential stream: {log:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_nonresumable_requires_discard() {
    let h = Harness::new().await;
    let size = 1_200_000u64;
    let id = h
        .enqueue(
            &h.url(&format!("/f/38/{size}?ranges=0&slow=400000&t=nr")),
            "nr.bin",
            None,
        )
        .await;
    wait_accepted(&h, id).await;

    let refused = h.engine.control(id, pause()).await;
    assert!(
        matches!(refused, Err(EngineError::NotResumable)),
        "{refused:?}"
    );
    assert_eq!(
        h.store.state(id),
        Some(DownloadState::Downloading),
        "nothing changed"
    );

    let paused = h
        .engine
        .control(
            id,
            Control::Pause {
                discard_progress: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(paused, DownloadState::Paused);
    h.engine.control(id, Control::Resume).await.unwrap();
    let (path, _) = completed(&h.sink.wait_terminal(id, WAIT).await);

    verify::file_matches(&path, 38, size).unwrap();
    assert!(h.store.row(id).unwrap().resets >= 1, "restarted from zero");
}

async fn wait_accepted(h: &Harness, id: Uuid) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while h.engine.live(id).is_none_or(|p| p.accepted == 0) {
        assert!(tokio::time::Instant::now() < deadline, "no bytes accepted");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_resume_cancel_idempotent_two_clients_race() {
    let h = Harness::new().await;
    let size = 2_000_000u64;
    let id = h
        .enqueue(
            &h.url(&format!("/f/39/{size}?slow=500000&t=race")),
            "race.bin",
            None,
        )
        .await;
    h.wait_committed(id, 1).await;

    let (a, b) = tokio::join!(h.engine.control(id, pause()), h.engine.control(id, pause()));
    assert!(matches!(
        a.unwrap(),
        DownloadState::Paused | DownloadState::Pausing
    ));
    assert!(matches!(
        b.unwrap(),
        DownloadState::Paused | DownloadState::Pausing
    ));
    h.store
        .wait_state(id, |s| s == DownloadState::Paused, WAIT)
        .await;
    assert_eq!(
        h.engine.control(id, pause()).await.unwrap(),
        DownloadState::Paused,
        "a repeated pause is a no-op"
    );

    let (a, b) = tokio::join!(
        h.engine.control(id, Control::Resume),
        h.engine.control(id, Control::Resume)
    );
    for state in [a.unwrap(), b.unwrap()] {
        assert!(
            matches!(
                state,
                DownloadState::Queued | DownloadState::Probing | DownloadState::Downloading
            ),
            "{state:?}"
        );
    }

    let (a, b) = tokio::join!(
        h.engine.control(id, Control::Cancel),
        h.engine.control(id, Control::Cancel)
    );
    assert_eq!(a.unwrap(), DownloadState::Cancelled);
    assert_eq!(b.unwrap(), DownloadState::Cancelled);
    assert_eq!(
        h.engine.control(id, Control::Cancel).await.unwrap(),
        DownloadState::Cancelled,
        "cancelling again is a no-op"
    );
    assert_eq!(h.store.state(id), Some(DownloadState::Cancelled));
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_keeps_part_remove_deletes_only_part() {
    let h = Harness::new().await;
    let size = 2_000_000u64;
    let decoy = h.target().join("decoy.txt");
    std::fs::write(&decoy, b"not ours").unwrap();
    let id = h
        .enqueue(
            &h.url(&format!("/f/40/{size}?slow=500000&t=rm")),
            "rm.bin",
            None,
        )
        .await;
    h.wait_committed(id, 1).await;

    let state = h.engine.control(id, Control::Cancel).await.unwrap();

    assert_eq!(state, DownloadState::Cancelled);
    let part = h.part_path(id);
    assert!(part.is_file(), "cancel keeps the .part");

    h.engine.remove(id, true).await.unwrap();

    assert!(!part.exists(), "remove deleted the recorded .part");
    assert!(decoy.is_file(), "and nothing else");
    assert_eq!(h.store.state(id), None, "the row is gone");
    assert!(
        h.sink
            .events()
            .iter()
            .any(|e| matches!(e, EngineEvent::Removed { id: i } if *i == id))
    );
    let again = h.engine.remove(id, true).await;
    assert!(matches!(again, Err(EngineError::NotFound)), "{again:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn remove_refuses_replaced_part_file() {
    let h = Harness::new().await;
    let id = h
        .enqueue(&h.url("/f/41/2000000?slow=500000&t=swap"), "swap.bin", None)
        .await;
    h.wait_committed(id, 1).await;
    h.engine.control(id, Control::Cancel).await.unwrap();
    let part = h.part_path(id);
    // Someone swaps the staging file for another file under the same name.
    std::fs::remove_file(&part).unwrap();
    std::fs::write(&part, b"precious user data").unwrap();

    h.engine.remove(id, true).await.unwrap();

    assert_eq!(
        std::fs::read(&part).unwrap(),
        b"precious user data",
        "a file that is not the recorded one is never deleted"
    );
    assert_eq!(h.store.state(id), None, "the row itself is removed");
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_then_restart_requeues() {
    let h = Harness::new().await;
    let size = 2_000_000u64;
    let id = h
        .enqueue(
            &h.url(&format!("/f/42/{size}?slow=500000&t=shut")),
            "shut.bin",
            None,
        )
        .await;
    h.wait_committed(id, 1).await;

    h.engine.shutdown().await;

    assert_eq!(h.store.state(id), Some(DownloadState::Interrupted));
    assert_eq!(
        h.store.row(id).unwrap().job.state_reason.as_deref(),
        Some("agent_shutdown")
    );
    let committed = h.committed(id);
    assert!(committed > 0, "shutdown checkpointed what was received");

    let restarted = Harness::engine_over(&h.store, &h.sink);
    assert_eq!(
        restarted.recover().await.unwrap(),
        1,
        "auto-resume requeues it"
    );
    let (path, sha) = completed(&h.sink.wait_terminal(id, WAIT).await);

    verify::file_matches(&path, 42, size).unwrap();
    assert_eq!(sha, expected_sha(42, size));
    let log = h.log("shut");
    let ranged = gets_with_range(&log);
    assert_eq!(
        ranged.last().unwrap().range.as_deref(),
        Some(format!("bytes={committed}-{}", size - 1).as_str()),
        "the restarted agent resumes at the committed offset"
    );
    assert!(ranged.last().unwrap().if_range.is_some());
}
