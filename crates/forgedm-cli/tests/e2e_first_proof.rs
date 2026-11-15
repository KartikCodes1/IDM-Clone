//! The FIRST PROOF (PLAN S1.9, milestone M1) through the real binaries: `forgedm add` a slow
//! file, kill the agent with TerminateProcess once at least 30% is committed, check the
//! committed bytes on disk against the generator, restart, and check that the download
//! resumes (never re-requesting committed bytes) and ends byte-identical.
//!
//! A matching hash alone does not pass: the request log must show the resume, and the bytes
//! served must stay bounded. Every spawned process is killed by a drop guard.
//!
//! Build the binaries first: `cargo build -p forgedm-agent -p forgedm-cli -p forgedm-fixture --bins`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stderr
)] // test code (the proof prints its evidence figures)

mod support;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use forgedm_fixture::verify;
use serde_json::Value;
use support::{AgentProc, FixtureProc, code, run_cli, run_cli_with_stdin};
use tempfile::TempDir;

const MIB: u64 = 1024 * 1024;
const WAIT: Duration = Duration::from_secs(120);
/// Bytes the kernel and hyper may hold between the server handing them over and the client
/// reading them, plus the progress between the last poll and the kill.
const IN_FLIGHT_SLACK: u64 = 6 * MIB;

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// `forgedm get <id> --json` as a JSON value.
fn get_json(data: &Path, id: &str) -> Value {
    let out = run_cli(data, &["get", id, "--json"]);
    assert_eq!(code(&out), 0, "get failed: {}", text(&out.stderr));
    serde_json::from_slice(&out.stdout).expect("get --json prints one JSON object")
}

fn number(value: &Value, key: &str) -> u64 {
    value[key]
        .as_u64()
        .unwrap_or_else(|| panic!("{key} in {value}"))
}

/// The id printed by `forgedm add`: `added <id> <name>`.
fn added_id(stdout: &str) -> String {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("added "))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("no 'added <id>' line in: {stdout}"))
        .to_owned()
}

