//! S1.3 token bucket (ARCHITECTURE 4.11, ADR-013). All timing uses tokio paused time.

use std::sync::Arc;
use std::time::Duration;

use forgedm_core::clock::Cancelled;
use forgedm_core::rate_limit::{ACQUIRE_CHUNK, MIN_BURST, TokenBucket, burst_for};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;

fn near(actual: Duration, expected: Duration, slack: Duration) -> bool {
    actual + slack >= expected && actual <= expected + slack
}

#[test]
fn burst_is_max_of_250ms_and_64kib() {
    assert_eq!(MIN_BURST, 64 * KIB);
    assert_eq!(ACQUIRE_CHUNK, 64 * KIB);
    assert_eq!(burst_for(0), MIN_BURST);
    assert_eq!(burst_for(KIB), MIN_BURST);
    assert_eq!(burst_for(MIB), 256 * KIB);
    assert_eq!(burst_for(40 * MIB), 10 * MIB);
}

#[tokio::test(start_paused = true)]
async fn bucket_rate_within_burst() {
    let bucket = TokenBucket::new(MIB);
    let cancel = CancellationToken::new();
    let start = Instant::now();

    // The initial burst (256 KiB at 1 MiB/s) is available immediately.
    bucket.acquire(256 * KIB, &cancel).await.unwrap();
    assert_eq!(start.elapsed(), Duration::ZERO);

    // Past the burst, throughput is the configured rate: 2 MiB takes about 2 s.
    bucket.acquire(2 * MIB, &cancel).await.unwrap();
    let elapsed = start.elapsed();
    assert!(
        near(elapsed, Duration::from_secs(2), Duration::from_millis(100)),
        "{elapsed:?}"
    );

    // Long-run average never exceeds the rate (plus the one burst).
    bucket.acquire(8 * MIB, &cancel).await.unwrap();
    let total = 256 * KIB + 2 * MIB + 8 * MIB;
    let allowed = (total - 256 * KIB) / MIB;
    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_secs(allowed) - Duration::from_millis(100),
        "{elapsed:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn rate_change_live() {
    let bucket = Arc::new(TokenBucket::new(100 * KIB));
    let cancel = CancellationToken::new();
    bucket.acquire(64 * KIB, &cancel).await.unwrap(); // drain the burst
    let start = Instant::now();

    let waiter = {
        let (bucket, cancel) = (Arc::clone(&bucket), cancel.clone());
        tokio::spawn(async move { bucket.acquire(MIB, &cancel).await })
    };
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(!waiter.is_finished(), "1 MiB at 100 KiB/s takes about 10 s");

    bucket.set_rate(10 * MIB);
    assert_eq!(bucket.rate(), 10 * MIB);
    waiter.await.unwrap().unwrap();
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(1500),
        "new rate applied to the waiter: {elapsed:?}"
    );

    // Lowering the rate slows later acquires.
    bucket.set_rate(64 * KIB);
    bucket.acquire(64 * KIB, &cancel).await.unwrap(); // refill to the new (smaller) burst
    let before = Instant::now();
    bucket.acquire(128 * KIB, &cancel).await.unwrap();
    assert!(before.elapsed() >= Duration::from_millis(1900));

    // Unlimited releases a waiter at once.
    let waiter = {
        let (bucket, cancel) = (Arc::clone(&bucket), cancel.clone());
        tokio::spawn(async move { bucket.acquire(8 * MIB, &cancel).await })
    };
    tokio::time::sleep(Duration::from_millis(10)).await;
    bucket.set_rate(0);
    let before = Instant::now();
    waiter.await.unwrap().unwrap();
    assert!(before.elapsed() <= Duration::from_millis(5));
}

#[tokio::test(start_paused = true)]
async fn unlimited_never_waits() {
    let bucket = TokenBucket::unlimited();
    assert_eq!(bucket.rate(), 0);
    let cancel = CancellationToken::new();
    let start = Instant::now();
    bucket.acquire(1024 * MIB, &cancel).await.unwrap();
    bucket.acquire(0, &cancel).await.unwrap();
    assert_eq!(start.elapsed(), Duration::ZERO);
}

#[tokio::test(start_paused = true)]
async fn hierarchical_both_enforced() {
    let cancel = CancellationToken::new();

    // The global limit is the slower one.
    let global = Arc::new(TokenBucket::new(MIB));
    let child = TokenBucket::with_parent(4 * MIB, Arc::clone(&global));
    let start = Instant::now();
    child.acquire(256 * KIB + 2 * MIB, &cancel).await.unwrap();
    let elapsed = start.elapsed();
    assert!(
        near(elapsed, Duration::from_secs(2), Duration::from_millis(150)),
        "{elapsed:?}"
    );

    // The per-download limit is the slower one.
    let global = Arc::new(TokenBucket::new(8 * MIB));
    let child = TokenBucket::with_parent(MIB, Arc::clone(&global));
    let start = Instant::now();
    child.acquire(256 * KIB + 2 * MIB, &cancel).await.unwrap();
    let elapsed = start.elapsed();
    assert!(
        near(elapsed, Duration::from_secs(2), Duration::from_millis(150)),
        "{elapsed:?}"
    );

    // An unlimited child still obeys a limited parent; two children share the parent.
    let global = Arc::new(TokenBucket::new(MIB));
    let a = Arc::new(TokenBucket::with_parent(0, Arc::clone(&global)));
    let b = Arc::new(TokenBucket::with_parent(0, Arc::clone(&global)));
    let start = Instant::now();
    let tasks: Vec<_> = [a, b]
        .into_iter()
        .map(|bucket| {
            let cancel = cancel.clone();
            tokio::spawn(async move { bucket.acquire(MIB, &cancel).await })
        })
        .collect();
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    // 2 MiB total, 256 KiB burst, 1 MiB/s: about 1.75 s.
    let elapsed = start.elapsed();
    assert!(
        near(
            elapsed,
            Duration::from_millis(1750),
            Duration::from_millis(200)
        ),
        "{elapsed:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn n_above_burst_no_deadlock() {
    let bucket = TokenBucket::new(64 * KIB); // burst == 64 KiB
    let cancel = CancellationToken::new();
    let start = Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(60), bucket.acquire(MIB, &cancel)).await;
    assert!(result.is_ok(), "an acquire far above the burst must finish");
    result.unwrap().unwrap();
    // 1 MiB minus the 64 KiB burst at 64 KiB/s: 15 s.
    let elapsed = start.elapsed();
    assert!(
        near(elapsed, Duration::from_secs(15), Duration::from_millis(300)),
        "{elapsed:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn cancel_while_waiting_prompt() {
    let bucket = Arc::new(TokenBucket::new(KIB));
    let cancel = CancellationToken::new();
    bucket.acquire(64 * KIB, &cancel).await.unwrap(); // drain the burst
    let start = Instant::now();

    let waiter = {
        let (bucket, cancel) = (Arc::clone(&bucket), cancel.clone());
        tokio::spawn(async move { bucket.acquire(64 * KIB, &cancel).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    cancel.cancel();
    let result = waiter.await.unwrap();
    assert_eq!(result, Err(Cancelled));
    assert!(
        start.elapsed() <= Duration::from_millis(150),
        "{:?}",
        start.elapsed()
    );

    // An already-cancelled token fails without waiting, even when unlimited.
    let unlimited = TokenBucket::unlimited();
    assert_eq!(unlimited.acquire(1, &cancel).await, Err(Cancelled));
}
