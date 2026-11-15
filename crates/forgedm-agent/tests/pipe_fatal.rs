//! A fatal pipe error after startup stops the agent with exit code 4 (PLAN A.8), and the
//! expected checksum survives event pruning (S1.7 MEDIUM follow-up). Both run the agent
//! in-process.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

use std::sync::Arc;
use std::time::Duration;

use forgedm_agent::app;
use forgedm_agent::config::{Config, ProxyMode};
use forgedm_agent::error::StartupError;
use forgedm_agent::store_adapter::StorageStore;
use forgedm_core::client::{HttpClient, ProxyMode as CoreProxyMode};
use forgedm_core::clock::BoxFuture;
use forgedm_core::engine::{self, EngineConfig, EngineDeps};
use forgedm_core::ports::{
    DownloadStore, EngineEvent, EngineEventSink, NewJob, StoreEvent, unix_ms,
};
use forgedm_core::url_policy::validate_user_url;
use forgedm_fixture::FixtureServer;
use forgedm_ipc::{Client, ClientConfig};
use forgedm_platform::motw::MotwService;
use forgedm_platform::pipe::{PipeSecurity, create_pipe_instance};
use forgedm_protocol::{ClientKind, DownloadErrorCode, DownloadState, Priority};
use forgedm_storage::Storage;
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(60);

/// After startup the server can only keep its name if it can always create the next listening
/// instance. When it cannot (here: every slot is taken), that is fatal: the agent stops and
/// its exit code is 4, because a name that cannot be re-created might be squatted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn post_startup_fatal_pipe_error_exits_4() {
    let data = TempDir::new().unwrap();
    let config = Config::new(
        data.path().to_path_buf(),
        ProxyMode::None,
        "warn".to_owned(),
    );
    let prepared = app::prepare(config).expect("prepare");
    let token = CancellationToken::new();
    let agent = app::start(prepared, token.clone()).await.expect("start");
    let name = agent.pipe_name().to_owned();
    // Fill every remaining instance slot (the server holds the first of 17).
    let security = PipeSecurity::for_current_user().expect("security");
    let fillers: Vec<_> = (0..16)
        .map(|_| create_pipe_instance(&name, false, &security).expect("filler"))
        .collect();

    // A connecting client makes the server accept, which needs a replacement instance.
    let _ = Client::connect(ClientConfig::new(name, std::env::current_exe().unwrap())).await;
    let outcome = tokio::time::timeout(Duration::from_secs(20), agent.wait())
        .await
        .expect("the agent stops by itself");

    let error = outcome.expect_err("a fatal pipe error is an error");
    assert!(matches!(error, StartupError::PipeSecurity(_)), "{error:?}");
    assert_eq!(error.exit_code(), 4);
    drop(fillers);
    drop(token);
}

/// Forwards terminal events to a channel.
struct ChannelSink(mpsc::UnboundedSender<EngineEvent>);

impl EngineEventSink for ChannelSink {
    fn emit(&self, event: EngineEvent) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let _ = self.0.send(event);
        })
    }

    fn try_emit(&self, _event: EngineEvent) -> bool {
        false
    }
}

/// The expected SHA-256 used to live only in the `enqueued` event, which the 200-event
/// retention prunes at a checkpoint; the download then failed to load or lost its check. It is
/// a column now: with events pruned away, a wrong digest must still fail the download.
#[tokio::test(flavor = "multi_thread")]
async fn expected_checksum_survives_event_pruning_and_still_verifies() {
    let fixture = FixtureServer::start_on(0).await.unwrap();
    let (data, target) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let storage = Storage::open(
        data.path().join("t.db"),
        Arc::new(forgedm_agent::sealer::DpapiSealer),
    )
    .await
    .unwrap();
    let store = Arc::new(StorageStore::new(storage.clone(), 0));
    let (tx, mut events) = mpsc::unbounded_channel();
    let handle = engine::start(
        EngineConfig::default(),
        EngineDeps {
            store: store.clone(),
            sink: Arc::new(ChannelSink(tx)),
            client: HttpClient::new(CoreProxyMode::None).unwrap(),
            motw: Arc::new(MotwService::start().unwrap()),
        },
    );
    let wrong = "0".repeat(64);
    let id = Uuid::now_v7();
    // 600 kB at 100 kB/s: checkpoints at 2 s and 4 s prune the events while it runs.
    let url = format!("{}/f/63/600000?slow=100000", fixture.base_url());
    handle
        .enqueue(NewJob {
            id,
            source: ClientKind::Cli,
            priority: Priority::Normal,
            url: validate_user_url(&url).unwrap(),
            target_dir: std::fs::canonicalize(target.path()).unwrap(),
            requested_name: Some("prune.bin".to_owned()),
            display_name: "prune.bin".to_owned(),
            expected_sha256: Some(wrong.clone()),
            max_connections: 4,
            created_at_ms: unix_ms(),
        })
        .await
        .unwrap();
    for n in 0..300 {
        let mut noise = StoreEvent::now("noise");
        noise.at_ms = n;
        store.append_event(id, noise).await.unwrap();
    }

    let failure = tokio::time::timeout(WAIT, async {
        loop {
            match events.recv().await.expect("the sink stays open") {
                EngineEvent::Failed { error, .. } => return error,
                EngineEvent::Completed { .. } => panic!("completed: the checksum check was lost"),
                _ => {}
            }
        }
    })
    .await
    .expect("a terminal event");

    assert_eq!(failure.code, DownloadErrorCode::ChecksumMismatch);
    let record = storage.downloads().get(id).await.unwrap().unwrap();
    assert_eq!(record.state, DownloadState::Failed);
    assert_eq!(record.checksum_expected.as_deref(), Some(wrong.as_str()));
    let events = storage.events().list(id, 500).await.unwrap();
    assert!(
        events.iter().all(|e| e.kind != "enqueued"),
        "the enqueued event was pruned, yet the expectation is intact"
    );
    let reloaded = store.load(id).await.unwrap().unwrap();
    assert_eq!(reloaded.expected_sha256.as_deref(), Some(wrong.as_str()));
    handle.shutdown().await;
    storage.close().await;
}
