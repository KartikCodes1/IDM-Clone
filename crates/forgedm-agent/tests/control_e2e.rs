//! Pause, Resume, Cancel, Retry and Remove through the real `forgedm-agent.exe`, plus the
//! shutdown-and-restart flow on a real database (PLAN S1.9). Every agent is a drop-guarded
//! child; the fixture server runs in the test process.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

mod support;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use forgedm_agent::sealer::DpapiSealer;
use forgedm_fixture::{FixtureServer, content, verify};
use forgedm_ipc::{Client, IpcError};
use forgedm_protocol::{
    DownloadDetail, DownloadIdRequest, DownloadState, Empty, EnqueueUrlRequest, ErrorCode,
    PauseRequest, RemoveRequest, RequestBody, ResultBody, RetryRequest,
};
use forgedm_storage::Storage;
use support::AgentProc;
use tempfile::TempDir;
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(60);

async fn enqueue(client: &mut Client, url: &str, dir: &Path, name: &str) -> Uuid {
    let mut request = EnqueueUrlRequest::new(url);
    request.target_dir = Some(dir.to_str().unwrap().to_owned());
    request.file_name = Some(name.to_owned());
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

async fn wait_for(
    client: &mut Client,
    id: Uuid,
    what: &str,
    done: impl Fn(&DownloadDetail) -> bool,
) -> DownloadDetail {
    let started = Instant::now();
    loop {
        let now = detail(client, id).await;
        if done(&now) {
            return now;
        }
        assert!(
            started.elapsed() < WAIT,
            "timed out waiting for {what}; state {:?}",
            now.summary.state
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn state_of(client: &mut Client, body: RequestBody) -> DownloadState {
    match client.call(body).await.unwrap() {
        ResultBody::Pause(r)
        | ResultBody::Resume(r)
        | ResultBody::Cancel(r)
        | ResultBody::Retry(r) => r.state,
        other => panic!("not a state result: {other:?}"),
    }
}

fn id_request(id: Uuid) -> DownloadIdRequest {
    DownloadIdRequest { download_id: id }
}

fn error_code(result: Result<ResultBody, IpcError>) -> ErrorCode {
    match result {
        Err(IpcError::Server(body)) => body.code,
        other => panic!("expected an error response, got {other:?}"),
    }
}

fn parts_in(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".forgedm.part"))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn control_actions_on_an_unknown_download_are_not_found() {
    let data = TempDir::new().unwrap();
    let agent = AgentProc::start(data.path(), &[]);
    let (mut client, _) = agent.connect().await;
    let id = Uuid::now_v7();

    let calls = [
        RequestBody::Pause(PauseRequest {
            download_id: id,
            discard_progress: false,
        }),
        RequestBody::Resume(id_request(id)),
        RequestBody::Cancel(id_request(id)),
        RequestBody::Retry(RetryRequest {
            download_id: id,
            restart_from_zero: false,
        }),
        RequestBody::Remove(RemoveRequest {
            download_id: id,
            delete_partial_file: false,
        }),
    ];
    for body in calls {
        let action = body.action();
        let code = error_code(client.call(body).await);
        assert_eq!(code, ErrorCode::NotFound, "{action:?}");
    }
    client.call(RequestBody::Ping(Empty {})).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_resume_cancel_retry_remove_over_the_pipe() {
    let fixture = FixtureServer::start_on(0).await.unwrap();
    let (data, target) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let agent = AgentProc::start(data.path(), &[]);
    let (mut client, _) = agent.connect().await;
    let size = 3_000_000u64;
    let url = format!("{}/f/61/{size}?slow=500000", fixture.base_url());
    let id = enqueue(&mut client, &url, target.path(), "ctl.bin").await;
    wait_for(&mut client, id, "a first checkpoint", |d| {
        d.summary.committed_bytes > 0
    })
    .await;

    let paused = state_of(
        &mut client,
        RequestBody::Pause(PauseRequest {
            download_id: id,
            discard_progress: false,
        }),
    )
    .await;
    assert_eq!(paused, DownloadState::Paused);
    let again = state_of(
        &mut client,
        RequestBody::Pause(PauseRequest {
            download_id: id,
            discard_progress: false,
        }),
    )
    .await;
    assert_eq!(again, DownloadState::Paused, "a repeated pause is a no-op");
    let at_pause = detail(&mut client, id).await;
    assert!(at_pause.summary.committed_bytes > 0);
    assert!(at_pause.summary.committed_bytes < size);

    let resumed = state_of(&mut client, RequestBody::Resume(id_request(id))).await;
    assert_eq!(resumed, DownloadState::Queued);
    let cancelled = state_of(&mut client, RequestBody::Cancel(id_request(id))).await;
    assert_eq!(cancelled, DownloadState::Cancelled);
    assert_eq!(parts_in(target.path()).len(), 1, "cancel keeps the .part");
    let invalid = error_code(client.call(RequestBody::Resume(id_request(id))).await);
    assert_eq!(
        invalid,
        ErrorCode::InvalidState,
        "a cancelled download needs Retry"
    );

    let retried = state_of(
        &mut client,
        RequestBody::Retry(RetryRequest {
            download_id: id,
            restart_from_zero: true,
        }),
    )
    .await;
    assert_eq!(retried, DownloadState::Queued);
    let done = wait_for(&mut client, id, "completion after the retry", |d| {
        d.summary.state == DownloadState::Completed
    })
    .await;
    verify::file_matches(
        Path::new(done.summary.final_path.as_deref().unwrap()),
        61,
        size,
    )
    .unwrap();
    let completed_pause = error_code(
        client
            .call(RequestBody::Pause(PauseRequest {
                download_id: id,
                discard_progress: false,
            }))
            .await,
    );
    assert_eq!(completed_pause, ErrorCode::InvalidState);

    let removed = client
        .call(RequestBody::Remove(RemoveRequest {
            download_id: id,
            delete_partial_file: true,
        }))
        .await
        .unwrap();
    assert!(matches!(removed, ResultBody::Remove(_)));
    assert!(
        Path::new(done.summary.final_path.as_deref().unwrap()).is_file(),
        "a completed file is never deleted by Remove"
    );
    let gone = error_code(client.call(RequestBody::GetDownload(id_request(id))).await);
    assert_eq!(gone, ErrorCode::NotFound);
    let twice = error_code(
        client
            .call(RequestBody::Remove(RemoveRequest {
                download_id: id,
                delete_partial_file: false,
            }))
            .await,
    );
    assert_eq!(twice, ErrorCode::NotFound, "a repeated remove is not_found");
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_then_restart_requeues() {
    let fixture = FixtureServer::start_on(0).await.unwrap();
    let (data, target) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let size = 3_000_000u64;
    let url = format!("{}/f/62/{size}?slow=500000&t=restart", fixture.base_url());
    let mut first = AgentProc::start(data.path(), &[]);
    let (mut client, _) = first.connect().await;
    let id = enqueue(&mut client, &url, target.path(), "restart.bin").await;
    wait_for(&mut client, id, "a first checkpoint", |d| {
        d.summary.committed_bytes > 0
    })
    .await;

    client
        .call(RequestBody::ShutdownAgentSafely(Empty {}))
        .await
        .unwrap();
    assert_eq!(
        first.wait_exit(WAIT).and_then(|s| s.code()),
        Some(0),
        "a graceful shutdown exits 0"
    );

    // The database now says what the shutdown settled.
    let storage = Storage::open(data.path().join("forgedm.db"), Arc::new(DpapiSealer))
        .await
        .unwrap();
    let record = storage.downloads().get(id).await.unwrap().unwrap();
    storage.close().await;
    assert_eq!(record.state, DownloadState::Interrupted);
    assert_eq!(record.state_reason.as_deref(), Some("agent_shutdown"));
    let committed = record.bytes_committed;
    assert!(committed > 0 && committed < size, "committed {committed}");

    let second = AgentProc::start(data.path(), &[]);
    let (mut client, _) = second.connect().await;
    let done = wait_for(&mut client, id, "auto-resume to completion", |d| {
        d.summary.state == DownloadState::Completed
    })
    .await;

    verify::file_matches(
        Path::new(done.summary.final_path.as_deref().unwrap()),
        62,
        size,
    )
    .unwrap();
    assert_eq!(
        done.checksum.actual.as_deref(),
        Some(hex::encode(content::sha256(62, size)).as_str())
    );
    let log = fixture.log().for_test("restart");
    let resumed = log
        .iter()
        .rfind(|e| e.method == "GET" && e.range.as_deref() != Some("bytes=0-0"))
        .expect("a transfer GET");
    assert_eq!(
        resumed.range.as_deref(),
        Some(format!("bytes={committed}-{}", size - 1).as_str()),
        "the restarted agent resumes at the committed offset"
    );
    assert!(resumed.if_range.is_some(), "with a strong If-Range");
}
