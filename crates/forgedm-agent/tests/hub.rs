//! StateHub behaviour through the real pipe server, driven in-process.
//!
//! There is no engine in this slice, so state changes enter through `HubHandle::publish`, the
//! same engine-facing API the S1.7 engine will use. Nothing here is a hook in the shipped
//! binary. The agent runs in the test process, so clients expect `current_exe()` as the
//! server image.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

mod support;

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use forgedm_agent::app::{self, RunningAgent};
use forgedm_agent::config::{Config, ProxyMode};
use forgedm_agent::events_hub::HubHandle;
use forgedm_ipc::{Client, ClientConfig, IpcError};
use forgedm_protocol::{
    ClientKind, DownloadRemovedData, DownloadState, DownloadSummary, Empty, ErrorCode, EventBody,
    Priority, RequestBody, ResultBody, ResumeSupport, SnapshotResult, SubscribeRequest,
};
use tempfile::TempDir;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const STEP: Duration = Duration::from_secs(20);

fn summary(n: u32, state: DownloadState, accepted: u64) -> DownloadSummary {
    DownloadSummary {
        id: Uuid::from_u128(u128::from(n) + 1),
        display_name: format!("file-{n}.bin"),
        url_display: "https://example.com/file.bin".to_owned(),
        state,
        state_reason: None,
        priority: Priority::Normal,
        source: ClientKind::Cli,
        target_dir: r"C:\Downloads".to_owned(),
        final_path: None,
        total_bytes: Some(1_000_000),
        accepted_bytes: accepted,
        committed_bytes: accepted,
        speed_bps: None,
        eta_ms: None,
        transfer_mode: None,
        resume: ResumeSupport::Unknown,
        last_error: None,
        created_at_ms: u64::from(n) + 1,
        completed_at_ms: None,
    }
}

struct TestAgent {
    _dir: TempDir,
    agent: RunningAgent,
    token: CancellationToken,
}

impl TestAgent {
    async fn start() -> Self {
        let dir = TempDir::new().unwrap();
        support::harden(dir.path());
        let config = Config::new(dir.path().to_path_buf(), ProxyMode::None, "warn".to_owned());
        let prepared = app::prepare(config).expect("prepare");
        let token = CancellationToken::new();
        let agent = app::start(prepared, token.clone()).await.expect("start");
        Self {
            _dir: dir,
            agent,
            token,
        }
    }

    fn hub(&self) -> HubHandle {
        self.agent.hub().clone()
    }

    async fn client(&self) -> (Client, Uuid) {
        connect(self.agent.pipe_name()).await
    }

    async fn stop(self) {
        self.token.cancel();
        self.agent.wait().await.expect("clean stop");
    }
}

async fn connect(pipe: &str) -> (Client, Uuid) {
    let config = ClientConfig::new(pipe, std::env::current_exe().unwrap());
    let mut client = Client::connect(config).await.expect("connect");
    let hello = client
        .handshake(ClientKind::Desktop, "hub-test", "0.0.0")
        .await
        .expect("handshake");
    (client, hello.agent_instance_id)
}

async fn snapshot(client: &mut Client) -> SnapshotResult {
    match client.call(RequestBody::GetSnapshot(Empty {})).await {
        Ok(ResultBody::GetSnapshot(s)) => *s,
        other => panic!("snapshot failed: {other:?}"),
    }
}

async fn subscribe(client: &mut Client, instance: Uuid, after_seq: u64) -> Result<u64, IpcError> {
    let request = RequestBody::Subscribe(SubscribeRequest {
        agent_instance_id: instance,
        after_seq,
    });
    match client.call(request).await? {
        ResultBody::Subscribe(r) => Ok(r.current_seq),
        other => panic!("unexpected {other:?}"),
    }
}

fn expect_resync(result: Result<u64, IpcError>) {
    match result {
        Err(IpcError::Server(error)) => {
            assert_eq!(error.code, ErrorCode::ResyncRequired);
            assert!(error.retryable);
        }
        other => panic!("expected resync_required, got {other:?}"),
    }
}

/// A client's reconstruction of the download table from snapshot plus events.
#[derive(Default)]
struct Mirror {
    map: BTreeMap<Uuid, DownloadSummary>,
    last_seq: u64,
}

