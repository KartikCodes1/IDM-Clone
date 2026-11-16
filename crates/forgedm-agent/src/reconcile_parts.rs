//! Startup reconcile of downloads left in `finalizing` (ARCHITECTURE 6.3 step 2, PCR-01).
//!
//! The pure decision is [`forgedm_core::recovery::decide_finalizing`]; this module applies it
//! to the database. It runs after [`crate::reconcile::on_agent_start`] and before the engine
//! starts, so no request is served while a download is half finalized:
//!
//! - the `.part` is there with the recorded file id: finalize runs again (idempotent) and the
//!   download becomes `completed`;
//! - the `.part` is gone but the file at the persisted rename candidate has the id: the
//!   download becomes `completed` after the shell attachment check is re-applied;
//! - no file anywhere has the id: `failed(part_file_invalid)`. A file that merely has the
//!   same name and size is never adopted.
//!
//! Other states keep their `.part` files; the engine validates them when the download
//! resumes (missing, replaced, truncated or corrupt segment rows restart from zero).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use forgedm_core::error::{Failure, TaskStop};
use forgedm_core::recovery::{
    FinalizingAction, FinalizingFacts, RedoInput, Redone, complete_moved, decide_finalizing,
    redo_finalize,
};
use forgedm_core::url_policy::{SecretUrl, validate_user_url};
use forgedm_platform::PlatformError;
use forgedm_platform::motw::MotwService;
use forgedm_protocol::{DownloadErrorCode, DownloadState, MotwStatus};
use forgedm_storage::{DownloadRecord, NewEvent, Storage, StorageError};
use uuid::Uuid;

use crate::reconcile::now_ms;

/// Rows fetched per round.
const PAGE: u32 = 100;
/// Event `kind` of the motw status record (the same kind the engine writes).
const EVENT_MOTW: &str = "motw";

/// What a pass changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PartsReport {
    /// `finalizing` downloads whose finalize ran again and completed.
    pub redone: u32,
    /// `finalizing` downloads found already renamed and completed.
    pub adopted: u32,
    /// `finalizing` downloads that failed (`part_file_invalid` and the like).
    pub failed: u32,
}

