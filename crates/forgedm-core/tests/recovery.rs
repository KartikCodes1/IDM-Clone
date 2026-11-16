//! Panic and settle-failure recovery (SFH-15, SFH-16): whatever goes wrong inside a task, the
//! stored state of the download ends terminal or interrupted, never stuck on an active state.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

#[path = "../src/testing.rs"]
#[allow(dead_code)] // each test file uses part of the shared fakes
mod testing;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use forgedm_core::client::{HttpClient, ProxyMode};
use forgedm_core::engine::{self, Control, EngineConfig, EngineDeps, EngineHandle};
use forgedm_core::error::StoreError;
use forgedm_core::ports::{
    DownloadStore, EngineEvent, NewJob, ProbeFacts, StoreEvent, StoreFuture, StoredJob, unix_ms,
};
use forgedm_core::url_policy::validate_user_url;
use forgedm_fixture::FixtureServer;
use forgedm_platform::motw::MotwService;
use forgedm_protocol::{ClientKind, DownloadErrorCode, DownloadState, Priority};
use testing::{MemoryStore, RecordingSink};
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(30);

/// What the faulty store should do wrong, once.
#[derive(Clone, Copy)]
enum Fault {
    /// Panic inside `save_probe` (the task panics while `probing`).
    PanicOnSaveProbe,
    /// Panic when the download is moved to `paused` (the task panics while `pausing`).
    PanicOnPaused,
    /// Fail the first transition to `cancelled` with a storage error.
    FailFirstCancel,
    /// Fail every transition out of an active state (the task cannot settle at all).
    FailAllSettles,
}

/// Delegates to a [`MemoryStore`] and misbehaves as told.
struct FaultyStore {
    inner: Arc<MemoryStore>,
    fault: Fault,
    hits: AtomicU32,
}

impl FaultyStore {
    fn new(inner: Arc<MemoryStore>, fault: Fault) -> Self {
        Self {
            inner,
            fault,
            hits: AtomicU32::new(0),
        }
    }
}

impl DownloadStore for FaultyStore {
    fn insert(&self, new: NewJob) -> StoreFuture<'_, ()> {
        self.inner.insert(new)
    }

    fn load(&self, id: Uuid) -> StoreFuture<'_, Option<StoredJob>> {
        self.inner.load(id)
    }

    fn startup_work(&self) -> StoreFuture<'_, Vec<StoredJob>> {
        self.inner.startup_work()
    }

    fn count(&self) -> StoreFuture<'_, usize> {
        self.inner.count()
    }

    fn transition(
        &self,
        id: Uuid,
        from: DownloadState,
        to: DownloadState,
        reason: Option<String>,
        event: StoreEvent,
    ) -> StoreFuture<'_, ()> {
        match (self.fault, to) {
            (Fault::PanicOnPaused, DownloadState::Paused) => panic!("injected panic while pausing"),
            (Fault::FailFirstCancel, DownloadState::Cancelled)
                if self.hits.fetch_add(1, Ordering::AcqRel) == 0 =>
            {
                return Box::pin(async { Err(StoreError::Unavailable("injected".to_owned())) });
            }
            (Fault::FailAllSettles, DownloadState::Cancelled) => {
                return Box::pin(async { Err(StoreError::Unavailable("injected".to_owned())) });
            }
            _ => {}
        }
        self.inner.transition(id, from, to, reason, event)
    }

    fn save_probe(&self, id: Uuid, facts: ProbeFacts) -> StoreFuture<'_, ()> {
        if matches!(self.fault, Fault::PanicOnSaveProbe) {
            panic!("injected panic while probing");
        }
        self.inner.save_probe(id, facts)
    }

    fn checkpoint(&self, id: Uuid, committed: u64, total: Option<u64>) -> StoreFuture<'_, ()> {
        self.inner.checkpoint(id, committed, total)
    }

    fn reset_progress(&self, id: Uuid) -> StoreFuture<'_, ()> {
        self.inner.reset_progress(id)
    }

    fn remove(&self, id: Uuid) -> StoreFuture<'_, ()> {
        self.inner.remove(id)
    }

    fn set_finalize_target(&self, id: Uuid, target: &Path) -> StoreFuture<'_, ()> {
        self.inner.set_finalize_target(id, target)
    }

    fn complete(
        &self,
        id: Uuid,
        final_path: &Path,
        sha256: &str,
        length_verified: bool,
    ) -> StoreFuture<'_, ()> {
        self.inner.complete(id, final_path, sha256, length_verified)
    }

    fn append_event(&self, id: Uuid, event: StoreEvent) -> StoreFuture<'_, ()> {
        self.inner.append_event(id, event)
    }
}

