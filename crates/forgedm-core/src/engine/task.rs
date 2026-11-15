//! One download's lifecycle: the sole mutator of its state (ARCHITECTURE 4.1, 6.2).
//!
//! `queued -> probing -> downloading -> verifying -> finalizing -> completed`, with every
//! step persisted (compare-and-set) before the matching event is emitted. A stop request
//! (pause, cancel, shutdown) is honoured at every await point of the transfer phases;
//! verifying honours cancel and shutdown only; `finalizing` has no way out (A.6).
//!
//! Where a run starts depends on what the store kept ([`plan_resume`]): a fresh probe, a
//! validated resume from the committed offset, or a restart from zero. The ranged transfer
//! lives in [`ranged`], settling after a stop or failure in [`settle`].

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

mod ranged;
mod segmented;
mod settle;

use super::REASON_RESTART_FROM_ZERO;
use super::cleanup::delete_stale_part;
use super::live::{LiveDownload, StopReason};
use crate::checkpoint::{StoreCommitter, flush_then_commit};
use crate::client::{ClientError, HttpClient, response_meta};
use crate::error::{Failure, TaskStop, failure_from_transfer, stop_from_store};
use crate::finalize::{Finalized, finalize};
use crate::ports::{DownloadStore, EngineEvent, EngineEventSink, StoreEvent, StoredJob};
use crate::probe::{Probed, probe};
use crate::resume::{
    Continuation, RestartReason, ResumePlan, is_resumable_mode, open_valid_part, plan_resume,
};
use crate::state::{Command, transition};
use crate::transfer::prealloc::{open_part_for_segments, prepare_part_on_volume};
use crate::transfer::segment_table::SegmentTable;
use crate::transfer::segmented::ConnectionCap;
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
    pub cap: Arc<ConnectionCap>,
}

/// What the first response of a plain GET produced.
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

/// Where a transfer ended: everything the verify and finalize steps need.
struct Landed {
    path: PathBuf,
    final_url: Url,
    name: String,
    total: Option<u64>,
}

/// A running download.
pub(crate) struct DownloadTask {
    job: StoredJob,
    ctx: TaskCtx,
    state: DownloadState,
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

impl DownloadTask {
    pub(crate) fn new(job: StoredJob, ctx: TaskCtx) -> Self {
        let state = job.state;
        ctx.live.set_state(state);
        Self { job, ctx, state }
    }

    /// Runs to a terminal or paused state, or until stopped.
    pub(crate) async fn run(mut self) {
        if let Err(stop) = self.drive().await {
            self.settle(stop).await;
        }
    }

    async fn drive(&mut self) -> Result<(), TaskStop> {
        let plan = plan_resume(&self.job);
        let reason = matches!(plan, ResumePlan::Restart(_)).then_some(REASON_RESTART_FROM_ZERO);
        self.advance(Command::Admit, reason).await?;
        let landed = match plan {
            ResumePlan::Continue(continuation) => self.resume(continuation).await?,
            ResumePlan::Restart(why) => {
                self.restart(why).await?;
                self.start_fresh().await?
            }
            ResumePlan::Fresh => self.start_fresh().await?,
        };
        let verified = self.verify(&landed.path, landed.total).await?;
        self.finish(&landed.path, &landed.final_url, &landed.name, &verified)
            .await
    }

    /// Stops for any reason (the transfer phases).
    fn stop_check(&self) -> Result<(), TaskStop> {
        self.ctx.live.stop().check()
    }

    // ---- starting ------------------------------------------------------------------

    /// Probes, then transfers: ranged when the probe verified a 206 with a known total,
    /// otherwise a plain GET.
    async fn start_fresh(&mut self) -> Result<Landed, TaskStop> {
        let probed = self.probe().await?;
        let ranged_total = is_resumable_mode(probed.result.transfer_mode)
            .then_some(probed.result.total)
            .flatten()
            .filter(|total| *total > 0);
        match ranged_total {
            Some(total) if probed.result.transfer_mode == TransferMode::Segmented => {
                self.fresh_segmented(probed, total).await
            }
            Some(total) => self.fresh_ranged(probed, total).await,
            None => self.plain().await,
        }
    }