/// Why the part reconcile could not run.
#[derive(Debug, thiserror::Error)]
pub enum PartsError {
    /// The database could not be read or written.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// The Mark-of-the-Web service could not start.
    #[error("cannot start the Mark-of-the-Web service: {0}")]
    Motw(#[from] PlatformError),
}

/// How one download ended up.
enum Outcome {
    Redone,
    Adopted,
    Failed,
}

/// Settles every `finalizing` download by what is on disk.
///
/// # Errors
/// [`PartsError`] when the database fails or the shell service cannot start.
pub async fn finalizing_parts(storage: &Storage) -> Result<PartsReport, PartsError> {
    let mut report = PartsReport::default();
    let mut motw: Option<Arc<MotwService>> = None;
    loop {
        let rows = storage
            .downloads()
            .list(Some(&[DownloadState::Finalizing]), PAGE, 0)
            .await?;
        if rows.is_empty() {
            return Ok(report);
        }
        let service = match &motw {
            Some(service) => Arc::clone(service),
            None => Arc::clone(motw.insert(Arc::new(MotwService::start()?))),
        };
        let mut progressed = false;
        for record in rows {
            match settle_one(storage, &service, &record).await {
                Ok(outcome) => {
                    progressed = true;
                    match outcome {
                        Outcome::Redone => report.redone += 1,
                        Outcome::Adopted => report.adopted += 1,
                        Outcome::Failed => report.failed += 1,
                    }
                }
                // Another writer moved it first; it is no longer ours to settle.
                Err(StorageError::Conflict { .. }) => {}
                Err(other) => return Err(other.into()),
            }
        }
        if !progressed {
            return Ok(report);
        }
    }
}

async fn settle_one(
    storage: &Storage,
    motw: &Arc<MotwService>,
    record: &DownloadRecord,
) -> Result<Outcome, StorageError> {
    let action = decide(record).await;
    let id = record.id;
    match action {
        FinalizingAction::Fail => {
            fail(
                storage,
                id,
                &Failure::new(DownloadErrorCode::PartFileInvalid),
            )
            .await
        }
        FinalizingAction::Inconclusive => {
            fail(
                storage,
                id,
                &Failure::new(DownloadErrorCode::FinalizeLocked),
            )
            .await
        }
        FinalizingAction::Redo => redo(storage, motw, record).await,
        FinalizingAction::Complete { path } => adopt(storage, motw, record, path).await,
    }
}

/// Runs the blocking disk inspection on a blocking thread.
async fn decide(record: &DownloadRecord) -> FinalizingAction {
    let dir = PathBuf::from(&record.target_dir);
    let part = record.part_path.as_deref().map(PathBuf::from);
    let part_id = record.part_file_id.clone();
    let target = record.finalize_target.as_deref().map(PathBuf::from);
    let final_path = record.final_path.as_deref().map(PathBuf::from);
    let decided = tokio::task::spawn_blocking(move || {
        decide_finalizing(&FinalizingFacts {
            target_dir: &dir,
            part_path: part.as_deref(),
            part_file_id: part_id.as_deref(),
            final_path: final_path.as_deref(),
            finalize_target: target.as_deref(),
        })
    })
    .await;
    // A panic while inspecting proves nothing: fail safe, never adopt.
    decided.unwrap_or(FinalizingAction::Fail)
}

/// The URL the zone information is derived from: the final URL, else the original.
///
/// An unusable stored URL is a failure of that one download, not of the agent: a sealed value
/// that cannot be unsealed (the DPAPI key is gone) is `secrets_unavailable`, a value that no
/// longer passes the URL policy is `internal`. Only database errors are returned as `Err`.
async fn source_url(
    storage: &Storage,
    id: Uuid,
) -> Result<Result<SecretUrl, Failure>, StorageError> {
    let downloads = storage.downloads();
    let sealed = match downloads.final_url(id).await {
        Ok(Some(url)) => Ok(url),
        Ok(None) => downloads.original_url(id).await,
        Err(err) => Err(err),
    };
    let sealed = match sealed {
        Ok(url) => url,
        Err(StorageError::Crypto(_) | StorageError::InvalidValue { .. }) => {
            tracing::warn!(%id, "the stored URL cannot be unsealed; failing the download");
            return Ok(Err(Failure::new(DownloadErrorCode::SecretsUnavailable)));
        }
        Err(other) => return Err(other),
    };
    Ok(validate_user_url(sealed.expose()).map_err(|_| {
        tracing::warn!(%id, "the stored URL no longer passes the URL policy; failing the download");
        Failure::new(DownloadErrorCode::Internal)
    }))
}

async fn redo(
    storage: &Storage,
    motw: &Arc<MotwService>,
    record: &DownloadRecord,
) -> Result<Outcome, StorageError> {
    let id = record.id;
    let source = match source_url(storage, id).await? {
        Ok(source) => source,
        Err(failure) => return fail(storage, id, &failure).await,
    };
    let (Some(part), Some(name)) = (
        record.part_path.as_deref().map(PathBuf::from),
        record.file_name.clone(),
    ) else {
        return fail(
            storage,
            id,
            &Failure::new(DownloadErrorCode::PartFileInvalid),
        )
        .await;
    };
    let dir = PathBuf::from(&record.target_dir);
    let (total, expected) = (record.total_size, record.checksum_expected.clone());
    let (runtime, store, service) = (
        tokio::runtime::Handle::current(),
        storage.clone(),
        Arc::clone(motw),
    );
    let done = tokio::task::spawn_blocking(move || {
        let mut persist = |candidate: &Path| {
            let target = candidate.to_string_lossy();
            runtime
                .block_on(store.downloads().set_finalize_target(id, &target, now_ms()))
                .map_err(|err| PlatformError::Io(io::Error::other(err.to_string())))
        };
        redo_finalize(
            &RedoInput {
                part: &part,
                dir: &dir,
                name: &name,
                source: source.as_url(),
                total,
                expected_sha256: expected.as_deref(),
            },
            &service,
            &mut persist,
        )
    })
    .await;
    match done {
        Ok(Ok(redone)) => record_redone(storage, id, &redone)
            .await
            .map(|()| Outcome::Redone),
        Ok(Err(stop)) => fail(storage, id, &failure_of(stop)).await,
        Err(_) => fail(storage, id, &Failure::new(DownloadErrorCode::Internal)).await,
    }
}

async fn record_redone(storage: &Storage, id: Uuid, redone: &Redone) -> Result<(), StorageError> {
    let path = redone.finalized.final_path.to_string_lossy().into_owned();
    storage
        .downloads()
        .complete(id, &path, &redone.sha256, redone.length_verified, now_ms())
        .await?;
    record_motw(storage, id, redone.finalized.motw_status).await;
    Ok(())
}

async fn adopt(
    storage: &Storage,
    motw: &Arc<MotwService>,
    record: &DownloadRecord,
    path: PathBuf,
) -> Result<Outcome, StorageError> {
    let id = record.id;
    let source = match source_url(storage, id).await? {
        Ok(source) => source,
        Err(failure) => return fail(storage, id, &failure).await,
    };
    let (service, moved) = (Arc::clone(motw), path.clone());
    let result = tokio::task::spawn_blocking(move || {
        let length = std::fs::metadata(&moved).map(|m| m.len());
        complete_moved(&moved, source.as_url(), &service).map(|done| (done, length))
    })
    .await;
    let ((sha256, status), length) = match result {
        Ok(Ok((done, Ok(length)))) => (done, length),
        Ok(Ok((_, Err(err)))) => {
            return fail(
                storage,
                id,
                &failure_of(forgedm_core::error::stop_from_io(&err)),
            )
            .await;
        }
        Ok(Err(stop)) => return fail(storage, id, &failure_of(stop)).await,
        Err(_) => return fail(storage, id, &Failure::new(DownloadErrorCode::Internal)).await,
    };
    if record.total_size.is_some_and(|total| total != length) {
        return fail(storage, id, &Failure::new(DownloadErrorCode::SizeMismatch)).await;
    }
    let verified = record.total_size.is_some();
    let text = path.to_string_lossy().into_owned();
    storage
        .downloads()
        .complete(id, &text, &sha256, verified, now_ms())
        .await?;
    record_motw(storage, id, status).await;
    Ok(Outcome::Adopted)
}

/// Marks a `finalizing` download `failed` with `failure` recorded as its last error.
async fn fail(storage: &Storage, id: Uuid, failure: &Failure) -> Result<Outcome, StorageError> {
    let mut event = NewEvent::new(now_ms(), "failed");
    event.code = Some(failure.code.as_str().to_owned());
    event.detail_json = match serde_json::to_string(&failure.to_download_error()) {
        Ok(json) => Some(json),
        Err(err) => {
            tracing::warn!(error = %err, "could not serialise the failure detail");
            None
        }
    };
    storage
        .downloads()
        .transition(
            id,
            DownloadState::Finalizing,
            DownloadState::Failed,
            None,
            &event,
        )
        .await?;
    tracing::warn!(
        code = failure.code.as_str(),
        "finalizing download failed at startup"
    );
    Ok(Outcome::Failed)
}

fn failure_of(stop: TaskStop) -> Failure {
    match stop {
        TaskStop::Fail(failure) => failure,
        other => {
            tracing::warn!(
                ?other,
                "recovery stopped for a reason that is not a failure"
            );
            Failure::new(DownloadErrorCode::Internal)
        }
    }
}

async fn record_motw(storage: &Storage, id: Uuid, status: MotwStatus) {
    let mut event = NewEvent::new(now_ms(), EVENT_MOTW);
    event.code = Some(status.as_str().to_owned());
    if let Err(err) = storage.events().append(id, &event).await {
        tracing::warn!(error = %err, "could not record the MOTW status");
    }
}