struct Rig {
    server: FixtureServer,
    store: Arc<MemoryStore>,
    sink: Arc<RecordingSink>,
    engine: EngineHandle,
    dir: tempfile::TempDir,
}

async fn rig(fault: Fault) -> Rig {
    let server = FixtureServer::start_on(0).await.unwrap();
    let store = Arc::new(MemoryStore::new());
    let sink = Arc::new(RecordingSink::new());
    let engine = engine::start(
        EngineConfig::default(),
        EngineDeps {
            store: Arc::new(FaultyStore::new(Arc::clone(&store), fault)),
            sink: sink.clone(),
            client: HttpClient::new(ProxyMode::None).unwrap(),
            motw: Arc::new(MotwService::start().unwrap()),
        },
    );
    Rig {
        server,
        store,
        sink,
        engine,
        dir: tempfile::tempdir().unwrap(),
    }
}

impl Rig {
    async fn enqueue(&self, path_and_query: &str) -> Uuid {
        let job = NewJob {
            id: Uuid::now_v7(),
            source: ClientKind::Cli,
            priority: Priority::Normal,
            url: validate_user_url(&format!("{}{path_and_query}", self.server.base_url())).unwrap(),
            target_dir: std::fs::canonicalize(self.dir.path()).unwrap(),
            requested_name: Some("r.bin".to_owned()),
            display_name: "r.bin".to_owned(),
            expected_sha256: None,
            max_connections: 4,
            created_at_ms: unix_ms(),
        };
        let id = job.id;
        self.engine.enqueue(job).await.unwrap();
        id
    }

    async fn wait_accepted(&self, id: Uuid) {
        let deadline = tokio::time::Instant::now() + WAIT;
        while self.engine.live(id).is_none_or(|p| p.accepted == 0) {
            assert!(tokio::time::Instant::now() < deadline, "no bytes accepted");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_panic_while_probing_leaves_the_download_failed_not_stuck() {
    let r = rig(Fault::PanicOnSaveProbe).await;

    let id = r.enqueue("/f/1/100000").await;
    let event = r.sink.wait_terminal(id, WAIT).await;

    let EngineEvent::Failed { error, .. } = event else {
        panic!("{event:?}");
    };
    assert_eq!(error.code, DownloadErrorCode::Internal);
    assert_eq!(r.store.state(id), Some(DownloadState::Failed));
    assert_eq!(r.engine.running(), 0, "the slot was released");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_panic_while_pausing_leaves_the_download_interrupted() {
    let r = rig(Fault::PanicOnPaused).await;
    let id = r.enqueue("/f/2/2000000?slow=500000").await;
    r.wait_accepted(id).await;

    // The task panics after persisting `pausing`; the pause cannot finish.
    let _ = r
        .engine
        .control(
            id,
            Control::Pause {
                discard_progress: false,
            },
        )
        .await;

    r.store
        .wait_state(id, |s| s == DownloadState::Interrupted, WAIT)
        .await;
    assert_eq!(r.engine.running(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_settle_is_retried_not_left_stale() {
    let r = rig(Fault::FailFirstCancel).await;
    let id = r.enqueue("/f/3/2000000?slow=500000").await;
    r.wait_accepted(id).await;

    let state = r.engine.control(id, Control::Cancel).await.unwrap();

    assert_eq!(
        state,
        DownloadState::Cancelled,
        "the second attempt stored the cancel"
    );
    assert_eq!(r.store.state(id), Some(DownloadState::Cancelled));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_settle_that_never_stores_ends_interrupted_not_active() {
    let r = rig(Fault::FailAllSettles).await;
    let id = r.enqueue("/f/4/2000000?slow=500000").await;
    r.wait_accepted(id).await;

    let _ = r.engine.control(id, Control::Cancel).await;

    r.store
        .wait_state(id, |s| s == DownloadState::Interrupted, WAIT)
        .await;
    assert_eq!(r.engine.running(), 0, "the supervisor released the slot");
}
