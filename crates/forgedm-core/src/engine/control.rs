//! User commands on one download: pause, resume, cancel, retry and remove (PLAN A.6).
//!
//! They arrive through the engine actor, so commands for one download are serialized. Every
//! command is first judged by the pure transition table against the authoritative state: the
//! state the owning task last persisted when it is running, the stored state otherwise.
//! Running downloads are asked to stop and the reply waits for them to settle; idle ones are
//! moved by a compare-and-set in the store. Repeated commands are no-ops that report the
//! state the download is in.

use std::time::Duration;

use forgedm_protocol::DownloadState;
use tokio::time::timeout;
use uuid::Uuid;

use super::{Actor, Control, StopReason, delete_stale_part};
use crate::error::{EngineError, StoreError};
use crate::ports::{EngineEvent, StoreEvent, StoredJob, StoredProgress};
use crate::resume::is_resumable_mode;
use crate::state::{Command, TransitionError, transition};

/// How long a command waits for a running download to settle. Shorter than the 5 s
/// `ENGINE_REPLY_TIMEOUT`, so the caller gets the state at that moment, not `busy`.
pub const SETTLE_WAIT: Duration = Duration::from_secs(3);

/// The state-machine command a [`Control`] stands for.
fn command_of(control: Control, resumable: bool) -> Command {
    match control {
        Control::Pause { discard_progress } => Command::Pause {
            resumable,
            discard_progress,
        },
        Control::Resume => Command::Resume,
        Control::Cancel => Command::Cancel,
        Control::Retry { .. } => Command::Retry,
    }
}

fn refusal(err: TransitionError) -> EngineError {
    match err {
        TransitionError::InvalidState { current, .. } => EngineError::InvalidState(current),
        TransitionError::NotResumable => EngineError::NotResumable,
    }
}

impl Actor {
    /// Judges and applies `control` to download `id`; returns the resulting state.
    pub(super) async fn control(
        &mut self,
        id: Uuid,
        control: Control,
    ) -> Result<DownloadState, EngineError> {
        if self.closing {
            return Err(EngineError::ShuttingDown);
        }
        let job = self
            .deps
            .store
            .load(id)
            .await?
            .ok_or(EngineError::NotFound)?;
        let live = self.registry.get(id);
        let state = live.as_ref().map_or(job.state, |live| live.state());
        // A download whose mode is not known yet (still probing) has nothing to lose.
        let resumable = job.transfer_mode.is_none_or(is_resumable_mode);
        let step = transition(state, command_of(control, resumable)).map_err(refusal)?;
        if !step.changed {
            return Ok(step.to);
        }
        match live {
            Some(live) => self.control_running(&live, job, control).await,
            None => self.control_idle(job, control).await,
        }
    }

    /// The download has a running task: ask it to stop (pause, cancel) and wait for it.
    async fn control_running(
        &mut self,
        live: &super::LiveDownload,
        job: StoredJob,
        control: Control,
    ) -> Result<DownloadState, EngineError> {
        match control {
            Control::Pause { .. } => live.stop().request(StopReason::Pause),
            Control::Cancel => live.stop().request(StopReason::Cancel),
            Control::Resume | Control::Retry { .. } => {}
        }
        let settled = timeout(SETTLE_WAIT, live.finished()).await.is_ok();
        match control {
            Control::Pause { .. } | Control::Cancel => self.stored_state(job.id).await,
            // The task was already finishing (for example it just failed): once it is gone
            // the download is idle and the command applies to what it left behind.
            Control::Resume | Control::Retry { .. } if settled => {
                let job = self
                    .deps
                    .store
                    .load(job.id)
                    .await?
                    .ok_or(EngineError::NotFound)?;
                self.control_idle(job, control).await
            }
            Control::Resume | Control::Retry { .. } => Err(EngineError::Busy),
        }
    }

