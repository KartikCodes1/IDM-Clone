//! Crash recovery through the real `forgedm-agent.exe` (PLAN S1.14): every fault point of
//! `FORGEDM_FAULT` ends the process at the worst moment, a second agent over the same data
//! directory and files must finish with the exact bytes, and repeated force-kills of a
//! segmented download must never corrupt or inflate what is committed.
//!
//! Run with `cargo test -p forgedm-agent --features fault-injection --test crash_matrix`.
//! Without the feature this file compiles to nothing (the agent has no fault hook then).
#![cfg(feature = "fault-injection")]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

mod support;

use std::os::windows::fs::FileExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use forgedm_agent::fault::FAULT_EXIT_CODE;
use forgedm_core::fault::FaultPoint;
use forgedm_fixture::{FixtureServer, content, verify};
use forgedm_ipc::Client;
use forgedm_protocol::{
    DownloadDetail, DownloadIdRequest, DownloadState, EnqueueUrlRequest, RequestBody, ResultBody,
};
use support::AgentProc;
use tempfile::TempDir;
use uuid::Uuid;

const MIB: u64 = 1024 * 1024;
const WAIT: Duration = Duration::from_secs(120);
/// Bytes the kernel and hyper may hold between the server and the client, plus the progress
/// between the last poll and a kill (the same slack as the first proof).
const IN_FLIGHT_SLACK: u64 = 6 * MIB;

struct Setup {
    data: TempDir,
    target: TempDir,
    fixture: FixtureServer,
}

impl Setup {
    async fn new() -> Self {
        Self {
            data: TempDir::new().unwrap(),
            target: TempDir::new().unwrap(),
            fixture: FixtureServer::start_on(0).await.unwrap(),
        }
    }

    fn data(&self) -> &Path {
        self.data.path()
    }

    fn target(&self) -> &Path {
        self.target.path()
    }

    fn url(&self, path_and_query: &str) -> String {
        format!("{}{path_and_query}", self.fixture.base_url())
    }

    /// Starts an agent that ends itself at `point`.
    fn faulty(&self, point: FaultPoint) -> AgentProc {
        AgentProc::start_with_env(self.data(), "none", &[("FORGEDM_FAULT", point.name())])
    }

    /// Starts a normal agent over the same data directory.
    fn healthy(&self) -> AgentProc {
        AgentProc::start(self.data(), &[])
    }
}

async fn enqueue(client: &mut Client, url: &str, dir: &Path, name: &str) -> Uuid {
    let mut request = EnqueueUrlRequest::new(url);
    request.target_dir = Some(dir.to_str().unwrap().to_owned());
    request.file_name = Some(name.to_owned());
    match client.call(RequestBody::EnqueueUrl(request)).await.unwrap() {
        ResultBody::EnqueueUrl(result) => result.download_id,
        other => panic!("not an enqueue result: {other:?}"),
    }
}

async fn detail(client: &mut Client, id: Uuid) -> DownloadDetail {
    let body = RequestBody::GetDownload(DownloadIdRequest { download_id: id });
    match client.call(body).await.unwrap() {
        ResultBody::GetDownload(detail) => *detail,
        other => panic!("not a detail: {other:?}"),
    }
}

