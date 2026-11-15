//! Agent startup, serving and shutdown (ARCHITECTURE 2.3 and 2.6).
//!
//! Startup order, with the exit code of each failure:
//! 1. arguments (2) are parsed by [`Config::parse`];
//! 2. [`prepare`]: resolve and validate the data directory (6), take `run\agent.lock`,
//!    retrying up to 3 s (3). The caller then starts the rolling file logs;
//! 3. [`start`]: claim the pipe with `first_pipe_instance(true)`, retrying `NameInUse` up to
//!    3 s, else a `forgedm::security` event (4). No connection is accepted yet;
//! 4. open the database with the DPAPI sealer and migrate (5), reconcile, start the StateHub;
//! 5. start serving and announce `ready`.
//!
//! Shutdown (`ShutdownAgentSafely`, Ctrl+C or the external token) stops accepting, drains,
//! settles active downloads, closes the database and releases the lock.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use forgedm_ipc::{Connection, HandlerFuture, IpcError, RequestHandler, Server, ServerOptions};
use forgedm_platform::instance_lock::{InstanceLock, LockError};
use forgedm_protocol::{AgentState, DownloadSummary, Request};
use forgedm_storage::{Storage, StorageError};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

use crate::config::{Config, effective_proxy_source, pipe_name};
use crate::data_dir::DataPaths;
use crate::error::StartupError;
use crate::events_hub::{self, HubHandle, HubInit};
use crate::handlers::{AgentHandler, starting_response};
use crate::reconcile;
use crate::sealer::DpapiSealer;
use crate::summary::summarize;

/// How long the instance lock and the pipe name are retried at startup (A.9).
pub const STARTUP_RETRY_TOTAL: Duration = Duration::from_secs(3);
/// Pause between startup retries.
const STARTUP_RETRY_INTERVAL: Duration = Duration::from_millis(250);
/// Longest a graceful shutdown waits for connections to drain (ARCHITECTURE 2.6).
const DRAIN_TIMEOUT: Duration = Duration::from_secs(15);
/// Rows read per page when loading downloads into the hub.
const LOAD_PAGE: u32 = 500;
/// Most downloads loaded at startup (the queue cap, A.9 `QUEUE_MAX`).
const LOAD_MAX: usize = 10_000;

/// Data directory validated and instance lock held; logging not started yet.
#[derive(Debug)]
pub struct Prepared {
    config: Config,
    paths: DataPaths,
    pipe_name: String,
    /// Held for its `Drop`: the OS lock lives as long as this value.
    _lock: InstanceLock,
}

impl Prepared {
    /// The `logs` directory to start file logging in.
    #[must_use]
    pub fn logs_dir(&self) -> &std::path::Path {
        &self.paths.logs
    }

    /// The configured `--log-level` filter.
    #[must_use]
    pub fn log_filter(&self) -> &str {
        &self.config.log_filter
    }
}

/// Resolves and validates the data directory and takes the single-instance lock.
///
/// # Errors
/// [`StartupError::DataDir`] (6), [`StartupError::AlreadyRunning`] (3) after retrying for
/// 3 s, or an internal error (1).
pub fn prepare(config: Config) -> Result<Prepared, StartupError> {
    let root = config.resolve_data_root().map_err(StartupError::Resolve)?;
    let paths = DataPaths::prepare(&root)?;
    let explicit_root = config.data_dir.as_ref().map(|_| paths.root.as_path());
    let pipe_name = pipe_name(explicit_root).map_err(StartupError::Resolve)?;
    let lock = take_lock(&paths)?;
    Ok(Prepared {
        config,
        paths,
        pipe_name,
        _lock: lock,
    })
}

