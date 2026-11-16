//! The timer that brings a `retry_waiting` download back (PLAN A.6 timer edge,
//! ARCHITECTURE 4.2): when the stored due time passes, `retry_waiting -> queued`, then admit.

use std::time::Duration;

use forgedm_protocol::DownloadState;
use tokio::time::sleep;
use uuid::Uuid;

use super::Actor;
use crate::ports::{EngineEvent, StoreEvent, unix_ms};
use crate::state::{Command, transition};

/// A retry timer that fired; `attempt` counts storage-error re-arms.
pub(super) struct Due {
    pub(super) id: Uuid,
    pub(super) attempt: u32,
}

/// Bounded re-arms of a retry timer after a storage error.
const RETRY_DUE_ATTEMPTS: u32 = 5;
/// Base delay of those re-arms (multiplied by the attempt number).
const RETRY_DUE_BACKOFF_MS: i64 = 1_000;
/// Wait used when the stored retry time cannot be read.
const RETRY_FALLBACK_MS: i64 = 30_000;

impl Actor {
    /// After a task ended: when it parked the download in `retry_waiting`, arms the timer
    /// that brings it back.
    pub(super) async fn arm_if_waiting(&mut self, id: Uuid) {
        let job = match self.deps.store.load(id).await {
            Ok(Some(job)) => job,
            Ok(None) => return,
            Err(err) => {
                tracing::error!(error = %err, "could not read a finished download to arm its retry");
                return;
            }
        };
        if job.state != DownloadState::RetryWaiting {
            return;
        }
        let at_ms = match self.deps.store.next_retry_at(id).await {
            Ok(at_ms) => at_ms.unwrap_or(0),
            Err(err) => {
                tracing::error!(error = %err, "could not read the retry time; using a fallback");
                unix_ms().saturating_add(RETRY_FALLBACK_MS)
            }
        };
        self.arm_timer(id, at_ms);
    }

    /// Sends `id` to the engine again at `at_ms` (epoch milliseconds; past means now).
    pub(super) fn arm_timer(&mut self, id: Uuid, at_ms: i64) {
        self.arm_timer_attempt(id, at_ms, 0);
    }

    fn arm_timer_attempt(&mut self, id: Uuid, at_ms: i64, attempt: u32) {
        // Reap the timers that already fired so the set stays small.
        while self.timers.try_join_next().is_some() {}
        let left_ms = u64::try_from(at_ms.saturating_sub(unix_ms())).unwrap_or(0);
        let due = self.due_tx.clone();
        self.timers.spawn(async move {
            sleep(Duration::from_millis(left_ms)).await;
            // The receiver only goes away when the engine stops; nothing is left to wake then.
            let _ = due.send(Due { id, attempt });
        });
    }

    /// Re-arms a timer after a storage error, with a growing delay, up to
    /// [`RETRY_DUE_ATTEMPTS`] times.
    fn rearm_after_error(&mut self, due: Due) {
        let attempt = due.attempt + 1;
        if attempt > RETRY_DUE_ATTEMPTS {
            tracing::error!(
                id = %due.id,
                "gave up requeueing a download whose retry is due; resume it or restart the agent"
            );
            return;
        }
        let delay = RETRY_DUE_BACKOFF_MS.saturating_mul(i64::from(attempt));
        self.arm_timer_attempt(due.id, unix_ms().saturating_add(delay), attempt);
    }

    /// The retry timer of `id` fired: `retry_waiting -> queued` (A.6 timer edge), then admit.
    /// A download that left `retry_waiting` in the meantime (resumed, cancelled) is left alone,
    /// and so is one that parked again with a later due time (a stale timer fired).
    pub(super) async fn retry_due(&mut self, due: Due) {
        if self.closing {
            return;
        }
        let id = due.id;
        let mut job = match self.deps.store.load(id).await {
            Ok(Some(job)) => job,
            Ok(None) => return,
            Err(err) => {
                tracing::warn!(error = %err, "could not read a download whose retry is due");
                self.rearm_after_error(due);
                return;
            }
        };
        let Ok(step) = transition(job.state, Command::TimerDue) else {
            return;
        };
        match self.deps.store.next_retry_at(id).await {
            Ok(Some(at_ms)) if at_ms > unix_ms() => {
                self.arm_timer(id, at_ms);
                return;
            }
            Ok(_) => {}
            Err(err) => {
                tracing::warn!(error = %err, "could not read the retry time of a due download");
                self.rearm_after_error(due);
                return;
            }
        }
        let event = StoreEvent::now("retry_due");
        let moved = self
            .deps
            .store
            .transition(id, job.state, step.to, None, event)
            .await;
        if let Err(err) = moved {
            tracing::warn!(error = %err, "could not requeue a download whose retry is due");
            self.rearm_after_error(due);
            return;
        }
        job.state = step.to;
        job.state_reason = None;
        self.deps.sink.emit(EngineEvent::Changed { id }).await;
        self.queue_job(job);
        self.admit().await;
    }
}