    async fn probe(&self) -> Result<Probed, TaskStop> {
        self.stop_check()?;
        let token = self.ctx.live.stop().token().clone();
        probe(&self.ctx.client, self.job.url.as_url(), &token)
            .await
            .map_err(|stop| match stop {
                TaskStop::Stopped(_) => self.ctx.live.stop().stopped(),
                other => other,
            })
    }

    /// Continues a stored download after checking its staging file. A file that is missing,
    /// replaced or short restarts the download from zero (PCR-01, before `finalizing`).
    async fn resume(&mut self, from: Continuation) -> Result<Landed, TaskStop> {
        self.stop_check()?;
        let table = if self.job.transfer_mode == Some(TransferMode::Segmented) {
            let Some(table) = self.restore_table(&from).await? else {
                self.restart(RestartReason::PartFileInvalid).await?;
                return self.start_fresh().await;
            };
            Some(table)
        } else {
            None
        };
        let job = self.job.clone();
        let committed = from.committed;
        let keep_len = table.as_ref().map(SegmentTable::max_committed);
        let opened = tokio::task::spawn_blocking(move || match keep_len {
            Some(len) => open_part_for_segments(&job, len),
            None => open_valid_part(&job, committed),
        })
        .await
        .map_err(|_| internal())?;
        let file = match opened {
            Ok(file) => file,
            Err(err) => {
                if let Some(stop) = err.keeps_progress() {
                    // Locked, denied or offline says nothing about the file: keep the
                    // progress and the .part, and let the user or a retry resume later.
                    tracing::warn!(
                        error = %err,
                        cause = ?std::error::Error::source(&err),
                        "staging file could not be opened for resume; progress kept"
                    );
                    return Err(stop);
                }
                tracing::warn!(
                    error = %err,
                    cause = ?std::error::Error::source(&err),
                    "staging file is not valid; restarting from zero"
                );
                self.restart(RestartReason::PartFileInvalid).await?;
                return self.start_fresh().await;
            }
        };
        let (Some(path), Some(name)) = (self.job.part_path.clone(), self.job.file_name.clone())
        else {
            // Progress without a recorded name or path cannot be trusted: start over.
            tracing::warn!("stored progress has no file name or path; restarting from zero");
            self.restart(RestartReason::PartFileInvalid).await?;
            return self.start_fresh().await;
        };
        self.ctx.live.set_total(Some(from.total));
        self.ctx.live.resume_from(from.committed);
        self.advance(Command::ProbeSucceeded, None).await?;
        let final_url = match table {
            Some(table) => {
                self.segmented_transfer(file, table, from.identity.clone())
                    .await?
            }
            None => self.ranged_transfer(file, &from).await?,
        };
        Ok(Landed {
            path,
            final_url,
            name,
            total: Some(from.total),
        })
    }

    /// Throws away the stale staging file (only if it is provably ours) and every stored
    /// fact about the earlier attempt, in one reset, and says so in the event log.
    async fn restart(&mut self, why: RestartReason) -> Result<(), TaskStop> {
        let stale = self.job.clone();
        match tokio::task::spawn_blocking(move || delete_stale_part(&stale)).await {
            Ok(outcome) => tracing::debug!(?outcome, "stale staging file cleanup"),
            Err(err) => tracing::warn!(error = %err, "stale staging file cleanup did not run"),
        }
        let id = self.job.id;
        let store = &self.ctx.store;
        store
            .reset_progress(id)
            .await
            .map_err(|e| stop_from_store(&e))?;
        let event = StoreEvent::now("restart_from_zero").with_code(why.code());
        store
            .append_event(id, event)
            .await
            .map_err(|e| stop_from_store(&e))?;
        self.job.part_path = None;
        self.job.part_file_id = None;
        self.job.transfer_mode = None;
        self.job.progress = crate::ports::StoredProgress::default();
        self.ctx.live.set_total(None);
        self.ctx.live.resume_from(0);
        self.ctx.sink.emit(EngineEvent::Changed { id }).await;
        Ok(())
    }

    // ---- plain GET -----------------------------------------------------------------

    /// A server that ignores ranges (or has no known length): one plain GET, sequentially.
    async fn plain(&mut self) -> Result<Landed, TaskStop> {
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
            self.transfer_plain(response, staged.file, &plan).await?;
        }
        Ok(Landed {
            path,
            final_url,
            name,
            total: plan.total,
        })
    }

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

