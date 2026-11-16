//! Deciding whether a stored download can continue, and checking its staging file
//! (ARCHITECTURE 4.8 and 6.3, ADR-021).
//!
//! A download resumes only when everything lines up: a resumable transfer mode, a known
//! total, a strong validator (so `If-Range` can prove the bytes are still the same
//! representation) and a staging file that is provably the one we created. Anything else
//! restarts from zero. The exception is a ranged or segmented download with a weak or missing
//! validator and at least [`SAMPLE_VERIFY_WINDOW`] committed bytes: [`sample_verify`]
//! re-requests a window before a committed offset (for a segment table, the offset of the
//! segment with the most committed data) and compares it with the disk, so a changed file is
//! caught (`remote_changed`) without a strong validator (S1.14, ARCHITECTURE 4.7).

use std::fs::File;
use std::os::windows::fs::FileExt;

use forgedm_platform::PlatformError;
use forgedm_platform::fs::{ensure_within, file_id, open_part_file};
use forgedm_protocol::{DownloadErrorCode, TransferMode};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::client::{
    ClientError, FetchSpec, HttpClient, classify_reqwest, response_meta, retry_after_of,
};
use crate::engine::StopReason;
use crate::error::{Failure, Fault, TaskStop, stop_from_io, stop_from_platform};
use crate::http::content_range::RangeRequest;
use crate::http::range_decision::{RangeDecision, RangedRequest, decide};
use crate::http::validators::ResumeIdentity;
use crate::ports::StoredJob;
use crate::transfer::encode_file_id;

/// Bytes before the committed offset that [`sample_verify`] re-requests (A.9
/// `SAMPLE_VERIFY_WINDOW`, 64 KiB).
pub const SAMPLE_VERIFY_WINDOW: u64 = 64 * 1024;

/// Why a download restarts from zero instead of resuming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartReason {
    /// The transfer is not resumable (the server ignored ranges, or the length is unknown).
    NotResumable,
    /// No strong validator was recorded, so nothing could prove the bytes unchanged.
    NoStrongValidator,
    /// The staging file is missing, replaced or shorter than what was committed.
    PartFileInvalid,
    /// The stored segment rows overlap, leave a gap, exceed their end or cannot be read.
    MetadataCorrupt,
}

impl RestartReason {
    /// The event code recorded with the restart.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::NotResumable => "not_resumable",
            Self::NoStrongValidator => "no_strong_validator",
            Self::PartFileInvalid => "part_file_invalid",
            Self::MetadataCorrupt => "metadata_corrupt",
        }
    }
}

/// What a resumed download continues from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Continuation {
    /// Bytes durably committed; the next request starts here.
    pub committed: u64,
    /// The complete length.
    pub total: u64,
    /// The stored remote identity.
    pub identity: ResumeIdentity,
    /// No strong validator is stored: [`sample_verify`] must pass before the first byte of
    /// the continuation is requested.
    pub sample: bool,
}

/// The decision for a download that is about to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumePlan {
    /// Nothing has been done before: probe and start.
    Fresh,
    /// Throw away what exists and start over.
    Restart(RestartReason),
    /// Continue from the committed offset.
    Continue(Continuation),
}

/// Whether a transfer mode can continue after an interruption: a verified 206 with a known
/// total. A segmented plan resumes from its persisted segment table.
#[must_use]
pub const fn is_resumable_mode(mode: TransferMode) -> bool {
    matches!(mode, TransferMode::SingleRanged | TransferMode::Segmented)
}

/// Decides how `job` continues. Pure: it reads only what the store kept.
#[must_use]
pub fn plan_resume(job: &StoredJob) -> ResumePlan {
    let stored = &job.progress;
    let has_part = job.part_path.is_some();
    if !has_part && stored.committed == 0 {
        return ResumePlan::Fresh;
    }
    let resumable = job.transfer_mode.is_some_and(is_resumable_mode);
    let Some(total) = stored.total.filter(|t| resumable && stored.committed <= *t) else {
        return ResumePlan::Restart(RestartReason::NotResumable);
    };
    let identity = ResumeIdentity::from_stored(
        Some(total),
        stored.etag.as_deref(),
        stored.etag_strong,
        stored.last_modified.as_deref(),
        stored.last_modified_strong,
    );
    let sample = !identity.has_strong_validator();
    if sample && !can_sample(job.transfer_mode, stored.committed) {
        return ResumePlan::Restart(RestartReason::NoStrongValidator);
    }
    if !has_part {
        return ResumePlan::Restart(RestartReason::PartFileInvalid);
    }
    ResumePlan::Continue(Continuation {
        committed: stored.committed,
        total,
        identity,
        sample,
    })
}

