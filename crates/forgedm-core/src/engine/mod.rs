//! The engine actor: admission, per-download tasks and live progress (ARCHITECTURE 4.1).
//!
//! Commands arrive on a bounded channel ([`DISPATCH_CAPACITY`]); a full channel, or no
//! reply within [`ENGINE_REPLY_TIMEOUT`], is `busy`. Admitted downloads run as one tokio
//! task each, up to [`DEFAULT_MAX_ACTIVE`]. S1.7 admits first come, first served;
//! priorities and per-host budgets arrive with S1.13.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use forgedm_platform::motw::MotwService;
use forgedm_protocol::DownloadState;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior, interval, timeout};
use tokio_util::task::TaskTracker;
use uuid::Uuid;

mod cleanup;
mod control;
mod live;
mod task;

pub use cleanup::{Cleanup, delete_stale_part};
pub use live::{LiveDownload, Registry, StopReason, StopSignal};

use crate::client::HttpClient;
use crate::error::EngineError;
use crate::ports::{
    DownloadStore, EngineEvent, EngineEventSink, NewJob, ProgressSample, StoreEvent, StoredJob,
    StoredProgress,
};
use crate::progress::TICK;
use crate::state::{Command, transition};
use task::{DownloadTask, TaskCtx};

/// Commands the dispatch channel holds (implementation constant, ARCHITECTURE 4.1).
pub const DISPATCH_CAPACITY: usize = 64;
/// How long a caller waits for the engine to answer (A.9 `ENGINE_REPLY_TIMEOUT`).
pub const ENGINE_REPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// Downloads that run at once by default (A.9 `MAX_ACTIVE_DOWNLOADS`).
pub const DEFAULT_MAX_ACTIVE: usize = 3;
/// Stored downloads the queue accepts (A.9 `QUEUE_MAX`).
pub const QUEUE_MAX: usize = 10_000;
/// How long shutdown waits for running downloads to settle (ARCHITECTURE 2.6).
pub const SHUTDOWN_WAIT: Duration = Duration::from_secs(15);
/// `state_reason` of a download restarted from zero instead of resumed.
pub const REASON_RESTART_FROM_ZERO: &str = "restart_from_zero";

/// A user command on one download (PLAN A.6 command semantics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// Pause. An active non-resumable download needs `discard_progress`.
    Pause {
        /// Accept losing the progress of a download that cannot resume.
        discard_progress: bool,
    },
    /// Resume a paused or interrupted download (retry now from `retry_waiting`).
    Resume,
    /// Cancel; the `.part` is kept.
    Cancel,
    /// Retry a failed or cancelled download.
    Retry {
        /// Forget progress and validators and start over.
        restart_from_zero: bool,
    },
}

/// Engine tunables.
#[derive(Debug, Clone, Copy)]
pub struct EngineConfig {
    /// Downloads that run at once.
    pub max_active: usize,
    /// Stored downloads the queue accepts before `limit_exceeded`.
    pub queue_max: usize,
    /// How long [`EngineHandle`] calls wait for an answer.
    pub reply_timeout: Duration,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            max_active: DEFAULT_MAX_ACTIVE,
            queue_max: QUEUE_MAX,
            reply_timeout: ENGINE_REPLY_TIMEOUT,
        }
    }
}

/// What the engine needs from outside.
pub struct EngineDeps {
    /// Persistence.
    pub store: Arc<dyn DownloadStore>,
    /// Where events go.
    pub sink: Arc<dyn EngineEventSink>,
    /// The HTTP client.
    pub client: HttpClient,
    /// Mark-of-the-Web service.
    pub motw: Arc<MotwService>,
}

/// The reply to an accepted enqueue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enqueued {
    /// The new download's id.
    pub id: Uuid,
    /// Its initial state (`queued`).
    pub state: DownloadState,
    /// The file name shown for it.
    pub display_name: String,
}

enum Cmd {
    Enqueue {
        job: Box<NewJob>,
        reply: oneshot::Sender<Result<Enqueued, EngineError>>,
    },
    Recover {
        reply: oneshot::Sender<Result<usize, EngineError>>,
    },
    Control {
        id: Uuid,
        control: Control,
        reply: oneshot::Sender<Result<DownloadState, EngineError>>,
    },
    Remove {
        id: Uuid,
        delete_partial: bool,
        reply: oneshot::Sender<Result<(), EngineError>>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

/// A cheap, cloneable handle to the engine.
#[derive(Clone)]
pub struct EngineHandle {
    tx: mpsc::Sender<Cmd>,
    registry: Arc<Registry>,
    reply_timeout: Duration,
}

impl std::fmt::Debug for EngineHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineHandle")
            .field("running", &self.registry.len())
            .finish_non_exhaustive()
    }
}

