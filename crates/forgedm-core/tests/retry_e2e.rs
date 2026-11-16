//! Retries, Retry-After, host cooldown and URL re-resolve through the real engine (PLAN S1.12).
//! Real HTTP against a scripted server, real files; waits go through a recording sleeper so no
//! test waits for a backoff.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

mod support;
#[path = "../src/testing.rs"]
#[allow(dead_code)] // each test file uses part of the shared fakes
mod testing;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use forgedm_core::client::{ClientOptions, ProxyMode};
use forgedm_core::engine::Control;
use forgedm_core::retry::RETRY_BASE;
use forgedm_protocol::{DownloadErrorCode, DownloadState};
use support::{Content, Harness, MIB, Reply, Req, Scripted, WAIT, expected_sha};

const SIZE: u64 = MIB;

/// Fails the first `n` transfer GETs with `status` (plus `headers`), then serves content.
fn failing_first(
    n: usize,
    status: u16,
    headers: Vec<(&'static str, String)>,
    content: Content,
) -> impl Fn(&Req) -> Reply + Send + Sync + 'static {
    let seen = AtomicUsize::new(0);
    move |req: &Req| {
        if req.is_transfer() && seen.fetch_add(1, Ordering::SeqCst) < n {
            return Reply::Status(status, headers.clone());
        }
        Reply::Content(content.clone())
    }
}