/// Whether a download without a strong validator may continue after sampling: a ranged or
/// segmented transfer with a full window committed. For a segment table the engine then picks
/// the segment to sample ([`crate::transfer::SegmentTable::sample_point`]) and restarts from
/// zero when no single segment has a full window.
const fn can_sample(mode: Option<TransferMode>, committed: u64) -> bool {
    matches!(
        mode,
        Some(TransferMode::SingleRanged | TransferMode::Segmented)
    ) && committed >= SAMPLE_VERIFY_WINDOW
}

/// Why [`sample_verify`] did not pass.
#[derive(Debug)]
pub enum SampleError {
    /// The server's bytes differ from the disk, or the server no longer serves the same
    /// representation: the download fails with `remote_changed`.
    Changed,
    /// The sample could not be taken: a retryable fault, a stop or a failure.
    Stop(TaskStop),
}

/// Re-requests the [`SAMPLE_VERIFY_WINDOW`] bytes before `from.committed` and compares them
/// with the same bytes of `file`. No `If-Range` is sent: this is for downloads without a
/// strong validator. The response is judged like any ranged response; anything but a 206
/// that covers exactly the window with identical bytes is a change.
///
/// # Errors
/// [`SampleError::Changed`] on any difference, [`SampleError::Stop`] when the request failed.
pub async fn sample_verify(
    client: &HttpClient,
    url: &Url,
    file: &File,
    from: &Continuation,
    cancel: &CancellationToken,
) -> Result<(), SampleError> {
    sample_verify_at(client, url, file, from, from.committed, cancel).await
}

/// Like [`sample_verify`], but samples the window that ends at `end` (a committed offset of
/// one segment) instead of the continuation's aggregate offset.
///
/// # Errors
/// [`SampleError::Changed`] on any difference, [`SampleError::Stop`] when the request failed.
pub async fn sample_verify_at(
    client: &HttpClient,
    url: &Url,
    file: &File,
    from: &Continuation,
    end: u64,
    cancel: &CancellationToken,
) -> Result<(), SampleError> {
    let start = end.saturating_sub(SAMPLE_VERIFY_WINDOW);
    let window = RangeRequest {
        start,
        end_excl: end,
    };
    let disk = read_window(file, start, end).await?;
    let spec = FetchSpec::ranged(window, None);
    let fetched = client
        .fetch(url, &spec, cancel)
        .await
        .map_err(sample_stop)?;
    let request = RangedRequest {
        range: window,
        if_range_sent: false,
        segment_no: 0,
        has_committed_data: true,
    };
    match decide(&request, &from.identity, &response_meta(&fetched.response)) {
        RangeDecision::Accept {
            start: s,
            end_excl: e,
            short: false,
        } if (s, e) == (start, end) => {}
        RangeDecision::OtherStatus { status }
            if status >= 500 || status == 429 || status == 408 =>
        {
            let retry_after = retry_after_of(&fetched.response);
            let fault = Fault::Status {
                status,
                retry_after,
            };
            return Err(SampleError::Stop(TaskStop::Retry(fault)));
        }
        RangeDecision::OtherStatus { status: 401 | 407 } => {
            return Err(SampleError::Stop(TaskStop::NeedsCredentials));
        }
        other => {
            tracing::warn!(decision = ?other, "sample response is not the requested window; treating the file as changed");
            return Err(SampleError::Changed);
        }
    }
    let remote = read_body(fetched.response, disk.len(), cancel).await?;
    if remote == disk {
        Ok(())
    } else {
        Err(SampleError::Changed)
    }
}

fn sample_stop(err: ClientError) -> SampleError {
    SampleError::Stop(match err {
        ClientError::Cancelled => TaskStop::Stopped(StopReason::Cancel),
        ClientError::Redirect(redirect) => TaskStop::Fail(Failure::new(redirect.code())),
        ClientError::Transport(kind) => TaskStop::Retry(Fault::Transport(kind)),
        ClientError::Build(_) => TaskStop::Fail(Failure::new(DownloadErrorCode::Internal)),
    })
}

