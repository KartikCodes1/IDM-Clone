//! Shared resource budgets (PLAN S1.13, ARCHITECTURE 4.2 and 4.11): connection permits
//! (global, then per host), bandwidth buckets (per download, then global) and the free-space
//! reading.
//!
//! A connection permit is taken in a fixed order, global first, then the host slot, and is
//! held for the lifetime of one request. The first connection of a download awaits its
//! permit; extra connections only `try_acquire` and stand back while any first connection is
//! waiting, so one download can never starve another's first connection.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use forgedm_protocol::DownloadErrorCode;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::{Instant, sleep};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::hosts::HostHandle;
use crate::clock::Cancelled;
use crate::error::TaskStop;
use crate::rate_limit::TokenBucket;

/// Connections open at once across every download (A.9 `CONNS_GLOBAL`).
pub const CONNS_GLOBAL: usize = 32;
/// Free space kept beyond the file itself (A.9 `FREE_SPACE_MARGIN`).
pub const FREE_SPACE_MARGIN: u64 = 64 * 1024 * 1024;
/// Free space is re-checked after this many bytes (A.9).
pub const FREE_SPACE_RECHECK: u64 = 1024 * 1024 * 1024;
/// How long an extra connection waits before it asks for a permit again.
pub const EXTRA_RETRY: Duration = Duration::from_millis(500);
/// How often a waiting first connection looks at the host again (the AIMD limit can rise
/// without a slot being returned).
const HOST_POLL: Duration = Duration::from_millis(250);

/// Reads the free space of the volume holding a directory.
pub trait FreeSpace: Send + Sync + 'static {
    /// Free bytes available to the user, or `None` when they cannot be read.
    fn free(&self, dir: &Path) -> Option<u64>;
}

/// The real volume (`GetDiskFreeSpaceExW`).
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemFreeSpace;

impl FreeSpace for SystemFreeSpace {
    fn free(&self, dir: &Path) -> Option<u64> {
        match forgedm_platform::fs::free_space(dir) {
            Ok(free) => Some(free),
            Err(err) => {
                tracing::warn!(error = %err, "free space query failed; continuing");
                None
            }
        }
    }
}

/// Checks that `needed` bytes plus [`FREE_SPACE_MARGIN`] fit.
///
/// # Errors
/// [`TaskStop::PauseFor`] with `disk_full` when they do not. An unreadable volume passes.
pub fn ensure_space(space: &dyn FreeSpace, dir: &Path, needed: u64) -> Result<(), TaskStop> {
    match space.free(dir) {
        Some(free) if free < needed.saturating_add(FREE_SPACE_MARGIN) => {
            Err(TaskStop::PauseFor(DownloadErrorCode::DiskFull))
        }
        _ => Ok(()),
    }
}

/// The budgets every download shares.
pub struct Budgets {
    conns: Arc<Semaphore>,
    global: Arc<TokenBucket>,
    buckets: Mutex<HashMap<Uuid, Arc<TokenBucket>>>,
    space: Arc<dyn FreeSpace>,
    /// First connections (of any download) waiting for a permit.
    first_waiting: AtomicU32,
}

/// Counts one waiting first connection in [`Budgets`] while alive.
struct WaitMark(Arc<Budgets>);

impl WaitMark {
    fn new(budgets: &Arc<Budgets>) -> Self {
        budgets.first_waiting.fetch_add(1, Ordering::AcqRel);
        Self(Arc::clone(budgets))
    }
}

impl Drop for WaitMark {
    fn drop(&mut self) {
        self.0.first_waiting.fetch_sub(1, Ordering::AcqRel);
    }
}

impl std::fmt::Debug for Budgets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Budgets")
            .field("free_connections", &self.conns.available_permits())
            .field("global_rate", &self.global.rate())
            .finish_non_exhaustive()
    }
}

/// `Some(rate)` for a limit, `None` for unlimited; the bucket uses 0 for unlimited.
fn bucket_rate(limit: Option<u64>) -> u64 {
    limit.unwrap_or(0)
}

fn limit_of(rate: u64) -> Option<u64> {
    (rate > 0).then_some(rate)
}

impl Budgets {
    /// Budgets of `global_connections` connections, unlimited bandwidth and the volume
    /// reading `space`.
    #[must_use]
    pub fn new(global_connections: usize, space: Arc<dyn FreeSpace>) -> Self {
        Self {
            conns: Arc::new(Semaphore::new(global_connections.max(1))),
            global: Arc::new(TokenBucket::unlimited()),
            buckets: Mutex::new(HashMap::new()),
            space,
            first_waiting: AtomicU32::new(0),
        }
    }

    /// The free-space reading.
    #[must_use]
    pub fn space(&self) -> &Arc<dyn FreeSpace> {
        &self.space
    }

    /// Sets the global rate limit in bytes per second (`None` = unlimited), live.
    pub fn set_global_rate(&self, limit: Option<u64>) {
        self.global.set_rate(bucket_rate(limit));
    }

