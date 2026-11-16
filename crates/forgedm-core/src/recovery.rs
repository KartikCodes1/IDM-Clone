//! Startup recovery of a download the previous agent left in `finalizing`
//! (ARCHITECTURE 6.3 step 2, PCR-01).
//!
//! A crash in `finalizing` can leave three situations, told apart only by file ids, never by
//! name or size:
//!
//! | on disk | action |
//! |---|---|
//! | the `.part` is there and has the recorded id | [`FinalizingAction::Redo`]: run finalize again |
//! | no `.part`, but the file at the recorded final path or rename candidate has the id | [`FinalizingAction::Complete`] |
//! | no file with the recorded id anywhere | [`FinalizingAction::Fail`] (`part_file_invalid`) |
//! | a candidate could not be inspected (locked, access denied) | [`FinalizingAction::Inconclusive`] (`finalize_locked`; the `.part` and progress are kept) |
//!
//! A file that merely has the same name and size is a foreign file and is never adopted.
//! Everything here is blocking: call it from a blocking thread.

use std::path::{Path, PathBuf};

use forgedm_platform::PlatformError;
use forgedm_platform::fs::{ensure_within, file_id, open_part_file};
use forgedm_platform::motw::MotwService;
use forgedm_protocol::MotwStatus;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::engine::StopReason;
use crate::error::{TaskStop, stop_from_platform};
use crate::finalize::{Finalized, apply_motw, finalize, zone_info};
use crate::hash::{HashError, sha256_file};
use crate::transfer::{encode_file_id, verify_part};

const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_PATH_NOT_FOUND: u32 = 3;

/// What the stored facts and the disk say about a `finalizing` download.
#[derive(Debug, Clone, Copy)]
pub struct FinalizingFacts<'a> {
    /// The download's target directory (a file must live inside it to count).
    pub target_dir: &'a Path,
    /// The recorded `.part` path.
    pub part_path: Option<&'a Path>,
    /// The recorded `.part` file id, as [`encode_file_id`] writes it.
    pub part_file_id: Option<&'a [u8]>,
    /// The recorded final path, when `completed` was nearly written.
    pub final_path: Option<&'a Path>,
    /// The last rename candidate persisted before the rename.
    pub finalize_target: Option<&'a Path>,
}

/// What to do with a `finalizing` download after a crash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinalizingAction {
    /// The `.part` is intact: write `Zone.Identifier`, rename and mark again.
    Redo,
    /// The rename already happened: the download is complete at `path`.
    Complete {
        /// Where the file is.
        path: PathBuf,
    },
    /// Nothing with the recorded id exists: `failed(part_file_invalid)`.
    Fail,
    /// No candidate was proven, but at least one could not be inspected (a sharing violation
    /// or access denied says nothing about the file): `failed(finalize_locked)`, keeping the
    /// `.part` so a retry can resume.
    Inconclusive,
}

/// What looking at one candidate file found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdCheck {
    /// A regular file inside the directory with the recorded id.
    Match,
    /// Provably not the file: missing, another id, a reparse point or outside the directory.
    NoMatch,
    /// The file could not be inspected (locked, access denied, an I/O error).
    Unknown,
}

/// Maps an open or containment error to "provably not it" or "cannot tell".
fn classify(err: &PlatformError, path: &Path) -> IdCheck {
    match err {
        PlatformError::PathEscape => {
            tracing::warn!(
                target: "forgedm::security",
                path = %path.display(),
                "a finalizing candidate escapes its directory; it is not adopted"
            );
            IdCheck::NoMatch
        }
        PlatformError::Win32 { code, .. }
            if matches!(*code, ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) =>
        {
            IdCheck::NoMatch
        }
        PlatformError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => IdCheck::NoMatch,
        other => {
            tracing::warn!(
                error = %other,
                path = %path.display(),
                "a finalizing candidate could not be inspected"
            );
            IdCheck::Unknown
        }
    }
}

