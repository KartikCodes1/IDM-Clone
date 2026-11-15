//! Agent bootstrap tests (PHASE_0_1_PLAN S1.8). They spawn the real `forgedm-agent.exe`
//! (build it first: `cargo build -p forgedm-agent --bins --locked`). Every agent started
//! through `ensure_agent` is shut down by a drop guard, so none outlives its test.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use forgedm_ipc::bootstrap::{BootstrapError, BootstrapOptions, ensure_agent, ensure_agent_with};
use forgedm_ipc::paths::pipe_name_for;
use forgedm_ipc::{Client, ClientConfig, IpcError};
use forgedm_protocol::{AgentStatusInfo, ClientKind, Empty, ProxySource, RequestBody, ResultBody};
use tempfile::TempDir;

const SETTLE: Duration = Duration::from_secs(15);
/// The agent's instance-lock retry (3 s, ARCHITECTURE 2.3) plus a margin.
const LOCK_RETRY_WINDOW: Duration = Duration::from_millis(3_500);

/// `forgedm-agent.exe` in the nearest ancestor directory of this test binary that holds it
/// (`target/<profile>`; cargo may nest test binaries below `deps` or `build`).
fn agent_exe() -> PathBuf {
    let test_exe = std::env::current_exe().expect("current exe");
    let agent = test_exe
        .ancestors()
        .skip(1)
        .map(|dir| dir.join("forgedm-agent.exe"))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| {
            panic!(
                "build the agent first: cargo build -p forgedm-agent --bins --locked (searched above {})",
                test_exe.display()
            )
        });
    std::fs::canonicalize(agent).expect("canonical agent path")
}

fn config_for(data_dir: &Path) -> ClientConfig {
    ClientConfig::new(
        pipe_name_for(Some(data_dir)).expect("pipe name"),
        agent_exe(),
    )
}

/// Asks the agent on `data_dir` to shut down and waits until its pipe is gone.
async fn shut_down(data_dir: PathBuf) {
    if let Ok(mut client) = Client::connect(config_for(&data_dir)).await {
        let _ = client
            .handshake(ClientKind::Cli, "bootstrap-test", "0")
            .await;
        let _ = client
            .call(RequestBody::ShutdownAgentSafely(Empty {}))
            .await;
    }
    wait_until_gone(&data_dir).await;
}

async fn wait_until_gone(data_dir: &Path) {
    let started = Instant::now();
    while started.elapsed() < SETTLE {
        if matches!(
            Client::connect(config_for(data_dir)).await,
            Err(IpcError::NotRunning)
        ) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Shuts down whatever agent serves the data directory when dropped.
struct AgentGuard {
    data_dir: PathBuf,
}

impl AgentGuard {
    fn new(dir: &TempDir) -> Self {
        Self {
            data_dir: dir.path().to_path_buf(),
        }
    }
}

impl Drop for AgentGuard {
    fn drop(&mut self) {
        let data_dir = self.data_dir.clone();
        let cleanup = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("cleanup runtime");
            runtime.block_on(shut_down(data_dir));
        });
        let _ = cleanup.join();
    }
}