/// Reads `[start, end)` of `file` on a blocking thread.
async fn read_window(file: &File, start: u64, end: u64) -> Result<Vec<u8>, SampleError> {
    let file = file
        .try_clone()
        .map_err(|e| SampleError::Stop(stop_from_io(&e)))?;
    let len = usize::try_from(end - start).unwrap_or(0);
    let internal = || SampleError::Stop(TaskStop::Fail(Failure::new(DownloadErrorCode::Internal)));
    tokio::task::spawn_blocking(move || {
        let mut buf = vec![0u8; len];
        let mut done = 0;
        while done < len {
            let n = file.seek_read(&mut buf[done..], start + done as u64)?;
            if n == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            done += n;
        }
        Ok(buf)
    })
    .await
    .map_err(|_| internal())?
    .map_err(|e: std::io::Error| SampleError::Stop(stop_from_io(&e)))
}

/// Reads the body to its end; a body longer than `want` is a change.
async fn read_body(
    mut response: reqwest::Response,
    want: usize,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, SampleError> {
    let mut body = Vec::with_capacity(want);
    loop {
        let chunk = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(sample_stop(ClientError::Cancelled)),
            chunk = response.chunk() => chunk,
        };
        match chunk {
            Ok(Some(bytes)) => {
                body.extend_from_slice(&bytes);
                if body.len() > want {
                    return Err(SampleError::Changed);
                }
            }
            Ok(None) => return Ok(body),
            Err(err) => {
                let kind = classify_reqwest(&err);
                return Err(SampleError::Stop(TaskStop::Retry(Fault::Transport(kind))));
            }
        }
    }
}

