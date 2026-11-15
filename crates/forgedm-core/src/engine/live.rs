//! Per-download live state shared between a [`DownloadTask`](super::task::DownloadTask) and
//! the engine: stop signalling and the progress counters.
//!
//! Counters are atomics written by the task and sampled by the engine's 250 ms tick
//! (ARCHITECTURE 4.12), so the numbers clients see are true bytes, never estimates.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::error::TaskStop;
use crate::ports::ProgressSample;
use crate::progress::Estimator;

/// Sentinel for "total unknown" in an atomic.
const NO_TOTAL: u64 = u64::MAX;

/// Why a download was asked to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The user cancelled: the download becomes `cancelled`, the `.part` is kept.
    Cancel,
    /// The agent is shutting down: the download becomes `interrupted`.
    Shutdown,
}

/// A cancellation token that remembers why it fired (the first reason wins).
#[derive(Debug, Clone)]
pub struct StopSignal {
    token: CancellationToken,
    reason: Arc<Mutex<Option<StopReason>>>,
}

impl Default for StopSignal {
    fn default() -> Self {
        Self::new()
    }
}

impl StopSignal {
    /// A signal that has not fired.
    #[must_use]
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
            reason: Arc::new(Mutex::new(None)),
        }
    }

    /// Fires the signal. Later calls keep the first reason.
    pub fn request(&self, reason: StopReason) {
        let mut slot = self.reason.lock().unwrap_or_else(PoisonError::into_inner);
        slot.get_or_insert(reason);
        drop(slot);
        self.token.cancel();
    }

    /// Completes when the signal fires.
    pub async fn cancelled(&self) {
        self.token.cancelled().await;
    }

    /// The underlying token (for blocking work that polls it).
    #[must_use]
    pub fn token(&self) -> &CancellationToken {
        &self.token
    }

    /// Whether the signal has fired.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.token.is_cancelled()
    }

    /// The reason, once fired.
    #[must_use]
    pub fn reason(&self) -> Option<StopReason> {
        *self.reason.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The [`TaskStop`] for a fired signal (`Cancel` if somehow no reason was recorded).
    #[must_use]
    pub fn stopped(&self) -> TaskStop {
        TaskStop::Stopped(self.reason().unwrap_or(StopReason::Cancel))
    }

    /// `Err(Stopped)` when the signal has fired.
    ///
    /// # Errors
    /// [`TaskStop::Stopped`].
    pub fn check(&self) -> Result<(), TaskStop> {
        if self.is_stopped() {
            Err(self.stopped())
        } else {
            Ok(())
        }
    }
}

/// Whole seconds-as-float to `u64`, truncating; negative, NaN and overflow become 0.
fn f64_to_u64(value: f64) -> u64 {
    Duration::try_from_secs_f64(value).map_or(0, |d| d.as_secs())
}

/// Live counters of one running download.
#[derive(Debug)]
pub struct LiveDownload {
    id: Uuid,
    accepted: AtomicU64,
    committed: AtomicU64,
    total: AtomicU64,
    connections: AtomicU32,
    estimator: Mutex<Estimator>,
    stop: StopSignal,
}

impl LiveDownload {
    /// Counters at zero for download `id`.
    #[must_use]
    pub fn new(id: Uuid, stop: StopSignal) -> Self {
        Self {
            id,
            accepted: AtomicU64::new(0),
            committed: AtomicU64::new(0),
            total: AtomicU64::new(NO_TOTAL),
            connections: AtomicU32::new(0),
            estimator: Mutex::new(Estimator::new(Instant::now(), None, 0)),
            stop,
        }
    }

    /// The stop signal of this download.
    #[must_use]
    pub fn stop(&self) -> &StopSignal {
        &self.stop
    }

    /// Records the declared total.
    pub fn set_total(&self, total: Option<u64>) {
        self.total
            .store(total.unwrap_or(NO_TOTAL), Ordering::Release);
        self.estimator
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .set_total(total);
    }

    /// Records the cumulative bytes received and handed to the writer.
    pub fn set_accepted(&self, bytes: u64) {
        self.accepted.store(bytes, Ordering::Release);
    }

    /// Records the cumulative bytes durable on disk and in the checkpoint.
    pub fn set_committed(&self, bytes: u64) {
        self.committed.store(bytes, Ordering::Release);
    }