impl EngineHandle {
    /// Queues a new download and starts it when a slot is free.
    ///
    /// # Errors
    /// [`EngineError::Busy`] when the dispatch channel is full or the engine does not
    /// answer in time, [`EngineError::LimitExceeded`] at the queue cap,
    /// [`EngineError::ShuttingDown`], or a storage failure.
    pub async fn enqueue(&self, job: NewJob) -> Result<Enqueued, EngineError> {
        let (reply, answer) = oneshot::channel();
        let cmd = Cmd::Enqueue {
            job: Box::new(job),
            reply,
        };
        self.call(cmd, answer).await?
    }

    /// Resumes work found in the store at agent start: queued downloads are admitted, and
    /// interrupted ones restart from zero. Returns how many were queued.
    ///
    /// # Errors
    /// As [`enqueue`](Self::enqueue).
    pub async fn recover(&self) -> Result<usize, EngineError> {
        let (reply, answer) = oneshot::channel();
        self.call(Cmd::Recover { reply }, answer).await?
    }

    /// Applies a user command and returns the resulting state. Commands are serialized by
    /// the engine and every state change is a compare-and-set, so a repeated or racing
    /// command is a no-op that reports the unchanged state. Pause and cancel of a running
    /// download wait (a few seconds at most) for it to settle and report the settled state.
    ///
    /// # Errors
    /// [`EngineError::NotFound`], [`EngineError::InvalidState`] when A.6 has no such edge,
    /// [`EngineError::NotResumable`] for a pause of a non-resumable download without
    /// `discard_progress`, plus the errors of [`enqueue`](Self::enqueue).
    pub async fn control(&self, id: Uuid, control: Control) -> Result<DownloadState, EngineError> {
        let (reply, answer) = oneshot::channel();
        self.call(Cmd::Control { id, control, reply }, answer)
            .await?
    }

    /// Cancels a running download if needed, then deletes it. With `delete_partial_file`
    /// the recorded `.part` goes too, but only when it is provably the file we created.
    /// Completed files are never deleted.
    ///
    /// # Errors
    /// [`EngineError::NotFound`] (a repeated remove), [`EngineError::Busy`] when the
    /// download does not settle in time, plus the errors of [`enqueue`](Self::enqueue).
    pub async fn remove(&self, id: Uuid, delete_partial_file: bool) -> Result<(), EngineError> {
        let (reply, answer) = oneshot::channel();
        let cmd = Cmd::Remove {
            id,
            delete_partial: delete_partial_file,
            reply,
        };
        self.call(cmd, answer).await?
    }

    /// Asks a running download to stop and become `cancelled` (the `.part` is kept).
    /// Returns whether `id` was running. [`control`](Self::control) is the checked,
    /// serialized form.
    #[must_use]
    pub fn cancel(&self, id: Uuid) -> bool {
        self.registry.get(id).is_some_and(|live| {
            live.stop().request(StopReason::Cancel);
            true
        })
    }

    /// The live numbers of a running download.
    #[must_use]
    pub fn live(&self, id: Uuid) -> Option<ProgressSample> {
        self.registry.get(id).map(|live| live.peek())
    }

    /// Downloads running right now.
    #[must_use]
    pub fn running(&self) -> usize {
        self.registry.len()
    }

    /// Stops every download (they become `interrupted`) and waits for them, up to
    /// [`SHUTDOWN_WAIT`].
    pub async fn shutdown(&self) {
        let (reply, answer) = oneshot::channel();
        if self.tx.send(Cmd::Shutdown { reply }).await.is_ok() {
            let _ = answer.await;
        }
    }

    async fn call<T>(&self, cmd: Cmd, answer: oneshot::Receiver<T>) -> Result<T, EngineError> {
        self.tx.try_send(cmd).map_err(|err| match err {
            mpsc::error::TrySendError::Full(_) => EngineError::Busy,
            mpsc::error::TrySendError::Closed(_) => EngineError::ShuttingDown,
        })?;
        match timeout(self.reply_timeout, answer).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(_)) => Err(EngineError::ShuttingDown),
            Err(_) => Err(EngineError::Busy),
        }
    }
}

