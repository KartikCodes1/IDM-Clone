//! `forgedm add` and `forgedm list` through the real `forgedm-agent.exe` and
//! `forgedm-fixture.exe` (PLAN S1.7). Every spawned process is killed by a drop guard.
//!
//! Build the binaries first: `cargo build -p forgedm-agent -p forgedm-cli -p forgedm-fixture --bins`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

mod support;

use std::path::Path;

use forgedm_fixture::verify;
use support::{
    AgentProc, AutoStartedAgent, FixtureProc, code, run_cli, run_cli_to_files, run_cli_with_stdin,
};
use tempfile::TempDir;

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn dir_arg(dir: &TempDir) -> String {
    dir.path().to_str().expect("utf-8 temp path").to_owned()
}

struct Setup {
    data: TempDir,
    target: TempDir,
    fixture: FixtureProc,
    /// Held so the agent is killed when the test ends.
    _agent: AgentProc,
}

fn setup() -> Setup {
    let data = TempDir::new().unwrap();
    let target = TempDir::new().unwrap();
    let fixture = FixtureProc::start();
    let agent = AgentProc::start(data.path());
    agent.wait_ready();
    Setup {
        data,
        target,
        fixture,
        _agent: agent,
    }
}

#[test]
fn add_watch_completes_prints_sha() {
    let s = setup();
    let size = 3_000_000u64;
    let sha = FixtureProc::hash(7, size);
    let url = format!("{}/f/7/{size}?slow=1500000", s.fixture.base_url());

    let out = run_cli(
        s.data.path(),
        &[
            "add",
            &url,
            "--dir",
            &dir_arg(&s.target),
            "--file-name",
            "watched.bin",
            "--sha256",
            &sha,
            "--watch",
        ],
    );

    let stdout = text(&out.stdout);
    assert_eq!(
        code(&out),
        0,
        "stdout: {stdout}\nstderr: {}",
        text(&out.stderr)
    );
    assert!(
        stdout.contains(&sha),
        "the verified sha is printed: {stdout}"
    );
    assert!(stdout.contains("completed"), "{stdout}");
    assert!(
        stdout.contains("watched.bin"),
        "the final path is printed: {stdout}"
    );
    let progress: Vec<&str> = stdout
        .lines()
        .filter(|l| l.starts_with("downloading"))
        .collect();
    assert!(
        !progress.is_empty(),
        "true progress is shown while it runs: {stdout}"
    );
    assert!(
        progress.iter().any(|l| l.contains("/s")),
        "speed is shown: {progress:?}"
    );
    assert!(
        progress.iter().any(|l| l.contains("MiB / 2.86 MiB")),
        "bytes against the real total: {progress:?}"
    );
    verify::file_matches(&s.target.path().join("watched.bin"), 7, size).unwrap();
}

#[test]
fn wrong_sha_exits_13() {
    let s = setup();
    let url = format!("{}/f/8/200000", s.fixture.base_url());
    let wrong = "0".repeat(64);

    let out = run_cli(
        s.data.path(),
        &[
            "add",
            &url,
            "--dir",
            &dir_arg(&s.target),
            "--file-name",
            "wrong.bin",
            "--sha256",
            &wrong,
            "--watch",
        ],
    );

    let combined = format!("{}{}", text(&out.stdout), text(&out.stderr));
    assert_eq!(code(&out), 13, "{combined}");
    assert!(combined.contains("checksum_mismatch"), "{combined}");
    assert!(
        !s.target.path().join("wrong.bin").exists(),
        "a mismatching file is never finalized"
    );
}

