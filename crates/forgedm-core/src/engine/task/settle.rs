//! Settling: what a download becomes when its task stops or fails (ARCHITECTURE 4.13).
//!
//! Every path ends with the stored state moved off the active states. A settle that cannot
//! be stored is retried, re-reading the stored state when another writer got there first;
//! if it still fails, the engine's supervisor moves the download to `interrupted` once the
//! task is gone (SFH-16), so no download is left looking active.

use std::time::Duration;

use forgedm_protocol::{DownloadErrorCode, DownloadState};
use tokio::time::sleep;

use super::DownloadTask;
use crate::engine::StopReason;
use crate::error::{Failure, StoreError, TaskStop};
use crate::ports::{EngineEvent, StoreEvent};
use crate::state::{Command, transition};

/// Attempts to store one settle before giving up.
const PARK_ATTEMPTS: u32 = 3;
/// Pause before the first retry; it doubles each time.
const PARK_RETRY_DELAY: Duration = Duration::from_millis(100);
/// The pause command used by a user stop: the transfer is always resumable by then (the
/// engine refused a pause of a non-resumable download before asking the task to stop).
const USER_PAUSE: Command = Command::Pause {
    resumable: true,
    discard_progress: false,
};

impl DownloadTask {
    /// Moves the download to the state `stop` calls for.
    pub(super) async fn settle(&mut self, stop: TaskStop) {
        match stop {
            TaskStop::Fail(failure) => self.fail(&failure).await,
            TaskStop::NeedsCredentials => {
                let failure = Failure::new(DownloadErrorCode::AuthRequired);
                let event = failure_event("needs_credentials", &failure);
                self.park(Command::NeedCredentials, None, event).await;
            }
            TaskStop::PauseFor(code) => self.pause_for(code).await,
            TaskStop::Stopped(StopReason::Cancel) => {
                let event = StoreEvent::now("cancelled").with_code("user");
                self.park(Command::Cancel, Some("user"), event).await;
            }
            TaskStop::Stopped(StopReason::Shutdown) => self.settle_shutdown().await,
            TaskStop::Stopped(StopReason::Pause) => self.settle_pause().await,
        }
    }

    /// Shutdown: active downloads become `interrupted(agent_shutdown)`; a download that was
    /// already `pausing` finishes pausing (ARCHITECTURE 2.6); a queued one stays queued.
    async fn settle_shutdown(&mut self) {
        match self.state {
            DownloadState::Queued => {}
            DownloadState::Pausing => self.quiesce().await,
            _ => {
                let event = StoreEvent::now("interrupted").with_code("agent_shutdown");
                self.park(Command::Interrupt, Some("agent_shutdown"), event)
                    .await;
            }
        }
    }

    /// User pause: `queued -> paused`, or `probing/downloading -> pausing -> paused` once the
    /// transfer has quiesced (the checkpoint was taken when the transfer ended).
    async fn settle_pause(&mut self) {
        match self.state {
            DownloadState::Queued => {
                let event = StoreEvent::now("paused").with_code("user");
                self.park(USER_PAUSE, Some("user"), event).await;
            }
            DownloadState::Probing | DownloadState::Downloading => {
                let event = StoreEvent::now("pausing").with_code("user");
                self.park(USER_PAUSE, Some("user"), event).await;
                if self.state == DownloadState::Pausing {
                    self.quiesce().await;
                }
            }
            DownloadState::Pausing => self.quiesce().await,
            other => tracing::warn!(state = ?other, "a pause arrived in a state that cannot pause"),
        }
    }

    async fn quiesce(&mut self) {
        let event = StoreEvent::now("paused").with_code("user");
        self.park(Command::Quiesced, Some("user"), event).await;
    }

    async fn pause_for(&mut self, code: DownloadErrorCode) {
        let failure = Failure::new(code);
        let event = failure_event("paused", &failure);
        if self.state == DownloadState::Downloading {
            self.park(Command::PauseForDisk, Some(code.as_str()), event)
                .await;
        } else {
            // Only `downloading` has a pause edge for a full disk (A.6): fail elsewhere.
            self.fail(&failure).await;
        }
    }

    async fn fail(&mut self, failure: &Failure) {
        let event = failure_event("failed", failure);
        let before = self.state;
        self.park(Command::Fail, None, event).await;
        if self.state != DownloadState::Failed {
            tracing::error!(?before, code = ?failure.code, "download could not be marked failed");
            return;
        }
        let id = self.job.id;
        self.ctx
            .sink
            .emit(EngineEvent::Failed {
                id,
                error: failure.to_download_error(),
            })
            .await;
    }

    /// Persists `cmd` with retries. A lost compare-and-set re-reads the stored state: if the
    /// command no longer applies to it, someone else already settled the download.
    async fn park(&mut self, cmd: Command, reason: Option<&str>, event: StoreEvent) {
        let mut delay = PARK_RETRY_DELAY;
        for attempt in 1..=PARK_ATTEMPTS {
            let error = match self.advance_with(cmd, reason, event.clone()).await {
                Ok(()) => return,
                Err(stop) => stop,
            };
            tracing::warn!(?error, attempt, state = ?self.state, "could not store a settle");
            if let Some(actual) = self.stored_state().await
                && actual != self.state
            {
                self.set_state(actual);
                if transition(actual, cmd).is_err() {
                    return;
                }
            }
            if attempt < PARK_ATTEMPTS {
                sleep(delay).await;
                delay *= 2;
            }
        }
        tracing::error!(state = ?self.state, ?cmd, "settle was not stored; the supervisor takes over");
    }

    /// The state in the store right now, when it can be read.
    async fn stored_state(&self) -> Option<DownloadState> {
        match self.ctx.store.load(self.job.id).await {
            Ok(Some(stored)) => Some(stored.state),
            Ok(None) => None,
            Err(StoreError::NotFound) => None,
            Err(err) => {
                tracing::warn!(error = %err, "cannot re-read the stored state");
                None
            }
        }
    }
}

/// An event recording a failure: its code and the serialised [`DownloadError`] as detail, so
/// `last_error` can be rebuilt from the event log.
///
/// [`DownloadError`]: forgedm_protocol::DownloadError
fn failure_event(kind: &str, failure: &Failure) -> StoreEvent {
    let event = StoreEvent::now(kind).with_code(failure.code.as_str());
    match serde_json::to_string(&failure.to_download_error()) {
        Ok(detail) => event.with_detail(detail),
        Err(_) => event,
    }
}
