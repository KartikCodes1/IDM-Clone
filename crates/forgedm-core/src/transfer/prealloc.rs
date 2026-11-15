//! Preallocation policy of a segmented staging file (ARCHITECTURE 5.2).
//!
//! Workers write at far-apart offsets, so on NTFS the file is made sparse right after it is
//! created: otherwise the first write past the valid data length makes Windows zero-fill
//! everything before it (the VDL stall). No `set_len` is done and `SetFileValidData` is never
//! used. If the sparse flag cannot be set the failure is logged and the download continues.
//! FAT32 cannot hold a file of 4 GiB or more, so that case fails before any byte is written.

use std::fs::File;
use std::os::windows::fs::MetadataExt;

use forgedm_platform::PlatformError;
use forgedm_platform::fs::{
    VolumeInfo, ensure_within, file_id, open_part_file, set_sparse, volume_info,
};
use forgedm_protocol::DownloadErrorCode;

use crate::error::{Failure, TaskStop};
use crate::ports::StoredJob;
use crate::resume::PartInvalid;
use crate::transfer::encode_file_id;

/// Largest file FAT32 can hold plus one: a total at or above this cannot be stored there.
pub const FAT32_FILE_LIMIT: u64 = 4 * 1024 * 1024 * 1024;
/// `FILE_ATTRIBUTE_SPARSE_FILE`.
const ATTRIBUTE_SPARSE: u32 = 0x200;

/// What to do with a fresh segmented staging file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preallocation {
    /// Whether to set the sparse flag.
    pub sparse: bool,
}

/// Decides the preallocation for a file of `total` bytes on `volume`.
///
/// # Errors
/// `file_system_limit` for a FAT32 volume and `total >= 4 GiB`.
pub fn plan_preallocation(volume: &VolumeInfo, total: u64) -> Result<Preallocation, TaskStop> {
    if volume.fs_name.eq_ignore_ascii_case("FAT32") && total >= FAT32_FILE_LIMIT {
        return Err(Failure::new(DownloadErrorCode::FileSystemLimit).into());
    }
    Ok(Preallocation {
        sparse: volume.supports_sparse,
    })
}

/// Applies [`plan_preallocation`] to a just-created staging file. `query` reads the volume
/// facts (injected so tests can pretend to be on FAT32); a failed query is logged and the
/// file is left as it is. Returns whether the file was made sparse.
///
/// # Errors
/// `file_system_limit`, before anything was written.
pub fn prepare_part(
    file: &File,
    total: u64,
    query: impl FnOnce(&File) -> Result<VolumeInfo, PlatformError>,
) -> Result<bool, TaskStop> {
    let volume = match query(file) {
        Ok(volume) => volume,
        Err(err) => {
            tracing::warn!(error = %err, "volume facts unavailable; no sparse preallocation");
            return Ok(false);
        }
    };
    if !plan_preallocation(&volume, total)?.sparse {
        return Ok(false);
    }
    match set_sparse(file, true) {
        Ok(()) => Ok(true),
        Err(err) => {
            tracing::warn!(error = %err, "could not make the staging file sparse; continuing");
            Ok(false)
        }
    }
}

/// [`prepare_part`] with the real volume query.
///
/// # Errors
/// As [`prepare_part`].
pub fn prepare_part_on_volume(file: &File, total: u64) -> Result<bool, TaskStop> {
    prepare_part(file, total, volume_info)
}

/// Opens the staging file of a segmented download for resume: the recorded path, not a
/// reparse point, the recorded file id, inside the target directory, and at least
/// `min_len` bytes long (the largest committed offset). Unlike a single-stream resume the
/// file is *not* truncated: other segments may legitimately lie beyond the first gap, and
/// everything outside `[start, committed)` of a segment is simply rewritten.
///
/// Blocking: call from `spawn_blocking`.
///
/// # Errors
/// [`PartInvalid`] for every way the file fails those checks.
pub fn open_part_for_segments(job: &StoredJob, min_len: u64) -> Result<File, PartInvalid> {
    let (Some(path), Some(recorded)) = (job.part_path.as_deref(), job.part_file_id.as_deref())
    else {
        return Err(PartInvalid::NotRecorded);
    };
    let file = open_part_file(path).map_err(PartInvalid::Unavailable)?;
    if !file_id(&file).is_ok_and(|id| encode_file_id(&id) == recorded) {
        return Err(PartInvalid::Replaced);
    }
    if ensure_within(&file, &job.target_dir).is_err() {
        return Err(PartInvalid::Outside);
    }
    let length = file
        .metadata()
        .map_err(|err| PartInvalid::Unavailable(PlatformError::Io(err)))?
        .len();
    if length < min_len {
        return Err(PartInvalid::Short);
    }
    Ok(file)
}

/// Clears the sparse flag once the file is complete. Best effort: a failure is logged.
pub fn clear_sparse(file: &File) {
    if let Err(err) = set_sparse(file, false) {
        tracing::warn!(error = %err, "could not clear the sparse flag of a finished file");
    }
}

/// Whether `file` currently has the sparse attribute.
///
/// # Errors
/// The I/O error from reading the file's attributes.
pub fn is_sparse(file: &File) -> std::io::Result<bool> {
    Ok(file.metadata()?.file_attributes() & ATTRIBUTE_SPARSE != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volume(name: &str, sparse: bool) -> VolumeInfo {
        VolumeInfo {
            fs_name: name.to_owned(),
            supports_sparse: sparse,
            supports_streams: true,
        }
    }

    #[test]
    fn fat32_refuses_files_of_4_gib_and_more_but_not_smaller_ones() {
        let fat = volume("FAT32", false);

        assert!(plan_preallocation(&fat, FAT32_FILE_LIMIT - 1).is_ok());
        assert_eq!(
            plan_preallocation(&fat, FAT32_FILE_LIMIT),
            Err(TaskStop::Fail(Failure::new(
                DownloadErrorCode::FileSystemLimit
            )))
        );
    }

    #[test]
    fn exfat_and_ntfs_take_big_files_and_only_sparse_volumes_go_sparse() {
        let big = 8 * FAT32_FILE_LIMIT;

        assert_eq!(
            plan_preallocation(&volume("exFAT", false), big),
            Ok(Preallocation { sparse: false })
        );
        assert_eq!(
            plan_preallocation(&volume("NTFS", true), big),
            Ok(Preallocation { sparse: true })
        );
    }
}
