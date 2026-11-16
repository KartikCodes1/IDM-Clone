//! The engine inside the agent: wires the core engine to storage, the StateHub and the
//! platform services (ARCHITECTURE 4.1).
//!
//! - [`EngineControl`] is what the request handlers call; the real implementation is
//!   [`EngineHost`], tests inject their own.
//! - [`HubSink`] turns [`EngineEvent`]s into wire events. It re-reads the stored download
//!   after every persisted change, so the hub only ever sees what is committed.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};

use forgedm_core::client::{HttpClient, ProxyMode as CoreProxyMode};
use forgedm_core::clock::BoxFuture;
use forgedm_core::engine::{
    self, Control, EffectiveGlobal, EngineConfig, EngineDeps, EngineHandle, GlobalLimits,
};
use forgedm_core::ports::{EngineEvent, EngineEventSink, NewJob, ProgressSample, unix_ms};
use forgedm_core::url_policy::SecretUrl;
use forgedm_platform::motw::MotwService;
use forgedm_protocol::{
    ClientKind, DownloadCompletedData, DownloadDetail, DownloadFailedData, DownloadRemovedData,
    DownloadState, DownloadSummary, EffectiveLimits, EnqueueUrlResult, EventBody, PauseAllResult,
    Priority, ProgressItem, ProgressSnapshotData, ResumeAllResult,
};
use forgedm_storage::Storage;
use uuid::Uuid;

use crate::config::ProxyMode;
use crate::detail::{build_detail, load_summary};
use crate::error::AgentError;
use crate::events_hub::HubHandle;
use crate::limits_store::{self, DEFAULT_CONNECTIONS, GlobalPatch};
use crate::store_adapter::StorageStore;

/// A validated enqueue request: everything the engine needs, nothing it must re-check.
#[derive(Debug, Clone)]
pub struct EnqueueSpec {
    /// The validated URL.
    pub url: SecretUrl,
    /// Canonical destination directory.
    pub target_dir: std::path::PathBuf,
    /// Sanitised requested file name.
    pub file_name: Option<String>,
    /// Expected SHA-256, lowercase hex.
    pub expected_sha256: Option<String>,
    /// Connection cap, `1..=16`.
    pub max_connections: u32,
    /// Scheduling priority.
    pub priority: Priority,
    /// The connection's client kind.
    pub source: ClientKind,
}

/// What the request handlers need from the engine side.
pub trait EngineControl: Send + Sync + 'static {
    /// Creates a download.
    fn enqueue(&self, spec: EnqueueSpec) -> BoxFuture<'_, Result<EnqueueUrlResult, AgentError>>;
    /// The full view of a download.
    fn detail(&self, id: Uuid) -> BoxFuture<'_, Result<DownloadDetail, AgentError>>;
    /// Pause, resume, cancel or retry; returns the resulting state.
    fn control(
        &self,
        _id: Uuid,
        _control: Control,
    ) -> BoxFuture<'_, Result<DownloadState, AgentError>> {
        Box::pin(async { Err(AgentError::NotImplemented) })
    }
    /// Sets the connection cap of a download (`1..=16`); returns the cap now in force.
    fn set_max_connections(
        &self,
        _id: Uuid,
        _value: u32,
    ) -> BoxFuture<'_, Result<u32, AgentError>> {
        Box::pin(async { Err(AgentError::NotImplemented) })
    }
    /// Removes a download (and, on request, its recorded `.part`).
    fn remove(&self, _id: Uuid, _delete_partial: bool) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async { Err(AgentError::NotImplemented) })
    }
    /// Applies a change to the agent-wide limits, live and stored, and returns the limits in
    /// force (an empty patch just reads them).
    fn update_global_limits(
        &self,
        _patch: GlobalPatch,
    ) -> BoxFuture<'_, Result<EffectiveLimits, AgentError>> {
        Box::pin(async { Err(AgentError::NotImplemented) })
    }
    /// Sets the rate limit of one download (`None` = unlimited); returns the limit in force.
    fn set_rate_limit(
        &self,
        _id: Uuid,
        _limit: Option<u64>,
    ) -> BoxFuture<'_, Result<Option<u64>, AgentError>> {
        Box::pin(async { Err(AgentError::NotImplemented) })
    }
    /// Changes the priority of a download.
    fn set_priority(
        &self,
        _id: Uuid,
        _priority: Priority,
    ) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async { Err(AgentError::NotImplemented) })
    }
    /// Pauses every pausable download.
    fn pause_all(
        &self,
        _discard_progress: bool,
    ) -> BoxFuture<'_, Result<PauseAllResult, AgentError>> {
        Box::pin(async { Err(AgentError::NotImplemented) })
    }
    /// Resumes every paused download.
    fn resume_all(&self) -> BoxFuture<'_, Result<ResumeAllResult, AgentError>> {
        Box::pin(async { Err(AgentError::NotImplemented) })
    }
    /// Connections a new download gets when the request names none.
    fn default_connections(&self) -> u32 {
        DEFAULT_CONNECTIONS
    }
}

