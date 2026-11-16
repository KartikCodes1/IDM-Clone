//! Global limits, PauseAll, ResumeAll and UpdatePriority through the real `forgedm-agent.exe`
//! (PLAN S1.13). Limits must be stored and come back after a restart.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

mod support;

use std::path::Path;
use std::time::{Duration, Instant};

use forgedm_fixture::FixtureServer;
use forgedm_ipc::Client;
use forgedm_protocol::{
    DownloadDetail, DownloadIdRequest, DownloadState, Empty, EnqueueUrlRequest, LimitScope,
    PauseAllRequest, Priority, RequestBody, ResultBody, UpdateLimitsRequest, UpdatePriorityRequest,
};
use support::AgentProc;
use tempfile::TempDir;
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(60);

async fn enqueue(client: &mut Client, url: &str, dir: &Path, name: &str) -> Uuid {
    let mut request = EnqueueUrlRequest::new(url);
    request.target_dir = Some(dir.to_str().unwrap().to_owned());
    request.file_name = Some(name.to_owned());
    request.max_connections = Some(1);
    match client.call(RequestBody::EnqueueUrl(request)).await.unwrap() {
        ResultBody::EnqueueUrl(result) => result.download_id,
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

async fn wait_state(client: &mut Client, id: Uuid, wanted: DownloadState) {
    let started = Instant::now();
    loop {
        let now = detail(client, id).await.summary.state;
        if now == wanted {
            return;
        }
        assert!(
            started.elapsed() < WAIT,
            "timed out waiting for {wanted:?}; state {now:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn global(client: &mut Client, patch: impl FnOnce(&mut UpdateLimitsRequest)) -> ResultBody {
    let mut request = UpdateLimitsRequest::new(LimitScope::Global);
    patch(&mut request);
    client
        .call(RequestBody::UpdateLimits(request))
        .await
        .unwrap()
}

fn effective(body: ResultBody) -> forgedm_protocol::EffectiveLimits {
    match body {
        ResultBody::UpdateLimits(result) => result.effective,
        other => panic!("not an UpdateLimits result: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn global_limits_survive_an_agent_restart() {
    let data = TempDir::new().unwrap();
    let mut first = AgentProc::start(data.path(), &[]);
    let (mut client, _) = first.connect().await;
    let defaults = effective(global(&mut client, |_| {}).await);
    assert_eq!(defaults.max_active_downloads, Some(3));
    assert_eq!(defaults.max_connections, Some(8));
    assert_eq!(defaults.rate_limit_bps, None);

    let set = effective(
        global(&mut client, |r| {
            r.max_active_downloads = Some(2);
            r.rate_limit_bps = Some(Some(65_536));
            r.max_connections = Some(5);
        })
        .await,
    );
    assert_eq!(set.max_active_downloads, Some(2));
    assert_eq!(set.rate_limit_bps, Some(65_536));
    assert_eq!(set.max_connections, Some(5));
    client
        .call(RequestBody::ShutdownAgentSafely(Empty {}))
        .await
        .unwrap();
    assert_eq!(first.wait_exit(WAIT).and_then(|s| s.code()), Some(0));

    let second = AgentProc::start(data.path(), &[]);
    let (mut client, _) = second.connect().await;
    let restored = effective(global(&mut client, |_| {}).await);
    assert_eq!(restored, set, "the stored limits came back");

    // A stored `null` is "unlimited", and it is stored too.
    let lifted = effective(global(&mut client, |r| r.rate_limit_bps = Some(None)).await);
    assert_eq!(lifted.rate_limit_bps, None);
    assert_eq!(lifted.max_active_downloads, Some(2));
}

#[tokio::test(flavor = "multi_thread")]
async fn active_cap_pause_all_resume_all_and_priority() {
    let fixture = FixtureServer::start_on(0).await.unwrap();
    let (data, target) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let slow = |seed: u64| {
        format!(
            "{}/f/{seed}/600000?slow=200000&t=l{seed}",
            fixture.base_url()
        )
    };
    let agent = AgentProc::start(data.path(), &[]);
    let (mut client, _) = agent.connect().await;
    global(&mut client, |r| r.max_active_downloads = Some(1)).await;

    let first = enqueue(&mut client, &slow(1), target.path(), "one.bin").await;
    let second = enqueue(&mut client, &slow(2), target.path(), "two.bin").await;
    wait_state(&mut client, first, DownloadState::Downloading).await;
    assert_eq!(
        detail(&mut client, second).await.summary.state,
        DownloadState::Queued,
        "one active download at a time"
    );

    let priority = RequestBody::UpdatePriority(UpdatePriorityRequest {
        download_id: second,
        priority: Priority::High,
    });
    assert!(matches!(
        client.call(priority).await.unwrap(),
        ResultBody::UpdatePriority(_)
    ));
    assert_eq!(
        detail(&mut client, second).await.summary.priority,
        Priority::High
    );

    let paused = match client
        .call(RequestBody::PauseAll(PauseAllRequest {
            discard_progress: false,
        }))
        .await
        .unwrap()
    {
        ResultBody::PauseAll(result) => result,
        other => panic!("not a PauseAll result: {other:?}"),
    };
    let mut paused_ids = paused.paused.clone();
    paused_ids.sort();
    let mut both = vec![first, second];
    both.sort();
    assert_eq!(paused_ids, both);
    assert!(paused.skipped_not_resumable.is_empty());
    wait_state(&mut client, first, DownloadState::Paused).await;
    wait_state(&mut client, second, DownloadState::Paused).await;

    let resumed = match client.call(RequestBody::ResumeAll(Empty {})).await.unwrap() {
        ResultBody::ResumeAll(result) => result.resumed,
        other => panic!("not a ResumeAll result: {other:?}"),
    };
    assert_eq!(resumed.len(), 2);
    wait_state(&mut client, first, DownloadState::Completed).await;
    wait_state(&mut client, second, DownloadState::Completed).await;
}
