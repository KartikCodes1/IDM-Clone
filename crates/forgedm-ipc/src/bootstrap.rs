//! Client bootstrap: connect to the agent, starting it when it is not running
//! (ARCHITECTURE 2.5, ADR-005, THREAT_MODEL Inv-18).
//!
//! This is the only place in the workspace that spawns a process, and it spawns exactly one
//! thing: the caller-supplied absolute path of the agent executable. There is no PATH
//! search and no environment override. The spawned agent has no window, its own process
//! group, null standard streams and no proxy variables, so its routing does not depend on
//! which client happened to start it. The server is verified (SID and image path) by
//! [`Client::connect`] before any byte is sent.
//!
//! Not verified: whether the agent survives a parent that closes its job object
//! (`CREATE_BREAKAWAY_FROM_JOB` is deliberately not used).

use std::ffi::OsString;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use forgedm_platform::PlatformError;
use tokio::time::{Instant, sleep, timeout_at};

use crate::client::{Client, ClientConfig};
use crate::error::IpcError;
use crate::paths::pipe_name_for;

/// `CREATE_NO_WINDOW`: no console window for the agent.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// `CREATE_NEW_PROCESS_GROUP`: the agent does not receive the client's Ctrl+C.
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
/// How long a bootstrap waits for the agent to come up (PHASE_0_1_PLAN S1.8).
pub const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(10);
/// Pause between connection attempts while the agent starts.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Agent exit code meaning another agent holds the instance lock (A.8).
const EXIT_ALREADY_RUNNING: i32 = 3;
/// Proxy variables removed from the agent's environment, both cases.
const PROXY_ENV_VARS: [&str; 8] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
];