/// Why the engine could not start.
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    /// The HTTP client could not be built.
    #[error("cannot build the HTTP client: {0}")]
    Client(#[from] forgedm_core::client::ClientError),
    /// The Mark-of-the-Web service could not start.
    #[error("cannot start the Mark-of-the-Web service: {0}")]
    Motw(#[from] forgedm_platform::PlatformError),
}

const fn core_proxy(mode: ProxyMode) -> CoreProxyMode {
    match mode {
        ProxyMode::System => CoreProxyMode::System,
        ProxyMode::None => CoreProxyMode::None,
    }
}

/// Publishes engine events to the hub.
struct HubSink {
    hub: HubHandle,
    storage: Storage,
    engine: Arc<OnceLock<EngineHandle>>,
}

impl HubSink {
    fn live(&self, id: Uuid) -> Option<ProgressSample> {
        self.engine.get().and_then(|engine| engine.live(id))
    }

    async fn summary(&self, id: Uuid) -> Option<DownloadSummary> {
        let live = self.live(id);
        match load_summary(&self.storage, id, live.as_ref()).await {
            Ok(summary) => summary,
            Err(err) => {
                tracing::error!(%id, error = %err, "cannot read a download to publish it");
                None
            }
        }
    }

    async fn publish(&self, body: EventBody) {
        if self.hub.publish(body).await.is_err() {
            tracing::warn!("state hub stopped; an engine event was dropped");
        }
    }
}

fn progress_item(sample: &ProgressSample) -> ProgressItem {
    ProgressItem {
        id: sample.id,
        accepted_bytes: sample.accepted,
        committed_bytes: sample.committed,
        total_bytes: sample.total,
        speed_bps: sample.speed_bps,
        eta_ms: sample.eta_ms,
        active_connections: sample.active_connections,
    }
}

impl EngineEventSink for HubSink {
    fn emit(&self, event: EngineEvent) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            match event {
                EngineEvent::Added { id } => {
                    if let Some(summary) = self.summary(id).await {
                        self.publish(EventBody::DownloadAdded(Box::new(summary)))
                            .await;
                    }
                }
                EngineEvent::Changed { id } => {
                    if let Some(summary) = self.summary(id).await {
                        self.publish(EventBody::DownloadChanged(Box::new(summary)))
                            .await;
                    }
                }
                EngineEvent::Progress(samples) => {
                    self.publish(progress_body(&samples)).await;
                }
                EngineEvent::Completed {
                    id,
                    final_path,
                    sha256,
                    length_verified,
                } => {
                    let data = DownloadCompletedData {
                        id,
                        final_path: final_path.to_string_lossy().into_owned(),
                        sha256,
                        length_verified,
                    };
                    self.publish(EventBody::DownloadCompleted(data)).await;
                }
                EngineEvent::Failed { id, error } => {
                    self.publish(EventBody::DownloadFailed(DownloadFailedData { id, error }))
                        .await;
                }
                EngineEvent::Removed { id } => {
                    self.publish(EventBody::DownloadRemoved(DownloadRemovedData { id }))
                        .await;
                }
            }
        })
    }

    fn try_emit(&self, event: EngineEvent) -> bool {
        match event {
            EngineEvent::Progress(samples) => self.hub.try_publish(progress_body(&samples)),
            // State events are never dropped: callers use `emit` for them.
            _ => false,
        }
    }
}