/// Why the recorded staging file cannot be used.
#[derive(Debug, Error)]
pub enum PartInvalid {
    /// No path or file id is recorded.
    #[error("the staging file is not recorded")]
    NotRecorded,
    /// The file is gone, unreadable, or a reparse point.
    #[error("the staging file cannot be opened")]
    Unavailable(#[source] PlatformError),
    /// The file at the path is not the one that was created (different file id).
    #[error("the staging file was replaced")]
    Replaced,
    /// The file is not inside the download's directory.
    #[error("the staging file is outside the download directory")]
    Outside,
    /// The file is shorter than the committed byte count.
    #[error("the staging file is shorter than the committed bytes")]
    Short,
    /// The uncommitted tail could not be cut off.
    #[error("cannot truncate the staging file")]
    Truncate(#[source] std::io::Error),
}

const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_PATH_NOT_FOUND: u32 = 3;

impl PartInvalid {
    /// A stop that keeps the stored progress, for failures that say nothing about the
    /// file's validity (it is locked, access is denied, the drive is offline, the cut
    /// failed). `None` means the file is provably missing, replaced, a reparse point or
    /// short, so the spec-mandated restart from zero applies (ARCHITECTURE 6.3).
    #[must_use]
    pub fn keeps_progress(&self) -> Option<TaskStop> {
        match self {
            Self::NotRecorded | Self::Replaced | Self::Outside | Self::Short => None,
            Self::Truncate(err) => Some(stop_from_io(err)),
            Self::Unavailable(err) => match err {
                PlatformError::PathEscape => None,
                PlatformError::Win32 { code, .. }
                    if matches!(*code, ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) =>
                {
                    None
                }
                PlatformError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => None,
                PlatformError::SharingViolation => {
                    Some(TaskStop::Fail(Failure::new(DownloadErrorCode::DiskIo)))
                }
                other => Some(stop_from_platform(other)),
            },
        }
    }
}

/// Opens `job`'s staging file for resume and checks it is ours: the recorded path, not a
/// reparse point, the recorded file id, inside the target directory, at least `committed`
/// bytes long. Then cuts it to exactly `committed` bytes: anything beyond was never
/// checkpointed and is not trusted.
///
/// Blocking: call from `spawn_blocking`.
///
/// # Errors
/// [`PartInvalid`] for every way the file fails those checks.
pub fn open_valid_part(job: &StoredJob, committed: u64) -> Result<File, PartInvalid> {
    let (Some(path), Some(recorded)) = (job.part_path.as_deref(), job.part_file_id.as_deref())
    else {
        return Err(PartInvalid::NotRecorded);
    };
    let file = open_part_file(path).map_err(|err| {
        if matches!(err, PlatformError::PathEscape) {
            tracing::warn!(
                target: "forgedm::security",
                "recorded staging path is a reparse point; resume refused"
            );
        }
        PartInvalid::Unavailable(err)
    })?;
    let same_file = file_id(&file).is_ok_and(|id| encode_file_id(&id) == recorded);
    if !same_file {
        return Err(PartInvalid::Replaced);
    }
    if ensure_within(&file, &job.target_dir).is_err() {
        return Err(PartInvalid::Outside);
    }
    let length = file
        .metadata()
        .map_err(|err| PartInvalid::Unavailable(PlatformError::Io(err)))?
        .len();
    if length < committed {
        return Err(PartInvalid::Short);
    }
    file.set_len(committed).map_err(PartInvalid::Truncate)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use forgedm_protocol::DownloadState;
    use uuid::Uuid;

    use super::*;
    use crate::ports::StoredProgress;
    use crate::url_policy::validate_user_url;

    fn job(mode: Option<TransferMode>, progress: StoredProgress, part: bool) -> StoredJob {
        StoredJob {
            id: Uuid::nil(),
            state: DownloadState::Interrupted,
            state_reason: None,
            url: validate_user_url("https://example.com/f").unwrap(),
            target_dir: PathBuf::from(r"C:\d"),
            requested_name: None,
            display_name: "f".to_owned(),
            file_name: Some("f".to_owned()),
            expected_sha256: None,
            part_path: part.then(|| PathBuf::from(r"C:\d\f.part")),
            part_file_id: part.then(|| vec![1]),
            transfer_mode: mode,
            progress,
            max_connections: 8,
            priority: forgedm_protocol::Priority::Normal,
            queue_position: 0,
            created_at_ms: 0,
        }
    }

    fn strong(committed: u64) -> StoredProgress {
        StoredProgress {
            total: Some(100),
            committed,
            etag: Some("\"v1\"".to_owned()),
            etag_strong: Some(true),
            ..StoredProgress::default()
        }
    }

    #[test]
    fn a_job_with_no_part_and_no_bytes_starts_fresh() {
        let fresh = job(None, StoredProgress::default(), false);
        assert_eq!(plan_resume(&fresh), ResumePlan::Fresh);
    }

    #[test]
    fn a_ranged_download_with_a_strong_validator_continues() {
        let plan = plan_resume(&job(Some(TransferMode::SingleRanged), strong(40), true));
        assert!(matches!(plan, ResumePlan::Continue(_)), "{plan:?}");
        let ResumePlan::Continue(c) = plan else {
            unreachable!("checked above");
        };
        assert_eq!((c.committed, c.total), (40, 100));
        assert_eq!(c.identity.if_range().as_deref(), Some("\"v1\""));
    }

    #[test]
    fn weak_or_missing_validators_and_non_ranged_modes_restart() {
        let weak = StoredProgress {
            etag_strong: Some(false),
            ..strong(40)
        };
        let no_etag = StoredProgress {
            etag: None,
            etag_strong: None,
            ..strong(40)
        };
        let no_total = StoredProgress {
            total: None,
            ..strong(40)
        };
        let beyond_total = StoredProgress {
            committed: 101,
            ..strong(0)
        };
        let ranged = Some(TransferMode::SingleRanged);
        let cases = [
            (ranged, weak, RestartReason::NoStrongValidator),
            (ranged, no_etag, RestartReason::NoStrongValidator),
            (
                Some(TransferMode::SingleNonresumable),
                strong(40),
                RestartReason::NotResumable,
            ),
            (
                Some(TransferMode::SingleUnknownLength),
                strong(40),
                RestartReason::NotResumable,
            ),
            (None, strong(40), RestartReason::NotResumable),
            (ranged, no_total, RestartReason::NotResumable),
            (ranged, beyond_total, RestartReason::NotResumable),
        ];
        for (mode, progress, why) in cases {
            assert_eq!(
                plan_resume(&job(mode, progress, true)),
                ResumePlan::Restart(why)
            );
        }
    }

    #[test]
    fn a_weak_ranged_download_with_a_full_window_continues_with_sampling() {
        let weak = |committed| StoredProgress {
            total: Some(10 * SAMPLE_VERIFY_WINDOW),
            committed,
            etag: Some("W/\"x\"".to_owned()),
            etag_strong: Some(false),
            ..StoredProgress::default()
        };
        let ranged = Some(TransferMode::SingleRanged);

        let plan = plan_resume(&job(ranged, weak(SAMPLE_VERIFY_WINDOW), true));
        let ResumePlan::Continue(c) = plan else {
            unreachable!("expected a continuation, got {plan:?}");
        };
        assert!(c.sample && c.identity.if_range().is_none());
        assert_eq!(c.committed, SAMPLE_VERIFY_WINDOW);

        assert_eq!(
            plan_resume(&job(ranged, weak(SAMPLE_VERIFY_WINDOW - 1), true)),
            ResumePlan::Restart(RestartReason::NoStrongValidator),
            "less than one window restarts from zero"
        );
        let segmented = Some(TransferMode::Segmented);
        let plan = plan_resume(&job(segmented, weak(SAMPLE_VERIFY_WINDOW), true));
        assert!(
            matches!(plan, ResumePlan::Continue(ref c) if c.sample),
            "a segment table with a full window committed is sampled: {plan:?}"
        );
        assert_eq!(
            plan_resume(&job(segmented, weak(SAMPLE_VERIFY_WINDOW - 1), true)),
            ResumePlan::Restart(RestartReason::NoStrongValidator),
            "a segmented download with less than one window restarts from zero"
        );
        let strong_plan = plan_resume(&job(ranged, strong(40), true));
        assert!(matches!(strong_plan, ResumePlan::Continue(c) if !c.sample));
    }

    #[test]
    fn a_last_modified_validator_counts_only_without_an_etag() {
        let lm = StoredProgress {
            total: Some(100),
            committed: 10,
            last_modified: Some("Wed, 21 Oct 2015 07:28:00 GMT".to_owned()),
            last_modified_strong: Some(true),
            ..StoredProgress::default()
        };
        assert!(matches!(
            plan_resume(&job(Some(TransferMode::SingleRanged), lm.clone(), true)),
            ResumePlan::Continue(_)
        ));
        let with_weak_etag = StoredProgress {
            etag: Some("W/\"x\"".to_owned()),
            etag_strong: Some(false),
            ..lm
        };
        assert_eq!(
            plan_resume(&job(Some(TransferMode::SingleRanged), with_weak_etag, true)),
            ResumePlan::Restart(RestartReason::NoStrongValidator),
            "a present weak ETag rules out the Last-Modified fallback"
        );
    }

    #[test]
    fn committed_bytes_without_a_recorded_part_restart() {
        assert_eq!(
            plan_resume(&job(Some(TransferMode::SingleRanged), strong(40), false)),
            ResumePlan::Restart(RestartReason::PartFileInvalid)
        );
    }

    #[test]
    fn a_missing_recorded_file_is_not_a_valid_part() {
        let mut gone = job(Some(TransferMode::SingleRanged), strong(10), true);
        gone.part_path = Some(PathBuf::from(r"C:\no\such\dir\x.part"));
        assert!(matches!(
            open_valid_part(&gone, 10),
            Err(PartInvalid::Unavailable(_))
        ));
        gone.part_file_id = None;
        assert!(matches!(
            open_valid_part(&gone, 10),
            Err(PartInvalid::NotRecorded)
        ));
    }

    #[test]
    fn only_failures_that_say_nothing_about_the_file_keep_progress() {
        let unavailable = |e| PartInvalid::Unavailable(e);
        let win32 = |code| PlatformError::Win32 {
            api: "CreateFileW",
            code,
        };
        assert_eq!(unavailable(win32(2)).keeps_progress(), None);
        assert_eq!(unavailable(win32(3)).keeps_progress(), None);
        assert_eq!(
            unavailable(PlatformError::PathEscape).keeps_progress(),
            None
        );
        assert_eq!(PartInvalid::Replaced.keeps_progress(), None);
        assert_eq!(PartInvalid::Short.keeps_progress(), None);
        assert_eq!(
            unavailable(PlatformError::SharingViolation).keeps_progress(),
            Some(TaskStop::Fail(Failure::new(DownloadErrorCode::DiskIo)))
        );
        assert_eq!(
            unavailable(win32(5)).keeps_progress(),
            Some(TaskStop::Fail(Failure::new(DownloadErrorCode::DiskIo)))
        );
        assert_eq!(
            unavailable(PlatformError::DestinationUnavailable).keeps_progress(),
            Some(TaskStop::PauseFor(
                DownloadErrorCode::DestinationUnavailable
            ))
        );
    }

    #[test]
    fn a_locked_part_file_is_not_a_missing_one() {
        let dir = std::env::temp_dir().join(format!(
            "forgedm-resume-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.part");
        std::fs::write(&path, b"data").unwrap();
        // Hold the file with no sharing at all, as another program would.
        let held = {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&path)
                .unwrap()
        };
        let mut locked = job(Some(TransferMode::SingleRanged), strong(2), true);
        locked.part_path = Some(path);
        let err = open_valid_part(&locked, 2).unwrap_err();
        drop(held);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(err.keeps_progress().is_some(), "{err:?}");
    }

    #[test]
    fn unrecorded_part_still_restarts() {
        let mut gone = job(Some(TransferMode::SingleRanged), strong(10), true);
        gone.part_file_id = None;
        assert!(matches!(
            open_valid_part(&gone, 10),
            Err(PartInvalid::NotRecorded)
        ));
    }
}
