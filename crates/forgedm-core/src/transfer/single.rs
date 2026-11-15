//! The single-stream transfer (PLAN S1.7): one plain GET, no `Range`, body streamed through
//! the writer thread into a staged `.part` file.
//!
//! Nothing is written before the response passes [`plan_response`]. Resume, ranges and
//! segments are out of scope here: every download this module handles is
//! `single_nonresumable`, `single_unknown_length` or `zero_length`, and an interrupted one
//! restarts from zero.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use forgedm_platform::PlatformError;
use forgedm_platform::fs::{
    create_part_file, delete_by_handle, ensure_within, file_id, free_space,
};
use forgedm_protocol::{DownloadErrorCode, TransferMode};
use reqwest::Response;
use url::Url;
use uuid::Uuid;

use crate::client::classify_reqwest;
use crate::engine::{LiveDownload, StopSignal};
use crate::error::{Failure, TaskStop, failure_from_transfer, stop_from_class, stop_from_platform};
use crate::http::range_decision::{ResponseMeta, parse_content_length};
use crate::http::validators::{EntityTag, Validators, last_modified_is_strong};
use crate::naming::{self, PART_SUFFIX_UTF16, sanitize_opt};
use crate::ports::{ProbeFacts, StoredJob};
use crate::transfer::encode_file_id;
use crate::url_policy::{SecretUrl, validate_user_url};
use crate::writer::{Writer, WriterError};

/// Checkpoint cadence while transferring (A.9 `CHECKPOINT_INTERVAL`).
pub const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(2);
/// Free space kept beyond the file itself (A.9 `FREE_SPACE_MARGIN`).
pub const FREE_SPACE_MARGIN: u64 = 64 * 1024 * 1024;
/// Free space is re-checked after this many bytes (A.9).
pub const FREE_SPACE_RECHECK: u64 = 1024 * 1024 * 1024;
/// Attempts at a unique staging name before giving up.
const PART_NAME_ATTEMPTS: usize = 8;
/// Hex characters in the staging suffix.
const ID_HEX_LEN: usize = 8;

/// What the first response means for the transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// How the body is transferred.
    pub transfer_mode: TransferMode,
    /// Complete length, when the server declared one.
    pub total: Option<u64>,
    /// `Content-Type`.
    pub mime: Option<String>,
    /// Validators as received.
    pub validators: Validators,
}

/// Judges the first response to a plain GET.
///
/// # Errors
/// [`TaskStop::NeedsCredentials`] for 401/407, `unsupported_content_encoding` for a
/// content-coded body, `range_protocol_violation` for a 206 (none was requested) or a
/// bad `Content-Length`, otherwise `http_status{status}`.
pub fn plan_response(meta: &ResponseMeta) -> Result<Plan, TaskStop> {
    match meta.status {
        200 => {}
        401 | 407 => return Err(TaskStop::NeedsCredentials),
        206 => return Err(Failure::protocol_violation("unrequested_partial_content").into()),
        status => return Err(Failure::http_status(status).into()),
    }
    if crate::http::range_decision::has_non_identity_encoding(meta) {
        return Err(Failure::new(DownloadErrorCode::UnsupportedContentEncoding).into());
    }
    let total = parse_content_length(meta)
        .map_err(|violation| Failure::protocol_violation(violation.reason()))?;
    let transfer_mode = match total {
        Some(0) => TransferMode::ZeroLength,
        Some(_) => TransferMode::SingleNonresumable,
        None => TransferMode::SingleUnknownLength,
    };
    Ok(Plan {
        transfer_mode,
        total,
        mime: meta.content_type.clone(),
        validators: meta.validators.clone(),
    })
}

/// The name the file will have: the user's, else the server's, else the URL's, always
/// sanitised.
#[must_use]
pub fn choose_name(job: &StoredJob, content_disposition: Option<&str>, final_url: &Url) -> String {
    job.requested_name
        .as_deref()
        .and_then(sanitize_opt)
        .unwrap_or_else(|| naming::suggest(content_disposition, final_url, job.id).file_name)
}

/// Strength of the `ETag` and `Last-Modified` in `v`, for the stored record.
#[must_use]
pub fn validator_strengths(v: &Validators) -> (Option<bool>, Option<bool>) {
    let etag = v
        .etag
        .as_deref()
        .map(|raw| EntityTag::parse(raw).is_ok_and(|tag| !tag.is_weak()));
    let modified = v
        .last_modified
        .as_deref()
        .map(|lm| last_modified_is_strong(lm, v.date.as_deref()));
    (etag, modified)
}