/// Inspects the file at `path`: is it a regular file (not a reparse point) inside `dir`
/// with `recorded` as its file id? Only a proven mismatch is [`IdCheck::NoMatch`].
#[must_use]
pub fn check_file_id(path: &Path, dir: &Path, recorded: &[u8]) -> IdCheck {
    let file = match open_part_file(path) {
        Ok(file) => file,
        Err(err) => return classify(&err, path),
    };
    match file_id(&file) {
        Ok(id) if encode_file_id(&id) == recorded => {}
        Ok(_) => return IdCheck::NoMatch,
        Err(err) => return classify(&err, path),
    }
    match ensure_within(&file, dir) {
        Ok(()) => IdCheck::Match,
        Err(err) => classify(&err, path),
    }
}

/// Decides the action for a `finalizing` download. Without a recorded file id nothing can
/// be proven, so the answer is [`FinalizingAction::Fail`]; a candidate that cannot be
/// inspected makes the answer [`FinalizingAction::Inconclusive`] unless another one matches.
#[must_use]
pub fn decide_finalizing(facts: &FinalizingFacts<'_>) -> FinalizingAction {
    let Some(recorded) = facts.part_file_id else {
        return FinalizingAction::Fail;
    };
    let mut unknown = false;
    if let Some(part) = facts.part_path {
        match check_file_id(part, facts.target_dir, recorded) {
            IdCheck::Match => return FinalizingAction::Redo,
            IdCheck::NoMatch => {}
            IdCheck::Unknown => unknown = true,
        }
    }
    for path in [facts.finalize_target, facts.final_path]
        .into_iter()
        .flatten()
    {
        match check_file_id(path, facts.target_dir, recorded) {
            IdCheck::Match => {
                return FinalizingAction::Complete {
                    path: path.to_path_buf(),
                };
            }
            IdCheck::NoMatch => {}
            IdCheck::Unknown => unknown = true,
        }
    }
    if unknown {
        FinalizingAction::Inconclusive
    } else {
        FinalizingAction::Fail
    }
}

/// Everything a redone finalize needs.
#[derive(Debug, Clone)]
pub struct RedoInput<'a> {
    /// The `.part` to finalize.
    pub part: &'a Path,
    /// The destination directory.
    pub dir: &'a Path,
    /// The file name to move it to.
    pub name: &'a str,
    /// The URL the zone information is derived from.
    pub source: &'a Url,
    /// The complete length, when known.
    pub total: Option<u64>,
    /// The checksum the user expects, lowercase hex.
    pub expected_sha256: Option<&'a str>,
}

/// The outcome of a redone finalize.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redone {
    /// Final path and Mark-of-the-Web status.
    pub finalized: Finalized,
    /// SHA-256 of the file, lowercase hex.
    pub sha256: String,
    /// Whether the length was proven.
    pub length_verified: bool,
}

/// Verifies the `.part` and runs the whole finalize sequence again. Every step is
/// idempotent: the stream write replaces its content and the move never replaces a file.
///
/// # Errors
/// A [`TaskStop`]: a size or checksum mismatch, or the finalize failure.
pub fn redo_finalize(
    input: &RedoInput<'_>,
    motw: &MotwService,
    on_candidate: &mut dyn FnMut(&Path) -> Result<(), forgedm_platform::PlatformError>,
) -> Result<Redone, TaskStop> {
    let token = CancellationToken::new();
    let verified = verify_part(
        input.part,
        input.total,
        input.expected_sha256,
        &token,
        || StopReason::Cancel,
    )?;
    if verified.checksum_matches == Some(false) {
        return Err(crate::error::Failure::new(
            forgedm_protocol::DownloadErrorCode::ChecksumMismatch,
        )
        .into());
    }
    let finalized = finalize(
        input.part,
        input.dir,
        input.name,
        input.source,
        motw,
        on_candidate,
    )?;
    Ok(Redone {
        finalized,
        sha256: verified.sha256,
        length_verified: verified.length_verified,
    })
}