async fn wait_for(
    client: &mut Client,
    id: Uuid,
    what: &str,
    done: impl Fn(&DownloadDetail) -> bool,
) -> DownloadDetail {
    let started = Instant::now();
    loop {
        let now = detail(client, id).await;
        if done(&now) {
            return now;
        }
        assert!(
            started.elapsed() < WAIT,
            "timed out waiting for {what}; state {:?}",
            now.summary.state
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn parts_in(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with(".forgedm.part"))
        .collect()
}

fn zone_identifier(path: &Path) -> Option<String> {
    std::fs::read_to_string(format!("{}:Zone.Identifier", path.display())).ok()
}

/// A scenario that reaches `point`: a small single stream for most, a straggler segment
/// (fast prefix, slow tail) for the split.
fn scenario(point: FaultPoint) -> (u64, u64, String) {
    match point {
        FaultPoint::MidSplit => {
            // Four 12 MiB segments; the last is slow from its first byte. When the fast
            // workers finish, 8 MiB or more remain on it and it is split.
            let size = 48 * MIB;
            (61, size, format!("?slow=655360&slow_from={}", 36 * MIB))
        }
        _ => (62, 2 * MIB, "?slow=655360".to_owned()),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn crash_matrix_each_fault_point_recovers_exact_sha() {
    for point in FaultPoint::ALL {
        let setup = Setup::new().await;
        let (seed, size, query) = scenario(point);
        let url = setup.url(&format!("/f/{seed}/{size}{query}"));

        let mut faulty = setup.faulty(point);
        let (mut client, _) = faulty.connect().await;
        let id = enqueue(&mut client, &url, setup.target(), "matrix.bin").await;
        let status = faulty
            .wait_exit(WAIT)
            .unwrap_or_else(|| panic!("{}: the agent never reached its fault point", point.name()));
        assert_eq!(
            status.code(),
            Some(FAULT_EXIT_CODE),
            "{}: the agent ended at the fault point",
            point.name()
        );
        drop(client);

        let healthy = setup.healthy();
        let (mut client, _) = healthy.connect().await;
        let done = wait_for(&mut client, id, "completion after the crash", |d| {
            matches!(
                d.summary.state,
                DownloadState::Completed | DownloadState::Failed
            )
        })
        .await;

        assert_eq!(
            done.summary.state,
            DownloadState::Completed,
            "{}: {:?}",
            point.name(),
            done.summary.last_error
        );
        let final_path = PathBuf::from(done.summary.final_path.clone().expect("final path"));
        verify::file_matches(&final_path, seed, size).unwrap();
        assert_eq!(
            done.checksum.actual.as_deref(),
            Some(hex::encode(content::sha256(seed, size)).as_str()),
            "{}: the agent's digest is the generator's",
            point.name()
        );
        assert!(
            parts_in(setup.target()).is_empty(),
            "{}: no staging file is left: {:?}",
            point.name(),
            parts_in(setup.target())
        );
        let files: Vec<_> = std::fs::read_dir(setup.target())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(
            files.len(),
            1,
            "{}: exactly one file: {files:?}",
            point.name()
        );
        let zone = zone_identifier(&final_path).expect("Zone.Identifier on the final file");
        assert!(zone.contains("ZoneId=3"), "{}: {zone}", point.name());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn zone_identifier_present_at_finalizing_before_rename() {
    let setup = Setup::new().await;
    let (seed, size) = (63u64, MIB);
    let url = setup.url(&format!("/f/{seed}/{size}?token=CANARY_Q"));

    let mut faulty = setup.faulty(FaultPoint::FinalizingBeforeRename);
    let (mut client, _) = faulty.connect().await;
    let id = enqueue(&mut client, &url, setup.target(), "zone.bin").await;
    let status = faulty.wait_exit(WAIT).expect("the fault point was reached");
    assert_eq!(status.code(), Some(FAULT_EXIT_CODE));
    drop(client);

    // The crash happened before the rename: the stream is already on the staging file, so
    // the final name can never be visible without it.
    let parts = parts_in(setup.target());
    assert_eq!(parts.len(), 1, "{parts:?}");
    let zone = zone_identifier(&parts[0]).expect("Zone.Identifier on the .part");
    assert!(zone.contains("ZoneId=3"), "{zone}");
    assert!(!zone.contains("CANARY_Q"), "no query in the stream: {zone}");
    assert!(
        !setup.target().join("zone.bin").exists(),
        "nothing has the final name yet"
    );

    let healthy = setup.healthy();
    let (mut client, _) = healthy.connect().await;
    let done = wait_for(&mut client, id, "completion", |d| {
        d.summary.state == DownloadState::Completed
    })
    .await;
    let final_path = PathBuf::from(done.summary.final_path.unwrap());
    assert!(zone_identifier(&final_path).unwrap().contains("ZoneId=3"));
}

#[tokio::test(flavor = "multi_thread")]
async fn zone_identifier_on_all_completed_files() {
    let setup = Setup::new().await;
    let agent = setup.healthy();
    let (mut client, _) = agent.connect().await;
    let cases = [
        ("single.bin", format!("/f/64/{}?token=CANARY_Q", 300_000)),
        ("segmented.bin", format!("/f/65/{}?token=CANARY_Q", 9 * MIB)),
        ("empty.bin", "/f/66/0?token=CANARY_Q".to_owned()),
        ("unknown.bin", format!("/f/67/{}?cl=0&ranges=0", 200_000)),
    ];
    let mut ids = Vec::new();
    for (name, path) in &cases {
        ids.push(enqueue(&mut client, &setup.url(path), setup.target(), name).await);
    }
    // Same name again: the no-replace move picks `name (1).ext`, and that one is marked too.
    ids.push(
        enqueue(
            &mut client,
            &setup.url("/f/64/300000?token=CANARY_Q"),
            setup.target(),
            "single.bin",
        )
        .await,
    );

    for id in ids {
        let done = wait_for(&mut client, id, "completion", |d| {
            matches!(
                d.summary.state,
                DownloadState::Completed | DownloadState::Failed
            )
        })
        .await;
        assert_eq!(
            done.summary.state,
            DownloadState::Completed,
            "{:?}",
            done.summary.last_error
        );
        let path = PathBuf::from(done.summary.final_path.unwrap());
        let zone = zone_identifier(&path)
            .unwrap_or_else(|| panic!("{} has no Zone.Identifier", path.display()));
        assert!(zone.contains("ZoneId=3"), "{}: {zone}", path.display());
        assert!(!zone.contains("CANARY_Q"), "{}: {zone}", path.display());
    }
}

/// What a killed agent left, read from the `.part` and the database.
struct Snapshot {
    committed: u64,
    segments: Vec<(u64, u64)>,
}

fn snapshot_of(d: &DownloadDetail) -> Snapshot {
    Snapshot {
        committed: d.summary.committed_bytes,
        segments: d.segments.iter().map(|s| (s.start, s.committed)).collect(),
    }
}

fn range_matches(path: &Path, seed: u64, start: u64, len: u64) {
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
        content::fill(seed, pos, &mut expected[..n]);
        assert_eq!(actual[..n], expected[..n], "mismatch at {pos}");
        pos += n as u64;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn multi_segment_force_kill_x3_no_corruption_no_inflation() {
    let setup = Setup::new().await;
    let (seed, size) = (71u64, 32 * MIB);
    let slow_from = size - 4 * MIB;
    let url = setup.url(&format!(
        "/f/{seed}/{size}?slow=655360&slow_from={slow_from}"
    ));

    let mut agent = setup.healthy();
    let (mut client, _) = agent.connect().await;
    let id = enqueue(&mut client, &url, setup.target(), "kill.bin").await;
    let mut previous = 0u64;
    let mut uncommitted_total = 0u64;
    let mut part: Option<PathBuf> = None;

    for round in 1..=3u32 {
        // Wait for fresh durable progress beyond the last round, then kill mid-download.
        let target_bytes = size * u64::from(round) * 20 / 100;
        let before = wait_for(&mut client, id, "durable progress", |d| {
            d.summary.committed_bytes >= target_bytes
        })
        .await;
        assert_ne!(
            before.summary.state,
            DownloadState::Completed,
            "kill mid-download"
        );
        let accepted = before.summary.accepted_bytes;
        let seen = snapshot_of(&before);
        agent.kill();
        drop(client);

        let part_path = part
            .get_or_insert_with(|| parts_in(setup.target()).remove(0))
            .clone();
        for (start, len) in &seen.segments {
            range_matches(&part_path, seed, *start, *len);
        }
        uncommitted_total += accepted.saturating_sub(seen.committed);

        agent = setup.healthy();
        let (mut fresh, _) = agent.connect().await;
        // The first look is the database as the killed agent left it, before the new
        // agent's own first checkpoint (two seconds away).
        let after = snapshot_of(&detail(&mut fresh, id).await);
        let db_sum: u64 = after.segments.iter().map(|s| s.1).sum();
        assert_eq!(
            after.committed, db_sum,
            "round {round}: committed equals the DB sum"
        );
        assert!(
            after.committed >= seen.committed.max(previous),
            "round {round}: commits only grow ({} < {})",
            after.committed,
            seen.committed.max(previous)
        );
        assert!(
            after.committed <= accepted.max(seen.committed) + IN_FLIGHT_SLACK,
            "round {round}: nothing is committed that was never accepted: {} > {accepted}",
            after.committed
        );
        for (start, len) in &after.segments {
            range_matches(&part_path, seed, *start, *len);
        }
        previous = after.committed;
        client = fresh;
    }

    let done = wait_for(&mut client, id, "completion after three kills", |d| {
        d.summary.state == DownloadState::Completed
    })
    .await;
    let final_path = PathBuf::from(done.summary.final_path.unwrap());
    verify::file_matches(&final_path, seed, size).unwrap();
    assert_eq!(
        done.checksum.actual.as_deref(),
        Some(hex::encode(content::sha256(seed, size)).as_str())
    );
    let served = setup.fixture.log().served_bytes();
    let bound = size + uncommitted_total + 3 * IN_FLIGHT_SLACK;
    assert!(
        served <= bound,
        "served {served} bytes; bound {bound} (size {size} + uncommitted {uncommitted_total} + slack)"
    );
}
