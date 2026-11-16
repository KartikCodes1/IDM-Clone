//! Admission, connection budgets, priorities and the batch commands (PLAN S1.13), through the
//! real engine against the fixture server. Only persistence is the in-memory fake.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

mod support;
#[path = "../src/testing.rs"]
#[allow(dead_code)] // each test file uses part of the shared fakes
mod testing;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use forgedm_core::engine::budgets::FreeSpace;
use forgedm_core::engine::{Control, EngineConfig};
use forgedm_core::error::EngineError;
use forgedm_core::ports::{DownloadStore, EngineEvent, StoredJob, StoredProgress, unix_ms};
use forgedm_core::url_policy::validate_user_url;
use forgedm_fixture::FixtureServer;
use forgedm_protocol::{DownloadState, Priority};
use support::{Harness, MIB, WAIT, expected_sha};
use uuid::Uuid;

const KIB: u64 = 1024;
const POLL: Duration = Duration::from_millis(10);

/// A download of `size` bytes whose body the fixture delivers at about `bps` per connection.
fn slow(server: &FixtureServer, seed: u64, size: u64, bps: u64, extra: &str) -> String {
    format!(
        "{}/f/{seed}/{size}?t=s{seed}&slow={bps}{extra}",
        server.base_url()
    )
}

fn fast(server: &FixtureServer, seed: u64, size: u64) -> String {
    format!("{}/f/{seed}/{size}?t=f{seed}", server.base_url())
}

/// The ids of every `Completed` event so far, in the order they happened.
fn completion_order(h: &Harness) -> Vec<Uuid> {
    h.sink
        .events()
        .into_iter()
        .filter_map(|event| match event {
            EngineEvent::Completed { id, .. } => Some(id),
            _ => None,
        })
        .collect()
}

async fn wait_all(h: &Harness, ids: &[Uuid]) {
    for id in ids {
        let event = h.sink.wait_terminal(*id, WAIT).await;
        assert!(
            matches!(event, EngineEvent::Completed { .. }),
            "expected a completion, got {event:?}"
        );
    }
}

fn active(state: DownloadState) -> bool {
    matches!(
        state,
        DownloadState::Probing | DownloadState::Downloading | DownloadState::Pausing
    )
}