    /// The global rate limit.
    #[must_use]
    pub fn global_rate(&self) -> Option<u64> {
        limit_of(self.global.rate())
    }

    /// The bucket of download `id` (created unlimited, with the global bucket as parent).
    pub fn download_bucket(&self, id: Uuid) -> Arc<TokenBucket> {
        let mut map = self.buckets.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(
            map.entry(id)
                .or_insert_with(|| Arc::new(TokenBucket::with_parent(0, Arc::clone(&self.global)))),
        )
    }

    /// Sets the rate limit of download `id` (`None` = unlimited), live. Returns the limit now
    /// in force.
    pub fn set_download_rate(&self, id: Uuid, limit: Option<u64>) -> Option<u64> {
        let bucket = self.download_bucket(id);
        bucket.set_rate(bucket_rate(limit));
        limit_of(bucket.rate())
    }

    /// Forgets the bucket of a removed download.
    pub fn forget(&self, id: Uuid) {
        self.buckets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
    }

    /// The connection gate of one download whose host is `host`.
    #[must_use]
    pub fn gate(self: &Arc<Self>, host: Arc<HostHandle>) -> Arc<ConnGate> {
        Arc::new(ConnGate {
            budgets: Arc::clone(self),
            host,
            held: AtomicU32::new(0),
        })
    }
}

/// Where one download takes its connection permits.
pub struct ConnGate {
    budgets: Arc<Budgets>,
    host: Arc<HostHandle>,
    /// Permits this download holds right now.
    held: AtomicU32,
}

impl std::fmt::Debug for ConnGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnGate")
            .field("held", &self.held.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

/// One connection's right to exist: a global permit and a host slot, returned on drop.
pub struct ConnPermit {
    _global: OwnedSemaphorePermit,
    gate: Arc<ConnGate>,
}

impl std::fmt::Debug for ConnPermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnPermit").finish_non_exhaustive()
    }
}

impl Drop for ConnPermit {
    fn drop(&mut self) {
        self.gate.held.fetch_sub(1, Ordering::AcqRel);
        self.gate.host.release_slot();
    }
}

impl ConnGate {
    fn granted(self: &Arc<Self>, global: OwnedSemaphorePermit) -> ConnPermit {
        self.held.fetch_add(1, Ordering::AcqRel);
        ConnPermit {
            _global: global,
            gate: Arc::clone(self),
        }
    }