/// The file already sits at `path`: hashes it and re-applies the shell attachment check
/// (idempotent), as the last steps of a finalize that crashed after the rename.
///
/// # Errors
/// A [`TaskStop`] when the file cannot be read or the shell refuses it.
pub fn complete_moved(
    path: &Path,
    source: &Url,
    motw: &MotwService,
) -> Result<(String, MotwStatus), TaskStop> {
    let hashed = sha256_file(path, &CancellationToken::new()).map_err(|err| match err {
        HashError::Io(io) => crate::error::stop_from_io(&io),
        HashError::Cancelled => TaskStop::Stopped(StopReason::Cancel),
    })?;
    let zone = zone_info(source).map_err(|e| stop_from_platform(&e))?;
    let status = apply_motw(motw, path, &zone)?;
    Ok((hashed.sha256, status))
}

#[cfg(test)]
mod tests {
    use forgedm_platform::fs::create_part_file;

    use super::*;

    fn staged(dir: &Path, name: &str) -> (PathBuf, Vec<u8>) {
        let path = dir.join(name);
        let file = create_part_file(&path).unwrap();
        let id = encode_file_id(&file_id(&file).unwrap());
        (path, id)
    }

    fn facts<'a>(
        dir: &'a Path,
        part: &'a Path,
        id: Option<&'a [u8]>,
        target: Option<&'a Path>,
    ) -> FinalizingFacts<'a> {
        FinalizingFacts {
            target_dir: dir,
            part_path: Some(part),
            part_file_id: id,
            final_path: None,
            finalize_target: target,
        }
    }

    #[test]
    fn an_intact_part_is_redone() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap();
        let (part, id) = staged(&dir, "a.forgedm.part");
        let action = decide_finalizing(&facts(&dir, &part, Some(&id), None));
        assert_eq!(action, FinalizingAction::Redo);
    }

    #[test]
    fn a_renamed_file_with_the_recorded_id_completes() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap();
        let (part, id) = staged(&dir, "a.forgedm.part");
        let target = dir.join("a.bin");
        forgedm_platform::fs::move_no_replace(&part, &target).unwrap();
        let action = decide_finalizing(&facts(&dir, &part, Some(&id), Some(&target)));
        assert_eq!(action, FinalizingAction::Complete { path: target });
    }

    #[test]
    fn a_foreign_file_with_the_same_name_is_never_adopted() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap();
        let (part, id) = staged(&dir, "a.forgedm.part");
        std::fs::remove_file(&part).unwrap();
        let foreign = dir.join("a.bin");
        std::fs::write(&foreign, b"someone else's file").unwrap();
        let action = decide_finalizing(&facts(&dir, &part, Some(&id), Some(&foreign)));
        assert_eq!(action, FinalizingAction::Fail);
    }

    #[test]
    fn a_locked_part_is_inconclusive_not_a_failure() {
        use std::os::windows::fs::OpenOptionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap();
        let (part, id) = staged(&dir, "a.forgedm.part");
        // Hold the file with no sharing at all, as an antivirus scan would.
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&part)
            .unwrap();

        let action = decide_finalizing(&facts(&dir, &part, Some(&id), None));
        let check = check_file_id(&part, &dir, &id);
        drop(held);

        assert_eq!(check, IdCheck::Unknown);
        assert_eq!(action, FinalizingAction::Inconclusive);
        assert_eq!(check_file_id(&part, &dir, &id), IdCheck::Match);
    }

    #[test]
    fn a_missing_file_and_another_id_are_proven_mismatches() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap();
        let (part, id) = staged(&dir, "a.forgedm.part");
        let (other, _) = staged(&dir, "b.forgedm.part");
        assert_eq!(check_file_id(&other, &dir, &id), IdCheck::NoMatch);
        std::fs::remove_file(&part).unwrap();
        assert_eq!(check_file_id(&part, &dir, &id), IdCheck::NoMatch);
    }

    #[test]
    fn nothing_recorded_cannot_be_proven_and_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap();
        let (part, _) = staged(&dir, "a.forgedm.part");
        assert_eq!(
            decide_finalizing(&facts(&dir, &part, None, None)),
            FinalizingAction::Fail
        );
    }
}