/// The fixture's request log (`GET /__log`) over a plain TCP connection.
fn fixture_log(base_url: &str) -> Value {
    let host = base_url.trim_start_matches("http://");
    let mut stream = TcpStream::connect(host).expect("connect to the fixture");
    stream
        .write_all(
            format!("GET /__log HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .expect("send");
    let mut raw = String::new();
    stream.read_to_string(&mut raw).expect("read the log");
    let body = raw.split("\r\n\r\n").nth(1).expect("an HTTP body");
    serde_json::from_str(body).expect("the log is JSON")
}

/// Start of a `bytes=A-B` range.
fn range_start(range: &str) -> u64 {
    range
        .strip_prefix("bytes=")
        .and_then(|r| r.split('-').next())
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("not a byte range: {range}"))
}

/// Checks that bytes `[start, start + len)` of `path` are the generator's bytes (S1.11: a
/// segmented download commits per segment, so what is durable is a set of ranges, not a
/// prefix).
fn range_matches(path: &Path, seed: u64, start: u64, len: u64) {
    use std::os::windows::fs::FileExt;
    let file = std::fs::File::open(path).expect("open the .part");
    let (mut pos, end) = (start, start + len);
    let mut actual = vec![0u8; 64 * 1024];
    let mut expected = vec![0u8; 64 * 1024];
    while pos < end {
        let n = usize::try_from((end - pos).min(64 * 1024)).unwrap();
        let mut got = 0;
        while got < n {
            let read = file
                .seek_read(&mut actual[got..n], pos + got as u64)
                .expect("read the .part");
            assert!(read > 0, "the .part ends before {end}");
            got += read;
        }
        forgedm_fixture::content::fill(seed, pos, &mut expected[..n]);
        assert_eq!(actual[..n], expected[..n], "mismatch at {pos}");
        pos += n as u64;
    }
}

/// `(start, committed bytes)` of every segment in a `get --json` object.
fn committed_ranges(detail: &Value) -> Vec<(u64, u64)> {
    detail["segments"]
        .as_array()
        .expect("segments")
        .iter()
        .map(|s| (number(s, "start"), number(s, "committed")))
        .collect()
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(started.elapsed() < WAIT, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn part_file(target: &Path) -> PathBuf {
    let found: Vec<PathBuf> = std::fs::read_dir(target)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with(".forgedm.part"))
        .collect();
    assert_eq!(found.len(), 1, "exactly one staging file: {found:?}");
    found.into_iter().next().unwrap()
}

#[test]
fn resume_after_agent_kill_identical_sha256() {
    let (data, target) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let fixture = FixtureProc::start();
    let (seed, size) = (71u64, 32 * MIB);
    // Fast for most of the file, then a crawl (the fixture caps a throttle at 640 KiB/s), so
    // the kill lands while the download is still running.
    let slow_from = size - 4 * MIB;
    let url = format!(
        "{}/f/{seed}/{size}?slow=655360&slow_from={slow_from}",
        fixture.base_url()
    );
    let mut agent = AgentProc::start(data.path());
    agent.wait_ready();
    let add = run_cli(
        data.path(),
        &[
            "add",
            &url,
            "--dir",
            target.path().to_str().unwrap(),
            "--file-name",
            "proof.bin",
        ],
    );
    assert_eq!(code(&add), 0, "add failed: {}", text(&add.stderr));
    let id = added_id(&text(&add.stdout));

    // Wait for at least 30% committed (a durable checkpoint, not just received bytes).
    let mut last = get_json(data.path(), &id);
    wait_until("30% committed", || {
        last = get_json(data.path(), &id);
        number(&last, "committed_bytes") >= size * 30 / 100
    });
    let committed_before = number(&last, "committed_bytes");
    let accepted_before = number(&last, "accepted_bytes");
    assert_ne!(
        last["state"], "completed",
        "the kill must land mid-download"
    );

    agent.kill();

    // Every committed range of the .part matches the generator.
    let part = part_file(target.path());
    let before_ranges = committed_ranges(&last);
    for (start, len) in &before_ranges {
        range_matches(&part, seed, *start, *len);
    }
    let requests_before_kill = fixture_log(fixture.base_url())["requests"]
        .as_array()
        .unwrap()
        .len();

    // Restart. The first look at the database's segments (before the new agent's own first
    // checkpoint, two seconds away) is what the killed agent left committed.
    let agent2 = AgentProc::start(data.path());
    agent2.wait_ready();
    let restarted = get_json(data.path(), &id);
    let after_ranges = committed_ranges(&restarted);
    let db_committed: u64 = after_ranges.iter().map(|r| r.1).sum();
    assert!(
        db_committed >= committed_before,
        "commits only grow: {db_committed} < {committed_before}"
    );
    for (start, len) in &after_ranges {
        range_matches(&part, seed, *start, *len);
    }

    // Auto-resume finishes it.
    let mut done = restarted;
    wait_until("completion after the restart", || {
        done = get_json(data.path(), &id);
        done["state"] == "completed"
    });

    // Identical bytes: the agent's digest, the file on disk and the fixture's own hash agree.
    let final_path = done["final_path"].as_str().expect("final_path").to_owned();
    verify::file_matches(Path::new(&final_path), seed, size).unwrap();
    let expected = FixtureProc::hash(seed, size);
    assert_eq!(done["checksum"]["actual"], expected.as_str());

    // The request log proves it was a resume, not a re-download.
    let log = fixture_log(fixture.base_url());
    let transfers: Vec<&Value> = log["requests"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["method"] == "GET" && r["range"].as_str() != Some("bytes=0-0"))
        .collect();
    assert!(
        transfers.len() >= 2,
        "a transfer before and after the kill: {transfers:?}"
    );
    assert_eq!(range_start(transfers[0]["range"].as_str().unwrap()), 0);
    // The requests after the kill are the resumed ones: each starts at or after the committed
    // offset of the segment that contains it (S1.11: segments resume independently).
    let resumed_gets: Vec<&Value> = log["requests"].as_array().unwrap()[requests_before_kill..]
        .iter()
        .filter(|r| r["method"] == "GET" && r["range"].as_str() != Some("bytes=0-0"))
        .collect();
    assert!(!resumed_gets.is_empty(), "the restart made requests");
    for resumed in &resumed_gets {
        let start = range_start(resumed["range"].as_str().unwrap());
        let (seg_start, seg_committed) = after_ranges
            .iter()
            .rev()
            .find(|(s, _)| *s <= start)
            .copied()
            .expect("a segment contains the request");
        assert!(
            start >= seg_start + seg_committed,
            "a resumed GET starts at or after its segment's committed offset: {start} < {}",
            seg_start + seg_committed
        );
        assert!(
            resumed["if_range"].is_string(),
            "the resume sends If-Range: {resumed}"
        );
    }
    let served = number(&log, "served_bytes");
    let bound = size + (accepted_before - committed_before) + IN_FLIGHT_SLACK;
    assert!(
        served <= bound,
        "served {served} bytes; bound {bound} (size {size} + uncommitted {} + slack)",
        accepted_before - committed_before
    );
    eprintln!(
        "FIRST PROOF evidence: size={size} committed_before_kill={committed_before} accepted_before_kill={accepted_before} db_committed_after_restart={db_committed} transfers={:?} served={served} bound={bound}",
        transfers
            .iter()
            .map(|t| t["range"].as_str().unwrap_or(""))
            .collect::<Vec<_>>()
    );
    assert!(
        served < size + size / 2,
        "far less than a second full download: {served}"
    );
}

#[test]
fn cli_remove_delete_partial_requires_yes() {
    let (data, target) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let fixture = FixtureProc::start();
    let agent = AgentProc::start(data.path());
    agent.wait_ready();
    let url = format!("{}/f/72/{}?slow=300000", fixture.base_url(), 4 * MIB);
    let add = run_cli(
        data.path(),
        &[
            "add",
            &url,
            "--dir",
            target.path().to_str().unwrap(),
            "--file-name",
            "rm.bin",
        ],
    );
    let id = added_id(&text(&add.stdout));
    wait_until("a staging file", || {
        std::fs::read_dir(target.path()).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".forgedm.part")
        })
    });
    let cancel = run_cli(data.path(), &["cancel", &id]);
    assert_eq!(code(&cancel), 0, "{}", text(&cancel.stderr));
    assert!(
        text(&cancel.stdout).contains("cancelled"),
        "{}",
        text(&cancel.stdout)
    );
    let part = part_file(target.path());

    // Without --yes (and with no terminal to ask) nothing is deleted and the exit code is 2.
    let refused = run_cli(data.path(), &["remove", &id, "--delete-partial"]);

    assert_eq!(code(&refused), 2, "stderr: {}", text(&refused.stderr));
    assert!(
        text(&refused.stderr).contains("--yes"),
        "{}",
        text(&refused.stderr)
    );
    assert!(part.is_file(), "the partial file is untouched");
    assert_eq!(
        code(&run_cli(data.path(), &["get", &id])),
        0,
        "the download is still listed"
    );

    // A plain remove keeps the partial file; --yes deletes it.
    let denied_by_stdin =
        run_cli_with_stdin(data.path(), &["remove", &id, "--delete-partial"], "y\n");
    assert_eq!(
        code(&denied_by_stdin),
        2,
        "stdin that is not a terminal is not a confirmation"
    );
    let removed = run_cli(data.path(), &["remove", &id, "--delete-partial", "--yes"]);
    assert_eq!(code(&removed), 0, "{}", text(&removed.stderr));
    assert!(!part.exists(), "the recorded .part is deleted");
    let again = run_cli(data.path(), &["remove", &id, "--yes"]);
    assert_eq!(code(&again), 0, "a repeated remove is treated as success");
    let missing = run_cli(data.path(), &["get", &id]);
    assert_eq!(
        code(&missing),
        11,
        "the download is gone (agent error not_found)"
    );
}