async fn agent_status(client: &mut Client) -> AgentStatusInfo {
    client
        .handshake(ClientKind::Cli, "bootstrap-test", "0")
        .await
        .expect("handshake");
    match client
        .call(RequestBody::GetAgentStatus(Empty {}))
        .await
        .expect("status")
    {
        ResultBody::GetAgentStatus(status) => status,
        other => panic!("unexpected result {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bootstraps_when_absent() {
    let dir = TempDir::new().unwrap();
    let _guard = AgentGuard::new(&dir);
    assert!(matches!(
        Client::connect(config_for(dir.path())).await,
        Err(IpcError::NotRunning)
    ));

    let mut client = ensure_agent(&agent_exe(), Some(dir.path()))
        .await
        .expect("bootstrap");

    let status = agent_status(&mut client).await;
    assert_ne!(status.agent_pid, 0);
    assert!(dir.path().join("forgedm.db").is_file());
    let mut again = ensure_agent(&agent_exe(), Some(dir.path()))
        .await
        .expect("already running");
    assert_eq!(agent_status(&mut again).await.agent_pid, status.agent_pid);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn five_concurrent_bootstraps_one_agent() {
    let dir = TempDir::new().unwrap();
    let _guard = AgentGuard::new(&dir);
    let agent = agent_exe();

    let mut tasks = Vec::new();
    for _ in 0..5 {
        let agent = agent.clone();
        let data_dir = dir.path().to_path_buf();
        tasks.push(tokio::spawn(async move {
            let mut client = ensure_agent(&agent, Some(&data_dir)).await?;
            Ok::<_, BootstrapError>(agent_status(&mut client).await.agent_pid)
        }));
    }
    let mut pids = HashSet::new();
    for task in tasks {
        pids.insert(task.await.unwrap().expect("every caller connects"));
    }

    assert_eq!(pids.len(), 1, "all callers reached one agent: {pids:?}");
    // Losing agents retry the instance lock for up to 3 s before exiting 3. If the winner
    // were shut down earlier, a loser would take over and outlive the test.
    tokio::time::sleep(LOCK_RETRY_WINDOW).await;
}

#[tokio::test]
async fn missing_sibling_binary_errors_no_path_search() {
    let dir = TempDir::new().unwrap();
    let _guard = AgentGuard::new(&dir);
    let missing = dir.path().join("forgedm-agent.exe");

    let absent = ensure_agent(&missing, Some(dir.path())).await;
    assert!(
        matches!(absent, Err(BootstrapError::AgentBinaryMissing(_))),
        "{absent:?}"
    );

    // A bare name would be resolved through PATH by CreateProcess; it must be refused.
    let bare = ensure_agent(Path::new("forgedm-agent.exe"), Some(dir.path())).await;
    assert!(
        matches!(bare, Err(BootstrapError::AgentPathNotAbsolute(_))),
        "{bare:?}"
    );

    assert!(matches!(
        Client::connect(config_for(dir.path())).await,
        Err(IpcError::NotRunning)
    ));
}

#[tokio::test]
async fn relative_data_dir_is_refused_before_spawning() {
    let result = ensure_agent(&agent_exe(), Some(Path::new("relative-dir"))).await;
    assert!(
        matches!(result, Err(BootstrapError::DataDirNotAbsolute(_))),
        "{result:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bootstrap_timeout_reported() {
    let dir = TempDir::new().unwrap();
    let _guard = AgentGuard::new(&dir);
    let options = BootstrapOptions::default().with_timeout(Duration::from_millis(1));

    let result = ensure_agent_with(&agent_exe(), Some(dir.path()), &options).await;

    assert!(matches!(result, Err(BootstrapError::Timeout)), "{result:?}");
    // The agent that did not come up in time is killed, not left running.
    wait_until_gone(dir.path()).await;
    assert!(matches!(
        Client::connect(config_for(dir.path())).await,
        Err(IpcError::NotRunning)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bootstrapped_agent_does_not_inherit_proxy_env() {
    let dir = TempDir::new().unwrap();
    let _guard = AgentGuard::new(&dir);
    // The parent environment cannot be edited without unsafe code, so the options inject
    // proxy variables as if inherited; the spawn must remove them (both cases).
    let polluted: Vec<(String, String)> = [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "no_proxy",
    ]
    .iter()
    .map(|name| ((*name).to_owned(), "http://127.0.0.1:9".to_owned()))
    .collect();
    let options = BootstrapOptions::default().with_simulated_parent_env(polluted);

    let mut client = ensure_agent_with(&agent_exe(), Some(dir.path()), &options)
        .await
        .expect("bootstrap");

    let status = agent_status(&mut client).await;
    assert_eq!(status.proxy_source, ProxySource::System);
}