    /// Waits for a permit: the global budget first, then a slot on the host. This is the
    /// path of a download's first connection (and of single-connection transfers).
    ///
    /// # Errors
    /// [`Cancelled`] as soon as `cancel` fires.
    pub async fn acquire(
        self: &Arc<Self>,
        cancel: &CancellationToken,
    ) -> Result<ConnPermit, Cancelled> {
        let _waiting = (self.host.first_waiter(), WaitMark::new(&self.budgets));
        let global = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(Cancelled),
            permit = Arc::clone(&self.budgets.conns).acquire_owned() => {
                permit.map_err(|_| Cancelled)?
            }
        };
        loop {
            let freed = self.host.slot_freed();
            tokio::pin!(freed);
            freed.as_mut().enable();
            if self.host.try_take_slot(Instant::now(), true) {
                return Ok(self.granted(global));
            }
            tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(Cancelled),
                () = &mut freed => {}
                () = sleep(HOST_POLL) => {}
            }
        }
    }

    /// Whether a connection that holds a permit should hand it back at the next chance: this
    /// download has another connection, and some download's first connection is waiting for
    /// room on this host (or for the global budget, when that is spent). Without this a
    /// long-running response would keep a small download waiting for the whole of it.
    #[must_use]
    pub fn should_yield(&self) -> bool {
        if self.held.load(Ordering::Acquire) < 2 {
            return false;
        }
        self.host.first_waiting() > 0
            || (self.budgets.first_waiting.load(Ordering::Acquire) > 0
                && self.budgets.conns.available_permits() == 0)
    }

    /// Takes a permit only if both budgets have room right now (and no first connection is
    /// waiting for the host).
    #[must_use]
    pub fn try_acquire(self: &Arc<Self>) -> Option<ConnPermit> {
        let global = Arc::clone(&self.budgets.conns).try_acquire_owned().ok()?;
        if !self.host.try_take_slot(Instant::now(), false) {
            return None;
        }
        Some(self.granted(global))
    }

    /// The permit of one request of a worker: the download's first connection waits for it,
    /// extra ones try again every [`EXTRA_RETRY`].
    ///
    /// # Errors
    /// [`Cancelled`] as soon as `cancel` fires.
    pub async fn acquire_for_worker(
        self: &Arc<Self>,
        cancel: &CancellationToken,
    ) -> Result<ConnPermit, Cancelled> {
        loop {
            if self.held.load(Ordering::Acquire) == 0 {
                return self.acquire(cancel).await;
            }
            if let Some(permit) = self.try_acquire() {
                return Ok(permit);
            }
            tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(Cancelled),
                () = sleep(EXTRA_RETRY) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;

    use super::*;

    struct Fixed(AtomicU64);

    impl FreeSpace for Fixed {
        fn free(&self, _dir: &Path) -> Option<u64> {
            Some(self.0.load(Ordering::SeqCst))
        }
    }

    struct Unknown;

    impl FreeSpace for Unknown {
        fn free(&self, _dir: &Path) -> Option<u64> {
            None
        }
    }

    fn budgets(global: usize) -> Arc<Budgets> {
        Arc::new(Budgets::new(global, Arc::new(SystemFreeSpace)))
    }

    #[test]
    fn the_constants_are_the_appendix_values() {
        assert_eq!(CONNS_GLOBAL, 32);
        assert_eq!(FREE_SPACE_MARGIN, 64 * 1024 * 1024);
        assert_eq!(FREE_SPACE_RECHECK, 1024 * 1024 * 1024);
    }

    #[test]
    fn space_needs_the_file_plus_the_margin() {
        let dir = Path::new(".");
        let free = Fixed(AtomicU64::new(100 * 1024 * 1024));
        assert!(ensure_space(&free, dir, 36 * 1024 * 1024).is_ok());
        assert_eq!(
            ensure_space(&free, dir, 36 * 1024 * 1024 + 1),
            Err(TaskStop::PauseFor(DownloadErrorCode::DiskFull))
        );
        assert!(ensure_space(&Unknown, dir, u64::MAX).is_ok());
        assert!(ensure_space(&free, dir, u64::MAX).is_err(), "no overflow");
    }

    #[tokio::test]
    async fn the_global_budget_is_taken_before_the_host_slot() {
        let budgets = budgets(1);
        let gate_a = budgets.gate(Arc::new(HostHandle::new(8)));
        let gate_b = budgets.gate(Arc::new(HostHandle::new(8)));
        let first = gate_a.try_acquire().expect("the one global permit");

        assert!(gate_b.try_acquire().is_none(), "the global budget is spent");
        assert_eq!(gate_b.host.slots_in_use(), 0, "no host slot was kept");
        drop(first);
        assert!(gate_b.try_acquire().is_some());
    }

    #[tokio::test]
    async fn the_host_budget_is_shared_by_downloads_of_one_host() {
        let budgets = budgets(32);
        let host = Arc::new(HostHandle::new(1));
        let a = budgets.gate(Arc::clone(&host));
        let b = budgets.gate(host);
        let permit = a.try_acquire().unwrap();

        assert!(b.try_acquire().is_none());
        let waiting = {
            let b = Arc::clone(&b);
            tokio::spawn(async move { b.acquire(&CancellationToken::new()).await.is_ok() })
        };
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished(), "the first connection waits");
        drop(permit);
        assert!(waiting.await.unwrap());
    }

    #[tokio::test]
    async fn a_waiting_first_connection_is_not_overtaken_by_extras() {
        let budgets = budgets(32);
        let host = Arc::new(HostHandle::new(1));
        let big = budgets.gate(Arc::clone(&host));
        let small = budgets.gate(host);
        let held = big.try_acquire().unwrap();
        let waiter = {
            let small = Arc::clone(&small);
            tokio::spawn(async move { small.acquire(&CancellationToken::new()).await })
        };
        tokio::task::yield_now().await;

        drop(held);
        // Racing with the waiter, an extra connection of `big` must lose.
        let overtaken = big.try_acquire();

        assert!(overtaken.is_none() || waiter.is_finished());
        assert!(waiter.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn waiting_for_a_permit_ends_when_cancelled() {
        let budgets = budgets(1);
        let gate = budgets.gate(Arc::new(HostHandle::new(8)));
        let _held = gate.try_acquire().unwrap();
        let cancel = CancellationToken::new();
        let waiting = {
            let (gate, cancel) = (Arc::clone(&gate), cancel.clone());
            tokio::spawn(async move { gate.acquire(&cancel).await.map(|_| ()) })
        };
        tokio::task::yield_now().await;

        cancel.cancel();

        assert_eq!(waiting.await.unwrap(), Err(Cancelled));
    }

    #[test]
    fn rates_are_live_and_per_download_buckets_have_the_global_parent() {
        let budgets = budgets(4);
        let id = Uuid::now_v7();
        assert_eq!(budgets.global_rate(), None);
        budgets.set_global_rate(Some(5000));
        assert_eq!(budgets.global_rate(), Some(5000));
        assert_eq!(budgets.set_download_rate(id, Some(2048)), Some(2048));
        assert_eq!(budgets.download_bucket(id).rate(), 2048);
        assert_eq!(budgets.set_download_rate(id, None), None);
        budgets.forget(id);
        assert_eq!(budgets.download_bucket(id).rate(), 0);
    }
}
