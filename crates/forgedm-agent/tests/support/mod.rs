//! Shared helpers for the agent process tests: a child guard that always kills the agent,
//! a ready-wait connect helper and an exit wait with a deadline.
#![allow(dead_code, clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test support fails by panicking

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use forgedm_agent::config::pipe_name;
use forgedm_ipc::{Client, ClientConfig, IpcError};
use forgedm_protocol::{ClientKind, HandshakeResult};

/// The agent binary built for this test run.
pub fn agent_exe() -> PathBuf {
    std::fs::canonicalize(env!("CARGO_BIN_EXE_forgedm-agent")).expect("agent exe exists")
}

/// How long a test waits for the agent to come up or to exit.
pub const WAIT: Duration = Duration::from_secs(20);

/// A running agent child that is killed (and reaped) when dropped, so a failing test never
/// leaves an agent behind.
pub struct AgentProc {
    child: Option<Child>,
    data_dir: PathBuf,
}

impl AgentProc {
    /// Starts the agent with `--data-dir <dir> --proxy none` plus `extra` arguments.
    pub fn start(data_dir: &Path, extra: &[&str]) -> Self {
        Self::start_raw(
            &["--data-dir", data_dir.to_str().expect("utf-8 path")],
            &["--proxy", "none"],
            extra,
            data_dir,
        )
    }

    /// Starts the agent with exactly the given leading and trailing arguments.
    pub fn start_raw(head: &[&str], proxy: &[&str], extra: &[&str], data_dir: &Path) -> Self {
        let child = Command::new(agent_exe())
            .args(head)
            .args(proxy)
            .args(extra)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn agent");
        Self {
            child: Some(child),
            data_dir: data_dir.to_path_buf(),
        }
    }

    /// Starts an agent with a custom environment edit (for the proxy-source test).
    pub fn start_with_env(data_dir: &Path, proxy: &str, env: &[(&str, &str)]) -> Self {
        let mut cmd = Command::new(agent_exe());
        cmd.args(["--data-dir", data_dir.to_str().expect("utf-8 path")])
            .args(["--proxy", proxy]);
        for name in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "NO_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
            "no_proxy",
        ] {
            cmd.env_remove(name);
        }
        for (key, value) in env {
            cmd.env(key, value);
        }
        let child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn agent");
        Self {
            child: Some(child),
            data_dir: data_dir.to_path_buf(),
        }
    }

    /// The agent's process id.
    pub fn pid(&self) -> u32 {
        self.child.as_ref().expect("child present").id()
    }

    /// The data directory this agent was started with.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Kills the process with `TerminateProcess` and reaps it.
    pub fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Waits for the process to exit by itself, up to `limit`; `None` on timeout.
    pub fn wait_exit(&mut self, limit: Duration) -> Option<ExitStatus> {
        let child = self.child.as_mut()?;
        let started = Instant::now();
        loop {
            if let Some(status) = child.try_wait().expect("try_wait") {
                self.child = None;
                return Some(status);
            }
            if started.elapsed() > limit {
                return None;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Connects (waiting for the agent to come up) and completes the handshake.
    pub async fn connect(&self) -> (Client, HandshakeResult) {
        connect_when_ready(&self.data_dir).await
    }
}

impl Drop for AgentProc {
    fn drop(&mut self) {
        self.kill();
    }
}

/// The client config for the agent serving `data_dir`.
pub fn client_config(data_dir: &Path) -> ClientConfig {
    let name = pipe_name(Some(data_dir)).expect("pipe name");
    ClientConfig::new(name, agent_exe())
}

/// Connects and handshakes, retrying while the agent is still starting.
pub async fn connect_when_ready(data_dir: &Path) -> (Client, HandshakeResult) {
    let started = Instant::now();
    loop {
        match Client::connect(client_config(data_dir)).await {
            Ok(mut client) => {
                match client
                    .handshake(ClientKind::Cli, "agent-test", "0.0.0")
                    .await
                {
                    Ok(result) => return (client, result),
                    Err(err) if started.elapsed() < WAIT => {
                        let _ = err;
                    }
                    Err(err) => panic!("handshake failed: {err}"),
                }
            }
            Err(IpcError::NotRunning | IpcError::Busy | IpcError::Closed)
                if started.elapsed() < WAIT => {}
            Err(err) => panic!("connect failed: {err}"),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The `logs` directory contents as `(file name, bytes)`.
pub fn log_files(data_dir: &Path) -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir(data_dir.join("logs")) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|e| {
            let text = std::fs::read_to_string(e.path()).unwrap_or_default();
            (e.file_name().to_string_lossy().into_owned(), text)
        })
        .collect()
}