fn assert_file(path: &std::path::Path, seed: u64, size: u64) {
    forgedm_fixture::verify::file_matches(path, seed, size).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn transient_5xx_backoff_within_jitter_bounds() {
    let server = Scripted::start(failing_first(3, 500, vec![], Content::new(1, SIZE))).await;
    let h = Harness::new();

    let (_, event) = h.run(&server.url("/a.bin"), "a.bin", 4).await;

    let (path, sha) = Harness::completed(&event);
    assert_eq!(sha, expected_sha(1, SIZE));
    assert_file(&path, 1, SIZE);
    let sleeps = h.sleeper.sleeps();
    assert_eq!(sleeps.len(), 3, "{sleeps:?}");
    for (attempt, wait) in sleeps.iter().enumerate() {
        let ceiling = RETRY_BASE * 2_u32.pow(u32::try_from(attempt).unwrap());
        assert!(
            *wait <= ceiling,
            "retry {attempt} waited {wait:?} > {ceiling:?}"
        );
    }
    assert_eq!(server.transfers().len(), 4, "3 failures and one success");
}

#[tokio::test(flavor = "multi_thread")]
async fn retry_after_honored_429_503() {
    for status in [429_u16, 503] {
        let retry_after = vec![("Retry-After", "3".to_owned())];
        let server =
            Scripted::start(failing_first(1, status, retry_after, Content::new(2, SIZE))).await;
        let h = Harness::new();

        let (_, event) = h.run(&server.url("/b.bin"), "b.bin", 4).await;

        let (path, _) = Harness::completed(&event);
        assert_file(&path, 2, SIZE);
        let waits = h.sleeper.sleeps();
        assert!(
            waits
                .iter()
                .any(|w| *w >= Duration::from_millis(2900) && *w <= Duration::from_secs(3)),
            "status {status}: Retry-After 3 s was not honoured: {waits:?}"
        );
        assert_eq!(server.transfers().len(), 2, "status {status}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn beyond_cap_enters_retry_waiting_until() {
    let server = Scripted::start(|req: &Req| {
        if req.is_transfer() {
            Reply::Status(429, vec![("Retry-After", "600".to_owned())])
        } else {
            Reply::Content(Content::new(3, SIZE))
        }
    })
    .await;
    let h = Harness::new();
    let before = forgedm_core::ports::unix_ms();

    let id = h.enqueue(&server.url("/c.bin"), "c.bin", 4).await;
    h.store
        .wait_state(id, |s| s == DownloadState::RetryWaiting, WAIT)
        .await;

    let row = h.store.row(id).unwrap();
    let due = row.next_retry_at_ms.expect("the due time is stored");
    assert!(due >= before + 599_000 && due <= before + 610_000, "{due}");
    assert_eq!(row.job.state_reason.as_deref(), Some("rate_limited"));
    assert_eq!(server.transfers().len(), 1, "no request while waiting");
    assert!(h.sleeper.sleeps().is_empty(), "parked, not slept inline");
    // A user can still cancel while it waits.
    let state = h.engine.control(id, Control::Cancel).await.unwrap();
    assert_eq!(state, DownloadState::Cancelled);
}

#[tokio::test(flavor = "multi_thread")]
async fn retry_after_over_an_hour_fails_rate_limited() {
    let server = Scripted::start(|req: &Req| {
        if req.is_transfer() {
            Reply::Status(503, vec![("Retry-After", "7200".to_owned())])
        } else {
            Reply::Content(Content::new(4, SIZE))
        }
    })
    .await;
    let h = Harness::new();

    let (id, event) = h.run(&server.url("/d.bin"), "d.bin", 4).await;

    assert_eq!(
        Harness::failure_code(&event),
        DownloadErrorCode::RateLimited
    );
    assert_eq!(h.store.state(id), Some(DownloadState::Failed));
}

#[tokio::test(flavor = "multi_thread")]
async fn due_retry_requeues_and_completes() {
    // The first transfer asks for 130 s (beyond the inline limit); the timer is shortened by
    // the test through the stored due time of a download recovered by a restarted engine.
    let server = Scripted::start(failing_first(
        1,
        429,
        vec![("Retry-After", "130".to_owned())],
        Content::new(5, SIZE),
    ))
    .await;
    let h = Harness::new();
    let id = h.enqueue(&server.url("/e.bin"), "e.bin", 4).await;
    h.store
        .wait_state(id, |s| s == DownloadState::RetryWaiting, WAIT)
        .await;

    // Retry now (Resume on retry_waiting): queued, then it runs and completes.
    let state = h.engine.control(id, Control::Resume).await.unwrap();

    assert_eq!(state, DownloadState::Queued);
    let event = h.sink.wait_terminal(id, WAIT).await;
    assert_file(&Harness::completed(&event).0, 5, SIZE);
}

#[tokio::test(flavor = "multi_thread")]
async fn n429_cooldown_pauses_all_segments_no_storm() {
    let size = 32 * MIB;
    let throttled = Arc::new(AtomicUsize::new(0));
    let counter = throttled.clone();
    let server = Scripted::start(move |req: &Req| {
        if req.is_transfer() && counter.fetch_add(1, Ordering::SeqCst) < 4 {
            return Reply::Status(429, vec![("Retry-After", "2".to_owned())]);
        }
        Reply::Content(Content::new(6, size))
    })
    .await;
    let h = Harness::new();

    let (_, event) = h.run(&server.url("/f.bin"), "f.bin", 4).await;

    let (path, sha) = Harness::completed(&event);
    assert_eq!(sha, expected_sha(6, size));
    assert_file(&path, 6, size);
    let holds = h
        .sleeper
        .sleeps()
        .into_iter()
        .filter(|w| *w >= Duration::from_millis(1900) && *w <= Duration::from_secs(2))
        .count();
    assert!(
        holds >= 4,
        "every connection waits the cooldown out: {holds}"
    );
    let transfers = server.transfers();
    let rejected = transfers.len() - transfers.iter().filter(|r| r.range.is_some()).count();
    assert_eq!(rejected, 0, "every transfer GET is ranged");
    assert_eq!(
        throttled.load(Ordering::SeqCst),
        transfers.len(),
        "the counter saw each transfer once"
    );
    // Four throttled requests (one per connection), then the retries: no retry storm.
    assert!(transfers.len() <= 4 + 4 + 8, "{} requests", transfers.len());
}

#[tokio::test(flavor = "multi_thread")]
async fn attempt_budget_limits_request_count() {
    let server = Scripted::start(|req: &Req| {
        if req.is_transfer() {
            Reply::Status(500, vec![])
        } else {
            Reply::Content(Content::new(7, SIZE))
        }
    })
    .await;
    let h = Harness::new();

    let (id, event) = h.run(&server.url("/g.bin"), "g.bin", 4).await;

    let error = Harness::failure(&event);
    assert_eq!(error.code, DownloadErrorCode::RetryBudgetExhausted);
    assert_eq!(
        error.details.and_then(|d| d.last),
        Some(DownloadErrorCode::HttpStatus)
    );
    assert_eq!(
        server.transfers().len(),
        8,
        "8 consecutive failures per segment"
    );
    assert_eq!(h.sleeper.sleeps().len(), 7);
    assert_eq!(h.store.state(id), Some(DownloadState::Failed));
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_during_retry_wait_immediate() {
    let server = Scripted::start(failing_first(100, 500, vec![], Content::new(8, SIZE))).await;
    let h = Harness::blocking();
    let id = h.enqueue(&server.url("/h.bin"), "h.bin", 4).await;
    let deadline = tokio::time::Instant::now() + WAIT;
    while h.sleeper.sleeps().is_empty() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "never started waiting"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let started = tokio::time::Instant::now();
    let state = h.engine.control(id, Control::Cancel).await.unwrap();

    assert_eq!(state, DownloadState::Cancelled);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn n401_needs_credentials_single_request() {
    let server = Scripted::start(|req: &Req| {
        if req.is_transfer() {
            Reply::Status(
                401,
                vec![("WWW-Authenticate", "Basic realm=\"x\"".to_owned())],
            )
        } else {
            Reply::Content(Content::new(9, SIZE))
        }
    })
    .await;
    let h = Harness::new();
    let id = h.enqueue(&server.url("/i.bin"), "i.bin", 4).await;

    h.store
        .wait_state(id, |s| s == DownloadState::NeedsCredentials, WAIT)
        .await;

    assert_eq!(server.transfers().len(), 1);
    assert!(h.sleeper.sleeps().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn n404_410_one_reresolve_then_needs_url_refresh() {
    for status in [404_u16, 410] {
        let server = Scripted::start(move |req: &Req| {
            if req.is_transfer() {
                Reply::Status(status, vec![])
            } else {
                Reply::Content(Content::new(10, SIZE))
            }
        })
        .await;
        let h = Harness::new();
        let id = h.enqueue(&server.url("/j.bin"), "j.bin", 4).await;

        h.store
            .wait_state(id, |s| s == DownloadState::NeedsUrlRefresh, WAIT)
            .await;

        let transfers = server.transfers();
        assert_eq!(
            transfers.len(),
            2,
            "status {status}: one request and one re-resolve"
        );
        assert!(
            h.sleeper.sleeps().is_empty(),
            "a re-resolve does not back off"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn re_resolve_recovers_when_the_link_works_again() {
    let server = Scripted::start(failing_first(1, 403, vec![], Content::new(11, SIZE))).await;
    let h = Harness::new();

    let (_, event) = h.run(&server.url("/k.bin"), "k.bin", 4).await;

    assert_file(&Harness::completed(&event).0, 11, SIZE);
    assert_eq!(server.transfers().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn re_resolve_with_another_identity_is_remote_changed() {
    let seen = AtomicUsize::new(0);
    let server = Scripted::start(move |req: &Req| {
        if req.is_transfer() && seen.fetch_add(1, Ordering::SeqCst) == 0 {
            return Reply::Status(404, vec![]);
        }
        let mut content = Content::new(12, SIZE);
        if req.is_transfer() {
            content.etag = "v2";
        }
        Reply::Content(content)
    })
    .await;
    let h = Harness::new();

    let (_, event) = h.run(&server.url("/l.bin"), "l.bin", 4).await;

    assert_eq!(
        Harness::failure_code(&event),
        DownloadErrorCode::RemoteChanged
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn mid_chunk_drop_resumes_from_claim_no_overlap() {
    const CUT: u64 = 700_000;
    let seen = AtomicUsize::new(0);
    let server = Scripted::start(move |req: &Req| {
        let mut content = Content::new(13, 2 * SIZE);
        if req.is_transfer() && seen.fetch_add(1, Ordering::SeqCst) < 2 {
            content.drop_after = Some(CUT);
        }
        Reply::Content(content)
    })
    .await;
    let h = Harness::new();

    let (_, event) = h.run(&server.url("/m.bin"), "m.bin", 1).await;

    let (path, sha) = Harness::completed(&event);
    assert_eq!(sha, expected_sha(13, 2 * SIZE));
    assert_file(&path, 13, 2 * SIZE);
    let starts: Vec<u64> = server
        .transfers()
        .iter()
        .map(|r| r.range.unwrap().0)
        .collect();
    assert_eq!(
        starts,
        vec![0, CUT, 2 * CUT],
        "each retry starts at the claim"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn stall_times_out_and_retries() {
    let seen = AtomicUsize::new(0);
    let server = Scripted::start(move |req: &Req| {
        let mut content = Content::new(14, SIZE);
        if req.is_transfer() && seen.fetch_add(1, Ordering::SeqCst) == 0 {
            content.stall_after = Some(100_000);
        }
        Reply::Content(content)
    })
    .await;
    let mut options = ClientOptions::new(ProxyMode::None);
    options.read_timeout = Duration::from_millis(400);
    let h = Harness::with_client(options);

    let (_, event) = h.run(&server.url("/n.bin"), "n.bin", 1).await;

    assert_file(&Harness::completed(&event).0, 14, SIZE);
    assert_eq!(h.sleeper.sleeps().len(), 1, "one timeout, one backoff");
    let starts: Vec<u64> = server
        .transfers()
        .iter()
        .map(|r| r.range.unwrap().0)
        .collect();
    assert_eq!(starts, vec![0, 100_000]);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "slow tier: waits for the real 15 s connect timeout (several times over)"]
async fn connect_timeout_15s_blackhole() {
    let h = Harness::new();

    // 10.255.255.1 is not routable: the SYN is never answered.
    let (_, event) = h.run("http://10.255.255.1:81/x.bin", "x.bin", 1).await;

    let error = Harness::failure(&event);
    assert_eq!(error.code, DownloadErrorCode::NetworkTimeout);
    assert_eq!(h.sleeper.sleeps().len(), 3, "limited retries");
}

/// A timer armed for an earlier due time must not requeue a download that parked again with
/// a later one (S1.12-rust-reviewer-1): the stale timer fires at ~0.7 s, the real due time is
/// a minute away.
#[tokio::test(flavor = "multi_thread")]
async fn stale_timer_does_not_requeue_before_the_new_due_time() {
    use forgedm_core::ports::{DownloadStore, StoredJob, StoredProgress, unix_ms};
    let server = Scripted::start(|_: &Req| Reply::Content(Content::new(6, SIZE))).await;
    let h = Harness::new();
    let id = uuid::Uuid::now_v7();
    h.store.seed(StoredJob {
        id,
        state: DownloadState::RetryWaiting,
        state_reason: Some("rate_limited".to_owned()),
        url: forgedm_core::url_policy::validate_user_url(&server.url("/s.bin")).unwrap(),
        target_dir: h.target(),
        requested_name: Some("s.bin".to_owned()),
        display_name: "s.bin".to_owned(),
        file_name: None,
        expected_sha256: None,
        part_path: None,
        part_file_id: None,
        transfer_mode: None,
        progress: StoredProgress::default(),
        max_connections: 4,
        priority: forgedm_protocol::Priority::Normal,
        queue_position: 0,
        created_at_ms: 0,
    });
    h.store.schedule_retry(id, unix_ms() + 700).await.unwrap();
    h.engine.recover().await.unwrap(); // arms the early timer
    h.store
        .schedule_retry(id, unix_ms() + 60_000)
        .await
        .unwrap();
    h.engine.recover().await.unwrap(); // arms the one for the later time

    tokio::time::sleep(Duration::from_millis(1800)).await;

    assert_eq!(h.store.state(id), Some(DownloadState::RetryWaiting));
    assert!(
        server.requests().is_empty(),
        "no request before the due time"
    );
}