/// Why the agent could not be reached or started. The CLI maps these to exit code 15, except
/// [`BootstrapError::Ipc`] which keeps its own mapping (for example identity failure, 14).
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    /// The agent path is not absolute; a bare name would be searched on `PATH`.
    #[error("the agent path is not absolute: {}", .0.display())]
    AgentPathNotAbsolute(PathBuf),
    /// There is no file at the agent path.
    #[error("the agent executable does not exist: {}", .0.display())]
    AgentBinaryMissing(PathBuf),
    /// `data_dir` is not absolute (the agent would refuse it with exit 2).
    #[error("the data directory is not absolute: {}", .0.display())]
    DataDirNotAbsolute(PathBuf),
    /// The pipe name could not be derived.
    #[error(transparent)]
    Platform(#[from] PlatformError),
    /// The agent process could not be started or inspected.
    #[error("the agent could not be started")]
    Spawn(#[source] std::io::Error),
    /// The agent exited on its own before serving (other than "another agent won").
    #[error("the agent exited before it was ready (exit code {code:?})")]
    AgentExited {
        /// The exit code, `None` if the process was terminated without one.
        code: Option<i32>,
    },
    /// The agent did not serve its pipe in time; it was killed.
    #[error("the agent did not become ready in time")]
    Timeout,
    /// Connecting failed for a reason other than "not running".
    #[error(transparent)]
    Ipc(#[from] IpcError),
}

/// Tunables of [`ensure_agent_with`].
#[derive(Debug, Clone)]
pub struct BootstrapOptions {
    timeout: Duration,
    simulated_parent_env: Vec<(OsString, OsString)>,
}

impl Default for BootstrapOptions {
    fn default() -> Self {
        Self {
            timeout: BOOTSTRAP_TIMEOUT,
            simulated_parent_env: Vec::new(),
        }
    }
}

impl BootstrapOptions {
    /// Replaces the time allowed for the agent to come up (default [`BOOTSTRAP_TIMEOUT`]).
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Test seam: variables set on the spawn command *before* the proxy variables are
    /// removed, standing in for a polluted parent environment. The removal must win.
    #[doc(hidden)]
    #[must_use]
    pub fn with_simulated_parent_env<K, V>(mut self, vars: impl IntoIterator<Item = (K, V)>) -> Self
    where
        K: Into<OsString>,
        V: Into<OsString>,
    {
        self.simulated_parent_env = vars
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        self
    }
}

/// Connects to the agent serving `data_dir` (`None` = the default root), starting
/// `expected_agent_path` first when nothing is listening.
///
/// `expected_agent_path` must be absolute and is also the image the server must run from.
/// Must run inside a tokio runtime.
///
/// # Errors
/// [`BootstrapError`]; see its variants.
pub async fn ensure_agent(
    expected_agent_path: &Path,
    data_dir: Option<&Path>,
) -> Result<Client, BootstrapError> {
    ensure_agent_with(expected_agent_path, data_dir, &BootstrapOptions::default()).await
}

/// [`ensure_agent`] with explicit [`BootstrapOptions`].
///
/// # Errors
/// As [`ensure_agent`].
pub async fn ensure_agent_with(
    expected_agent_path: &Path,
    data_dir: Option<&Path>,
    options: &BootstrapOptions,
) -> Result<Client, BootstrapError> {
    validate_inputs(expected_agent_path, data_dir)?;
    let config = ClientConfig::new(pipe_name_for(data_dir)?, expected_agent_path);
    match Client::connect(config.clone()).await {
        Ok(client) => return Ok(client),
        Err(IpcError::NotRunning) => {}
        Err(other) => return Err(other.into()),
    }
    let child = spawn_agent(expected_agent_path, data_dir, options)?;
    wait_for_agent(&config, child, options.timeout).await
}

fn validate_inputs(agent: &Path, data_dir: Option<&Path>) -> Result<(), BootstrapError> {
    if let Some(dir) = data_dir
        && !dir.is_absolute()
    {
        return Err(BootstrapError::DataDirNotAbsolute(dir.to_path_buf()));
    }
    if !agent.is_absolute() {
        return Err(BootstrapError::AgentPathNotAbsolute(agent.to_path_buf()));
    }
    if !agent.is_file() {
        return Err(BootstrapError::AgentBinaryMissing(agent.to_path_buf()));
    }
    Ok(())
}

/// Starts the agent: explicit path, no window, own process group, null stdio, no proxy env.
fn spawn_agent(
    agent: &Path,
    data_dir: Option<&Path>,
    options: &BootstrapOptions,
) -> Result<Child, BootstrapError> {
    let mut command = Command::new(agent);
    if let Some(dir) = data_dir {
        command.arg("--data-dir").arg(dir);
    }
    for (name, value) in &options.simulated_parent_env {
        command.env(name, value);
    }
    for name in PROXY_ENV_VARS {
        command.env_remove(name);
    }
    command
        .creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(BootstrapError::Spawn)
}

/// Polls the pipe until a verified client connects, our child fails, or time runs out.
///
/// A child that exits with 3 lost the instance-lock race: another agent is (or is about to
/// be) serving, so polling simply continues. Any other early exit is reported. On timeout
/// the child is killed. On success it is left running: it is either the agent or a loser
/// that is about to exit 3 by itself.
async fn wait_for_agent(
    config: &ClientConfig,
    child: Child,
    limit: Duration,
) -> Result<Client, BootstrapError> {
    let deadline = Instant::now() + limit;
    let mut child = Some(child);
    while Instant::now() < deadline {
        match timeout_at(deadline, Client::connect(config.clone())).await {
            Ok(Ok(client)) => return Ok(client),
            Ok(Err(IpcError::NotRunning | IpcError::Busy)) | Err(_) => {}
            Ok(Err(other)) => {
                kill_child(child);
                return Err(other.into());
            }
        }
        check_child(&mut child)?;
        sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now()))).await;
    }
    kill_child(child);
    Err(BootstrapError::Timeout)
}

/// Best effort: a failed kill means the process already exited.
fn kill_child(child: Option<Child>) {
    if let Some(mut running) = child {
        let _ = running.kill();
        let _ = running.wait();
    }
}

/// Drops the handle when the child lost the race (exit 3); fails on any other exit.
fn check_child(child: &mut Option<Child>) -> Result<(), BootstrapError> {
    let Some(running) = child.as_mut() else {
        return Ok(());
    };
    match running.try_wait().map_err(BootstrapError::Spawn)? {
        None => Ok(()),
        Some(status) if status.code() == Some(EXIT_ALREADY_RUNNING) => {
            *child = None;
            Ok(())
        }
        Some(status) => Err(BootstrapError::AgentExited {
            code: status.code(),
        }),
    }
}
