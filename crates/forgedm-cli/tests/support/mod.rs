//! Shared helpers for the CLI process tests: locating both binaries, a guard that always
//! kills a spawned agent, and a runner for the `forgedm` binary.
#![allow(dead_code, clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test support fails by panicking

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

/// How long a test waits for the agent to come up or to exit.
pub const WAIT: Duration = Duration::from_secs(20);

/// Restricts an existing `dir` to the current user, SYSTEM and Administrators, as the agent
/// requires of an explicit `--data-dir` (S1.14). Temporary directories inherit
/// machine-specific ACEs, so tests harden theirs before an agent can be started on it.
pub fn harden(dir: &Path) {
    if !dir.is_dir() {
        return;
    }
    let user = format!(
        r"{}\{}",
        std::env::var("USERDOMAIN").expect("USERDOMAIN"),
        std::env::var("USERNAME").expect("USERNAME")
    );
    let out = Command::new("icacls")
        .arg(dir)
        .args(["/inheritance:r", "/grant:r"])
        .arg(format!("{user}:(OI)(CI)F"))
        .args(["*S-1-5-18:(OI)(CI)F", "*S-1-5-32-544:(OI)(CI)F"])
        .output()
        .expect("run icacls");
    assert!(
        out.status.success(),
        "icacls failed: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

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
    harden(data_dir);
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
        harden(data_dir);
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

    /// Kills the agent at once (TerminateProcess) and reaps it: a crash, not a shutdown.
    pub fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
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

/// `forgedm-fixture.exe` next to the CLI binary. Fails (never skips) when it is missing.
pub fn fixture_exe() -> PathBuf {
    let sibling = cli_exe().with_file_name("forgedm-fixture.exe");
    assert!(
        sibling.is_file(),
        "{} is missing: run `cargo build -p forgedm-agent -p forgedm-cli -p forgedm-fixture --bins` first",
        sibling.display()
    );
    sibling
}

/// A running `forgedm-fixture.exe serve`, killed when dropped.
pub struct FixtureProc {
    child: Option<Child>,
    base_url: String,
}

impl FixtureProc {
    /// Starts the fixture on an ephemeral loopback port and reads the base URL it prints.
    pub fn start() -> Self {
        let mut child = Command::new(fixture_exe())
            .args(["serve", "--port", "0"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn fixture");
        let stdout = child.stdout.take().expect("fixture stdout");
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .expect("fixture base url");
        let base_url = line.trim().to_owned();
        assert!(
            base_url.starts_with("http://127.0.0.1:"),
            "unexpected fixture output: {line:?}"
        );
        Self {
            child: Some(child),
            base_url,
        }
    }

    /// `http://127.0.0.1:PORT`.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The fixture's own SHA-256 of `size` bytes of the `seed` stream.
    pub fn hash(seed: u64, size: u64) -> String {
        let out = Command::new(fixture_exe())
            .args([
                "hash",
                "--seed",
                &seed.to_string(),
                "--size",
                &size.to_string(),
            ])
            .output()
            .expect("run fixture hash");
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }
}

impl Drop for FixtureProc {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Runs `forgedm --data-dir <dir> <args...>` with `stdin_text` on standard input.
pub fn run_cli_with_stdin(data_dir: &Path, args: &[&str], stdin_text: &str) -> Output {
    harden(data_dir);
    let mut child = Command::new(cli_exe())
        .arg("--data-dir")
        .arg(data_dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn forgedm");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin_text.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("wait forgedm")
}

/// Stops an agent that the CLI started on its own: asks it to exit, then kills it by pid if
/// it is still there. Dropped at the end of every test that lets `add` auto-start an agent.
pub struct AutoStartedAgent {
    data_dir: PathBuf,
}

impl AutoStartedAgent {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
        }
    }

    fn pid(&self) -> Option<u32> {
        let out = run_cli(&self.data_dir, &["status"]);
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let after = text.split("(pid ").nth(1)?;
        after.split(')').next()?.trim().parse().ok()
    }
}

impl Drop for AutoStartedAgent {
    fn drop(&mut self) {
        let pid = self.pid();
        let _ = run_cli(&self.data_dir, &["agent", "stop"]);
        std::thread::sleep(Duration::from_millis(500));
        if let Some(pid) = pid {
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

/// The result of [`run_cli_to_files`].
pub struct FileRun {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Runs `forgedm --data-dir <dir> <args...>` with stdout and stderr redirected to files in
/// `scratch`, and waits for the process itself.
///
/// Used where the CLI may start an agent: the agent inherits the CLI's inheritable handles,
/// so with pipes `Command::output` would wait for the agent to exit (see the open issue
/// recorded for S1.7 about `ipc::bootstrap`).
pub fn run_cli_to_files(data_dir: &Path, args: &[&str], scratch: &Path) -> FileRun {
    harden(data_dir);
    let out_path = scratch.join("cli-stdout.txt");
    let err_path = scratch.join("cli-stderr.txt");
    let status = Command::new(cli_exe())
        .arg("--data-dir")
        .arg(data_dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(&out_path).expect("stdout file"))
        .stderr(std::fs::File::create(&err_path).expect("stderr file"))
        .status()
        .expect("run forgedm");
    FileRun {
        code: status.code().expect("exit code"),
        stdout: std::fs::read_to_string(out_path).unwrap_or_default(),
        stderr: std::fs::read_to_string(err_path).unwrap_or_default(),
    }
}
