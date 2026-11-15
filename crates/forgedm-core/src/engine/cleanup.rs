//! Removing the stale staging file of a download that restarts from zero (DUR-10).
//!
//! The agent deletes only files it created and recorded: the path must be inside the
//! download's directory, must not be a reparse point, and its file id must equal the one
//! stored when the file was created. Deletion is by handle, never by name.

use forgedm_platform::PlatformError;
use forgedm_platform::fs::{delete_by_handle, ensure_within, file_id, open_part_file};

use crate::ports::StoredJob;
use crate::transfer::encode_file_id;

/// What happened to a stale staging file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cleanup {
    /// The file was ours and is deleted.
    Deleted,
    /// Nothing recorded, or the file is already gone.
    Nothing,
    /// A file is there but is not provably ours: left alone.
    Kept,
}

/// Deletes `job`'s recorded `.part` if (and only if) it is provably the file we created.
///
/// Blocking: call from `spawn_blocking`.
#[must_use]
pub fn delete_stale_part(job: &StoredJob) -> Cleanup {
    let Some(path) = job.part_path.as_deref() else {
        return Cleanup::Nothing;
    };
    // A recorded path without a recorded file id cannot be proven ours: fail safe and leave
    // the file alone (never "nothing to do", which would silently disable cleanup).
    let Some(recorded) = job.part_file_id.as_deref() else {
        tracing::warn!("a staging file is recorded without its file id; it was not deleted");
        return Cleanup::Kept;
    };
    let file = match open_part_file(path) {
        Ok(file) => file,
        Err(PlatformError::PathEscape) => {
            tracing::warn!(
                target: "forgedm::security",
                "recorded staging path is a reparse point; it was not deleted"
            );
            return Cleanup::Kept;
        }
        Err(PlatformError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
            return Cleanup::Nothing;
        }
        Err(err) => {
            tracing::warn!(error = %err, "could not open the stale staging file");
            return Cleanup::Kept;
        }
    };
    let ours = file_id(&file).is_ok_and(|id| encode_file_id(&id) == recorded)
        && ensure_within(&file, &job.target_dir).is_ok();
    if !ours {
        return Cleanup::Kept;
    }
    match delete_by_handle(&file) {
        Ok(()) => Cleanup::Deleted,
        Err(err) => {
            tracing::warn!(error = %err, "could not delete the stale staging file");
            Cleanup::Kept
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use forgedm_protocol::DownloadState;
    use uuid::Uuid;

    use super::*;
    use crate::ports::StoredProgress;
    use crate::url_policy::validate_user_url;

    fn job(part: Option<PathBuf>, id: Option<Vec<u8>>, dir: PathBuf) -> StoredJob {
        StoredJob {
            id: Uuid::nil(),
            state: DownloadState::Interrupted,
            state_reason: None,
            url: validate_user_url("https://example.com/f").unwrap(),
            target_dir: dir,
            requested_name: None,
            display_name: "f".to_owned(),
            file_name: None,
            expected_sha256: None,
            part_path: part,
            part_file_id: id,
            transfer_mode: None,
            progress: StoredProgress::default(),
        }
    }

    #[test]
    fn a_recorded_part_without_a_file_id_is_kept_never_ignored_or_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let part = canonical.join("a.bin.0123abcd.forgedm.part");
        std::fs::write(&part, b"data").unwrap();

        let outcome = delete_stale_part(&job(Some(part.clone()), None, canonical.clone()));

        assert_eq!(outcome, Cleanup::Kept, "fail safe, not 'nothing to do'");
        assert!(part.is_file(), "the file is untouched");
        let none = delete_stale_part(&job(None, None, canonical));
        assert_eq!(
            none,
            Cleanup::Nothing,
            "no recorded path means nothing to clean"
        );
    }
}
