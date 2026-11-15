//! Process-level tests: the real `forgedm-agent.exe` on a temporary data directory.
//!
//! Every child is held by an `AgentProc` guard that kills it on drop, so no agent outlives
//! its test. Kill tests use `Child::kill` (TerminateProcess).
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stderr
)] // test code

mod support;

use forgedm_agent::config::pipe_name;
use forgedm_ipc::IpcError;
use forgedm_platform::instance_lock::InstanceLock;
use forgedm_platform::pipe::{PipeSecurity, create_pipe_instance};
use forgedm_protocol::{
    AgentState, Empty, ErrorCode, LimitScope, ListDownloadsRequest, PauseAllRequest, Priority,
    ProxySource, RequestBody, ResultBody, UpdateLimitsRequest, UpdatePriorityRequest,
};
use support::{AgentProc, WAIT, log_files};
use tempfile::TempDir;
use uuid::Uuid;

const EXIT_USAGE: i32 = 2;
const EXIT_ALREADY_RUNNING: i32 = 3;
const EXIT_PIPE_SECURITY: i32 = 4;
const EXIT_STORAGE: i32 = 5;
const EXIT_DATA_DIR: i32 = 6;

/// `--proxy` value, extra environment and the proxy source the agent must report.
type ProxyCase<'a> = (&'a str, Vec<(&'a str, &'a str)>, ProxySource);

fn exit_code(status: Option<std::process::ExitStatus>) -> i32 {
    status
        .expect("agent exited within the deadline")
        .code()
        .expect("exit code")
}

#[tokio::test]
async fn starts_creates_db_and_pipe_and_handshakes() {
    let dir = TempDir::new().unwrap();
    let agent = AgentProc::start(dir.path(), &[]);

    let (_client, hello) = agent.connect().await;

    assert_eq!(hello.protocol_version, 1);
    assert_eq!(hello.protocol_minor, 0);
    assert_eq!(hello.agent_pid, agent.pid());
    assert_eq!(hello.agent_version, env!("CARGO_PKG_VERSION"));
    assert!(hello.capabilities.iter().any(|c| c == "subscribe_replay"));
    assert!(dir.path().join("forgedm.db").is_file(), "database created");
    assert!(dir.path().join("run").join("agent.lock").is_file());
}

#[tokio::test]
async fn second_agent_same_data_dir_exits_3() {
    let dir = TempDir::new().unwrap();
    let first = AgentProc::start(dir.path(), &[]);
    let _ = first.connect().await;

    let mut second = AgentProc::start(dir.path(), &[]);

    assert_eq!(
        exit_code(second.wait_exit(WAIT)),
        EXIT_ALREADY_RUNNING,
        "lock still held after the 3 s retry"
    );
    // The first agent is unaffected.
    let (mut client, _) = first.connect().await;
    client.call(RequestBody::Ping(Empty {})).await.unwrap();
}

#[tokio::test]
async fn shutdown_exits_0_releases_lock_restart_ok() {
    let dir = TempDir::new().unwrap();
    let mut agent = AgentProc::start(dir.path(), &[]);
    let (mut client, _) = agent.connect().await;

    let reply = client
        .call(RequestBody::ShutdownAgentSafely(Empty {}))
        .await;

    assert!(
        matches!(reply, Ok(ResultBody::ShutdownAgentSafely(_))),
        "{reply:?}"
    );
    assert_eq!(exit_code(agent.wait_exit(WAIT)), 0);
    // The lock is free once the process is gone.
    let lock = InstanceLock::try_acquire(&dir.path().join("run").join("agent.lock"));
    assert!(lock.is_ok(), "lock released: {lock:?}");
    drop(lock);

    let mut restarted = AgentProc::start(dir.path(), &[]);
    let (mut client, _) = restarted.connect().await;
    client
        .call(RequestBody::ShutdownAgentSafely(Empty {}))
        .await
        .unwrap();
    assert_eq!(exit_code(restarted.wait_exit(WAIT)), 0);
}

#[tokio::test]
async fn restart_immediately_after_kill_succeeds() {
    let dir = TempDir::new().unwrap();
    let mut agent = AgentProc::start(dir.path(), &[]);
    let _ = agent.connect().await;

    agent.kill();
    let restarted = AgentProc::start(dir.path(), &[]);

    let (mut client, hello) = restarted.connect().await;
    assert_eq!(hello.agent_pid, restarted.pid());
    client.call(RequestBody::Ping(Empty {})).await.unwrap();
}