fn single_slot() -> EngineConfig {
    EngineConfig {
        max_active: 1,
        ..EngineConfig::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn max_three_active() {
    let server = FixtureServer::start_on(0).await.unwrap();
    let h = Harness::new();
    let mut ids = Vec::new();
    for seed in 1..=5 {
        let url = slow(&server, seed, MIB, 512 * KIB, "");
        ids.push(h.enqueue(&url, &format!("a{seed}.bin"), 2).await);
    }

    let mut peak = 0;
    while !ids.iter().all(|id| {
        matches!(
            h.store.state(*id),
            Some(DownloadState::Completed | DownloadState::Failed)
        )
    }) {
        let running = ids
            .iter()
            .filter(|id| h.store.state(**id).is_some_and(active))
            .count();
        assert!(running <= 3, "{running} downloads were active at once");
        assert!(h.engine.running() <= 3, "the engine ran more than three");
        peak = peak.max(running);
        tokio::time::sleep(POLL).await;
    }

    assert_eq!(peak, 3, "three should have run together");
    wait_all(&h, &ids).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn priority_then_fifo_admission() {
    let server = FixtureServer::start_on(0).await.unwrap();
    let h = Harness::configured(single_slot());
    let running = h
        .enqueue(&slow(&server, 1, MIB, 512 * KIB, ""), "run.bin", 1)
        .await;
    h.store
        .wait_state(running, |s| s == DownloadState::Downloading, WAIT)
        .await;
    let low = h
        .enqueue_with(&fast(&server, 2, 64 * KIB), "low.bin", 1, Priority::Low)
        .await;
    let n1 = h
        .enqueue_with(&fast(&server, 3, 64 * KIB), "n1.bin", 1, Priority::Normal)
        .await;
    let high = h
        .enqueue_with(&fast(&server, 4, 64 * KIB), "high.bin", 1, Priority::High)
        .await;
    let n2 = h
        .enqueue_with(&fast(&server, 5, 64 * KIB), "n2.bin", 1, Priority::Normal)
        .await;

    wait_all(&h, &[low, n1, high, n2]).await;

    assert_eq!(completion_order(&h), [running, high, n1, n2, low]);
}

#[tokio::test(flavor = "multi_thread")]
async fn update_priority_reorders() {
    let server = FixtureServer::start_on(0).await.unwrap();
    let h = Harness::configured(single_slot());
    let running = h
        .enqueue(&slow(&server, 1, MIB, 512 * KIB, ""), "run.bin", 1)
        .await;
    h.store
        .wait_state(running, |s| s == DownloadState::Downloading, WAIT)
        .await;
    let b = h.enqueue(&fast(&server, 2, 64 * KIB), "b.bin", 1).await;
    let c = h.enqueue(&fast(&server, 3, 64 * KIB), "c.bin", 1).await;

    h.engine.set_priority(c, Priority::High).await.unwrap();

    assert_eq!(h.store.row(c).unwrap().job.priority, Priority::High);
    wait_all(&h, &[b, c]).await;
    assert_eq!(completion_order(&h), [running, c, b]);
    let missing = h.engine.set_priority(Uuid::now_v7(), Priority::Low).await;
    assert!(matches!(missing, Err(EngineError::NotFound)));
}

#[tokio::test(flavor = "multi_thread")]
async fn per_host_budget_no_503_with_max_conns_8() {
    let server = FixtureServer::start_on(0).await.unwrap();
    let h = Harness::new();
    let size = 24 * MIB;
    let first = slow(&server, 11, size, MIB, "&max_conns=8");
    let second = slow(&server, 12, size, MIB, "&max_conns=8");

    let a = h.enqueue(&first, "a.bin", 8).await;
    let b = h.enqueue(&second, "b.bin", 8).await;
    wait_all(&h, &[a, b]).await;

    let seen = server.log().max_concurrency();
    assert!(seen <= 8, "{seen} requests were in flight on one host");
    assert!(seen >= 5, "the host budget was never used ({seen})");
    // A refused request would have been retried through the sleeper.
    assert!(
        h.sleeper.sleeps().is_empty(),
        "a request was refused and retried: {:?}",
        h.sleeper.sleeps()
    );
    let (_, sha) = Harness::completed(
        &h.sink
            .events()
            .into_iter()
            .find(|e| matches!(e, EngineEvent::Completed { id, .. } if *id == a))
            .unwrap(),
    );
    assert_eq!(sha, expected_sha(11, size));
}

#[tokio::test(flavor = "multi_thread")]
async fn global_budget() {
    let first = FixtureServer::start_on(0).await.unwrap();
    let second = FixtureServer::start_on(0).await.unwrap();
    let config = EngineConfig {
        global_connections: 4,
        ..EngineConfig::default()
    };
    let h = Harness::configured(config);
    let size = 16 * MIB;
    let a = h
        .enqueue(&slow(&first, 21, size, MIB, ""), "ga.bin", 8)
        .await;
    let b = h
        .enqueue(&slow(&second, 22, size, MIB, ""), "gb.bin", 8)
        .await;

    let mut peak = 0;
    while ![a, b]
        .iter()
        .all(|id| h.store.state(*id) == Some(DownloadState::Completed))
    {
        let now = first.log().current_concurrency() + second.log().current_concurrency();
        peak = peak.max(now);
        assert!(now <= 4, "{now} connections were open with a budget of 4");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    assert!(peak >= 3, "the global budget was never used ({peak})");
    wait_all(&h, &[a, b]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn small_download_not_starved() {
    let server = FixtureServer::start_on(0).await.unwrap();
    let config = EngineConfig {
        host_connections: 2,
        ..EngineConfig::default()
    };
    let h = Harness::configured(config);
    let big = h
        .enqueue(&slow(&server, 31, 12 * MIB, MIB, ""), "big.bin", 8)
        .await;
    h.store
        .wait_state(big, |s| s == DownloadState::Downloading, WAIT)
        .await;
    tokio::time::sleep(Duration::from_millis(600)).await;

    let started = tokio::time::Instant::now();
    let small = h
        .enqueue(&fast(&server, 32, 256 * KIB), "small.bin", 8)
        .await;
    let event = h.sink.wait_terminal(small, WAIT).await;

    assert!(matches!(event, EngineEvent::Completed { .. }));
    assert_eq!(
        h.store.state(big),
        Some(DownloadState::Downloading),
        "the small download had to finish while the big one still ran"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the small download waited {:?}",
        started.elapsed()
    );
    wait_all(&h, &[big]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_all_resume_all() {
    let server = FixtureServer::start_on(0).await.unwrap();
    let config = EngineConfig {
        max_active: 4,
        ..EngineConfig::default()
    };
    let h = Harness::configured(config);
    let r1 = h
        .enqueue(&slow(&server, 41, MIB, 256 * KIB, ""), "r1.bin", 1)
        .await;
    let r2 = h
        .enqueue(&slow(&server, 42, MIB, 256 * KIB, ""), "r2.bin", 1)
        .await;
    let plain = h
        .enqueue(&slow(&server, 43, MIB, 256 * KIB, "&ranges=0"), "p.bin", 1)
        .await;
    let r3 = h
        .enqueue(&slow(&server, 44, MIB, 256 * KIB, ""), "r3.bin", 1)
        .await;
    let queued = h
        .enqueue(&slow(&server, 45, 128 * KIB, 256 * KIB, ""), "q.bin", 1)
        .await;
    for id in [r1, r2, plain, r3] {
        h.store
            .wait_state(id, |s| s == DownloadState::Downloading, WAIT)
            .await;
    }
    assert_eq!(h.store.state(queued), Some(DownloadState::Queued));

    let outcome = h.engine.pause_all(false).await.unwrap();

    let mut paused = outcome.paused.clone();
    paused.sort();
    let mut expected = vec![r1, r2, r3, queued];
    expected.sort();
    assert_eq!(paused, expected);
    assert_eq!(outcome.skipped_not_resumable, [plain]);
    for id in &expected {
        assert_eq!(h.store.state(*id), Some(DownloadState::Paused));
    }
    assert!(
        h.store.state(plain).is_some_and(active),
        "plain keeps going"
    );

    let mut resumed = h.engine.resume_all().await.unwrap();
    resumed.sort();
    assert_eq!(resumed, expected);
    let mut everything = expected.clone();
    everything.push(plain);
    wait_all(&h, &everything).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_all_with_discard_progress_includes_non_resumable() {
    let server = FixtureServer::start_on(0).await.unwrap();
    let h = Harness::new();
    let plain = h
        .enqueue(&slow(&server, 51, MIB, 256 * KIB, "&ranges=0"), "p.bin", 1)
        .await;
    h.store
        .wait_state(plain, |s| s == DownloadState::Downloading, WAIT)
        .await;

    let outcome = h.engine.pause_all(true).await.unwrap();

    assert_eq!(outcome.paused, [plain]);
    assert!(outcome.skipped_not_resumable.is_empty());
    assert_eq!(h.store.state(plain), Some(DownloadState::Paused));
    assert_eq!(h.engine.resume_all().await.unwrap(), [plain]);
    wait_all(&h, &[plain]).await;
}

fn retry_waiting_job(h: &Harness, server: &FixtureServer, priority: Priority) -> StoredJob {
    StoredJob {
        id: Uuid::now_v7(),
        state: DownloadState::RetryWaiting,
        state_reason: None,
        url: validate_user_url(&fast(server, 61, 64 * KIB)).unwrap(),
        target_dir: h.target(),
        requested_name: Some("due.bin".to_owned()),
        display_name: "due.bin".to_owned(),
        file_name: None,
        expected_sha256: None,
        part_path: None,
        part_file_id: None,
        transfer_mode: None,
        progress: StoredProgress::default(),
        max_connections: 2,
        priority,
        queue_position: unix_ms() + 1_000_000,
        created_at_ms: unix_ms() + 1_000_000,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn due_retry_requeued_before_admission() {
    let server = FixtureServer::start_on(0).await.unwrap();
    let h = Harness::configured(single_slot());
    let running = h
        .enqueue(&slow(&server, 1, MIB, 512 * KIB, ""), "run.bin", 1)
        .await;
    h.store
        .wait_state(running, |s| s == DownloadState::Downloading, WAIT)
        .await;
    let low = h
        .enqueue_with(&fast(&server, 2, 64 * KIB), "low.bin", 1, Priority::Low)
        .await;
    let normal = h.enqueue(&fast(&server, 3, 64 * KIB), "n.bin", 1).await;
    let due = retry_waiting_job(&h, &server, Priority::High);
    let due_id = due.id;
    h.store.seed(due);
    h.store.schedule_retry(due_id, unix_ms() - 1).await.unwrap();

    h.engine.recover().await.unwrap();

    // The timer edge moves it to `queued` while the only slot is still taken.
    h.store
        .wait_state(due_id, |s| s == DownloadState::Queued, WAIT)
        .await;
    assert_eq!(h.store.state(running), Some(DownloadState::Downloading));
    assert!(
        h.store
            .row(due_id)
            .unwrap()
            .events
            .iter()
            .any(|e| e.kind == "retry_due"),
        "the requeue goes through the A.6 timer edge"
    );
    wait_all(&h, &[low, normal, due_id]).await;
    assert_eq!(completion_order(&h), [running, due_id, normal, low]);
}

/// Free space that tests can change.
struct Injected(AtomicU64);

impl FreeSpace for Injected {
    fn free(&self, _dir: &Path) -> Option<u64> {
        Some(self.0.load(Ordering::SeqCst))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn free_space_precheck_disk_full() {
    let server = FixtureServer::start_on(0).await.unwrap();
    let space = Arc::new(Injected(AtomicU64::new(MIB)));
    let h = Harness::with_free_space(EngineConfig::default(), space.clone());
    let size = 2 * MIB;

    let id = h.enqueue(&fast(&server, 71, size), "full.bin", 2).await;

    h.store
        .wait_state(id, |s| s == DownloadState::Paused, WAIT)
        .await;
    let row = h.store.row(id).unwrap();
    assert_eq!(row.job.state_reason.as_deref(), Some("disk_full"));
    assert!(
        h.sink
            .events()
            .iter()
            .all(|e| !matches!(e, EngineEvent::Failed { .. })),
        "a full disk pauses, it does not fail"
    );

    space.0.store(10 * 1024 * MIB, Ordering::SeqCst);
    let state = h.engine.control(id, Control::Resume).await.unwrap();
    assert_eq!(state, DownloadState::Queued);
    let event = h.sink.wait_terminal(id, WAIT).await;
    let (_, sha) = Harness::completed(&event);
    assert_eq!(sha, expected_sha(71, size));
}
