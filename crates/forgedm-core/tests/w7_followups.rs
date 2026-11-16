//! Regression tests for the W7 follow-ups carried by S1.12: completion must not wait for
//! connections still in their stagger, and a per-download connection cap set at runtime must
//! survive an agent restart.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

mod support;
#[path = "../src/testing.rs"]
#[allow(dead_code)] // each test file uses part of the shared fakes
mod testing;

use std::sync::Arc;
use std::time::Duration;

use forgedm_core::client::{HttpClient, ProxyMode};
use forgedm_core::engine::{self, EngineConfig, EngineDeps, EngineHandle};
use forgedm_core::ports::{DownloadStore, EngineEvent, unix_ms};
use forgedm_fixture::FixtureServer;
use forgedm_platform::motw::MotwService;
use forgedm_protocol::DownloadState;
use support::{Harness, MIB, WAIT, expected_sha};
use testing::{MemoryStore, RecordingSink};

#[tokio::test(flavor = "multi_thread")]
async fn completion_not_held_by_connection_stagger() {
    let server = FixtureServer::start_on(0).await.unwrap();
    let h = Harness::new();
    let size = 32 * MIB;
    let url = format!("{}/f/21/{size}", server.base_url());

    let started = tokio::time::Instant::now();
    let (_, event) = h.run(&url, "stagger.bin", 16).await;
    let elapsed = started.elapsed();

    let (_, sha) = Harness::completed(&event);
    assert_eq!(sha, expected_sha(21, size));
    // Eight connections (the per-host limit) open 150 ms apart: the last would start 1.05 s in. Over loopback
    // the whole file is in long before that, and nobody may wait for the sleepers.
    assert!(
        elapsed < Duration::from_millis(800),
        "completion waited for staggered workers: {elapsed:?}"
    );
}

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

#[tokio::test(flavor = "multi_thread")]
async fn max_connections_set_by_update_limits_survives_restart() {
    let server = FixtureServer::start_on(0).await.unwrap();
    let store = Arc::new(MemoryStore::new());
    let sink = Arc::new(RecordingSink::new());
    let first = engine_over(&store, &sink);
    let size = 24 * MIB;
    let job = forgedm_core::ports::NewJob {
        id: uuid::Uuid::now_v7(),
        source: forgedm_protocol::ClientKind::Cli,
        priority: forgedm_protocol::Priority::Normal,
        display_name: "cap.bin".to_owned(),
        url: forgedm_core::url_policy::validate_user_url(&format!(
            "{}/f/22/{size}?slow=6000000",
            server.base_url()
        ))
        .unwrap(),
        target_dir: std::fs::canonicalize(tempfile::tempdir().unwrap().keep()).unwrap(),
        requested_name: Some("cap.bin".to_owned()),
        expected_sha256: None,
        max_connections: 8,
        created_at_ms: unix_ms(),
    };
    let id = job.id;
    first.enqueue(job).await.unwrap();

    let effective = first.set_max_connections(id, 2).await.unwrap();

    assert_eq!(effective, 2);
    let row = store.row(id).unwrap();
    assert_eq!(
        row.persisted_max_connections,
        Some(2),
        "the store kept the cap"
    );
    assert_eq!(store.load(id).await.unwrap().unwrap().max_connections, 2);

    // "Restart the agent": the first engine stops, a new one recovers from the store.
    first.shutdown().await;
    server.log().clear();
    let second = engine_over(&store, &sink);
    second.recover().await.unwrap();
    let state = store
        .wait_state(id, |s| s == DownloadState::Completed, WAIT)
        .await;

    assert_eq!(state, DownloadState::Completed);
    assert!(
        server.log().max_concurrency() <= 2,
        "the recovered download used the persisted cap, saw {} at once",
        server.log().max_concurrency()
    );
    assert!(matches!(
        sink.wait_terminal(id, WAIT).await,
        EngineEvent::Completed { .. }
    ));
}
