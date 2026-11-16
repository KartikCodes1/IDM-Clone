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
use tokio::time::{Instant, timeout, timeout_at};
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

/// What [`EngineHandle::pause_all`](super::EngineHandle::pause_all) did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PauseAllOutcome {
    /// Downloads that are paused now.
    pub paused: Vec<Uuid>,
    /// Active non-resumable downloads left running (pausing would lose their progress).
    pub skipped_not_resumable: Vec<Uuid>,
}

/// States `PauseAll` can move to `paused` when no task runs them (A.6).
const PAUSABLE_IDLE: [DownloadState; 3] = [
    DownloadState::Queued,
    DownloadState::RetryWaiting,
    DownloadState::Interrupted,
];

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

    /// Pauses every pausable download. Running ones are all asked to stop first and then
    /// awaited together, so the call lasts one settle wait, not one per download.
    pub(super) async fn pause_all(
        &mut self,
        discard_progress: bool,
    ) -> Result<PauseAllOutcome, EngineError> {
        if self.closing {
            return Err(EngineError::ShuttingDown);
        }
        let pause = Control::Pause { discard_progress };
        let mut outcome = PauseAllOutcome::default();
        let mut stopping = Vec::new();
        for id in self.pausable_ids().await? {
            let Some(job) = self.deps.store.load(id).await? else {
                continue;
            };
            let live = self.registry.get(id);
            let state = live.as_ref().map_or(job.state, |live| live.state());
            let resumable = job.transfer_mode.is_none_or(is_resumable_mode);
            match transition(state, command_of(pause, resumable)) {
                Err(TransitionError::NotResumable) => outcome.skipped_not_resumable.push(id),
                Err(TransitionError::InvalidState { .. }) => {}
                Ok(step) if !step.changed => {}
                Ok(_) => match live {
                    Some(live) => {
                        live.stop().request(StopReason::Pause);
                        stopping.push(live);
                    }
                    None => {
                        if self.control_idle(job, pause).await? == DownloadState::Paused {
                            outcome.paused.push(id);
                        }
                    }
                },
            }
        }
        let deadline = Instant::now() + SETTLE_WAIT;
        for live in stopping {
            let _ = timeout_at(deadline, live.finished()).await;
            let id = live.id();
            if self.stored_state(id).await.ok() == Some(DownloadState::Paused) {
                outcome.paused.push(id);
            }
        }
        Ok(outcome)
    }

    /// Every download `PauseAll` may touch: the running ones and the idle pausable ones.
    async fn pausable_ids(&self) -> Result<Vec<Uuid>, EngineError> {
        let mut ids = self.registry.ids();
        for id in self.deps.store.ids_in_states(&PAUSABLE_IDLE).await? {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    /// Resumes every paused download and admits them together, best first.
    pub(super) async fn resume_all(&mut self) -> Result<Vec<Uuid>, EngineError> {
        if self.closing {
            return Err(EngineError::ShuttingDown);
        }
        let mut resumed = Vec::new();
        for id in self
            .deps
            .store
            .ids_in_states(&[DownloadState::Paused])
            .await?
        {
            let Some(job) = self.deps.store.load(id).await? else {
                continue;
            };
            match self.requeue_idle(job, Control::Resume).await {
                Ok(DownloadState::Queued) => resumed.push(id),
                Ok(_) | Err(EngineError::InvalidState(_)) => {}
                Err(other) => return Err(other),
            }
        }
        self.admit().await;
        Ok(resumed)
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

    /// No task is running: move the stored state by compare-and-set, then admit.
    async fn control_idle(
        &mut self,
        job: StoredJob,
        control: Control,
    ) -> Result<DownloadState, EngineError> {
        let state = self.requeue_idle(job, control).await?;
        if state == DownloadState::Queued {
            self.admit().await;
        }
        Ok(state)
    }

    /// [`control_idle`](Self::control_idle) without the admission, for batches.
    async fn requeue_idle(
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
        self.queue.remove(job.id);
        self.deps
            .sink
            .emit(EngineEvent::Changed { id: job.id })
            .await;
        if step.to == DownloadState::Queued {
            job.state = DownloadState::Queued;
            job.state_reason = None;
            self.queue_job(job);
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
        self.queue.remove(id);
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