    /// Records how many connections are open.
    pub fn set_connections(&self, count: u32) {
        self.connections.store(count, Ordering::Release);
    }

    /// The current numbers without advancing the estimator (what `GetDownload` reports).
    #[must_use]
    pub fn peek(&self) -> ProgressSample {
        self.sample(None)
    }

    /// Advances the estimator to `now` and returns the numbers (the 250 ms tick).
    #[must_use]
    pub fn tick(&self, now: Instant) -> ProgressSample {
        self.sample(Some(now))
    }

    fn sample(&self, now: Option<Instant>) -> ProgressSample {
        let accepted = self.accepted.load(Ordering::Acquire);
        let committed = self.committed.load(Ordering::Acquire).min(accepted);
        let total = Some(self.total.load(Ordering::Acquire)).filter(|t| *t != NO_TOTAL);
        let mut estimator = self
            .estimator
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(now) = now {
            estimator.sample(now, accepted);
        }
        let speed = f64_to_u64(estimator.speed());
        let eta_ms = estimator
            .eta()
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        ProgressSample {
            id: self.id,
            accepted,
            committed,
            total,
            speed_bps: (speed > 0).then_some(speed),
            eta_ms,
            active_connections: self.connections.load(Ordering::Acquire),
        }
    }
}

/// The running downloads, by id.
#[derive(Debug, Default)]
pub struct Registry {
    live: Mutex<HashMap<Uuid, Arc<LiveDownload>>>,
}

impl Registry {
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, Arc<LiveDownload>>> {
        self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Registers a running download.
    pub fn insert(&self, live: Arc<LiveDownload>) {
        self.map().insert(live.id, live);
    }

    /// Forgets a finished download.
    pub fn remove(&self, id: Uuid) {
        self.map().remove(&id);
    }

    /// The live counters of `id`, if it is running.
    #[must_use]
    pub fn get(&self, id: Uuid) -> Option<Arc<LiveDownload>> {
        self.map().get(&id).cloned()
    }

    /// Fires `reason` on every running download.
    pub fn stop_all(&self, reason: StopReason) {
        for live in self.map().values() {
            live.stop.request(reason);
        }
    }

    /// Samples every running download at `now`, oldest id first.
    #[must_use]
    pub fn tick_all(&self, now: Instant) -> Vec<ProgressSample> {
        let mut samples: Vec<ProgressSample> =
            self.map().values().map(|live| live.tick(now)).collect();
        samples.sort_by_key(|s| s.id);
        samples
    }

    /// Number of running downloads.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map().len()
    }

    /// Whether nothing is running.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_stop_reason_wins() {
        let stop = StopSignal::new();
        assert!(stop.check().is_ok());
        stop.request(StopReason::Cancel);
        stop.request(StopReason::Shutdown);
        assert_eq!(stop.reason(), Some(StopReason::Cancel));
        assert_eq!(stop.check(), Err(TaskStop::Stopped(StopReason::Cancel)));
    }

    #[test]
    fn counters_are_reported_and_committed_never_exceeds_accepted() {
        let live = LiveDownload::new(Uuid::nil(), StopSignal::new());
        live.set_total(Some(100));
        live.set_accepted(40);
        live.set_committed(60);
        let sample = live.peek();
        assert_eq!(sample.accepted, 40);
        assert_eq!(sample.committed, 40);
        assert_eq!(sample.total, Some(100));
        live.set_total(None);
        assert_eq!(live.peek().total, None);
    }

    #[test]
    fn float_conversion_never_wraps() {
        assert_eq!(f64_to_u64(-1.0), 0);
        assert_eq!(f64_to_u64(f64::NAN), 0);
        assert_eq!(f64_to_u64(1234.9), 1234);
    }

    #[test]
    fn stop_all_reaches_every_running_download() {
        let registry = Registry::default();
        let live = Arc::new(LiveDownload::new(Uuid::from_u128(1), StopSignal::new()));
        registry.insert(Arc::clone(&live));
        registry.stop_all(StopReason::Shutdown);
        assert_eq!(live.stop().reason(), Some(StopReason::Shutdown));
        registry.remove(Uuid::from_u128(1));
        assert!(registry.is_empty());
    }
}
