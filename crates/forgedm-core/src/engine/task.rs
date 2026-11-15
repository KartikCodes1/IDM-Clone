//! One download's lifecycle: the sole mutator of its state (ARCHITECTURE 4.1, 6.2).
//!
//! `queued -> probing -> downloading -> verifying -> finalizing -> completed`, with every
//! step persisted (compare-and-set) before the matching event is emitted. A stop request
//! (cancel, shutdown) is honoured at every await point except `finalizing`, which has no
//! way out (A.6).

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use forgedm_platform::PlatformError;
use forgedm_platform::motw::MotwService;
use forgedm_protocol::{DownloadErrorCode, DownloadState, MotwStatus, TransferMode};
use reqwest::Response;
use reqwest::header::CONTENT_DISPOSITION;
use url::Url;
use uuid::Uuid;

use super::live::{LiveDownload, StopReason};
use crate::client::{ClientError, HttpClient, response_meta};
use crate::clock::BoxFuture;
use crate::error::{Failure, StoreError, TaskStop, failure_from_transfer};
use crate::finalize::{Finalized, finalize};
use crate::ports::{DownloadStore, EngineEvent, EngineEventSink, StoreEvent, StoredJob};
use crate::state::{Command, transition};
use crate::transfer::single::{
    Checkpointer, PartFile, Plan, Stream, choose_name, create_part, ensure_free_space, id8,
    plan_response, probe_facts, stream_body, writer_stop,
};
use crate::transfer::{Verified, verify_part};
use crate::writer::Writer;

/// Services a task uses.
pub(crate) struct TaskCtx {
    pub store: Arc<dyn DownloadStore>,
    pub sink: Arc<dyn EngineEventSink>,
    pub client: HttpClient,
    pub motw: Arc<MotwService>,
    pub live: Arc<LiveDownload>,
}

/// What the first response produced.
struct Opened {
    response: Response,
    final_url: Url,
    plan: Plan,
    name: String,
}

/// The staged file after the probe facts were persisted.
struct Staged {
    path: PathBuf,
    file: File,
}

/// A running download.
pub(crate) struct DownloadTask {
    job: StoredJob,
    ctx: TaskCtx,
    state: DownloadState,
}

/// Maps a store failure raised while the task runs.
fn store_stop(err: &StoreError) -> TaskStop {
    match err {
        StoreError::SecretsUnavailable => {
            TaskStop::Fail(Failure::new(DownloadErrorCode::SecretsUnavailable))
        }
        other => {
            tracing::error!(error = %other, "storage failed while a download was running");
            internal()
        }
    }
}

fn internal() -> TaskStop {
    TaskStop::Fail(Failure::new(DownloadErrorCode::Internal))
}

fn client_stop(err: ClientError, ctx: &TaskCtx) -> TaskStop {
    match err {
        ClientError::Cancelled => ctx.live.stop().stopped(),
        ClientError::Redirect(redirect) => TaskStop::Fail(Failure::new(redirect.code())),
        ClientError::Transport(kind) => failure_from_transfer(kind),
        ClientError::Build(_) => internal(),
    }
}

/// Persists committed progress through the store (the stream loop's checkpoint hook).
struct StoreCheckpoint {
    store: Arc<dyn DownloadStore>,
    id: Uuid,
    total: Option<u64>,
}

impl Checkpointer for StoreCheckpoint {
    fn checkpoint(&self, committed: u64) -> BoxFuture<'_, Result<(), TaskStop>> {
        Box::pin(async move {
            self.store
                .checkpoint(self.id, committed, self.total)
                .await
                .map_err(|e| store_stop(&e))
        })
    }
}

impl DownloadTask {
    pub(crate) fn new(job: StoredJob, ctx: TaskCtx) -> Self {
        let state = job.state;
        Self { job, ctx, state }
    }

    /// Runs to a terminal or paused state, or until stopped.
    pub(crate) async fn run(mut self) {
        if let Err(stop) = self.drive().await {
            self.settle(stop).await;
        }
    }

    async fn drive(&mut self) -> Result<(), TaskStop> {
        self.advance(Command::Admit, None).await?;
        let Opened {
            response,
            final_url,
            plan,
            name,
        } = self.open().await?;
        let staged = self.stage(&plan, &final_url, &name).await?;
        let path = staged.path.clone();
        if plan.transfer_mode == TransferMode::ZeroLength {
            drop(staged.file);
        } else {
            self.transfer(response, staged.file, &plan).await?;
        }
        let verified = self.verify(&path, plan.total).await?;
        self.finish(&path, &final_url, &name, &verified).await
    }

