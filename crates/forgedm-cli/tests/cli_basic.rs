//! Process tests for the `forgedm` CLI against a real `forgedm-agent` (PLAN S0.9).

mod support;

use support::{AgentProc, code, run_cli, run_cli_raw};

#[test]
fn status_without_agent_exits_10_with_message() {
    let dir = tempfile::tempdir().unwrap();

    let out = run_cli(dir.path(), &["status"]);

    assert_eq!(code(&out), 10);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not running"), "stderr: {stderr}");
    assert!(out.stdout.is_empty(), "nothing on stdout");
}

#[test]
fn status_does_not_start_an_agent() {
    let dir = tempfile::tempdir().unwrap();

    let first = run_cli(dir.path(), &["status"]);
    let second = run_cli(dir.path(), &["status"]);

    assert_eq!(code(&first), 10);
    assert_eq!(code(&second), 10, "status must never auto-start the agent");
    assert!(!dir.path().join("run").exists(), "no agent touched the dir");
}

#[test]
fn status_with_real_agent_prints_versions() {
    let dir = tempfile::tempdir().unwrap();
    let agent = AgentProc::start(dir.path());
    agent.wait_ready();

    let out = run_cli(dir.path(), &["status"]);

    assert_eq!(
        code(&out),
        0,
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let version = env!("CARGO_PKG_VERSION");
    assert!(
        stdout.contains(&format!("agent {version}")),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("protocol 1."), "stdout: {stdout}");
    assert!(
        stdout.contains(&format!("pid {}", agent.pid())),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("ready"), "stdout: {stdout}");
}

#[test]
fn list_empty_and_json_shape() {
    let dir = tempfile::tempdir().unwrap();
    let agent = AgentProc::start(dir.path());
    agent.wait_ready();

    let json = run_cli(dir.path(), &["list", "--json"]);
    let human = run_cli(dir.path(), &["list"]);

    assert_eq!(code(&json), 0);
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value, serde_json::json!({"items": [], "total": 0}));
    assert_eq!(code(&human), 0);
    assert!(String::from_utf8_lossy(&human.stdout).contains("No downloads"));
}

#[test]
fn unknown_subcommand_exits_2() {
    let out = run_cli_raw(&["frobnicate"]);

    assert_eq!(code(&out), 2);
    assert!(!out.stderr.is_empty());
}

#[test]
fn relative_data_dir_exits_2() {
    let out = run_cli_raw(&["--data-dir", r"relative\dir", "status"]);

    assert_eq!(code(&out), 2);
}

#[test]
fn agent_stop_without_agent_exits_10() {
    let dir = tempfile::tempdir().unwrap();

    let out = run_cli(dir.path(), &["agent", "stop"]);

    assert_eq!(code(&out), 10);
}

#[test]
fn agent_stop_shuts_agent_down() {
    let dir = tempfile::tempdir().unwrap();
    let mut agent = AgentProc::start(dir.path());
    agent.wait_ready();

    let out = run_cli(dir.path(), &["agent", "stop"]);

    assert_eq!(
        code(&out),
        0,
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let status = agent
        .wait_exit(support::WAIT)
        .expect("agent exits after stop");
    assert_eq!(status.code(), Some(0));
    let after = run_cli(dir.path(), &["status"]);
    assert_eq!(code(&after), 10);
}

#[test]
fn cli_not_next_to_the_real_agent_exits_14() {
    // A copy of the CLI in another directory expects `<that dir>\forgedm-agent.exe`, which
    // is not the running agent's image: the client must refuse to talk (fail closed).
    let data = tempfile::tempdir().unwrap();
    let agent = AgentProc::start(data.path());
    agent.wait_ready();
    let elsewhere = tempfile::tempdir().unwrap();
    let copy = elsewhere.path().join("forgedm.exe");
    std::fs::copy(support::cli_exe(), &copy).unwrap();

    let out = std::process::Command::new(&copy)
        .arg("--data-dir")
        .arg(data.path())
        .arg("status")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();

    assert_eq!(
        code(&out),
        14,
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "no data may be accepted from the peer"
    );
}

// ---- output errors (PHASE_0_REPORT 9a: CLI output write errors) -------------------------

#[test]
fn output_to_a_closed_pipe_is_a_quiet_success() {
    use std::process::{Command, Stdio};
    let dir = tempfile::tempdir().unwrap();
    let agent = AgentProc::start(dir.path());
    agent.wait_ready();

    let mut child = Command::new(support::cli_exe())
        .arg("--data-dir")
        .arg(dir.path())
        .arg("status")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The reader goes away before the CLI has anything to say (like `forgedm status | head -0`).
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();

    assert_eq!(code(&out), 0, "a closed pipe is not an error");
    assert!(
        out.stderr.is_empty(),
        "and it is quiet: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn output_write_failure_other_than_a_closed_pipe_exits_1() {
    use std::process::{Command, Stdio};
    let dir = tempfile::tempdir().unwrap();
    let agent = AgentProc::start(dir.path());
    agent.wait_ready();
    // A file opened for reading only: every write to it fails with "access denied".
    let read_only = dir.path().join("read-only.txt");
    std::fs::write(&read_only, b"x").unwrap();
    let stdout = std::fs::File::open(&read_only).unwrap();

    let out = Command::new(support::cli_exe())
        .arg("--data-dir")
        .arg(dir.path())
        .arg("status")
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(Stdio::piped())
        .output()
        .unwrap();

    assert_eq!(
        code(&out),
        1,
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("cannot write to standard output"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