/// Takes `run\agent.lock`, retrying while it is held: after a kill the OS can release the
/// previous holder's lock a moment late.
fn take_lock(paths: &DataPaths) -> Result<InstanceLock, StartupError> {
    let started = Instant::now();
    loop {
        match InstanceLock::try_acquire(&paths.lock) {
            Ok(lock) => return Ok(lock),
            Err(LockError::AlreadyHeld) if started.elapsed() < STARTUP_RETRY_TOTAL => {
                std::thread::sleep(STARTUP_RETRY_INTERVAL);
            }
            Err(LockError::AlreadyHeld) => return Err(StartupError::AlreadyRunning),
            Err(LockError::Io(err)) => return Err(StartupError::Lock(err)),
        }
    }
}

/// The handler installed once startup finished; until then clients get `busy`.
#[derive(Default)]
struct Deferred {
    inner: OnceLock<AgentHandler>,
}

impl RequestHandler for Deferred {
    fn handle<'a>(&'a self, conn: &'a mut Connection, request: Request) -> HandlerFuture<'a> {
        match self.inner.get() {
            Some(handler) => handler.handle(conn, request),
            None => Box::pin(async move { starting_response(&request) }),
        }
    }
}

/// A running agent: the hub, the pipe and the supervising task.
#[derive(Debug)]
pub struct RunningAgent {
    hub: HubHandle,
    pipe_name: String,
    task: JoinHandle<Result<(), StartupError>>,
}

impl RunningAgent {
    /// The state hub (the engine publishes committed changes through it).
    #[must_use]
    pub fn hub(&self) -> &HubHandle {
        &self.hub
    }

    /// The pipe name clients connect to.
    #[must_use]
    pub fn pipe_name(&self) -> &str {
        &self.pipe_name
    }

    /// Waits until the agent has shut down.
    ///
    /// # Errors
    /// [`StartupError::PipeSecurity`] when the server stopped on a fatal pipe error,
    /// [`StartupError::Internal`] when the supervising task panicked.
    pub async fn wait(self) -> Result<(), StartupError> {
        self.task
            .await
            .map_err(|err| StartupError::Internal(format!("supervisor task failed: {err}")))?
    }
}

/// Starts a graceful shutdown: broadcast `shutting_down`, then stop accepting.
pub async fn request_shutdown(hub: &HubHandle, shutdown: &CancellationToken) {
    if hub.set_state(AgentState::ShuttingDown).await.is_err() {
        tracing::warn!("state hub already stopped while starting shutdown");
    }
    shutdown.cancel();
}

/// Claims the pipe, opens storage, reconciles, starts the hub and begins serving.
///
/// Cancelling `shutdown` stops the agent gracefully.
///
/// # Errors
/// [`StartupError::PipeSecurity`] (4) or [`StartupError::Storage`] (5). The lock is
/// released when this returns an error.
pub async fn start(
    prepared: Prepared,
    shutdown: CancellationToken,
) -> Result<RunningAgent, StartupError> {
    let started = Instant::now();
    let deferred = Arc::new(Deferred::default());
    let server = claim_pipe(&prepared.pipe_name, deferred.clone()).await?;
    let storage = open_storage(&prepared).await?;
    let hub = match start_hub(&prepared, &storage, started).await {
        Ok(hub) => hub,
        Err(err) => {
            storage.close().await;
            return Err(err);
        }
    };
    let handler = AgentHandler::new(hub.clone(), shutdown.clone(), started);
    let ready = if deferred.inner.set(handler).is_err() {
        Err(StartupError::Internal("handler installed twice".to_owned()))
    } else {
        hub.set_state(AgentState::Ready)
            .await
            .map_err(|err| StartupError::Internal(err.to_string()))
    };
    if let Err(err) = ready {
        storage.close().await;
        return Err(err);
    }
    tracing::info!(pid = std::process::id(), "agent ready");
    let pipe_name = prepared.pipe_name.clone();
    let task = tokio::spawn(supervise(server, shutdown, storage, hub.clone(), prepared));
    Ok(RunningAgent {
        hub,
        pipe_name,
        task,
    })
}

