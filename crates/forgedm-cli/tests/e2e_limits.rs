//! `forgedm limits`, `pause-all`, `resume-all` and `priority` through the real
//! `forgedm-agent.exe` and `forgedm-fixture.exe` (PLAN S1.13). Exit codes follow A.8.
//!
//! Build the binaries first: `cargo build -p forgedm-agent -p forgedm-cli -p forgedm-fixture --bins`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

mod support;

use std::time::{Duration, Instant};

use support::{AgentProc, FixtureProc, code, run_cli};
use tempfile::TempDir;

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn ok(data: &TempDir, args: &[&str]) -> String {
    let out = run_cli(data.path(), args);
    assert_eq!(
        code(&out),
        0,
        "{args:?}: stdout {} stderr {}",
        text(&out.stdout),
        text(&out.stderr)
    );
    text(&out.stdout)
}

fn started() -> (TempDir, AgentProc) {
    let data = TempDir::new().unwrap();
    let agent = AgentProc::start(data.path());
    agent.wait_ready();
    (data, agent)
}

fn add(data: &TempDir, target: &TempDir, url: &str, name: &str) -> String {
    let out = ok(
        data,
        &[
            "add",
            url,
            "--dir",
            target.path().to_str().unwrap(),
            "--file-name",
            name,
            "--connections",
            "1",
        ],
    );
    out.split_whitespace()
        .nth(1)
        .expect("an id after `added`")
        .to_owned()
}

fn wait_for_state(data: &TempDir, id: &str, wanted: &str) {
    let started = Instant::now();
    loop {
        let shown = ok(data, &["get", id]);
        if shown
            .lines()
            .any(|l| l.starts_with("state") && l.contains(wanted))
        {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "timed out waiting for {wanted}: {shown}"
        );
        std::thread::sleep(Duration::from_millis(150));
    }
}

#[test]
fn limits_show_set_and_clear() {
    let (data, _agent) = started();

    let shown = ok(&data, &["limits"]);
    assert!(shown.contains("max active downloads 3"), "{shown}");
    assert!(shown.contains("speed limit off"), "{shown}");
    assert!(shown.contains("connections per download 8"), "{shown}");

    let set = ok(
        &data,
        &[
            "limits",
            "--max-active",
            "2",
            "--speed",
            "1048576",
            "--connections",
            "4",
        ],
    );
    assert!(set.contains("max active downloads 2"), "{set}");
    assert!(set.contains("1048576 B/s"), "{set}");
    assert!(set.contains("connections per download 4"), "{set}");

    let cleared = ok(&data, &["limits", "--speed", "off"]);
    assert!(cleared.contains("speed limit off"), "{cleared}");
    assert!(
        cleared.contains("max active downloads 2"),
        "other limits stay: {cleared}"
    );
}

#[test]
fn bad_values_are_usage_errors_and_an_unknown_download_is_an_agent_error() {
    let (data, _agent) = started();

    for args in [
        &["limits", "--max-active", "0"][..],
        &["limits", "--speed", "10"],
        &["limits", "--connections", "99"],
        &["priority", "not-an-id", "high"],
    ] {
        assert_eq!(code(&run_cli(data.path(), args)), 2, "{args:?}");
    }
    let unknown = run_cli(
        data.path(),
        &["priority", "018f0000-0000-7000-8000-000000000000", "high"],
    );
    assert_eq!(code(&unknown), 11, "{}", text(&unknown.stderr));
}

#[test]
fn without_an_agent_the_new_commands_exit_10() {
    let data = TempDir::new().unwrap();
    for args in [&["limits"][..], &["pause-all"], &["resume-all"]] {
        assert_eq!(code(&run_cli(data.path(), args)), 10, "{args:?}");
    }
}

#[test]
fn pause_all_resume_all_and_priority_end_to_end() {
    let (data, _agent) = started();
    let target = TempDir::new().unwrap();
    let fixture = FixtureProc::start();
    let url = |seed: u32| format!("{}/f/{seed}/500000?slow=200000", fixture.base_url());
    let first = add(&data, &target, &url(31), "one.bin");
    let second = add(&data, &target, &url(32), "two.bin");
    wait_for_state(&data, &first, "downloading");

    let prio = ok(&data, &["priority", &second, "high"]);
    assert!(prio.contains("priority high"), "{prio}");
    let paused = ok(&data, &["pause-all"]);
    assert!(paused.contains(&format!("paused {first}")), "{paused}");
    assert!(paused.contains(&format!("paused {second}")), "{paused}");
    assert!(paused.contains("2 paused, 0 skipped"), "{paused}");
    wait_for_state(&data, &first, "paused");

    let resumed = ok(&data, &["resume-all"]);
    assert!(resumed.contains("2 resumed"), "{resumed}");
    wait_for_state(&data, &first, "completed");
    wait_for_state(&data, &second, "completed");
}
