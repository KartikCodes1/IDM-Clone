//! Resuming without a strong validator (weak-validator sampling), expiring signed links and
//! the re-resolve cap (PLAN S1.14), through the real engine and real HTTP.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

mod support;
#[path = "../src/testing.rs"]
#[allow(dead_code)] // each test file uses part of the shared fakes
mod testing;

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use forgedm_core::client::{HttpClient, ProxyMode};
use forgedm_core::engine::hosts::HostHandle;
use forgedm_core::engine::{self, Control, EngineConfig, EngineDeps, EngineHandle};
use forgedm_core::error::{TaskStop, stop_from_io, stop_from_platform};
use forgedm_core::ports::{EngineEvent, StoredJob, StoredProgress};
use forgedm_core::resume::SAMPLE_VERIFY_WINDOW;
use forgedm_core::transfer::encode_file_id;
use forgedm_core::transfer::resilience::{Resilience, RetryEnv, Verdict};
use forgedm_core::url_policy::validate_user_url;
use forgedm_fixture::{FixtureServer, LoggedRequest, content, verify};
use forgedm_platform::PlatformError;
use forgedm_platform::fs::{create_part_file, file_id};
use forgedm_platform::motw::MotwService;
use forgedm_protocol::{DownloadErrorCode, DownloadState, TransferMode};
use support::{Content, Reply, Req, Scripted};
use testing::{MemoryStore, RecordingSink};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(60);
const WINDOW: u64 = SAMPLE_VERIFY_WINDOW;

struct Rig {
    server: FixtureServer,
    store: Arc<MemoryStore>,
    sink: Arc<RecordingSink>,
    engine: EngineHandle,
    dir: tempfile::TempDir,
}

