//! The store adapter keeps what S1.12 needs to survive an agent restart: a per-download
//! connection cap changed at runtime (W7 follow-up) and the due time of a `retry_waiting`
//! download. Real SQLite, real sealer; no engine.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

use std::sync::Arc;

use forgedm_agent::sealer::DpapiSealer;
use forgedm_agent::store_adapter::StorageStore;
use forgedm_core::ports::{DownloadStore, NewJob, StoreEvent, unix_ms};
use forgedm_core::url_policy::validate_user_url;
use forgedm_protocol::{ClientKind, DownloadState, Priority};
use forgedm_storage::Storage;
use tempfile::TempDir;
use uuid::Uuid;

async fn open(dir: &TempDir) -> Storage {
    Storage::open(dir.path().join("t.db"), Arc::new(DpapiSealer))
        .await
        .unwrap()
}

fn job(dir: &TempDir) -> NewJob {
    NewJob {
        id: Uuid::now_v7(),
        source: ClientKind::Cli,
        priority: Priority::Normal,
        url: validate_user_url("https://example.test/a.bin?token=s3cret").unwrap(),
        target_dir: dir.path().to_path_buf(),
        requested_name: None,
        display_name: "a.bin".to_owned(),
        expected_sha256: None,
        max_connections: 8,
        created_at_ms: unix_ms(),
    }
}

#[tokio::test]
async fn max_connections_set_at_runtime_is_what_a_restarted_agent_loads() {
    let dir = TempDir::new().unwrap();
    let new = job(&dir);
    let id = new.id;
    {
        let store = StorageStore::new(open(&dir).await, 0);
        store.insert(new).await.unwrap();
        store.set_max_connections(id, 3).await.unwrap();
    }

    // A new store over the same database file: the "restarted agent".
    let restarted = StorageStore::new(open(&dir).await, 1);
    let loaded = restarted.load(id).await.unwrap().unwrap();

    assert_eq!(loaded.max_connections, 3);
    assert!(restarted.set_max_connections(id, 0).await.is_err());
    assert_eq!(
        restarted.load(id).await.unwrap().unwrap().max_connections,
        3
    );
}

#[tokio::test]
async fn a_retry_waiting_download_is_found_again_with_its_due_time() {
    let dir = TempDir::new().unwrap();
    let new = job(&dir);
    let id = new.id;
    let store = StorageStore::new(open(&dir).await, 0);
    store.insert(new).await.unwrap();
    for (from, to) in [
        (DownloadState::Queued, DownloadState::Probing),
        (DownloadState::Probing, DownloadState::Downloading),
    ] {
        store
            .transition(id, from, to, None, StoreEvent::now("state"))
            .await
            .unwrap();
    }
    store.schedule_retry(id, 1_900_000_000_000).await.unwrap();
    store
        .transition(
            id,
            DownloadState::Downloading,
            DownloadState::RetryWaiting,
            Some("rate_limited".to_owned()),
            StoreEvent::now("retry_waiting"),
        )
        .await
        .unwrap();

    let work = store.startup_work().await.unwrap();

    assert!(
        work.iter()
            .any(|j| j.id == id && j.state == DownloadState::RetryWaiting)
    );
    assert_eq!(
        store.next_retry_at(id).await.unwrap(),
        Some(1_900_000_000_000)
    );
}