    async fn stored_state(&self, id: Uuid) -> Result<DownloadState, EngineError> {
        let stored = self.deps.store.load(id).await?;
        stored.map(|job| job.state).ok_or(EngineError::NotFound)
    }

    /// No task is running: move the stored state by compare-and-set.
    async fn control_idle(
        &mut self,
        mut job: StoredJob,
        control: Control,
    ) -> Result<DownloadState, EngineError> {
        let resumable = job.transfer_mode.is_none_or(is_resumable_mode);
        let step = transition(job.state, command_of(control, resumable)).map_err(refusal)?;
        if !step.changed {
            return Ok(step.to);
        }
        if let Control::Retry {
            restart_from_zero: true,
        } = control
        {
            self.wipe_for_restart(&mut job).await?;
        }
        let (reason, kind) = match step.to {
            DownloadState::Paused => (Some("user"), "paused"),
            DownloadState::Cancelled => (Some("user"), "cancelled"),
            _ => (None, "requeued"),
        };
        let event = StoreEvent::now(kind).with_code("user");
        let moved = self
            .deps
            .store
            .transition(job.id, job.state, step.to, reason.map(str::to_owned), event)
            .await;
        match moved {
            Ok(()) => {}
            Err(StoreError::Conflict {
                actual: Some(actual),
                ..
            }) => return Err(EngineError::InvalidState(actual)),
            Err(other) => return Err(other.into()),
        }
        self.queue.retain(|queued| queued.id != job.id);
        self.deps
            .sink
            .emit(EngineEvent::Changed { id: job.id })
            .await;
        if step.to == DownloadState::Queued {
            job.state = DownloadState::Queued;
            job.state_reason = None;
            self.queue.push_back(job);
            self.admit().await;
        }
        Ok(step.to)
    }

    /// `Retry{restart_from_zero}`: delete the stale `.part` (only if provably ours) and
    /// forget progress and validators in one reset, before anything new can be written.
    async fn wipe_for_restart(&self, job: &mut StoredJob) -> Result<(), EngineError> {
        let stale = job.clone();
        match tokio::task::spawn_blocking(move || delete_stale_part(&stale)).await {
            Ok(outcome) => tracing::debug!(?outcome, "stale staging file cleanup"),
            Err(err) => tracing::warn!(error = %err, "stale staging file cleanup did not run"),
        }
        self.deps.store.reset_progress(job.id).await?;
        let event = StoreEvent::now("restart_from_zero").with_code("user");
        self.deps.store.append_event(job.id, event).await?;
        job.part_path = None;
        job.part_file_id = None;
        job.transfer_mode = None;
        job.progress = StoredProgress::default();
        Ok(())
    }

    /// Cancels a running download, then deletes the row (and, on request, the `.part`).
    pub(super) async fn remove(
        &mut self,
        id: Uuid,
        delete_partial: bool,
    ) -> Result<(), EngineError> {
        let job = self
            .deps
            .store
            .load(id)
            .await?
            .ok_or(EngineError::NotFound)?;
        if let Some(live) = self.registry.get(id) {
            live.stop().request(StopReason::Cancel);
            if timeout(SETTLE_WAIT, live.finished()).await.is_err() {
                return Err(EngineError::Busy);
            }
        }
        self.queue.retain(|queued| queued.id != id);
        // Re-read: the task may have recorded the staging file while it settled.
        let job = self.deps.store.load(id).await?.unwrap_or(job);
        if delete_partial && job.state != DownloadState::Completed {
            let stale = job.clone();
            match tokio::task::spawn_blocking(move || delete_stale_part(&stale)).await {
                Ok(outcome) => tracing::info!(?outcome, "staging file cleanup on remove"),
                Err(err) => tracing::warn!(error = %err, "staging file cleanup did not run"),
            }
        }
        self.deps.store.remove(id).await?;
        self.deps.sink.emit(EngineEvent::Removed { id }).await;
        Ok(())
    }
}
