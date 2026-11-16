//! Watches one download task and settles what it leaves behind.

use std::sync::Arc;
use std::time::Duration;

use forgedm_protocol::DownloadState;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::live::{LiveDownload, Registry};
use crate::ports::{DownloadStore, EngineEvent, EngineEventSink, StoreEvent};
use crate::state::{Command, transition};

/// Watches one task. Whatever way the task ends, the stored state is not left active: a
/// panic surfaces as `failed(internal)` (or `interrupted` from `pausing`), and a task that
/// ended without settling is moved to `interrupted`; the agent keeps running
/// (ARCHITECTURE 4.13, SFH-15 and SFH-16).
pub(super) struct Supervisor {
    pub(super) store: Arc<dyn DownloadStore>,
    pub(super) sink: Arc<dyn EngineEventSink>,
    pub(super) registry: Arc<Registry>,
    pub(super) finished: mpsc::UnboundedSender<Uuid>,
    pub(super) live: Arc<LiveDownload>,
}

/// Attempts to move a leftover active download off its state.
const LEFTOVER_ATTEMPTS: u32 = 3;
/// Pause between those attempts.
const LEFTOVER_RETRY: Duration = Duration::from_millis(100);

impl Supervisor {
    pub(super) async fn watch(self, id: Uuid, inner: JoinHandle<()>) {
        let panicked = matches!(inner.await, Err(err) if err.is_panic());
        if panicked {
            tracing::error!(%id, "a download task panicked");
        }
        self.settle_leftovers(id, panicked).await;
        self.registry.remove_live(&self.live);
        self.live.mark_finished();
        let _ = self.finished.send(id);
    }

    /// What the stored state of a finished task must become, if it is still active.
    fn leftover_target(state: DownloadState, panicked: bool) -> Option<Command> {
        use DownloadState as S;
        match (state, panicked) {
            (S::Probing | S::Downloading | S::Verifying | S::Finalizing, true) => {
                Some(Command::Fail)
            }
            (S::Pausing, _) | (S::Probing | S::Downloading | S::Verifying, false) => {
                Some(Command::Interrupt)
            }
            _ => None,
        }
    }

    async fn settle_leftovers(&self, id: Uuid, panicked: bool) {
        let failure = crate::error::Failure::new(forgedm_protocol::DownloadErrorCode::Internal);
        for attempt in 1..=LEFTOVER_ATTEMPTS {
            let stored = match self.store.load(id).await {
                Ok(Some(stored)) => stored,
                Ok(None) => return,
                Err(err) => {
                    tracing::warn!(%id, error = %err, "cannot read a finished download");
                    tokio::time::sleep(LEFTOVER_RETRY).await;
                    continue;
                }
            };
            let Some(cmd) = Self::leftover_target(stored.state, panicked) else {
                return;
            };
            let Ok(step) = transition(stored.state, cmd) else {
                return;
            };
            let failing = cmd == Command::Fail;
            let (kind, code, reason) = if failing {
                ("failed", failure.code.as_str(), None)
            } else {
                ("interrupted", "settle_failed", Some("crash".to_owned()))
            };
            let event = StoreEvent::now(kind).with_code(code);
            let moved = self
                .store
                .transition(id, stored.state, step.to, reason, event)
                .await;
            match moved {
                Ok(()) => {
                    tracing::warn!(%id, from = ?stored.state, to = ?step.to, "settled a leftover download");
                    self.sink.emit(EngineEvent::Changed { id }).await;
                    if failing {
                        let error = failure.to_download_error();
                        self.sink.emit(EngineEvent::Failed { id, error }).await;
                    }
                    return;
                }
                Err(err) => {
                    tracing::warn!(%id, attempt, error = %err, "could not settle a leftover download");
                    tokio::time::sleep(LEFTOVER_RETRY).await;
                }
            }
        }
        tracing::error!(%id, "a finished download is still marked active");
    }
}