/// Starts the engine and its progress ticker; both end when [`EngineHandle::shutdown`] is
/// called or every handle is dropped.
#[must_use]
pub fn start(config: EngineConfig, deps: EngineDeps) -> EngineHandle {
    let (tx, rx) = mpsc::channel(DISPATCH_CAPACITY);
    let (finished_tx, finished_rx) = mpsc::unbounded_channel();
    let registry = Arc::new(Registry::default());
    let ticker = spawn_ticker(Arc::clone(&registry), Arc::clone(&deps.sink));
    let actor = Actor {
        config,
        deps,
        registry: Arc::clone(&registry),
        queue: VecDeque::new(),
        active: 0,
        tracker: TaskTracker::new(),
        finished_tx,
        closing: false,
    };
    tokio::spawn(actor.run(rx, finished_rx, ticker));
    EngineHandle {
        tx,
        registry,
        reply_timeout: config.reply_timeout,
    }
}

/// One `Progress` event per tick for every running download (4 Hz, A.9). Dropped when the
/// sink has no room: progress is allowed to drop.
fn spawn_ticker(registry: Arc<Registry>, sink: Arc<dyn EngineEventSink>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = interval(TICK);
        // After a stall, skip the missed ticks: progress is 4 Hz, never a catch-up burst.
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let samples = registry.tick_all(Instant::now());
            if !samples.is_empty() {
                let _ = sink.try_emit(EngineEvent::Progress(samples));
            }
        }
    })
}

struct Actor {
    config: EngineConfig,
    deps: EngineDeps,
    registry: Arc<Registry>,
    queue: VecDeque<StoredJob>,
    active: usize,
    tracker: TaskTracker,
    finished_tx: mpsc::UnboundedSender<Uuid>,
    closing: bool,
}

impl Actor {
    async fn run(
        mut self,
        mut rx: mpsc::Receiver<Cmd>,
        mut finished: mpsc::UnboundedReceiver<Uuid>,
        ticker: JoinHandle<()>,
    ) {
        loop {
            tokio::select! {
                cmd = rx.recv() => match cmd {
                    Some(cmd) => {
                        if self.handle(cmd).await {
                            break;
                        }
                    }
                    None => break,
                },
                Some(_id) = finished.recv() => {
                    self.active = self.active.saturating_sub(1);
                    self.admit().await;
                }
            }
        }
        ticker.abort();
    }

    /// Returns true when the engine should stop.
    async fn handle(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::Enqueue { job, reply } => {
                let result = self.enqueue(*job).await;
                let _ = reply.send(result);
                false
            }
            Cmd::Recover { reply } => {
                let _ = reply.send(self.recover().await);
                false
            }
            Cmd::Control { id, control, reply } => {
                let _ = reply.send(self.control(id, control).await);
                false
            }
            Cmd::Remove {
                id,
                delete_partial,
                reply,
            } => {
                let _ = reply.send(self.remove(id, delete_partial).await);
                false
            }
            Cmd::Shutdown { reply } => {
                self.shutdown().await;
                let _ = reply.send(());
                true
            }
        }
    }

    async fn enqueue(&mut self, new: NewJob) -> Result<Enqueued, EngineError> {
        if self.closing {
            return Err(EngineError::ShuttingDown);
        }
        if self.deps.store.count().await? >= self.config.queue_max {
            return Err(EngineError::LimitExceeded);
        }
        let stored = StoredJob {
            id: new.id,
            state: DownloadState::Queued,
            state_reason: None,
            url: new.url.clone(),
            target_dir: new.target_dir.clone(),
            requested_name: new.requested_name.clone(),
            display_name: new.display_name.clone(),
            file_name: None,
            expected_sha256: new.expected_sha256.clone(),
            part_path: None,
            part_file_id: None,
            transfer_mode: None,
            progress: StoredProgress::default(),
        };
        let enqueued = Enqueued {
            id: new.id,
            state: DownloadState::Queued,
            display_name: new.display_name.clone(),
        };
        self.deps.store.insert(new).await?;
        self.deps
            .sink
            .emit(EngineEvent::Added { id: stored.id })
            .await;
        self.queue.push_back(stored);
        self.admit().await;
        Ok(enqueued)
    }

    /// Requeues the work a previous agent left: `queued` downloads are admitted and
    /// `interrupted` ones auto-resume (each task decides whether it can continue from its
    /// committed bytes or must restart from zero).
    async fn recover(&mut self) -> Result<usize, EngineError> {
        let work = self.deps.store.startup_work().await?;
        let mut queued = 0;
        for job in work {
            match job.state {
                DownloadState::Queued => self.queue.push_back(job),
                DownloadState::Interrupted => {
                    if let Some(requeued) = self.auto_resume(job).await {
                        self.queue.push_back(requeued);
                    } else {
                        continue;
                    }
                }
                _ => continue,
            }
            queued += 1;
        }
        self.admit().await;
        Ok(queued)
    }

    /// `interrupted -> queued`, recorded and announced.
    async fn auto_resume(&self, mut job: StoredJob) -> Option<StoredJob> {
        let step = transition(job.state, Command::AutoResume).ok()?;
        let event = StoreEvent::now("auto_resume").with_code("interrupted");
        let moved = self
            .deps
            .store
            .transition(job.id, job.state, step.to, None, event)
            .await;
        if let Err(err) = moved {
            tracing::error!(error = %err, "could not requeue an interrupted download");
            return None;
        }
        job.state = step.to;
        job.state_reason = None;
        self.deps
            .sink
            .emit(EngineEvent::Changed { id: job.id })
            .await;
        Some(job)
    }

    /// Starts queued downloads while slots are free.
    async fn admit(&mut self) {
        while !self.closing && self.active < self.config.max_active {
            let Some(job) = self.queue.pop_front() else {
                return;
            };
            self.active += 1;
            self.spawn_task(job);
        }
    }

    fn spawn_task(&self, job: StoredJob) {
        let id = job.id;
        let live = Arc::new(LiveDownload::new(id, StopSignal::new()));
        self.registry.insert(Arc::clone(&live));
        let ctx = TaskCtx {
            store: Arc::clone(&self.deps.store),
            sink: Arc::clone(&self.deps.sink),
            client: self.deps.client.clone(),
            motw: Arc::clone(&self.deps.motw),
            live: Arc::clone(&live),
        };
        let supervisor = Supervisor {
            store: Arc::clone(&self.deps.store),
            sink: Arc::clone(&self.deps.sink),
            registry: Arc::clone(&self.registry),
            finished: self.finished_tx.clone(),
            live: Arc::clone(&live),
        };
        let inner = tokio::spawn(DownloadTask::new(job, ctx).run());
        self.tracker.spawn(supervisor.watch(id, inner));
    }

    async fn shutdown(&mut self) {
        self.closing = true;
        self.queue.clear();
        self.registry.stop_all(StopReason::Shutdown);
        self.tracker.close();
        if timeout(SHUTDOWN_WAIT, self.tracker.wait()).await.is_err() {
            tracing::warn!("downloads did not settle before the shutdown deadline");
        }
    }
}