impl Rig {
    async fn new() -> Self {
        let store = Arc::new(MemoryStore::new());
        let sink = Arc::new(RecordingSink::new());
        let deps = EngineDeps {
            store: store.clone(),
            sink: sink.clone(),
            client: HttpClient::new(ProxyMode::None).unwrap(),
            motw: Arc::new(MotwService::start().unwrap()),
        };
        Self {
            server: FixtureServer::start_on(0).await.unwrap(),
            engine: engine::start(EngineConfig::default(), deps),
            store,
            sink,
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn target(&self) -> PathBuf {
        std::fs::canonicalize(self.dir.path()).unwrap()
    }

    fn log(&self, key: &str) -> Vec<LoggedRequest> {
        self.server.log().for_test(key)
    }

    /// Stages what a crash leaves: `committed` bytes of `disk_seed` content on disk and in
    /// the store, no validator, `interrupted`.
    fn stage_unvalidated(&self, disk_seed: u64, url: &str, size: u64, committed: u64) -> Uuid {
        self.stage(
            disk_seed,
            url,
            size,
            committed,
            None,
            TransferMode::SingleRanged,
        )
    }

    /// Like [`Self::stage_unvalidated`], with an optional strong `ETag` and a transfer mode.
    fn stage(
        &self,
        disk_seed: u64,
        url: &str,
        size: u64,
        committed: u64,
        etag: Option<String>,
        mode: TransferMode,
    ) -> Uuid {
        let part = self.target().join(format!(
            "w.bin.{}.forgedm.part",
            &Uuid::now_v7().simple().to_string()[24..]
        ));
        let file = create_part_file(&part).unwrap();
        let mut bytes = vec![0u8; usize::try_from(committed).unwrap()];
        content::fill(disk_seed, 0, &mut bytes);
        std::os::windows::fs::FileExt::seek_write(&file, &bytes, 0).unwrap();
        let id = Uuid::now_v7();
        self.store.seed(StoredJob {
            id,
            state: DownloadState::Interrupted,
            state_reason: Some("crash".to_owned()),
            url: validate_user_url(url).unwrap(),
            target_dir: self.target(),
            requested_name: Some("w.bin".to_owned()),
            display_name: "w.bin".to_owned(),
            file_name: Some("w.bin".to_owned()),
            expected_sha256: None,
            part_path: Some(part),
            part_file_id: Some(encode_file_id(&file_id(&file).unwrap())),
            transfer_mode: Some(mode),
            max_connections: 4,
            priority: forgedm_protocol::Priority::Normal,
            queue_position: 0,
            created_at_ms: 0,
            progress: StoredProgress {
                total: Some(size),
                committed,
                etag_strong: etag.as_ref().map(|_| true),
                etag,
                ..StoredProgress::default()
            },
        });
        id
    }

    fn part_of(&self, id: Uuid) -> PathBuf {
        self.store.row(id).unwrap().job.part_path.unwrap()
    }
}

fn ranges(log: &[LoggedRequest]) -> Vec<&LoggedRequest> {
    log.iter().filter(|e| e.method == "GET").collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn sampling_detects_change_without_strong_validator() {
    let rig = Rig::new().await;
    let size = 400_000u64;
    let committed = 2 * WINDOW;
    // The disk holds seed 7; the server now serves seed 8 and offers no validator at all.
    let url = format!("{}/f/8/{size}?etag=none&t=chg", rig.server.base_url());
    let id = rig.stage_unvalidated(7, &url, size, committed);

    assert_eq!(rig.engine.recover().await.unwrap(), 1);
    let event = rig.sink.wait_terminal(id, WAIT).await;

    let EngineEvent::Failed { error, .. } = event else {
        panic!("expected a failure, got {event:?}");
    };
    assert_eq!(error.code, DownloadErrorCode::RemoteChanged);
    let log = rig.log("chg");
    let gets = ranges(&log);
    assert_eq!(gets.len(), 1, "only the sample was requested: {log:?}");
    assert_eq!(
        gets[0].range.as_deref(),
        Some(format!("bytes={}-{}", committed - WINDOW, committed - 1).as_str())
    );
    assert_eq!(gets[0].if_range, None, "nothing strong to send");
    assert!(
        rig.server.log().served_bytes() <= WINDOW,
        "no more than the sample window was transferred"
    );
    let part = rig.store.row(id).unwrap().job.part_path.unwrap();
    assert_eq!(std::fs::metadata(part).unwrap().len(), committed);
}

#[tokio::test(flavor = "multi_thread")]
async fn sampling_passes_for_an_unchanged_file_and_continues() {
    let rig = Rig::new().await;
    let (seed, size) = (9u64, 400_000u64);
    let committed = 2 * WINDOW;
    let url = format!("{}/f/{seed}/{size}?etag=none&t=same", rig.server.base_url());
    let id = rig.stage_unvalidated(seed, &url, size, committed);

    assert_eq!(rig.engine.recover().await.unwrap(), 1);
    let event = rig.sink.wait_terminal(id, WAIT).await;

    let EngineEvent::Completed { final_path, .. } = event else {
        panic!("expected completion, got {event:?}");
    };
    verify::file_matches(&final_path, seed, size).unwrap();
    let log = rig.log("same");
    let gets = ranges(&log);
    assert_eq!(gets.len(), 2, "{log:?}");
    assert_eq!(
        gets[0].range.as_deref(),
        Some(format!("bytes={}-{}", committed - WINDOW, committed - 1).as_str())
    );
    assert_eq!(
        gets[1].range.as_deref(),
        Some(format!("bytes={committed}-{}", size - 1).as_str())
    );
    assert!(gets.iter().all(|g| g.if_range.is_none()));
}

#[tokio::test(flavor = "multi_thread")]
async fn less_than_a_window_committed_without_validator_restarts_from_zero() {
    let rig = Rig::new().await;
    let (seed, size) = (10u64, 300_000u64);
    let url = format!(
        "{}/f/{seed}/{size}?etag=none&t=small",
        rig.server.base_url()
    );
    let id = rig.stage_unvalidated(seed, &url, size, WINDOW - 1);

    assert_eq!(rig.engine.recover().await.unwrap(), 1);
    let event = rig.sink.wait_terminal(id, WAIT).await;

    let EngineEvent::Completed { final_path, .. } = event else {
        panic!("expected completion, got {event:?}");
    };
    verify::file_matches(&final_path, seed, size).unwrap();
    assert!(rig.store.row(id).unwrap().resets >= 1);
    let log = rig.log("small");
    let last = *ranges(&log).last().unwrap();
    assert_eq!(
        last.range.as_deref(),
        Some(format!("bytes=0-{}", size - 1).as_str())
    );
}

/// Stages a segmented download without a validator: two segments of a 400 000-byte file,
/// the first with `first` committed bytes and the second with `second` beyond its start.
async fn stage_weak_segments(
    rig: &Rig,
    disk_seed: u64,
    url: &str,
    first: u64,
    second: u64,
) -> Uuid {
    use forgedm_core::ports::DownloadStore;
    use forgedm_core::transfer::segment_table::SegmentSnapshot;
    let size = 400_000u64;
    let on_disk = 200_000 + second;
    let id = rig.stage(disk_seed, url, size, on_disk, None, TransferMode::Segmented);
    let snapshots = vec![
        SegmentSnapshot {
            seg_no: 0,
            start: 0,
            end: 200_000,
            committed: first,
        },
        SegmentSnapshot {
            seg_no: 1,
            start: 200_000,
            end: size,
            committed: on_disk,
        },
    ];
    rig.store.checkpoint_segments(id, snapshots).await.unwrap();
    id
}

#[tokio::test(flavor = "multi_thread")]
async fn segmented_without_strong_validator_samples_the_fullest_segment() {
    let rig = Rig::new().await;
    let (seed, size) = (11u64, 400_000u64);
    let url = format!(
        "{}/f/{seed}/{size}?etag=none&t=segsame",
        rig.server.base_url()
    );
    let id = stage_weak_segments(&rig, seed, &url, 100_000, 10_000).await;

    assert_eq!(rig.engine.recover().await.unwrap(), 1);
    let event = rig.sink.wait_terminal(id, WAIT).await;

    let EngineEvent::Completed { final_path, .. } = event else {
        panic!("expected completion, got {event:?}");
    };
    verify::file_matches(&final_path, seed, size).unwrap();
    let log = rig.log("segsame");
    let first = *ranges(&log).first().unwrap();
    assert_eq!(
        first.range.as_deref(),
        Some(format!("bytes={}-{}", 100_000 - WINDOW, 99_999).as_str()),
        "the tail of the segment with the most committed data is sampled: {log:?}"
    );
    assert!(first.if_range.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn segmented_sampling_detects_a_changed_file() {
    let rig = Rig::new().await;
    let size = 400_000u64;
    let url = format!("{}/f/13/{size}?etag=none&t=segchg", rig.server.base_url());
    let id = stage_weak_segments(&rig, 12, &url, 100_000, 10_000).await;

    assert_eq!(rig.engine.recover().await.unwrap(), 1);
    let event = rig.sink.wait_terminal(id, WAIT).await;

    let EngineEvent::Failed { error, .. } = event else {
        panic!("expected a failure, got {event:?}");
    };
    assert_eq!(error.code, DownloadErrorCode::RemoteChanged);
    assert_eq!(ranges(&rig.log("segchg")).len(), 1, "only the sample ran");
}

#[tokio::test(flavor = "multi_thread")]
async fn segmented_without_a_full_window_in_any_segment_restarts_from_zero() {
    let rig = Rig::new().await;
    let (seed, size) = (14u64, 400_000u64);
    let url = format!(
        "{}/f/{seed}/{size}?etag=none&t=segsmall",
        rig.server.base_url()
    );
    // 40 000 + 40 000 committed: 80 000 in total, but no single segment has a full window.
    let id = stage_weak_segments(&rig, seed, &url, 40_000, 40_000).await;

    assert_eq!(rig.engine.recover().await.unwrap(), 1);
    let event = rig.sink.wait_terminal(id, WAIT).await;

    let EngineEvent::Completed { final_path, .. } = event else {
        panic!("expected completion, got {event:?}");
    };
    verify::file_matches(&final_path, seed, size).unwrap();
    assert!(rig.store.row(id).unwrap().resets >= 1);
}

/// Serves `seed`/`total` content behind a redirect whose signed target expires after one
/// transfer request and is then replaced by a fresh one, like a CDN token. The first
/// transfer body is cut short so that a second transfer request is needed.
struct Signed {
    generation: AtomicUsize,
    transfers_on_current: AtomicUsize,
    first_cut: AtomicUsize,
}

fn signed_server_handler(seed: u64, total: u64) -> impl Fn(&Req) -> Reply + Send + Sync {
    let state = Signed {
        generation: AtomicUsize::new(1),
        transfers_on_current: AtomicUsize::new(0),
        first_cut: AtomicUsize::new(0),
    };
    move |req: &Req| {
        let path = req.target.split('?').next().unwrap_or_default();
        let current = state.generation.load(Ordering::SeqCst);
        if path == "/origin" {
            return Reply::Status(302, vec![("Location", format!("/signed/{current}"))]);
        }
        let generation: usize = path
            .strip_prefix("/signed/")
            .and_then(|g| g.parse().ok())
            .unwrap_or(0);
        if generation != current {
            return Reply::Status(403, vec![]);
        }
        if req.is_transfer() {
            if state.transfers_on_current.fetch_add(1, Ordering::SeqCst) >= 1 {
                state.generation.fetch_add(1, Ordering::SeqCst);
                state.transfers_on_current.store(0, Ordering::SeqCst);
                return Reply::Status(403, vec![]);
            }
            if state.first_cut.fetch_add(1, Ordering::SeqCst) == 0 {
                let mut cut = Content::new(seed, total);
                cut.drop_after = Some(total / 3);
                return Reply::Content(cut);
            }
        }
        Reply::Content(Content::new(seed, total))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn signed_expiring_reresolved_once() {
    let (seed, size) = (21u64, 600_000u64);
    let server = Scripted::start(signed_server_handler(seed, size)).await;
    let h = support::Harness::new();

    let (_, event) = h.run(&server.url("/origin"), "signed.bin", 1).await;

    let (path, sha) = support::Harness::completed(&event);
    assert_eq!(sha, support::expected_sha(seed, size));
    verify::file_matches(&path, seed, size).unwrap();
    let log = server.requests();
    let expired: Vec<_> = log
        .iter()
        .filter(|r| r.is_transfer() && r.target.starts_with("/signed/"))
        .collect();
    let first_gen_transfers = expired.iter().filter(|r| r.target == "/signed/1").count();
    assert_eq!(
        first_gen_transfers, 2,
        "one served, one refused: {expired:?}"
    );
    let second_gen = expired.iter().filter(|r| r.target == "/signed/2").count();
    assert!(second_gen >= 1, "the download continued on the new link");
    // After the 403 the next request went back to the origin, never to the dead link.
    let refused = log
        .iter()
        .position(|r| r.target == "/signed/1" && r.is_transfer() && r.index > 0)
        .and_then(|first| {
            log.iter()
                .skip(first + 1)
                .position(|r| r.target == "/signed/1" && r.is_transfer())
                .map(|p| p + first + 1)
        })
        .expect("a second transfer on the first link");
    assert_eq!(
        log[refused + 1].target,
        "/origin",
        "the refused request is followed by a re-resolve from the original URL"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_refusal_after_the_reresolve_needs_a_url_refresh() {
    let server = Scripted::start(|req: &Req| {
        let path = req.target.split('?').next().unwrap_or_default();
        if path == "/origin" {
            return Reply::Status(302, vec![("Location", "/dead".to_owned())]);
        }
        if req.is_transfer() {
            return Reply::Status(403, vec![]);
        }
        Reply::Content(Content::new(5, 300_000))
    })
    .await;
    let h = support::Harness::new();

    let id = h.enqueue(&server.url("/origin"), "dead.bin", 1).await;
    h.store
        .wait_state(id, |s| s == DownloadState::NeedsUrlRefresh, WAIT)
        .await;

    let origin_after_probe = server
        .requests()
        .iter()
        .filter(|r| r.is_transfer() && r.target == "/dead")
        .count();
    assert_eq!(origin_after_probe, 2, "the refusal and the one re-resolve");
}

#[tokio::test(start_paused = true)]
async fn reresolve_capped_3_per_hour() {
    let env = RetryEnv::default();
    let res = Resilience::new(&env, Arc::new(HostHandle::new(8)));
    let cancel = CancellationToken::new();
    let refused = forgedm_core::error::Fault::Status {
        status: 403,
        retry_after: None,
    };

    let mut verdicts = Vec::new();
    for _ in 0..4 {
        verdicts.push(
            res.on_fault(forgedm_core::retry::Phase::Transfer, 0, refused, &cancel)
                .await,
        );
        // Progress between refusals lets the same scope re-resolve again.
        res.on_progress(0, 1);
        tokio::time::advance(Duration::from_secs(60)).await;
    }

    assert_eq!(
        verdicts[..3],
        [Verdict::Retry, Verdict::Retry, Verdict::Retry]
    );
    assert_eq!(
        verdicts[3],
        Verdict::Stop(TaskStop::NeedsUrlRefresh),
        "the 4th re-resolve within the hour makes no request"
    );
    // An hour after the first one, a slot is free again.
    tokio::time::advance(Duration::from_secs(3600)).await;
    let again = res
        .on_fault(forgedm_core::retry::Phase::Transfer, 0, refused, &cancel)
        .await;
    assert_eq!(again, Verdict::Retry);
}

/// Free space a test can change.
struct Space(AtomicU64);

impl forgedm_core::engine::budgets::FreeSpace for Space {
    fn free(&self, _dir: &std::path::Path) -> Option<u64> {
        Some(self.0.load(Ordering::SeqCst))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn disk_full_injected_pauses_then_resumes() {
    let space = Arc::new(Space(AtomicU64::new(u64::MAX / 2)));
    let h = support::Harness::with_free_space(EngineConfig::default(), space.clone());
    let (seed, size) = (31u64, 3 * support::MIB);
    let server = FixtureServer::start_on(0).await.unwrap();
    let url = format!("{}/f/{seed}/{size}?slow=600000", server.base_url());
    let id = h.enqueue(&url, "full.bin", 1).await;
    h.store
        .wait_state(id, |s| s == DownloadState::Downloading, WAIT)
        .await;
    let control = Control::Pause {
        discard_progress: false,
    };
    h.engine.control(id, control).await.unwrap();
    let committed = h.store.row(id).unwrap().checkpoints.last().unwrap().0;
    assert!(committed > 0 && committed < size);

    // The volume fills up while the download is paused: resuming is refused, not failed.
    space.0.store(1024, Ordering::SeqCst);
    h.engine.control(id, Control::Resume).await.unwrap();
    h.store
        .wait_state(id, |s| s == DownloadState::Paused, WAIT)
        .await;
    assert_eq!(
        h.store.row(id).unwrap().job.state_reason.as_deref(),
        Some("disk_full")
    );
    assert_eq!(
        h.store.row(id).unwrap().checkpoints.last().unwrap().0,
        committed
    );

    space.0.store(u64::MAX / 2, Ordering::SeqCst);
    h.engine.control(id, Control::Resume).await.unwrap();
    let event = h.sink.wait_terminal(id, WAIT).await;
    let (path, sha) = support::Harness::completed(&event);
    assert_eq!(sha, support::expected_sha(seed, size));
    verify::file_matches(&path, seed, size).unwrap();
}

#[test]
fn device_gone_injected_pauses() {
    // ERROR_NOT_READY (21) and ERROR_DEV_NOT_EXIST (55) are what a pulled drive reports.
    for code in [21, 55, 67, 1167] {
        let stop = stop_from_io(&io::Error::from_raw_os_error(code));
        assert_eq!(
            stop,
            TaskStop::PauseFor(DownloadErrorCode::DestinationUnavailable),
            "os error {code}"
        );
    }
    assert_eq!(
        stop_from_platform(&PlatformError::DestinationUnavailable),
        TaskStop::PauseFor(DownloadErrorCode::DestinationUnavailable)
    );
    // The disk-full side of the same seam.
    assert_eq!(
        stop_from_io(&io::Error::from_raw_os_error(112)),
        TaskStop::PauseFor(DownloadErrorCode::DiskFull)
    );
}

fn strong_etag(seed: u64, size: u64) -> Option<String> {
    Some(format!("\"{seed}-{size}\""))
}

/// Runs the staged download to its end and checks the restart was recorded with `code` and
/// the transfer began at byte 0.
async fn assert_restarts_and_completes(
    rig: &Rig,
    id: Uuid,
    key: &str,
    seed: u64,
    size: u64,
    code: &str,
) {
    assert_eq!(rig.engine.recover().await.unwrap(), 1);
    let event = rig.sink.wait_terminal(id, WAIT).await;
    let EngineEvent::Completed {
        final_path, sha256, ..
    } = event
    else {
        panic!("expected completion, got {event:?}");
    };
    verify::file_matches(&final_path, seed, size).unwrap();
    assert_eq!(sha256, hex::encode(content::sha256(seed, size)));
    let row = rig.store.row(id).unwrap();
    assert!(row.resets >= 1, "progress was reset");
    assert!(
        row.events
            .iter()
            .any(|e| e.kind == "restart_from_zero" && e.code.as_deref() == Some(code)),
        "the restart is recorded as {code}: {:?}",
        row.events
    );
    let log = rig.log(key);
    let first = *ranges(&log).first().unwrap();
    assert!(
        first
            .range
            .as_deref()
            .is_some_and(|r| r.starts_with("bytes=0-")),
        "the first request starts from zero: {first:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_part_restarts() {
    let rig = Rig::new().await;
    let (seed, size) = (41u64, 300_000u64);
    let url = format!("{}/f/{seed}/{size}?t=missing", rig.server.base_url());
    let id = rig.stage(
        seed,
        &url,
        size,
        100_000,
        strong_etag(seed, size),
        TransferMode::SingleRanged,
    );
    std::fs::remove_file(rig.part_of(id)).unwrap();

    assert_restarts_and_completes(&rig, id, "missing", seed, size, "part_file_invalid").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn replaced_or_truncated_part_restarts() {
    let (seed, size) = (42u64, 300_000u64);
    // Truncated: the file is ours but shorter than what the database says was committed.
    let rig = Rig::new().await;
    let url = format!("{}/f/{seed}/{size}?t=short", rig.server.base_url());
    let id = rig.stage(
        seed,
        &url,
        size,
        100_000,
        strong_etag(seed, size),
        TransferMode::SingleRanged,
    );
    let part = rig.part_of(id);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&part)
        .unwrap()
        .set_len(50_000)
        .unwrap();
    assert_restarts_and_completes(&rig, id, "short", seed, size, "part_file_invalid").await;

    // Replaced: another file now sits at the recorded path. It is never deleted or adopted.
    let rig = Rig::new().await;
    let url = format!("{}/f/{seed}/{size}?t=swap", rig.server.base_url());
    let id = rig.stage(
        seed,
        &url,
        size,
        100_000,
        strong_etag(seed, size),
        TransferMode::SingleRanged,
    );
    let part = rig.part_of(id);
    std::fs::remove_file(&part).unwrap();
    let foreign = vec![0xAB_u8; 150_000];
    std::fs::write(&part, &foreign).unwrap();
    assert_restarts_and_completes(&rig, id, "swap", seed, size, "part_file_invalid").await;
    assert_eq!(
        std::fs::read(&part).unwrap(),
        foreign,
        "a file that is not provably ours is left alone"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn corrupt_segment_rows_safe_restart() {
    use forgedm_core::ports::DownloadStore;
    use forgedm_core::transfer::segment_table::SegmentSnapshot;
    let rig = Rig::new().await;
    let (seed, size) = (43u64, 400_000u64);
    let url = format!("{}/f/{seed}/{size}?t=corrupt", rig.server.base_url());
    let id = rig.stage(
        seed,
        &url,
        size,
        100_000,
        strong_etag(seed, size),
        TransferMode::Segmented,
    );
    let overlapping = vec![
        SegmentSnapshot {
            seg_no: 0,
            start: 0,
            end: 250_000,
            committed: 100_000,
        },
        SegmentSnapshot {
            seg_no: 1,
            start: 200_000,
            end: size,
            committed: 200_000,
        },
    ];
    rig.store
        .checkpoint_segments(id, overlapping)
        .await
        .unwrap();

    assert_restarts_and_completes(&rig, id, "corrupt", seed, size, "metadata_corrupt").await;
}
