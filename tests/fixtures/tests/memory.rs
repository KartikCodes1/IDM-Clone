//! Regression: no scenario may buffer unbounded data. The fixture runs as a separate process and
//! its working set is sampled (via `tasklist`) while adversarial requests run, so the check
//! covers the server's real memory use. Windows only, like the rest of ForgeDM.

mod common;

use common::{Client, get};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_forgedm-fixture");
const KIB_PER_MIB: u64 = 1024;
/// The task budget is about 300 MB for any process; the fixture needs a small fraction.
const PEAK_BUDGET_MIB: u64 = 100;
const SAMPLE_EVERY: Duration = Duration::from_millis(50);
const STREAMED_BYTES: usize = 24 * 1024 * 1024;
const TERABYTE: u64 = 1_000_000_000_000;

/// Kills the child on drop, so a failing assertion never leaks a server process.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Working set of `pid` in KiB, from `tasklist /FO CSV /NH` (last field, e.g. `"12,345 K"`).
fn working_set_kib(pid: u32) -> Option<u64> {
    let out = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let field = text.trim().rsplit("\",\"").next()?;
    let digits: String = field.chars().filter(char::is_ascii_digit).collect();
    digits.parse().ok()
}

fn spawn_server() -> (Server, String) {
    let mut child = Command::new(BIN)
        .args(["serve", "--port", "0"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn");
    let mut line = String::new();
    BufReader::new(child.stdout.take().expect("stdout"))
        .read_line(&mut line)
        .expect("read");
    (Server(child), line.trim().to_owned())
}

async fn adversarial_requests(base: &str) {
    // Overlapping multipart ranges over huge sizes: the sum must not be allocated.
    for size in [u64::MAX, 1 << 63, TERABYTE] {
        let target = format!("/f/1/{size}?multipart=1");
        let mut c = Client::connect(base).await;
        let head = c
            .send_head_only("GET", &target, &[("Range", "bytes=0-,0-,0-")])
            .await;
        assert!(head.status == 200 || head.status == 206, "{}", head.status);
    }
    // More parts than the fixture assembles in memory.
    let many = format!("bytes={}", vec!["0-0"; 400].join(","));
    let r = get(base, "/f/1/1000?multipart=1", &[("Range", many.as_str())]).await;
    assert_eq!(r.status, 200, "falls back to the plain response");
    // A terabyte body is streamed chunk by chunk, never buffered.
    let mut c = Client::connect(base).await;
    let head = c
        .send_head_only("GET", &format!("/f/2/{TERABYTE}"), &[])
        .await;
    assert_eq!(head.status, 200);
    let mut read = 0;
    while read < STREAMED_BYTES {
        read += c.read_n(64 * 1024).await.len();
    }
    drop(c);
    // An abort scenario over a huge body.
    let r = get(base, &format!("/f/3/{TERABYTE}?drop_after=2000000"), &[]).await;
    assert!(!r.complete && r.body.len() <= 2_000_000);
}

#[tokio::test]
async fn adversarial_scenarios_keep_the_server_working_set_small() {
    let (server, base) = spawn_server();
    let pid = server.0.id();
    let stop = Arc::new(AtomicBool::new(false));
    let peak = Arc::new(AtomicU64::new(working_set_kib(pid).unwrap_or(0)));
    let sampler = {
        let (stop, peak) = (Arc::clone(&stop), Arc::clone(&peak));
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if let Some(kib) = working_set_kib(pid) {
                    peak.fetch_max(kib, Ordering::Relaxed);
                }
                thread::sleep(SAMPLE_EVERY);
            }
        })
    };

    adversarial_requests(&base).await;
    assert_eq!(
        get(&base, "/f/1/10", &[]).await.status,
        200,
        "still serving"
    );

    stop.store(true, Ordering::Relaxed);
    sampler.join().expect("sampler thread");
    let peak_mib = peak.load(Ordering::Relaxed) / KIB_PER_MIB;
    assert!(peak_mib > 0, "the sampler never read the working set");
    assert!(
        peak_mib < PEAK_BUDGET_MIB,
        "peak working set {peak_mib} MiB (budget {PEAK_BUDGET_MIB} MiB)"
    );
}