/// The probe facts to persist once the staging file exists.
///
/// # Errors
/// `internal` when the final URL no longer validates (it passed redirect policy, so this
/// is a bug).
pub fn probe_facts(
    plan: &Plan,
    final_url: &Url,
    name: &str,
    part: &PartFile,
) -> Result<ProbeFacts, TaskStop> {
    let final_url: SecretUrl = validate_user_url(final_url.as_str())
        .map_err(|_| TaskStop::Fail(Failure::new(DownloadErrorCode::Internal)))?;
    let (etag_strong, last_modified_strong) = validator_strengths(&plan.validators);
    Ok(ProbeFacts {
        transfer_mode: plan.transfer_mode,
        total: plan.total,
        mime: plan.mime.clone(),
        final_url,
        file_name: name.to_owned(),
        part_path: part.path.clone(),
        part_file_id: part.file_id.clone(),
        etag: plan.validators.etag.clone(),
        etag_strong,
        last_modified: plan.validators.last_modified.clone(),
        last_modified_strong,
    })
}

/// A freshly created staging file.
#[derive(Debug)]
pub struct PartFile {
    /// The open handle (read, write and delete access).
    pub file: File,
    /// Its path.
    pub path: PathBuf,
    /// Its file id, when the file system supports one.
    pub file_id: Option<Vec<u8>>,
}

/// The 8-hex-digit staging suffix for `attempt`: windows of the random tail of the UUID, so
/// two downloads of the same name started together do not collide.
pub(crate) fn id8(id: Uuid, attempt: usize) -> String {
    let simple = id.simple().to_string();
    let end = simple.len().saturating_sub(attempt);
    let start = end.saturating_sub(ID_HEX_LEN);
    simple.get(start..end).unwrap_or(&simple).to_owned()
}

/// Staging file name `<name>.<id8>.forgedm.part`.
#[must_use]
pub fn part_name(name: &str, id: Uuid, attempt: usize) -> String {
    format!("{name}.{}.forgedm.part", id8(id, attempt))
}

/// Creates the staging file in `dir` with `create_new` semantics, then proves it landed
/// inside `dir` once every link is resolved (FS-02). A file that escaped is deleted by
/// handle and the download fails with `path_escape`.
///
/// Blocking: call from `spawn_blocking`.
///
/// # Errors
/// `path_escape`, `path_too_long`, a pause for a full disk, or `disk_io`.
pub fn create_part(dir: &Path, name: &str, id: Uuid) -> Result<PartFile, TaskStop> {
    debug_assert!(name.encode_utf16().count() + PART_SUFFIX_UTF16 <= 255);
    for attempt in 0..PART_NAME_ATTEMPTS {
        let path = dir.join(part_name(name, id, attempt));
        match create_part_file(&path) {
            Ok(file) => return verify_placement(file, path, dir),
            Err(PlatformError::AlreadyExists) => {}
            Err(other) => return Err(stop_from_platform(&other)),
        }
    }
    Err(Failure::new(DownloadErrorCode::DiskIo).into())
}

fn verify_placement(file: File, path: PathBuf, dir: &Path) -> Result<PartFile, TaskStop> {
    if let Err(err) = ensure_within(&file, dir) {
        if let Err(delete_err) = delete_by_handle(&file) {
            tracing::warn!(error = %delete_err, "could not delete an escaped staging file");
        }
        if matches!(err, PlatformError::PathEscape) {
            tracing::error!(
                target: "forgedm::security",
                "staging file resolved outside the target directory; download failed"
            );
        }
        return Err(stop_from_platform(&err));
    }
    let file_id = file_id(&file)
        .map(|id| encode_file_id(&id))
        .map_err(|err| tracing::warn!(error = %err, "file id unavailable for the staging file"))
        .ok();
    Ok(PartFile {
        file,
        path,
        file_id,
    })
}

/// Fails (as a pause) unless `dir`'s volume has `needed` bytes plus the margin free. A
/// failed query is logged and does not block the download.
///
/// # Errors
/// [`TaskStop::PauseFor`] `disk_full`.
pub fn ensure_free_space(dir: &Path, needed: u64) -> Result<(), TaskStop> {
    match free_space(dir) {
        Ok(free) if free < needed.saturating_add(FREE_SPACE_MARGIN) => {
            Err(TaskStop::PauseFor(DownloadErrorCode::DiskFull))
        }
        Ok(_) => Ok(()),
        Err(err) => {
            tracing::warn!(error = %err, "free space query failed; continuing");
            Ok(())
        }
    }
}

/// Maps a writer failure to the task's stop.
#[must_use]
pub fn writer_stop(err: &WriterError) -> TaskStop {
    match err {
        WriterError::Failed(failure) => stop_from_class(failure.class),
        WriterError::NotContiguous { .. } | WriterError::Spawn(_) => {
            TaskStop::Fail(Failure::new(DownloadErrorCode::Internal))
        }
    }
}

/// Inputs of [`stream_body`].
pub struct Stream<'a> {
    /// The checkpoint sink and counters of this download.
    pub live: &'a LiveDownload,
    /// Stop requests.
    pub stop: &'a StopSignal,
    /// Directory of the staging file (for the free-space re-check).
    pub dir: &'a Path,
    /// Declared total, when known.
    pub total: Option<u64>,
}