#[test]
fn corrupt_db_exits_5_file_preserved() {
    let dir = TempDir::new().unwrap();
    let garbage: Vec<u8> = (0..8192u32)
        .map(|i| u8::try_from(i % 251).unwrap() ^ 0xA5)
        .collect();
    let db = dir.path().join("forgedm.db");
    std::fs::write(&db, &garbage).unwrap();
    let before = std::fs::read(&db).unwrap();

    let mut agent = AgentProc::start(dir.path(), &[]);

    assert_eq!(exit_code(agent.wait_exit(WAIT)), EXIT_STORAGE);
    let after = std::fs::read(&db).unwrap();
    assert_eq!(
        before, after,
        "the corrupt database must be left byte-for-byte unchanged"
    );
}

#[tokio::test]
async fn squatted_pipe_exits_4_and_logs_security_event() {
    let dir = TempDir::new().unwrap();
    let name = pipe_name(Some(dir.path())).unwrap();
    let security = PipeSecurity::for_current_user().unwrap();
    // We are the squatter: we hold the first instance of the agent's pipe name.
    let _squatter = create_pipe_instance(&name, true, &security).unwrap();

    let mut agent = AgentProc::start(dir.path(), &[]);

    assert_eq!(exit_code(agent.wait_exit(WAIT)), EXIT_PIPE_SECURITY);
    let logs = log_files(dir.path());
    assert!(
        logs.iter()
            .any(|(_, text)| text.contains("forgedm::security")),
        "a forgedm::security event is logged: {logs:?}"
    );
}

