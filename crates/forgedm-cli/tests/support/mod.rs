//! Shared helpers for the CLI process tests: locating both binaries, a guard that always
//! kills a spawned agent, and a runner for the `forgedm` binary.
#![allow(dead_code, clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test support fails by panicking

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

/// How long a test waits for the agent to come up or to exit.
pub const WAIT: Duration = Duration::from_secs(20);

/// The CLI binary built for this test run.
pub fn cli_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_forgedm"))
}

/// `forgedm-agent.exe` next to the CLI binary. Fails (never skips) when it is missing.
pub fn agent_exe() -> PathBuf {
    let sibling = cli_exe().with_file_name("forgedm-agent.exe");
    assert!(
        sibling.is_file(),
        "{} is missing: run `cargo build --workspace --bins` (or `cargo build -p forgedm-agent -p forgedm-cli --bins`) first",
        sibling.display()
    );
    sibling
}

/// Runs `forgedm --data-dir <dir> <args...>` to completion.
pub fn run_cli(data_dir: &Path, args: &[&str]) -> Output {
    Command::new(cli_exe())
        .arg("--data-dir")
        .arg(data_dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run forgedm")
}

/// Runs `forgedm <args...>` with no `--data-dir`.
pub fn run_cli_raw(args: &[&str]) -> Output {
    Command::new(cli_exe())
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run forgedm")
}

/// The exit code of a finished process.
pub fn code(output: &Output) -> i32 {
    output.status.code().expect("process exited with a code")
}

/// A running agent child that is killed (and reaped) when dropped.
pub struct AgentProc {
    child: Option<Child>,
    data_dir: PathBuf,
}

impl AgentProc {
    /// Starts the agent with `--data-dir <dir> --proxy none`.
    pub fn start(data_dir: &Path) -> Self {
        let child = Command::new(agent_exe())
            .arg("--data-dir")
            .arg(data_dir)
            .args(["--proxy", "none"])
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

    /// Waits until `forgedm status` succeeds against this agent.
    pub fn wait_ready(&self) {
        let started = Instant::now();
        loop {
            if run_cli(&self.data_dir, &["status"]).status.success() {
                return;
            }
            assert!(started.elapsed() < WAIT, "agent did not become ready");
            std::thread::sleep(Duration::from_millis(100));
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

    /// The agent's process id.
    pub fn pid(&self) -> u32 {
        self.child.as_ref().expect("child present").id()
    }
}

impl Drop for AgentProc {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