    fn stop_check(&self) -> Result<(), TaskStop> {
        self.ctx.live.stop().check()
    }

    // ---- probing -------------------------------------------------------------------

    async fn open(&mut self) -> Result<Opened, TaskStop> {
        self.stop_check()?;
        let token = self.ctx.live.stop().token().clone();
        let fetched = self
            .ctx
            .client
            .get(self.job.url.as_url(), &token)
            .await
            .map_err(|e| client_stop(e, &self.ctx))?;
        let plan = plan_response(&response_meta(&fetched.response))?;
        let disposition = fetched
            .response
            .headers()
            .get(CONTENT_DISPOSITION)
            .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned());
        let name = choose_name(&self.job, disposition.as_deref(), &fetched.final_url);
        Ok(Opened {
            response: fetched.response,
            final_url: fetched.final_url,
            plan,
            name,
        })
    }

    /// Creates the staging file, persists the probe facts and leaves `probing`.
    async fn stage(
        &mut self,
        plan: &Plan,
        final_url: &Url,
        name: &str,
    ) -> Result<Staged, TaskStop> {
        self.stop_check()?;
        let dir = self.job.target_dir.clone();
        ensure_free_space(&dir, plan.total.unwrap_or(0))?;
        let (id, owned_name) = (self.job.id, name.to_owned());
        let part = tokio::task::spawn_blocking(move || create_part(&dir, &owned_name, id))
            .await
            .map_err(|_| internal())??;
        if let Err(stop) = self.save_facts(plan, final_url, name, &part).await {
            // The file's name and id were never recorded, so nothing could clean it up later.
            if let Err(err) = forgedm_platform::fs::delete_by_handle(&part.file) {
                tracing::warn!(error = %err, "could not delete an unrecorded staging file");
            }
            return Err(stop);
        }
        let cmd = if plan.transfer_mode == TransferMode::ZeroLength {
            Command::ZeroLength
        } else {
            Command::ProbeSucceeded
        };
        self.advance(cmd, None).await?;
        Ok(Staged {
            path: part.path,
            file: part.file,
        })
    }

    /// Persists the probe facts of a freshly created staging file.
    async fn save_facts(
        &self,
        plan: &Plan,
        final_url: &Url,
        name: &str,
        part: &PartFile,
    ) -> Result<(), TaskStop> {
        let facts = probe_facts(plan, final_url, name, part)?;
        self.ctx.live.set_total(plan.total);
        self.ctx
            .store
            .save_probe(self.job.id, facts)
            .await
            .map_err(|e| store_stop(&e))
    }

    // ---- downloading ---------------------------------------------------------------

    async fn transfer(
        &mut self,
        response: Response,
        file: File,
        plan: &Plan,
    ) -> Result<(), TaskStop> {
        let id = self.job.id;
        let name = format!("forgedm-writer-{}", id8(id, 0));
        let mut writer =
            Writer::spawn(name, file, Arc::new(AtomicU64::new(0))).map_err(|e| writer_stop(&e))?;
        let checkpoint = StoreCheckpoint {
            store: Arc::clone(&self.ctx.store),
            id,
            total: plan.total,
        };
        let env = Stream {
            live: &self.ctx.live,
            stop: self.ctx.live.stop(),
            dir: &self.job.target_dir,
            total: plan.total,
        };
        self.ctx.live.set_connections(1);
        let outcome = stream_body(response, &mut writer, &env, &checkpoint).await;
        self.ctx.live.set_connections(0);
        match outcome {
            Ok(received) => {
                let frontier = writer.close().await.map_err(|e| writer_stop(&e))?;
                if frontier != received {
                    tracing::error!(
                        received,
                        frontier,
                        "writer frontier differs from bytes received"
                    );
                    return Err(internal());
                }
                self.ctx.live.set_committed(frontier);
                checkpoint.checkpoint(frontier).await?;
                self.advance(Command::TransferComplete, None).await
            }
            Err(stop) => {
                // Best effort: flush what was received and release the file; the `.part` stays.
                if let Err(err) = writer.close().await {
                    tracing::warn!(error = %err, "writer did not close cleanly after a stop");
                }
                Err(stop)
            }
        }
    }

    // ---- verifying and finalizing --------------------------------------------------

    async fn verify(&mut self, path: &Path, total: Option<u64>) -> Result<Verified, TaskStop> {
        self.stop_check()?;
        let (part, expected) = (path.to_owned(), self.job.expected_sha256.clone());
        let stop = self.ctx.live.stop().clone();
        let verified = tokio::task::spawn_blocking(move || {
            verify_part(&part, total, expected.as_deref(), stop.token(), || {
                stop.reason().unwrap_or(StopReason::Cancel)
            })
        })
        .await
        .map_err(|_| internal())??;
        if verified.checksum_matches == Some(false) {
            self.record_mismatch(&verified).await;
            return Err(Failure::new(DownloadErrorCode::ChecksumMismatch).into());
        }
        Ok(verified)
    }

    async fn record_mismatch(&self, verified: &Verified) {
        let detail = serde_json::json!({
            "actual": verified.sha256,
            "expected": self.job.expected_sha256,
        });
        let event = StoreEvent::now("checksum")
            .with_code("mismatch")
            .with_detail(detail.to_string());
        if let Err(err) = self.ctx.store.append_event(self.job.id, event).await {
            tracing::warn!(error = %err, "could not record the checksum mismatch");
        }
    }

    async fn finish(
        &mut self,
        part: &Path,
        final_url: &Url,
        name: &str,
        verified: &Verified,
    ) -> Result<(), TaskStop> {
        self.advance(Command::Verified, None).await?;
        let finalized = self.run_finalize(part, final_url, name).await?;
        self.record_motw(finalized.motw_status).await;
        let id = self.job.id;
        self.ctx
            .store
            .complete(
                id,
                &finalized.final_path,
                &verified.sha256,
                verified.length_verified,
            )
            .await
            .map_err(|e| store_stop(&e))?;
        self.state = DownloadState::Completed;
        self.ctx.sink.emit(EngineEvent::Changed { id }).await;
        self.ctx
            .sink
            .emit(EngineEvent::Completed {
                id,
                final_path: finalized.final_path,
                sha256: verified.sha256.clone(),
                length_verified: verified.length_verified,
            })
            .await;
        Ok(())
    }

    async fn run_finalize(
        &self,
        part: &Path,
        final_url: &Url,
        name: &str,
    ) -> Result<Finalized, TaskStop> {
        let (store, motw, id) = (
            Arc::clone(&self.ctx.store),
            Arc::clone(&self.ctx.motw),
            self.job.id,
        );
        let (part, dir) = (part.to_owned(), self.job.target_dir.clone());
        let (url, name) = (final_url.clone(), name.to_owned());
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let mut persist = |candidate: &Path| {
                runtime
                    .block_on(store.set_finalize_target(id, candidate))
                    .map_err(|err| PlatformError::Io(io::Error::other(err.to_string())))
            };
            finalize(&part, &dir, &name, &url, &motw, &mut persist)
        })
        .await
        .map_err(|_| internal())?
    }

    async fn record_motw(&self, status: MotwStatus) {
        let event = StoreEvent::now("motw").with_code(status.as_str());
        if let Err(err) = self.ctx.store.append_event(self.job.id, event).await {
            tracing::warn!(error = %err, "could not record the MOTW status");
        }
    }

    // ---- transitions ---------------------------------------------------------------

    async fn advance(&mut self, cmd: Command, reason: Option<&str>) -> Result<(), TaskStop> {
        self.advance_with(cmd, reason, StoreEvent::now("state"))
            .await
    }

    /// Applies `cmd` through the pure table, persists it, then emits `Changed`.
    async fn advance_with(
        &mut self,
        cmd: Command,
        reason: Option<&str>,
        event: StoreEvent,
    ) -> Result<(), TaskStop> {
        let step = transition(self.state, cmd).map_err(|err| {
            tracing::error!(error = %err, "illegal state transition in a download task");
            internal()
        })?;
        if !step.changed {
            return Ok(());
        }
        let id = self.job.id;
        self.ctx
            .store
            .transition(id, self.state, step.to, reason.map(str::to_owned), event)
            .await
            .map_err(|e| store_stop(&e))?;
        self.state = step.to;
        self.ctx.sink.emit(EngineEvent::Changed { id }).await;
        Ok(())
    }

    // ---- settling ------------------------------------------------------------------

    async fn settle(&mut self, stop: TaskStop) {
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
            TaskStop::Stopped(StopReason::Shutdown) => {
                let event = StoreEvent::now("interrupted").with_code("agent_shutdown");
                self.park(Command::Interrupt, Some("agent_shutdown"), event)
                    .await;
            }
        }
    }

    async fn park(&mut self, cmd: Command, reason: Option<&str>, event: StoreEvent) {
        if let Err(stop) = self.advance_with(cmd, reason, event).await {
            tracing::error!(?stop, state = ?self.state, "could not settle a stopped download");
        }
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
