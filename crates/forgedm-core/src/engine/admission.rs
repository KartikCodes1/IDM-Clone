//! Admission: which queued download runs next, and the live limits that steer it (PLAN
//! S1.13, ARCHITECTURE 4.2).
//!
//! Queued downloads are ordered by priority (high first), then `queue_position`, then
//! `created_at_ms`, and finally by arrival so ties are first come, first served. The queue
//! never holds a download twice. A due `retry_waiting` download reaches this queue only
//! through the A.6 timer edge (`retry_waiting -> queued`), so every admitted download is
//! `queued` first.

use std::cmp::Reverse;

use forgedm_protocol::Priority;
use uuid::Uuid;

use super::{Actor, DEFAULT_MAX_ACTIVE};
use crate::error::EngineError;
use crate::ports::StoredJob;

/// Fewest downloads that may run at once (A.9).
pub const MIN_ACTIVE: usize = 1;
/// Most downloads that may run at once (A.9).
pub const MAX_ACTIVE_LIMIT: usize = 10;
/// Smallest rate limit accepted, in bytes per second (A.3).
pub const MIN_RATE_LIMIT_BPS: u64 = 1024;

const _: () = assert!(DEFAULT_MAX_ACTIVE >= MIN_ACTIVE && DEFAULT_MAX_ACTIVE <= MAX_ACTIVE_LIMIT);

/// Clamps a rate limit to the accepted range (`None` stays unlimited).
#[must_use]
pub fn clamp_rate(limit: Option<u64>) -> Option<u64> {
    limit.map(|bps| bps.max(MIN_RATE_LIMIT_BPS))
}

const fn rank(priority: Priority) -> u8 {
    match priority {
        Priority::High => 2,
        Priority::Normal => 1,
        Priority::Low => 0,
    }
}

/// A change to the agent-wide limits. `None` leaves a limit as it is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GlobalLimits {
    /// Downloads that run at once (clamped to `1..=10`).
    pub max_active: Option<usize>,
    /// Rate limit in bytes per second: `Some(None)` is unlimited, `Some(Some(n))` a cap
    /// (at least 1024).
    pub rate_limit_bps: Option<Option<u64>>,
}

/// The agent-wide limits in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveGlobal {
    /// Downloads that run at once.
    pub max_active: usize,
    /// Rate limit in bytes per second; `None` is unlimited.
    pub rate_limit_bps: Option<u64>,
}

struct Entry {
    job: StoredJob,
    arrival: u64,
}

impl Entry {
    fn key(&self) -> (Reverse<u8>, i64, i64, u64) {
        (
            Reverse(rank(self.job.priority)),
            self.job.queue_position,
            self.job.created_at_ms,
            self.arrival,
        )
    }
}

/// The downloads waiting for a slot.
#[derive(Default)]
pub struct Queue {
    entries: Vec<Entry>,
    arrivals: u64,
}

impl Queue {
    /// An empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a download; `false` (and no change) when it is already waiting.
    pub fn push(&mut self, job: StoredJob) -> bool {
        if self.contains(job.id) {
            return false;
        }
        self.arrivals += 1;
        self.entries.push(Entry {
            job,
            arrival: self.arrivals,
        });
        true
    }

    /// Removes and returns the download that should run next.
    pub fn pop_next(&mut self) -> Option<StoredJob> {
        let best = self
            .entries
            .iter()
            .enumerate()
            .min_by_key(|(_, entry)| entry.key())
            .map(|(index, _)| index)?;
        Some(self.entries.remove(best).job)
    }

    /// Whether `id` is waiting.
    #[must_use]
    pub fn contains(&self, id: Uuid) -> bool {
        self.entries.iter().any(|entry| entry.job.id == id)
    }

    /// Takes `id` out of the queue; whether it was there.
    pub fn remove(&mut self, id: Uuid) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.job.id != id);
        self.entries.len() != before
    }

    /// Changes the priority of a waiting download; whether it was there.
    pub fn set_priority(&mut self, id: Uuid, priority: Priority) -> bool {
        self.entries
            .iter_mut()
            .find(|entry| entry.job.id == id)
            .map(|entry| entry.job.priority = priority)
            .is_some()
    }

    /// Drops everything.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// How many downloads wait.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing waits.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The waiting ids in the order they would be admitted.
    #[cfg(test)]
    #[must_use]
    pub fn admission_order(&self) -> Vec<Uuid> {
        let mut sorted: Vec<&Entry> = self.entries.iter().collect();
        sorted.sort_by_key(|entry| entry.key());
        sorted.into_iter().map(|entry| entry.job.id).collect()
    }
}

impl Actor {
    /// Queues `job` unless it already waits or runs (a repeated recover must not run a
    /// download twice).
    pub(super) fn queue_job(&mut self, job: StoredJob) {
        if self.registry.get(job.id).is_some() {
            return;
        }
        self.queue.push(job);
    }

    /// Starts queued downloads, best first, while slots are free.
    pub(super) async fn admit(&mut self) {
        while !self.closing && self.active < self.config.max_active {
            let Some(job) = self.queue.pop_next() else {
                return;
            };
            self.active += 1;
            self.spawn_task(job);
        }
    }

