//! The engine actor: admission, per-download tasks and live progress (ARCHITECTURE 4.1).
//!
//! Commands arrive on a bounded channel ([`DISPATCH_CAPACITY`]); a full channel, or no
//! reply within [`ENGINE_REPLY_TIMEOUT`], is `busy`. Admitted downloads run as one tokio
//! task each, up to the configured number of active downloads ([`DEFAULT_MAX_ACTIVE`]),
//! admitted by priority, then queue position, then creation time ([`admission`]). Connection
//! permits, bandwidth buckets and the free-space reading are shared through [`budgets`].

use std::sync::Arc;
use std::time::Duration;

use forgedm_platform::motw::MotwService;
use forgedm_protocol::DownloadState;
use tokio::sync::{mpsc, oneshot};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{Instant, MissedTickBehavior, interval, timeout};
use tokio_util::task::TaskTracker;
use uuid::Uuid;

pub mod admission;
pub mod budgets;
mod cleanup;
mod control;
pub mod hosts;
mod live;
mod retry_timer;
mod supervisor;
mod task;

pub use admission::{EffectiveGlobal, GlobalLimits, Queue};
pub use cleanup::{Cleanup, delete_stale_part};
pub use control::PauseAllOutcome;
pub use live::{LiveDownload, Registry, StopReason, StopSignal};

