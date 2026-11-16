//! Bandwidth caps through the real engine (PLAN S1.13): a global limit bounds the whole
//! transfer from below, and a per-download limit changes live. The exact bucket arithmetic is
//! covered by the paused-time tests of S1.3; these runs only check the wiring, in real time.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

mod support;
#[path = "../src/testing.rs"]
#[allow(dead_code)] // each test file uses part of the shared fakes
mod testing;

use std::time::Duration;

use forgedm_core::engine::GlobalLimits;
use forgedm_core::rate_limit::burst_for;
use forgedm_fixture::FixtureServer;
use support::{Harness, MIB, expected_sha};

const KIB: u64 = 1024;

async fn limit_globally(h: &Harness, bps: Option<u64>) {
    let patch = GlobalLimits {
        rate_limit_bps: Some(bps),
        ..GlobalLimits::default()
    };
    let effective = h.engine.update_global_limits(patch).await.unwrap();
    assert_eq!(effective.rate_limit_bps, bps);
}

/// Downloads `size` bytes under a global limit of `rate` and checks the lower bound on the
/// time it took: at best the burst goes through at once, the rest at `rate`.
async fn run_under_global_limit(size: u64, rate: u64, connections: u32) {
    let server = FixtureServer::start_on(0).await.unwrap();
    let h = Harness::new();
    limit_globally(&h, Some(rate)).await;
    let url = format!("{}/f/81/{size}?t=limit", server.base_url());

    let started = tokio::time::Instant::now();
    let (_, event) = h.run(&url, "limited.bin", connections).await;
    let elapsed = started.elapsed();

    let (_, sha) = Harness::completed(&event);
    assert_eq!(sha, expected_sha(81, size));
    let bound = Duration::from_secs_f64(size as f64 / (rate + burst_for(rate)) as f64);
    assert!(
        elapsed >= bound,
        "{size} bytes at {rate} B/s took {elapsed:?}, below the bound {bound:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn global_speed_limit_upper_bound() {
    run_under_global_limit(3 * MIB, MIB, 1).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn global_speed_limit_bounds_segmented_transfers_too() {
    run_under_global_limit(10 * MIB, 4 * MIB, 4).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn per_download_limit_live_change() {
    let server = FixtureServer::start_on(0).await.unwrap();
    let h = Harness::new();
    let size = 24 * MIB;
    // Four connections at about 1 MiB/s each: roughly 4 MiB/s with no limit.
    let url = format!("{}/f/82/{size}?t=live&slow={}", server.base_url(), MIB);
    let id = h.enqueue(&url, "live.bin", 4).await;
    let accepted = |h: &Harness| h.engine.live(id).map_or(0, |sample| sample.accepted);
    while accepted(&h) < 512 * KIB {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let applied = h.engine.set_rate_limit(id, Some(256 * KIB)).await.unwrap();
    assert_eq!(applied, Some(256 * KIB));
    // Let what was already buffered drain, then measure.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let before = accepted(&h);
    tokio::time::sleep(Duration::from_secs(2)).await;
    let throttled = accepted(&h) - before;

    assert!(
        throttled <= 2 * MIB,
        "{throttled} bytes in 2 s under a 256 KiB/s limit"
    );

    let lifted = h.engine.set_rate_limit(id, None).await.unwrap();
    assert_eq!(lifted, None);
    // At 256 KiB/s the remaining ~20 MiB would need well over a minute.
    let event = h.sink.wait_terminal(id, Duration::from_secs(30)).await;
    let (_, sha) = Harness::completed(&event);
    assert_eq!(sha, expected_sha(82, size));
}

#[tokio::test(flavor = "multi_thread")]
async fn rate_limit_of_an_unknown_download_is_not_found() {
    let h = Harness::new();

    let result = h
        .engine
        .set_rate_limit(uuid::Uuid::now_v7(), Some(2048))
        .await;

    assert!(matches!(
        result,
        Err(forgedm_core::error::EngineError::NotFound)
    ));
}