    async fn transfer_plain(
        &mut self,
        response: Response,
        file: File,
        plan: &Plan,
    ) -> Result<(), TaskStop> {
        let (mut writer, committer) = self.start_session(file, 0, plan.total)?;
        let env = Stream {
            live: &self.ctx.live,
            stop: self.ctx.live.stop(),
            dir: &self.job.target_dir,
            start: 0,
            limit: plan.total,
        };
        self.ctx.live.set_connections(1);
        let outcome = stream_body(response, &mut writer, &env, &committer).await;
        self.end_session(writer, &committer, outcome).await
    }

    // ---- staging and sessions ------------------------------------------------------

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
        if plan.transfer_mode == TransferMode::Segmented {
            let total = plan.total.unwrap_or(0);
            if let Err(stop) = prepare_part_on_volume(&part.file, total) {
                if let Err(err) = forgedm_platform::fs::delete_by_handle(&part.file) {
                    tracing::warn!(error = %err, "could not delete a refused staging file");
                }
                return Err(stop);
            }
        }
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
            .map_err(|e| stop_from_store(&e))
    }

    /// Starts the writer thread over `file`, whose first `committed` bytes are already on
    /// disk, and the committer for this download.
    fn start_session(
        &self,
        file: File,
        committed: u64,
        total: Option<u64>,
    ) -> Result<(Writer, StoreCommitter), TaskStop> {
        let id = self.job.id;
        let name = format!("forgedm-writer-{}", id8(id, 0));
        let written = Arc::new(AtomicU64::new(committed));
        let writer = Writer::spawn(name, file, written).map_err(|e| writer_stop(&e))?;
        let committer = StoreCommitter::new(Arc::clone(&self.ctx.store), id, total);
        Ok((writer, committer))
    }

    /// Ends a transfer. On success: close (`sync_all`), commit the final frontier and move
    /// to `verifying`. On any stop or error: flush and commit what was accepted (pause,
    /// cancel, shutdown and error all checkpoint), then release the file; the `.part` stays.
    async fn end_session(
        &mut self,
        mut writer: Writer,
        committer: &StoreCommitter,
        outcome: Result<u64, TaskStop>,
    ) -> Result<(), TaskStop> {
        self.ctx.live.set_connections(0);
        match outcome {
            Ok(reached) => {
                let frontier = writer.close().await.map_err(|e| writer_stop(&e))?;
                if frontier != reached {
                    tracing::error!(
                        reached,
                        frontier,
                        "writer frontier differs from bytes received"
                    );
                    return Err(internal());
                }
                committer.checkpoint(frontier).await?;
                self.ctx.live.set_committed(frontier);
                self.advance(Command::TransferComplete, None).await
            }
            Err(stop) => {
                if let Err(err) = flush_then_commit(&mut writer, committer, &self.ctx.live).await {
                    tracing::warn!(?err, "could not checkpoint after a stop");
                }
                if let Err(err) = writer.close().await {
                    tracing::warn!(error = %err, "writer did not close cleanly after a stop");
                }
                Err(stop)
            }
        }
    }

    // ---- verifying and finalizing --------------------------------------------------

    async fn verify(&mut self, path: &Path, total: Option<u64>) -> Result<Verified, TaskStop> {
        // A pause never interrupts verification: only cancel and shutdown do.
        self.ctx.live.stop().check_hard()?;
        let (part, expected) = (path.to_owned(), self.job.expected_sha256.clone());
        let stop = self.ctx.live.stop().clone();
        let verified = tokio::task::spawn_blocking(move || {
            verify_part(&part, total, expected.as_deref(), stop.hard_token(), || {
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
            .map_err(|e| stop_from_store(&e))?;
        self.set_state(DownloadState::Completed);
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

    fn set_state(&mut self, state: DownloadState) {
        self.state = state;
        self.ctx.live.set_state(state);
    }

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
            .map_err(|e| stop_from_store(&e))?;
        self.set_state(step.to);
        self.ctx.sink.emit(EngineEvent::Changed { id }).await;
        Ok(())
    }
}