/// Creates the first pipe instance, retrying `NameInUse` for 3 s. A name that stays taken
/// while we hold the lock is squatted: the failure is a security event and exit 4.
async fn claim_pipe(name: &str, handler: Arc<Deferred>) -> Result<Server, StartupError> {
    let started = Instant::now();
    loop {
        match Server::bind(name, handler.clone(), ServerOptions::default()) {
            Ok(server) => return Ok(server),
            Err(IpcError::NameInUse) if started.elapsed() < STARTUP_RETRY_TOTAL => {
                sleep(STARTUP_RETRY_INTERVAL).await;
            }
            Err(err) => return Err(StartupError::PipeSecurity(err)),
        }
    }
}

async fn open_storage(prepared: &Prepared) -> Result<Storage, StartupError> {
    let storage = Storage::open(&prepared.paths.db, Arc::new(DpapiSealer)).await?;
    match reconcile::on_agent_start(&storage).await {
        Ok(settled) => {
            tracing::info!(?settled, "startup reconcile finished");
            Ok(storage)
        }
        Err(err) => {
            storage.clone().close().await;
            Err(err.into())
        }
    }
}

async fn start_hub(
    prepared: &Prepared,
    storage: &Storage,
    started: Instant,
) -> Result<HubHandle, StartupError> {
    let initial = load_summaries(storage).await?;
    let init = HubInit {
        instance_id: uuid::Uuid::now_v7(),
        proxy_source: effective_proxy_source(prepared.config.proxy, |name| {
            std::env::var(name).ok()
        }),
        data_dir: prepared.paths.root.to_string_lossy().into_owned(),
        started,
        pid: std::process::id(),
    };
    Ok(events_hub::spawn(init, initial))
}

/// Loads stored downloads (newest first) for the hub, up to the queue cap.
async fn load_summaries(storage: &Storage) -> Result<Vec<DownloadSummary>, StorageError> {
    let downloads = storage.downloads();
    let mut all = Vec::new();
    while all.len() < LOAD_MAX {
        let offset = u32::try_from(all.len()).unwrap_or(u32::MAX);
        let page = downloads.list(None, LOAD_PAGE, offset).await?;
        let last = page.len() < usize::try_from(LOAD_PAGE).unwrap_or(usize::MAX);
        all.extend(page.iter().map(summarize));
        if last {
            break;
        }
    }
    if all.len() >= LOAD_MAX {
        tracing::warn!(loaded = all.len(), "download table is at the loading cap");
    }
    Ok(all)
}

/// Serves until shutdown, then settles downloads, closes the database and releases the lock.
async fn supervise(
    server: Server,
    shutdown: CancellationToken,
    storage: Storage,
    hub: HubHandle,
    prepared: Prepared,
) -> Result<(), StartupError> {
    let served = serve_until_shutdown(server, &shutdown).await;
    request_shutdown(&hub, &shutdown).await;
    if let Err(err) = reconcile::on_agent_shutdown(&storage).await {
        tracing::error!(error = %err, "could not settle downloads at shutdown");
    }
    storage.close().await;
    tracing::info!("agent stopped");
    drop(prepared);
    served.map_err(StartupError::PipeSecurity)
}

/// Runs the accept loop. After shutdown is requested, connections get [`DRAIN_TIMEOUT`] to
/// finish; whatever is still open after that is dropped.
async fn serve_until_shutdown(
    server: Server,
    shutdown: &CancellationToken,
) -> Result<(), IpcError> {
    let serve = server.serve(shutdown.clone());
    tokio::pin!(serve);
    tokio::select! {
        result = &mut serve => result,
        () = shutdown.cancelled() => match timeout(DRAIN_TIMEOUT, &mut serve).await {
            Ok(result) => result,
            Err(_elapsed) => {
                tracing::warn!(
                    timeout_secs = DRAIN_TIMEOUT.as_secs(),
                    "connections did not drain in time; closing them"
                );
                Ok(())
            }
        },
    }
}