    /// Changes the priority of `id`: stored first, then applied to the waiting queue, so a
    /// reordered queue never disagrees with what a restart would load.
    pub(super) async fn set_priority(
        &mut self,
        id: Uuid,
        priority: Priority,
    ) -> Result<(), EngineError> {
        if self.closing {
            return Err(EngineError::ShuttingDown);
        }
        self.deps
            .store
            .load(id)
            .await?
            .ok_or(EngineError::NotFound)?;
        self.deps.store.set_priority(id, priority).await?;
        self.queue.set_priority(id, priority);
        self.deps
            .sink
            .emit(crate::ports::EngineEvent::Changed { id })
            .await;
        Ok(())
    }

    /// Applies new agent-wide limits live and returns what is now in force. Lowering the
    /// number of active downloads never stops a running one; admission just waits.
    pub(super) async fn update_global_limits(
        &mut self,
        patch: GlobalLimits,
    ) -> Result<EffectiveGlobal, EngineError> {
        if self.closing {
            return Err(EngineError::ShuttingDown);
        }
        if let Some(value) = patch.max_active {
            self.config.max_active = value.clamp(MIN_ACTIVE, MAX_ACTIVE_LIMIT);
        }
        if let Some(limit) = patch.rate_limit_bps {
            self.budgets.set_global_rate(clamp_rate(limit));
        }
        self.admit().await;
        Ok(self.effective_global())
    }

    pub(super) fn effective_global(&self) -> EffectiveGlobal {
        EffectiveGlobal {
            max_active: self.config.max_active,
            rate_limit_bps: self.budgets.global_rate(),
        }
    }

    /// Sets the rate limit of one download, live.
    pub(super) async fn set_rate_limit(
        &mut self,
        id: Uuid,
        limit: Option<u64>,
    ) -> Result<Option<u64>, EngineError> {
        if self.closing {
            return Err(EngineError::ShuttingDown);
        }
        self.deps
            .store
            .load(id)
            .await?
            .ok_or(EngineError::NotFound)?;
        Ok(self.budgets.set_download_rate(id, clamp_rate(limit)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::StoredProgress;
    use crate::url_policy::validate_user_url;
    use forgedm_protocol::DownloadState;

    fn job(priority: Priority, position: i64, created: i64) -> StoredJob {
        StoredJob {
            id: Uuid::now_v7(),
            state: DownloadState::Queued,
            state_reason: None,
            url: validate_user_url("https://example.com/f").unwrap(),
            target_dir: std::path::PathBuf::from(r"C:\d"),
            requested_name: None,
            display_name: "f".to_owned(),
            file_name: None,
            expected_sha256: None,
            part_path: None,
            part_file_id: None,
            transfer_mode: None,
            progress: StoredProgress::default(),
            max_connections: 8,
            priority,
            queue_position: position,
            created_at_ms: created,
        }
    }

    fn drain(queue: &mut Queue) -> Vec<Uuid> {
        std::iter::from_fn(|| queue.pop_next().map(|job| job.id)).collect()
    }

    #[test]
    fn order_is_priority_then_position_then_creation_then_arrival() {
        let mut queue = Queue::new();
        let low = job(Priority::Low, 0, 0);
        let late_normal = job(Priority::Normal, 5, 1);
        let early_normal = job(Priority::Normal, 2, 9);
        let tie_a = job(Priority::Normal, 5, 0);
        let tie_b = job(Priority::Normal, 5, 0);
        let high = job(Priority::High, 99, 99);
        let ids = [
            low.id,
            late_normal.id,
            early_normal.id,
            tie_a.id,
            tie_b.id,
            high.id,
        ];
        for entry in [low, late_normal, early_normal, tie_a, tie_b, high] {
            assert!(queue.push(entry));
        }

        let order = queue.admission_order();

        let expected = [ids[5], ids[2], ids[3], ids[4], ids[1], ids[0]];
        assert_eq!(order, expected);
        assert_eq!(drain(&mut queue), expected);
        assert!(queue.is_empty());
    }

    #[test]
    fn a_download_is_never_queued_twice() {
        let mut queue = Queue::new();
        let first = job(Priority::Normal, 0, 0);
        let id = first.id;
        let mut again = job(Priority::High, 0, 0);
        again.id = id;

        assert!(queue.push(first));
        assert!(!queue.push(again));

        assert_eq!(queue.len(), 1);
        assert!(queue.contains(id));
    }

    #[test]
    fn a_new_priority_reorders_and_unknown_ids_are_ignored() {
        let mut queue = Queue::new();
        let a = job(Priority::Normal, 1, 1);
        let b = job(Priority::Normal, 2, 2);
        let (a_id, b_id) = (a.id, b.id);
        queue.push(a);
        queue.push(b);

        assert!(queue.set_priority(b_id, Priority::High));
        assert!(!queue.set_priority(Uuid::now_v7(), Priority::High));

        assert_eq!(queue.admission_order(), [b_id, a_id]);
        assert!(queue.remove(a_id));
        assert!(!queue.remove(a_id));
    }

    #[test]
    fn limits_are_clamped_to_the_accepted_range() {
        assert_eq!(clamp_rate(None), None);
        assert_eq!(clamp_rate(Some(1)), Some(MIN_RATE_LIMIT_BPS));
        assert_eq!(clamp_rate(Some(5_000_000)), Some(5_000_000));
    }
}