impl Mirror {
    fn from_snapshot(s: &SnapshotResult) -> Self {
        Self {
            map: s.downloads.iter().map(|d| (d.id, d.clone())).collect(),
            last_seq: s.seq,
        }
    }

    /// Applies one event; the sequence must be contiguous (no event lost).
    fn apply(&mut self, seq: u64, body: &EventBody) {
        assert_eq!(
            seq,
            self.last_seq + 1,
            "event sequence has a gap or a repeat"
        );
        self.last_seq = seq;
        match body {
            EventBody::DownloadAdded(d) | EventBody::DownloadChanged(d) => {
                self.map.insert(d.id, (**d).clone());
            }
            EventBody::DownloadRemoved(r) => {
                self.map.remove(&r.id);
            }
            _ => {}
        }
    }
}

/// Publishes a deterministic mix of adds, changes and removals.
async fn publish_workload(hub: HubHandle, total: u32) {
    const IDS: u32 = 40;
    const STATES: [DownloadState; 4] = [
        DownloadState::Downloading,
        DownloadState::Paused,
        DownloadState::Queued,
        DownloadState::Verifying,
    ];
    let mut present: HashSet<u32> = HashSet::new();
    for i in 0..total {
        let n = i % IDS;
        let accepted = u64::from(i);
        let body = if !present.contains(&n) {
            present.insert(n);
            EventBody::DownloadAdded(Box::new(summary(n, DownloadState::Queued, accepted)))
        } else if i % 5 == 3 {
            present.remove(&n);
            EventBody::DownloadRemoved(DownloadRemovedData {
                id: summary(n, DownloadState::Queued, 0).id,
            })
        } else {
            let state = STATES[usize::try_from(i).unwrap() % STATES.len()];
            EventBody::DownloadChanged(Box::new(summary(n, state, accepted)))
        };
        hub.publish(body).await.unwrap();
        tokio::task::yield_now().await;
        if i % 5 == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
}

struct Shared {
    publisher_done: AtomicBool,
    final_seq: AtomicU64,
    /// Connections opened by all clients, to prove the test really reconnected.
    connections: AtomicU64,
    /// Events applied from live or replayed delivery across all clients.
    applied_events: AtomicU64,
}

/// One reconnecting client: snapshot, subscribe, read a few events, drop the connection,
/// resubscribe from the last applied sequence. Returns its mirror once it reaches the final
/// sequence.
async fn reconnecting_client(pipe: String, shared: Arc<Shared>, events_per_cycle: usize) -> Mirror {
    let mut mirror: Option<Mirror> = None;
    loop {
        let (mut client, instance) = connect(&pipe).await;
        shared.connections.fetch_add(1, Ordering::SeqCst);
        let fresh = match mirror.take() {
            Some(m) => m,
            None => Mirror::from_snapshot(&snapshot(&mut client).await),
        };
        let mut current = fresh;
        match subscribe(&mut client, instance, current.last_seq).await {
            Ok(_) => {}
            Err(IpcError::Server(e)) if e.code == ErrorCode::ResyncRequired => {
                current = Mirror::from_snapshot(&snapshot(&mut client).await);
                subscribe(&mut client, instance, current.last_seq)
                    .await
                    .expect("subscribe after a fresh snapshot");
            }
            Err(other) => panic!("subscribe failed: {other:?}"),
        }
        let mut read = 0;
        let mut resync = false;
        while read < events_per_cycle || shared.publisher_done.load(Ordering::SeqCst) {
            let target = shared.final_seq.load(Ordering::SeqCst);
            if shared.publisher_done.load(Ordering::SeqCst)
                && target != 0
                && current.last_seq >= target
            {
                return current;
            }
            let next = timeout(Duration::from_millis(300), client.next_event()).await;
            let Ok(event) = next else {
                if shared.publisher_done.load(Ordering::SeqCst) {
                    continue;
                }
                break;
            };
            let event = event.expect("event stream");
            if let EventBody::ResyncRequired(_) = event.body {
                resync = true;
                break;
            }
            current.apply(event.seq, &event.body);
            shared.applied_events.fetch_add(1, Ordering::SeqCst);
            read += 1;
        }
        // Reconnect (the old connection is dropped here), keeping the mirror unless a
        // resync was requested.
        mirror = if resync { None } else { Some(current) };
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_event_lost_between_snapshot_and_subscribe() {
    let test = TestAgent::start().await;
    let shared = Arc::new(Shared {
        publisher_done: AtomicBool::new(false),
        final_seq: AtomicU64::new(0),
        connections: AtomicU64::new(0),
        applied_events: AtomicU64::new(0),
    });
    let pipe = test.agent.pipe_name().to_owned();
    let mut clients = Vec::new();
    for events_per_cycle in [1usize, 3, 7, 25] {
        clients.push(tokio::spawn(reconnecting_client(
            pipe.clone(),
            shared.clone(),
            events_per_cycle,
        )));
    }
    let publisher = tokio::spawn(publish_workload(test.hub(), 3000));

    timeout(STEP, publisher)
        .await
        .expect("publisher in time")
        .unwrap();
    let (mut control, _) = test.client().await;
    let final_snapshot = snapshot(&mut control).await;
    shared.final_seq.store(final_snapshot.seq, Ordering::SeqCst);
    shared.publisher_done.store(true, Ordering::SeqCst);

    for client in clients {
        let mirror = timeout(STEP, client)
            .await
            .expect("client in time")
            .unwrap();
        // The agent's periodic status event can be published after the final snapshot, and
        // a client may read it before it sees publisher_done. Such events never touch the
        // download map, so the map comparison below still proves that no download event was
        // lost; the sequence must only have reached the snapshot.
        assert!(
            mirror.last_seq >= final_snapshot.seq,
            "mirror stopped at {} before the final snapshot {}",
            mirror.last_seq,
            final_snapshot.seq
        );
        let expected: BTreeMap<Uuid, DownloadSummary> = final_snapshot
            .downloads
            .iter()
            .map(|d| (d.id, d.clone()))
            .collect();
        assert_eq!(
            mirror.map, expected,
            "reconstructed state equals the final GetSnapshot"
        );
    }
    assert!(
        shared.connections.load(Ordering::SeqCst) >= 12,
        "clients must have reconnected repeatedly, got {}",
        shared.connections.load(Ordering::SeqCst)
    );
    assert!(
        shared.applied_events.load(Ordering::SeqCst) >= 300,
        "clients must have applied many events, got {}",
        shared.applied_events.load(Ordering::SeqCst)
    );
    drop(control);
    test.stop().await;
}

#[tokio::test]
async fn subscribe_after_evicted_seq_requires_resync() {
    let test = TestAgent::start().await;
    let hub = test.hub();
    for i in 0..1100u32 {
        hub.publish(EventBody::DownloadAdded(Box::new(summary(
            i,
            DownloadState::Queued,
            0,
        ))))
        .await
        .unwrap();
    }
    let (mut client, instance) = test.client().await;
    let snap = snapshot(&mut client).await;
    assert!(snap.seq >= 1100, "seq counts every event: {}", snap.seq);

    // Evicted: the ring holds only the last 1024 events.
    expect_resync(subscribe(&mut client, instance, 0).await);
    // Too far behind to replay through a 256-frame queue.
    expect_resync(subscribe(&mut client, instance, snap.seq - 1000).await);
    // A different agent instance.
    expect_resync(subscribe(&mut client, Uuid::now_v7(), snap.seq).await);
    // A sequence from the future cannot be replayed either.
    expect_resync(subscribe(&mut client, instance, snap.seq + 1000).await);

    // Up to date: no replay, then live events.
    assert_eq!(
        subscribe(&mut client, instance, snap.seq).await.unwrap(),
        snap.seq
    );
    hub.publish(EventBody::DownloadRemoved(DownloadRemovedData {
        id: summary(0, DownloadState::Queued, 0).id,
    }))
    .await
    .unwrap();
    let live = timeout(STEP, client.next_event()).await.unwrap().unwrap();
    assert_eq!(live.seq, snap.seq + 1);

    // Close enough: replays exactly (after_seq, current] in order.
    let (mut other, _) = test.client().await;
    let current = subscribe(&mut other, instance, snap.seq - 100)
        .await
        .unwrap();
    assert_eq!(current, snap.seq + 1);
    for expected in (snap.seq - 99)..=(snap.seq + 1) {
        let event = timeout(STEP, other.next_event()).await.unwrap().unwrap();
        assert_eq!(event.seq, expected);
    }
    drop((client, other));
    test.stop().await;
}