use crate::client::HttpClient;
use crate::error::EngineError;
use crate::ports::{
    DownloadStore, EngineEvent, EngineEventSink, NewJob, ProgressSample, StoreEvent, StoredJob,
    StoredProgress,
};
use crate::progress::TICK;
use crate::state::{Command, transition};
use crate::transfer::resilience::RetryEnv;
use crate::transfer::segmented::ConnectionCaps;
use budgets::{Budgets, CONNS_GLOBAL, FreeSpace, SystemFreeSpace};
use forgedm_protocol::{MAX_CONNECTIONS, MIN_CONNECTIONS, Priority};
use hosts::{CONNS_PER_HOST, HostRegistry};
use retry_timer::Due;
use supervisor::Supervisor;
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
    /// Connections open at once across all downloads (A.9 `CONNS_GLOBAL`).
    pub global_connections: usize,
    /// Connections open at once to one host (A.9 `CONNS_PER_HOST`, AIMD-adjusted below it).
    pub host_connections: u32,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            max_active: DEFAULT_MAX_ACTIVE,
            queue_max: QUEUE_MAX,
            reply_timeout: ENGINE_REPLY_TIMEOUT,
            global_connections: CONNS_GLOBAL,
            host_connections: CONNS_PER_HOST,
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
    SetMaxConnections {
        id: Uuid,
        value: u32,
        reply: oneshot::Sender<Result<u32, EngineError>>,
    },
    SetPriority {
        id: Uuid,
        priority: Priority,
        reply: oneshot::Sender<Result<(), EngineError>>,
    },
    SetRateLimit {
        id: Uuid,
        limit: Option<u64>,
        reply: oneshot::Sender<Result<Option<u64>, EngineError>>,
    },
    UpdateGlobal {
        patch: GlobalLimits,
        reply: oneshot::Sender<Result<EffectiveGlobal, EngineError>>,
    },
    PauseAll {
        discard_progress: bool,
        reply: oneshot::Sender<Result<PauseAllOutcome, EngineError>>,
    },
    ResumeAll {
        reply: oneshot::Sender<Result<Vec<Uuid>, EngineError>>,
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

    /// Sets the connection cap of download `id` (`1..=16`, clamped), live: a running
    /// segmented download opens or retires connections within a quarter of a second, and a
    /// queued one starts with it. Returns the cap now in force.
    ///
    /// # Errors
    /// [`EngineError::NotFound`] for an unknown download, plus the errors of
    /// [`enqueue`](Self::enqueue).
    pub async fn set_max_connections(&self, id: Uuid, value: u32) -> Result<u32, EngineError> {
        let (reply, answer) = oneshot::channel();
        let cmd = Cmd::SetMaxConnections { id, value, reply };
        self.call(cmd, answer).await?
    }

    /// Changes the priority of download `id`; a queued download is reordered at once.
    ///
    /// # Errors
    /// [`EngineError::NotFound`] for an unknown download, plus the errors of
    /// [`enqueue`](Self::enqueue).
    pub async fn set_priority(&self, id: Uuid, priority: Priority) -> Result<(), EngineError> {
        let (reply, answer) = oneshot::channel();
        let cmd = Cmd::SetPriority {
            id,
            priority,
            reply,
        };
        self.call(cmd, answer).await?
    }

    /// Sets the rate limit of download `id` in bytes per second (`None` = unlimited, at
    /// least 1024 otherwise), live. Returns the limit now in force. Lasts for the agent
    /// session; only the global limit is stored.
    ///
    /// # Errors
    /// [`EngineError::NotFound`] for an unknown download, plus the errors of
    /// [`enqueue`](Self::enqueue).
    pub async fn set_rate_limit(
        &self,
        id: Uuid,
        limit: Option<u64>,
    ) -> Result<Option<u64>, EngineError> {
        let (reply, answer) = oneshot::channel();
        let cmd = Cmd::SetRateLimit { id, limit, reply };
        self.call(cmd, answer).await?
    }

    /// Changes the agent-wide limits live and returns what is in force.
    ///
    /// # Errors
    /// As [`enqueue`](Self::enqueue).
    pub async fn update_global_limits(
        &self,
        patch: GlobalLimits,
    ) -> Result<EffectiveGlobal, EngineError> {
        let (reply, answer) = oneshot::channel();
        self.call(Cmd::UpdateGlobal { patch, reply }, answer)
            .await?
    }

    /// Pauses every pausable download (A.6): queued, active, retrying and interrupted ones.
    /// Active non-resumable downloads are skipped unless `discard_progress`.
    ///
    /// # Errors
    /// As [`enqueue`](Self::enqueue).
    pub async fn pause_all(&self, discard_progress: bool) -> Result<PauseAllOutcome, EngineError> {
        let (reply, answer) = oneshot::channel();
        let cmd = Cmd::PauseAll {
            discard_progress,
            reply,
        };
        self.call(cmd, answer).await?
    }

    /// Resumes every paused download; returns the ids that were queued again.
    ///
    /// # Errors
    /// As [`enqueue`](Self::enqueue).
    pub async fn resume_all(&self) -> Result<Vec<Uuid>, EngineError> {
        let (reply, answer) = oneshot::channel();
        self.call(Cmd::ResumeAll { reply }, answer).await?
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
    start_with(config, deps, RetryEnv::default())
}

/// [`start`] with an explicit retry environment (the sleeper that backoff waits go through
/// and the jitter seed), so tests can run retries without real waiting.
#[must_use]
pub fn start_with(config: EngineConfig, deps: EngineDeps, retry: RetryEnv) -> EngineHandle {
    start_with_space(config, deps, retry, Arc::new(SystemFreeSpace))
}

/// [`start_with`] with an explicit free-space reading, so tests can inject a full disk.
#[must_use]
pub fn start_with_space(
    config: EngineConfig,
    deps: EngineDeps,
    retry: RetryEnv,
    space: Arc<dyn FreeSpace>,
) -> EngineHandle {
    let (tx, rx) = mpsc::channel(DISPATCH_CAPACITY);
    let (finished_tx, finished_rx) = mpsc::unbounded_channel();
    // Unbounded on purpose: one small message per armed retry timer, never more than one per
    // download at a time in practice, and the sender must never block a timer task.
    let (due_tx, due_rx) = mpsc::unbounded_channel();
    let registry = Arc::new(Registry::default());
    let ticker = spawn_ticker(Arc::clone(&registry), Arc::clone(&deps.sink));
    let actor = Actor {
        config,
        deps,
        registry: Arc::clone(&registry),
        queue: Queue::new(),
        active: 0,
        tracker: TaskTracker::new(),
        finished_tx,
        closing: false,
        caps: Arc::new(ConnectionCaps::default()),
        hosts: Arc::new(HostRegistry::new(config.host_connections)),
        budgets: Arc::new(Budgets::new(config.global_connections, space)),
        retry,
        due_tx,
        timers: JoinSet::new(),
    };
    tokio::spawn(actor.run(rx, finished_rx, due_rx, ticker));
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
    queue: Queue,
    active: usize,
    tracker: TaskTracker,
    finished_tx: mpsc::UnboundedSender<Uuid>,
    closing: bool,
    caps: Arc<ConnectionCaps>,
    hosts: Arc<HostRegistry>,
    budgets: Arc<Budgets>,
    retry: RetryEnv,
    due_tx: mpsc::UnboundedSender<Due>,
    timers: JoinSet<()>,
}

impl Actor {
    async fn run(
        mut self,
        mut rx: mpsc::Receiver<Cmd>,
        mut finished: mpsc::UnboundedReceiver<Uuid>,
        mut due: mpsc::UnboundedReceiver<Due>,
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
                Some(id) = finished.recv() => {
                    self.active = self.active.saturating_sub(1);
                    self.arm_if_waiting(id).await;
                    self.admit().await;
                }
                Some(due) = due.recv() => self.retry_due(due).await,
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
                let result = self.remove(id, delete_partial).await;
                if result.is_ok() {
                    self.caps.forget(id);
                    self.budgets.forget(id);
                }
                let _ = reply.send(result);
                false
            }
            Cmd::SetMaxConnections { id, value, reply } => {
                let _ = reply.send(self.set_max_connections(id, value).await);
                false
            }
            Cmd::SetPriority {
                id,
                priority,
                reply,
            } => {
                let _ = reply.send(self.set_priority(id, priority).await);
                false
            }
            Cmd::SetRateLimit { id, limit, reply } => {
                let _ = reply.send(self.set_rate_limit(id, limit).await);
                false
            }
            Cmd::UpdateGlobal { patch, reply } => {
                let _ = reply.send(self.update_global_limits(patch).await);
                false
            }
            Cmd::PauseAll {
                discard_progress,
                reply,
            } => {
                let _ = reply.send(self.pause_all(discard_progress).await);
                false
            }
            Cmd::ResumeAll { reply } => {
                let _ = reply.send(self.resume_all().await);
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
            max_connections: new.max_connections,
            priority: new.priority,
            queue_position: new.created_at_ms,
            created_at_ms: new.created_at_ms,
        };
        self.caps.set(new.id, new.max_connections);
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
        self.queue_job(stored);
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
                DownloadState::Queued => self.queue_job(job),
                DownloadState::Interrupted => {
                    if let Some(requeued) = self.auto_resume(job).await {
                        self.queue_job(requeued);
                    } else {
                        continue;
                    }
                }
                DownloadState::RetryWaiting => {
                    let at_ms = self.deps.store.next_retry_at(job.id).await?;
                    self.arm_timer(job.id, at_ms.unwrap_or(0));
                    continue;
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

    /// Applies a new connection cap to download `id`.
    async fn set_max_connections(&mut self, id: Uuid, value: u32) -> Result<u32, EngineError> {
        if self.closing {
            return Err(EngineError::ShuttingDown);
        }
        self.deps
            .store
            .load(id)
            .await?
            .ok_or(EngineError::NotFound)?;
        // Persist first: a cap that was not stored would silently revert at the next start.
        let value = value.clamp(MIN_CONNECTIONS, MAX_CONNECTIONS);
        self.deps.store.set_max_connections(id, value).await?;
        self.caps.set(id, value);
        Ok(self.caps.cap_for(id, value).get())
    }

    pub(super) fn spawn_task(&self, job: StoredJob) {
        let id = job.id;
        let live = Arc::new(LiveDownload::new(id, StopSignal::new()));
        self.registry.insert(Arc::clone(&live));
        let ctx = TaskCtx {
            store: Arc::clone(&self.deps.store),
            sink: Arc::clone(&self.deps.sink),
            client: self.deps.client.clone(),
            motw: Arc::clone(&self.deps.motw),
            live: Arc::clone(&live),
            cap: self.caps.cap_for(id, job.max_connections),
            hosts: Arc::clone(&self.hosts),
            budgets: Arc::clone(&self.budgets),
            bucket: self.budgets.download_bucket(id),
            retry: self.retry.clone(),
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
        assert_eq!(config.global_connections, 32);
        assert_eq!(config.host_connections, 8);
    }
}
