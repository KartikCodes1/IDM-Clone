//! Token bucket rate limiting (ARCHITECTURE 4.11, ADR-013).
//!
//! A [`TokenBucket`] limits bytes per second with a burst of `max(250 ms of rate, 64 KiB)`.
//! A bucket may have a parent: acquiring from a per-download bucket also acquires from its
//! parent (the global bucket), in pieces of at most [`ACQUIRE_CHUNK`]. The rate can change
//! while tasks are waiting, and every wait ends promptly when the caller is cancelled.
//!
//! Tokens are tracked as integer byte-nanoseconds so no floating point rounding accumulates.
//! Waits use `tokio::time`, so tests run on paused time.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub use crate::clock::Cancelled;

/// Largest number of bytes requested from a bucket in one step (A.9: acquire chunk).
pub const ACQUIRE_CHUNK: u64 = 64 * 1024;

/// Smallest burst, whatever the rate (A.9).
pub const MIN_BURST: u64 = 64 * 1024;

/// Burst window: the bucket holds this much time worth of tokens (A.9: 250 ms of rate).
const BURST_WINDOW_DIVISOR: u64 = 4;

/// Token units per byte: tokens are counted in byte-nanoseconds per second of rate.
const NANOS_PER_SEC: u128 = 1_000_000_000;

/// Burst size in bytes for `rate` bytes per second: `max(250 ms of rate, 64 KiB)`.
#[must_use]
pub const fn burst_for(rate: u64) -> u64 {
    let window = rate / BURST_WINDOW_DIVISOR;
    if window > MIN_BURST {
        window
    } else {
        MIN_BURST
    }
}

#[derive(Debug)]
struct State {
    /// Available tokens in byte-nanoseconds (`bytes * 1e9`).
    tokens: u128,
    last: Instant,
}

/// A live-adjustable token bucket with an optional parent bucket.
#[derive(Debug)]
pub struct TokenBucket {
    /// Bytes per second; 0 means unlimited.
    rate: AtomicU64,
    state: Mutex<State>,
    changed: Notify,
    parent: Option<Arc<TokenBucket>>,
}

fn scaled(bytes: u64) -> u128 {
    u128::from(bytes) * NANOS_PER_SEC
}

impl State {
    fn refill(&mut self, rate: u64, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last).as_nanos();
        self.last = now;
        let cap = scaled(burst_for(rate));
        self.tokens = (self.tokens + elapsed * u128::from(rate)).min(cap);
    }
}

impl TokenBucket {
    /// A bucket limited to `rate` bytes per second (0 = unlimited), starting with a full burst.
    #[must_use]
    pub fn new(rate: u64) -> Self {
        Self::build(rate, None)
    }

    /// A bucket that never waits.
    #[must_use]
    pub fn unlimited() -> Self {
        Self::new(0)
    }

    /// A bucket whose acquisitions are also charged to `parent` (hierarchical limiting).
    #[must_use]
    pub fn with_parent(rate: u64, parent: Arc<TokenBucket>) -> Self {
        Self::build(rate, Some(parent))
    }

    fn build(rate: u64, parent: Option<Arc<TokenBucket>>) -> Self {
        Self {
            rate: AtomicU64::new(rate),
            state: Mutex::new(State {
                tokens: scaled(burst_for(rate)),
                last: Instant::now(),
            }),
            changed: Notify::new(),
            parent,
        }
    }

    /// The current rate in bytes per second (0 = unlimited).
    #[must_use]
    pub fn rate(&self) -> u64 {
        self.rate.load(Ordering::Acquire)
    }

    /// Changes the rate (0 = unlimited). Tasks already waiting re-evaluate immediately.
    pub fn set_rate(&self, new_rate: u64) {
        {
            let mut state = self.lock();
            let now = Instant::now();
            let old_rate = self.rate.load(Ordering::Acquire);
            if old_rate == 0 {
                state.tokens = scaled(burst_for(new_rate));
                state.last = now;
            } else {
                state.refill(old_rate, now);
                state.tokens = state.tokens.min(scaled(burst_for(new_rate)));
            }
            self.rate.store(new_rate, Ordering::Release);
        }
        self.changed.notify_waiters();
    }

    /// Waits until `n` bytes may be transferred, here and in every ancestor bucket.
    ///
    /// Requests larger than the burst are served in pieces of at most [`ACQUIRE_CHUNK`], so
    /// they always complete.
    ///
    /// # Errors
    ///
    /// Returns [`Cancelled`] as soon as `cancel` fires (also if it already had).
    pub async fn acquire(&self, n: u64, cancel: &CancellationToken) -> Result<(), Cancelled> {
        let mut remaining = n;
        while remaining > 0 {
            if cancel.is_cancelled() {
                return Err(Cancelled);
            }
            let piece = remaining.min(ACQUIRE_CHUNK);
            let mut current = Some(self);
            while let Some(bucket) = current {
                bucket.acquire_piece(piece, cancel).await?;
                current = bucket.parent.as_deref();
            }
            remaining -= piece;
        }
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Takes `n` tokens if available, else returns how long until they would be.
    fn try_take(&self, n: u64) -> Option<Duration> {
        let mut state = self.lock();
        let rate = self.rate.load(Ordering::Acquire);
        if rate == 0 {
            return None;
        }
        state.refill(rate, Instant::now());
        let need = scaled(n);
        if state.tokens >= need {
            state.tokens -= need;
            return None;
        }
        let wait_nanos = (need - state.tokens).div_ceil(u128::from(rate));
        Some(Duration::from_nanos(
            u64::try_from(wait_nanos).unwrap_or(u64::MAX),
        ))
    }

    async fn acquire_piece(&self, n: u64, cancel: &CancellationToken) -> Result<(), Cancelled> {
        loop {
            // Register for rate-change wakeups before reading state, so none are missed.
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let Some(wait) = self.try_take(n) else {
                return Ok(());
            };
            tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(Cancelled),
                () = &mut changed => {}
                () = tokio::time::sleep(wait) => {}
            }
        }
    }
}