fn progress_body(samples: &[ProgressSample]) -> EventBody {
    EventBody::ProgressSnapshot(ProgressSnapshotData {
        items: samples.iter().map(progress_item).collect(),
    })
}

/// The running engine and what the handlers need to reach it.
#[derive(Clone)]
pub struct EngineHost {
    engine: EngineHandle,
    storage: Storage,
    default_connections: Arc<AtomicU32>,
}

impl std::fmt::Debug for EngineHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineHost")
            .field("engine", &self.engine)
            .finish_non_exhaustive()
    }
}

impl EngineHost {
    /// Starts the engine over `storage`. `existing` is the number of stored downloads (it
    /// counts toward the queue cap).
    ///
    /// # Errors
    /// [`HostError`] when the HTTP client or the MOTW service cannot start.
    pub fn start(
        storage: Storage,
        hub: HubHandle,
        proxy: ProxyMode,
        config: EngineConfig,
        existing: usize,
    ) -> Result<Self, HostError> {
        let engine_cell = Arc::new(OnceLock::new());
        let sink = Arc::new(HubSink {
            hub,
            storage: storage.clone(),
            engine: Arc::clone(&engine_cell),
        });
        let deps = EngineDeps {
            store: Arc::new(StorageStore::new(storage.clone(), existing)),
            sink,
            client: HttpClient::new(core_proxy(proxy))?,
            motw: Arc::new(MotwService::start()?),
        };
        let engine = engine::start(config, deps);
        // The cell is private to this function: setting it cannot fail.
        let _ = engine_cell.set(engine.clone());
        Ok(Self {
            engine,
            storage,
            default_connections: Arc::new(AtomicU32::new(DEFAULT_CONNECTIONS)),
        })
    }

    /// Resumes stored work: queued downloads run, interrupted ones auto-resume from their
    /// committed bytes when they can, and restart from zero when they cannot.
    pub async fn recover(&self) {
        self.apply_saved_limits().await;
        match self.engine.recover().await {
            Ok(count) => tracing::info!(count, "resumed stored downloads"),
            Err(err) => tracing::error!(error = %err, "could not resume stored downloads"),
        }
    }

    /// Applies the limits stored by an earlier run, before any download is admitted.
    async fn apply_saved_limits(&self) {
        let saved = match limits_store::load(&self.storage).await {
            Ok(saved) => saved,
            Err(err) => {
                tracing::error!(error = %err, "could not read the stored limits; using defaults");
                return;
            }
        };
        if let Some(value) = saved.max_connections {
            self.default_connections.store(value, Ordering::Release);
        }
        let patch = GlobalLimits {
            max_active: saved.max_active_downloads.map(|v| v as usize),
            rate_limit_bps: saved.rate_limit_bps,
        };
        if let Err(err) = self.engine.update_global_limits(patch).await {
            tracing::error!(error = %err, "could not apply the stored limits");
        }
    }

    /// The limits in force, as the wire reports them.
    fn effective(&self, global: EffectiveGlobal) -> EffectiveLimits {
        EffectiveLimits {
            max_connections: Some(self.default_connections.load(Ordering::Acquire)),
            rate_limit_bps: global.rate_limit_bps,
            max_active_downloads: u32::try_from(global.max_active).ok(),
        }
    }

    /// Stops every download and waits for them to settle.
    pub async fn shutdown(&self) {
        self.engine.shutdown().await;
    }
}