/// Watches one task. Whatever way the task ends, the stored state is not left active: a
/// panic surfaces as `failed(internal)` (or `interrupted` from `pausing`), and a task that
/// ended without settling is moved to `interrupted`; the agent keeps running
/// (ARCHITECTURE 4.13, SFH-15 and SFH-16).
struct Supervisor {
    store: Arc<dyn DownloadStore>,
    sink: Arc<dyn EngineEventSink>,
    registry: Arc<Registry>,
    finished: mpsc::UnboundedSender<Uuid>,
    live: Arc<LiveDownload>,
}

/// Attempts to move a leftover active download off its state.
const LEFTOVER_ATTEMPTS: u32 = 3;
/// Pause between those attempts.
const LEFTOVER_RETRY: Duration = Duration::from_millis(100);

impl Supervisor {
    async fn watch(self, id: Uuid, inner: JoinHandle<()>) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn handle_with_unserved_channel(capacity: usize) -> (EngineHandle, mpsc::Receiver<Cmd>) {
        let (tx, rx) = mpsc::channel(capacity);
        let handle = EngineHandle {
            tx,
            registry: Arc::new(Registry::default()),
            reply_timeout: Duration::from_millis(50),
        };
        (handle, rx)
    }

    #[tokio::test]
    async fn a_full_dispatch_channel_is_busy() {
        let (handle, _rx) = handle_with_unserved_channel(1);
        let (reply, _answer) = oneshot::channel();
        handle.tx.try_send(Cmd::Shutdown { reply }).unwrap();

        let result = handle.recover().await;

        assert!(matches!(result, Err(EngineError::Busy)));
    }

    #[tokio::test(start_paused = true)]
    async fn no_reply_within_the_timeout_is_busy() {
        let (handle, _rx) = handle_with_unserved_channel(4);

        let result = handle.recover().await;

        assert!(matches!(result, Err(EngineError::Busy)));
    }

    #[tokio::test]
    async fn a_stopped_engine_reports_shutting_down() {
        let (handle, rx) = handle_with_unserved_channel(4);
        drop(rx);

        let result = handle.recover().await;

        assert!(matches!(result, Err(EngineError::ShuttingDown)));
    }

    #[test]
    fn defaults_match_the_appendix() {
        let config = EngineConfig::default();
        assert_eq!(config.max_active, 3);
        assert_eq!(config.queue_max, 10_000);
        assert_eq!(config.reply_timeout, Duration::from_secs(5));
    }
}