#[tokio::test]
async fn status_reports_pid_version_and_proxy_source() {
    let dir = TempDir::new().unwrap();
    let agent = AgentProc::start(dir.path(), &[]);
    let (mut client, hello) = agent.connect().await;

    let status = match client
        .call(RequestBody::GetAgentStatus(Empty {}))
        .await
        .unwrap()
    {
        ResultBody::GetAgentStatus(s) => s,
        other => panic!("unexpected result {other:?}"),
    };

    assert_eq!(status.agent_pid, agent.pid());
    assert_eq!(status.agent_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(status.agent_instance_id, hello.agent_instance_id);
    assert_eq!(status.state, AgentState::Ready);
    assert_eq!(status.proxy_source, ProxySource::None);
    assert_eq!(
        (status.active, status.queued, status.total_speed_bps),
        (0, 0, 0)
    );
    assert_eq!(
        std::fs::canonicalize(&status.data_dir).unwrap(),
        std::fs::canonicalize(dir.path()).unwrap()
    );
    drop(client);
    drop(agent);

    let cases: [ProxyCase; 2] = [
        ("system", Vec::new(), ProxySource::System),
        (
            "system",
            vec![("HTTPS_PROXY", "http://127.0.0.1:9")],
            ProxySource::Environment,
        ),
    ];
    for (proxy, env, expected) in cases {
        let dir = TempDir::new().unwrap();
        let agent = AgentProc::start_with_env(dir.path(), proxy, &env);
        let (mut client, _) = agent.connect().await;
        let ResultBody::GetAgentStatus(status) = client
            .call(RequestBody::GetAgentStatus(Empty {}))
            .await
            .unwrap()
        else {
            panic!("unexpected result");
        };
        assert_eq!(status.proxy_source, expected, "--proxy {proxy} env {env:?}");
    }
}

#[tokio::test]
async fn list_empty() {
    let dir = TempDir::new().unwrap();
    let agent = AgentProc::start(dir.path(), &[]);
    let (mut client, hello) = agent.connect().await;

    let list = client
        .call(RequestBody::ListDownloads(ListDownloadsRequest {
            states: None,
            limit: 50,
            offset: 0,
        }))
        .await
        .unwrap();
    let snapshot = client
        .call(RequestBody::GetSnapshot(Empty {}))
        .await
        .unwrap();

    let ResultBody::ListDownloads(list) = list else {
        panic!("not a list")
    };
    assert!(list.items.is_empty());
    assert_eq!(list.total, 0);
    let ResultBody::GetSnapshot(snapshot) = snapshot else {
        panic!("not a snapshot")
    };
    assert!(snapshot.downloads.is_empty());
    assert!(!snapshot.truncated);
    assert_eq!(snapshot.agent_instance_id, hello.agent_instance_id);
    assert_eq!(snapshot.agent_status.state, AgentState::Ready);
}

#[tokio::test]
async fn unimplemented_actions_return_not_implemented() {
    let dir = TempDir::new().unwrap();
    let agent = AgentProc::start(dir.path(), &[]);
    let (mut client, _) = agent.connect().await;
    let id = Uuid::nil();
    // EnqueueUrl and GetDownload are served since S1.7 (tests/enqueue_e2e.rs) and the five
    // control actions since S1.9 (tests/control_e2e.rs).
    let requests = vec![
        RequestBody::PauseAll(PauseAllRequest {
            discard_progress: false,
        }),
        RequestBody::ResumeAll(Empty {}),
        RequestBody::UpdateLimits(UpdateLimitsRequest::new(LimitScope::Global)),
        RequestBody::UpdatePriority(UpdatePriorityRequest {
            download_id: id,
            priority: Priority::High,
        }),
    ];

    for body in requests {
        let action = body.action();
        match client.call(body).await {
            Err(IpcError::Server(error)) => {
                assert_eq!(error.code, ErrorCode::NotImplemented, "{action:?}");
                assert!(!error.retryable, "{action:?}");
            }
            other => panic!("{action:?} should be not_implemented, got {other:?}"),
        }
    }
    // The connection is still usable afterwards.
    client.call(RequestBody::Ping(Empty {})).await.unwrap();
}

#[test]
fn relative_data_dir_exits_2() {
    let dir = TempDir::new().unwrap();
    let relative = "forgedm-relative-data-dir-test";
    let mut agent = AgentProc::start_raw(
        &["--data-dir", relative],
        &["--proxy", "none"],
        &[],
        dir.path(),
    );

    assert_eq!(exit_code(agent.wait_exit(WAIT)), EXIT_USAGE);
    assert!(
        !std::path::Path::new(relative).exists(),
        "a relative path must not be created in the working directory"
    );
}

#[test]
fn bad_arguments_exit_2() {
    let dir = TempDir::new().unwrap();
    let data = dir.path().to_str().unwrap();
    for args in [
        vec!["--data-dir", data, "--proxy", "bogus"],
        vec!["--data-dir", data, "--unknown-flag"],
        vec!["--data-dir"],
        vec!["--data-dir", data, "--log-level", "not[a[filter"],
    ] {
        let mut agent = AgentProc::start_raw(&args, &[], &[], dir.path());
        assert_eq!(exit_code(agent.wait_exit(WAIT)), EXIT_USAGE, "{args:?}");
    }
}

/// Today's date in UTC as `YYYY-MM-DD` (civil-from-days, no date crate needed).
fn utc_date_today() -> String {
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970")
        .as_secs()
        / 86_400;
    let z = i64::try_from(days).expect("days fit") + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[tokio::test]
async fn logs_written_under_data_dir() {
    let dir = TempDir::new().unwrap();
    let utc_before = utc_date_today();
    let mut agent = AgentProc::start(dir.path(), &[]);
    let (mut client, _) = agent.connect().await;
    client
        .call(RequestBody::ShutdownAgentSafely(Empty {}))
        .await
        .unwrap();
    assert_eq!(exit_code(agent.wait_exit(WAIT)), 0);
    let utc_after = utc_date_today();

    let logs = log_files(dir.path());
    let names: Vec<&str> = logs.iter().map(|(name, _)| name.as_str()).collect();
    assert!(
        names.contains(&format!("agent.log.{utc_before}").as_str())
            || names.contains(&format!("agent.log.{utc_after}").as_str()),
        "the log file name carries the UTC date ({utc_before}): {names:?}"
    );

    assert!(!logs.is_empty(), "a log file exists under <data-dir>\\logs");
    assert!(
        logs.iter().all(|(name, _)| name.starts_with("agent.log.")),
        "daily rolling names: {logs:?}"
    );
    assert!(
        logs.iter().any(|(_, text)| text.contains("agent ready")),
        "{logs:?}"
    );
}

#[test]
fn data_dir_with_reparse_point_exits_6() {
    let dir = TempDir::new().unwrap();
    let elsewhere = TempDir::new().unwrap();
    // A junction needs no privilege (a symlink would, and the test would then pass vacuously
    // by skipping). If the junction cannot be made, the test FAILS rather than passing.
    let link = dir.path().join("logs");
    let made = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(elsewhere.path())
        .output()
        .expect("run cmd /C mklink");
    assert!(
        made.status.success(),
        "mklink /J failed, so this test cannot check anything: {}{}",
        String::from_utf8_lossy(&made.stdout),
        String::from_utf8_lossy(&made.stderr)
    );
    assert!(
        std::fs::symlink_metadata(&link).is_ok() && std::fs::read_dir(&link).is_ok(),
        "the junction exists and resolves"
    );

    let mut agent = AgentProc::start(dir.path(), &[]);

    assert_eq!(exit_code(agent.wait_exit(WAIT)), EXIT_DATA_DIR);
    assert_eq!(
        std::fs::read_dir(elsewhere.path()).unwrap().count(),
        0,
        "nothing is written through the reparse point"
    );
}