#[test]
fn url_from_stdin_keeps_secret_out_of_argv() {
    let s = setup();
    let size = 100_000u64;
    let url = format!(
        "{}/f/9/{size}?token=SUPERSECRET42&t=stdin",
        s.fixture.base_url()
    );

    let out = run_cli_with_stdin(
        s.data.path(),
        &[
            "add",
            "-",
            "--dir",
            &dir_arg(&s.target),
            "--file-name",
            "stdin.bin",
            "--watch",
        ],
        &format!("{url}\n"),
    );

    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(code(&out), 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(!stdout.contains("SUPERSECRET42") && !stderr.contains("SUPERSECRET42"));
    verify::file_matches(&s.target.path().join("stdin.bin"), 9, size).unwrap();
    let listed = text(&run_cli(s.data.path(), &["list", "--json"]).stdout);
    assert!(
        !listed.contains("SUPERSECRET42"),
        "the list shows only the redacted URL: {listed}"
    );
    assert!(listed.contains("/f/9/100000"), "{listed}");
    let table = text(&run_cli(s.data.path(), &["list"]).stdout);
    assert!(!table.contains("SUPERSECRET42"));
}

#[test]
fn list_status_real_states() {
    let s = setup();
    let ok = format!("{}/f/10/50000", s.fixture.base_url());
    let bad = format!("{}/f/10/50000?status=404", s.fixture.base_url());
    let dir = dir_arg(&s.target);
    let done = run_cli(
        s.data.path(),
        &[
            "add",
            &ok,
            "--dir",
            &dir,
            "--file-name",
            "good.bin",
            "--watch",
        ],
    );
    let failed = run_cli(
        s.data.path(),
        &[
            "add",
            &bad,
            "--dir",
            &dir,
            "--file-name",
            "bad.bin",
            "--watch",
        ],
    );
    assert_eq!(code(&done), 0, "{}", text(&done.stderr));
    assert_eq!(
        code(&failed),
        13,
        "a failed download exits 13 under --watch"
    );

    let json = run_cli(s.data.path(), &["list", "--json"]);
    let status = run_cli(s.data.path(), &["status"]);

    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let items = value["items"].as_array().unwrap();
    let state_of = |name: &str| {
        items
            .iter()
            .find(|i| i["display_name"] == name)
            .map(|i| i["state"].as_str().unwrap().to_owned())
    };
    assert_eq!(state_of("good.bin").as_deref(), Some("completed"));
    assert_eq!(state_of("bad.bin").as_deref(), Some("failed"));
    let bad_item = items
        .iter()
        .find(|i| i["display_name"] == "bad.bin")
        .unwrap();
    assert_eq!(bad_item["last_error"]["code"], "http_status");
    assert_eq!(bad_item["last_error"]["details"]["status"], 404);
    assert_eq!(value["total"], 2);
    let human = text(&run_cli(s.data.path(), &["list"]).stdout);
    assert!(
        human.contains("completed") && human.contains("failed"),
        "{human}"
    );
    assert!(
        text(&status.stdout).contains("0 active, 0 queued"),
        "{}",
        text(&status.stdout)
    );
}

#[test]
fn add_auto_starts_agent_unless_no_start() {
    let scratch = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    let target = TempDir::new().unwrap();
    let fixture = FixtureProc::start();
    let url = format!("{}/f/11/20000", fixture.base_url());
    let dir = dir_arg(&target);

    let refused = run_cli(data.path(), &["add", &url, "--dir", &dir, "--no-start"]);

    assert_eq!(
        code(&refused),
        10,
        "--no-start with no agent: {}",
        text(&refused.stderr)
    );
    assert!(!data.path().join("run").exists(), "nothing was started");
    let _cleanup = AutoStartedAgent::new(data.path());
    let started = run_cli_to_files(
        data.path(),
        &[
            "add",
            &url,
            "--dir",
            &dir,
            "--file-name",
            "auto.bin",
            "--watch",
        ],
        scratch.path(),
    );
    assert_eq!(
        started.code, 0,
        "stdout: {}
stderr: {}",
        started.stdout, started.stderr
    );
    verify::file_matches(&Path::new(&dir).join("auto.bin"), 11, 20_000).unwrap();
    assert!(
        run_cli(data.path(), &["status"]).status.success(),
        "the agent is now running"
    );
}