/// Periodic work the stream loop asks its caller to do.
pub trait Checkpointer: Send + Sync {
    /// Persists `committed` durable bytes.
    fn checkpoint(&self, committed: u64) -> crate::clock::BoxFuture<'_, Result<(), TaskStop>>;
}

/// Streams `response` into `writer`: accepted bytes are counted as they arrive, a flush and
/// checkpoint run every [`CHECKPOINT_INTERVAL`], and the loop stops promptly on a stop
/// request. Returns the number of bytes received.
///
/// # Errors
/// A network failure classified through the retry rules, a writer failure, a pause for a
/// full disk, `size_mismatch` when the server sends more than it declared, or
/// [`TaskStop::Stopped`].
pub async fn stream_body(
    mut response: Response,
    writer: &mut Writer,
    env: &Stream<'_>,
    checkpointer: &dyn Checkpointer,
) -> Result<u64, TaskStop> {
    let mut offset = 0u64;
    let mut last_checkpoint = Instant::now();
    let mut next_space_check = FREE_SPACE_RECHECK;
    loop {
        let chunk = tokio::select! {
            biased;
            () = env.stop.cancelled() => return Err(env.stop.stopped()),
            next = response.chunk() => next,
        };
        let bytes = match chunk {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return Ok(offset),
            Err(err) => return Err(failure_from_transfer(classify_reqwest(&err))),
        };
        writer
            .write(offset, &bytes)
            .await
            .map_err(|e| writer_stop(&e))?;
        offset += bytes.len() as u64;
        env.live.set_accepted(offset);
        if env.total.is_some_and(|t| offset > t) {
            return Err(Failure::new(DownloadErrorCode::SizeMismatch).into());
        }
        if offset >= next_space_check {
            next_space_check = next_space_check.saturating_add(FREE_SPACE_RECHECK);
            let remaining = env.total.map_or(0, |t| t.saturating_sub(offset));
            ensure_free_space(env.dir, remaining)?;
        }
        if last_checkpoint.elapsed() >= CHECKPOINT_INTERVAL {
            let frontier = writer.flush().await.map_err(|e| writer_stop(&e))?;
            env.live.set_committed(frontier);
            checkpointer.checkpoint(frontier).await?;
            last_checkpoint = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(status: u16) -> ResponseMeta {
        ResponseMeta {
            status,
            ..ResponseMeta::default()
        }
    }

    #[test]
    fn a_plain_200_with_length_is_nonresumable_and_without_is_unknown_length() {
        let mut m = meta(200);
        m.content_length = Some("10".to_owned());
        let plan = plan_response(&m).unwrap();
        assert_eq!(plan.transfer_mode, TransferMode::SingleNonresumable);
        assert_eq!(plan.total, Some(10));

        let unknown = plan_response(&meta(200)).unwrap();
        assert_eq!(unknown.transfer_mode, TransferMode::SingleUnknownLength);
        assert_eq!(unknown.total, None);

        m.content_length = Some("0".to_owned());
        assert_eq!(
            plan_response(&m).unwrap().transfer_mode,
            TransferMode::ZeroLength
        );
    }

    #[test]
    fn content_coded_and_partial_responses_are_refused_before_any_write() {
        let mut gz = meta(200);
        gz.content_encoding = Some("gzip".to_owned());
        assert_eq!(
            plan_response(&gz),
            Err(TaskStop::Fail(Failure::new(
                DownloadErrorCode::UnsupportedContentEncoding
            )))
        );
        let partial = plan_response(&meta(206));
        assert!(matches!(
            partial,
            Err(TaskStop::Fail(f)) if f.code == DownloadErrorCode::RangeProtocolViolation
        ));
        let mut bad = meta(200);
        bad.content_length = Some("12x".to_owned());
        assert!(plan_response(&bad).is_err());
    }

    #[test]
    fn error_statuses_map_to_credentials_or_http_status() {
        assert_eq!(plan_response(&meta(401)), Err(TaskStop::NeedsCredentials));
        assert_eq!(plan_response(&meta(407)), Err(TaskStop::NeedsCredentials));
        assert_eq!(
            plan_response(&meta(404)),
            Err(TaskStop::Fail(Failure::http_status(404)))
        );
        assert_eq!(
            plan_response(&meta(503)),
            Err(TaskStop::Fail(Failure::http_status(503)))
        );
    }

    #[test]
    fn part_names_use_the_random_tail_and_stay_within_the_limit() {
        let id = Uuid::now_v7();
        let first = part_name("a.bin", id, 0);
        let second = part_name("a.bin", id, 1);
        assert!(first.ends_with(".forgedm.part"));
        assert_ne!(first, second);
        let long = "x".repeat(naming::NAME_MAX_UTF16);
        assert!(part_name(&long, id, 0).encode_utf16().count() <= 255);
    }
}