impl EngineControl for EngineHost {
    fn enqueue(&self, spec: EnqueueSpec) -> BoxFuture<'_, Result<EnqueueUrlResult, AgentError>> {
        Box::pin(async move {
            let id = Uuid::now_v7();
            let display_name = spec.file_name.clone().unwrap_or_else(|| {
                forgedm_core::naming::suggest(None, spec.url.as_url(), id).display_name
            });
            let job = NewJob {
                id,
                source: spec.source,
                priority: spec.priority,
                url: spec.url,
                target_dir: spec.target_dir,
                requested_name: spec.file_name,
                display_name,
                expected_sha256: spec.expected_sha256,
                max_connections: spec.max_connections,
                created_at_ms: unix_ms(),
            };
            let queued = self.engine.enqueue(job).await?;
            Ok(EnqueueUrlResult {
                download_id: queued.id,
                state: queued.state,
                file_name: queued.display_name,
            })
        })
    }

    fn detail(&self, id: Uuid) -> BoxFuture<'_, Result<DownloadDetail, AgentError>> {
        Box::pin(async move {
            let live = self.engine.live(id);
            build_detail(&self.storage, id, live.as_ref()).await
        })
    }

    fn control(
        &self,
        id: Uuid,
        control: Control,
    ) -> BoxFuture<'_, Result<DownloadState, AgentError>> {
        Box::pin(async move { Ok(self.engine.control(id, control).await?) })
    }

    fn remove(&self, id: Uuid, delete_partial: bool) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async move { Ok(self.engine.remove(id, delete_partial).await?) })
    }

    fn set_max_connections(&self, id: Uuid, value: u32) -> BoxFuture<'_, Result<u32, AgentError>> {
        Box::pin(async move { Ok(self.engine.set_max_connections(id, value).await?) })
    }

    fn update_global_limits(
        &self,
        patch: GlobalPatch,
    ) -> BoxFuture<'_, Result<EffectiveLimits, AgentError>> {
        Box::pin(async move {
            // Stored first: a limit that was not kept would silently revert at the next start.
            limits_store::save(&self.storage, &patch, unix_ms()).await?;
            if let Some(value) = patch.max_connections {
                self.default_connections.store(value, Ordering::Release);
            }
            let applied = GlobalLimits {
                max_active: patch.max_active_downloads.map(|v| v as usize),
                rate_limit_bps: patch.rate_limit_bps,
            };
            let global = self.engine.update_global_limits(applied).await?;
            Ok(self.effective(global))
        })
    }

    fn set_rate_limit(
        &self,
        id: Uuid,
        limit: Option<u64>,
    ) -> BoxFuture<'_, Result<Option<u64>, AgentError>> {
        Box::pin(async move { Ok(self.engine.set_rate_limit(id, limit).await?) })
    }

    fn set_priority(&self, id: Uuid, priority: Priority) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async move { Ok(self.engine.set_priority(id, priority).await?) })
    }

    fn pause_all(
        &self,
        discard_progress: bool,
    ) -> BoxFuture<'_, Result<PauseAllResult, AgentError>> {
        Box::pin(async move {
            let outcome = self.engine.pause_all(discard_progress).await?;
            Ok(PauseAllResult {
                paused: outcome.paused,
                skipped_not_resumable: outcome.skipped_not_resumable,
            })
        })
    }

    fn resume_all(&self) -> BoxFuture<'_, Result<ResumeAllResult, AgentError>> {
        Box::pin(async move {
            Ok(ResumeAllResult {
                resumed: self.engine.resume_all().await?,
            })
        })
    }

    fn default_connections(&self) -> u32 {
        self.default_connections.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use forgedm_core::error::EngineError;
    use forgedm_protocol::ErrorCode;

    use super::*;

    #[test]
    fn engine_errors_map_to_wire_codes() {
        let cases = [
            (EngineError::Busy, ErrorCode::Busy),
            (EngineError::ShuttingDown, ErrorCode::ShuttingDown),
            (EngineError::LimitExceeded, ErrorCode::LimitExceeded),
        ];
        for (error, code) in cases {
            assert_eq!(AgentError::from(error).to_wire().code, code);
        }
    }

    #[test]
    fn progress_items_carry_every_counter() {
        let sample = ProgressSample {
            id: Uuid::nil(),
            accepted: 10,
            committed: 8,
            total: Some(100),
            speed_bps: Some(5),
            eta_ms: Some(1000),
            active_connections: 1,
            active_ranges: Vec::new(),
        };
        let item = progress_item(&sample);
        assert_eq!((item.accepted_bytes, item.committed_bytes), (10, 8));
        assert_eq!(item.total_bytes, Some(100));
        assert_eq!(item.active_connections, 1);
    }
}
