//! Finalize: Mark-of-the-Web on the staged file, the no-replace move into place and the
//! shell's attachment check (ARCHITECTURE 5.5 steps 4 to 7, 5.6, ADR-015).
//!
//! Everything here is blocking and runs on a blocking thread. The caller has already
//! persisted the `finalizing` state; `on_candidate` persists each rename candidate before
//! it is tried (DUR-9). A completed file is never replaced: the move uses the platform's
//! no-replace primitive and collides to `name (1).ext`, `name (2).ext`, ...

use std::path::{Path, PathBuf};
use std::time::Duration;

use forgedm_platform::PlatformError;
use forgedm_platform::fs::finalize_move;
use forgedm_platform::motw::{
    INTERNET_ZONE_ID, MotwOutcome, MotwService, ZoneInfo, write_zone_identifier, zone_source_url,
};
use forgedm_protocol::{DownloadErrorCode, MotwStatus};
use url::Url;

use crate::error::{Failure, TaskStop, stop_from_platform};

/// Total time spent retrying a locked destination (A.9 `FINALIZE_LOCK_RETRY`).
pub const FINALIZE_LOCK_RETRY: Duration = Duration::from_secs(10);

/// What finalizing produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finalized {
    /// Where the file now lives.
    pub final_path: PathBuf,
    /// How Mark-of-the-Web was applied.
    pub motw_status: MotwStatus,
}

/// The sanitised `Zone.Identifier` content for a file downloaded from `source`:
/// `scheme://host[:port]/path` without userinfo, query or fragment (PLAN D8).
///
/// # Errors
/// [`PlatformError::InvalidInput`] when the URL contains a control character.
pub fn zone_info(source: &Url) -> Result<ZoneInfo, PlatformError> {
    Ok(ZoneInfo {
        zone_id: INTERNET_ZONE_ID,
        host_url: zone_source_url(source.as_str(), false)?,
        referrer_url: None,
    })
}

/// Writes the Zone.Identifier stream onto the staged file, so the final name is never
/// visible without it. Returns `false` when the volume has no alternate data streams.
///
/// # Errors
/// Platform errors other than "unsupported".
pub fn mark_part(part: &Path, zone: &ZoneInfo) -> Result<bool, PlatformError> {
    match write_zone_identifier(part, zone) {
        Ok(()) => Ok(true),
        Err(PlatformError::Unsupported(_)) => Ok(false),
        Err(other) => Err(other),
    }
}

/// Moves `part` to `dir\name` without replacing anything, calling `on_candidate` with each
/// candidate path before its first attempt.
///
/// # Errors
/// [`PlatformError`] from the move; a persistently locked destination is
/// [`PlatformError::SharingViolation`].
pub fn move_into_place(
    part: &Path,
    dir: &Path,
    name: &str,
    on_candidate: &mut dyn FnMut(&Path) -> Result<(), PlatformError>,
) -> Result<PathBuf, PlatformError> {
    finalize_move(
        part,
        dir,
        name,
        on_candidate,
        FINALIZE_LOCK_RETRY,
        &mut std::thread::sleep,
    )
}

/// Runs the shell attachment check on the final file and maps the outcome.
///
/// # Errors
/// `blocked_by_policy` or `blocked_by_antivirus` when the shell refused the file,
/// otherwise the platform failure.
pub fn apply_motw(
    motw: &MotwService,
    final_path: &Path,
    zone: &ZoneInfo,
) -> Result<MotwStatus, TaskStop> {
    match motw.protect(final_path, zone) {
        Ok(MotwOutcome::Marked) => Ok(MotwStatus::Marked),
        Ok(MotwOutcome::MarkedDirect) => Ok(MotwStatus::MarkedDirect),
        Ok(MotwOutcome::UnsupportedFs) => Ok(MotwStatus::UnsupportedFs),
        Ok(MotwOutcome::BlockedByPolicy) => {
            Err(Failure::new(DownloadErrorCode::BlockedByPolicy).into())
        }
        Ok(MotwOutcome::RemovedByAntivirus) => {
            Err(Failure::new(DownloadErrorCode::BlockedByAntivirus).into())
        }
        Err(err) => Err(stop_from_platform(&err)),
    }
}

/// The whole finalize sequence: Zone.Identifier on the `.part`, the no-replace move, then
/// the attachment check on the final path.
///
/// # Errors
/// A [`TaskStop`] describing the failure; the `.part` is left in place when the move did
/// not happen.
pub fn finalize(
    part: &Path,
    dir: &Path,
    name: &str,
    source: &Url,
    motw: &MotwService,
    on_candidate: &mut dyn FnMut(&Path) -> Result<(), PlatformError>,
) -> Result<Finalized, TaskStop> {
    let zone = zone_info(source).map_err(|e| stop_from_platform(&e))?;
    let marked_part = mark_part(part, &zone).map_err(|e| stop_from_platform(&e))?;
    let final_path =
        move_into_place(part, dir, name, on_candidate).map_err(|e| stop_from_platform(&e))?;
    let motw_status = if marked_part {
        apply_motw(motw, &final_path, &zone)?
    } else {
        MotwStatus::UnsupportedFs
    };
    Ok(Finalized {
        final_path,
        motw_status,
    })
}
