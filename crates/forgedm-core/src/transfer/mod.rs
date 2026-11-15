//! Transfers: how a response body becomes a verified file on disk.
//!
//! S1.7 has one transfer, the plain single stream ([`single`]). Segmented transfer joins in
//! S1.11.

use std::path::Path;

use forgedm_platform::fs::FileId;
use forgedm_protocol::DownloadErrorCode;
use tokio_util::sync::CancellationToken;

use crate::engine::StopReason;
use crate::error::{Failure, TaskStop, stop_from_io};
use crate::hash::{HashError, sha256_file};

pub mod single;

/// The `part_file_id` bytes stored for a staged file: volume serial (little endian) then
/// the 128-bit file id.
#[must_use]
pub fn encode_file_id(id: &FileId) -> Vec<u8> {
    let mut bytes = id.volume_serial.to_le_bytes().to_vec();
    bytes.extend_from_slice(&id.id);
    bytes
}

/// What verification of a staged file found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    /// SHA-256 of the file on disk, lowercase hex.
    pub sha256: String,
    /// Bytes on disk.
    pub length: u64,
    /// Whether the length equals a known total.
    pub length_verified: bool,
    /// Whether the digest equals the expected checksum; `None` when none was supplied.
    pub checksum_matches: Option<bool>,
}

/// Verifies the staged file (blocking): streams it from disk through SHA-256 and compares
/// the length with `total` and the digest with `expected_sha256`.
///
/// # Errors
/// `size_mismatch` when a known total differs from the length on disk; a disk error when
/// the file cannot be read; [`TaskStop::Stopped`] when `cancel` fires. A checksum mismatch
/// is reported in [`Verified::checksum_matches`], not as an error, so the caller can record
/// the actual digest.
pub fn verify_part(
    part: &Path,
    total: Option<u64>,
    expected_sha256: Option<&str>,
    cancel: &CancellationToken,
    reason: impl Fn() -> StopReason,
) -> Result<Verified, TaskStop> {
    let hashed = sha256_file(part, cancel).map_err(|err| match err {
        HashError::Io(io) => stop_from_io(&io),
        HashError::Cancelled => TaskStop::Stopped(reason()),
    })?;
    if total.is_some_and(|t| t != hashed.length) {
        return Err(Failure::new(DownloadErrorCode::SizeMismatch).into());
    }
    let checksum_matches =
        expected_sha256.map(|expected| expected.eq_ignore_ascii_case(&hashed.sha256));
    Ok(Verified {
        length_verified: total.is_some(),
        length: hashed.length,
        sha256: hashed.sha256,
        checksum_matches,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn staged(data: &[u8]) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(data).unwrap();
        file.flush().unwrap();
        file
    }

    #[test]
    fn matching_total_and_checksum_verify() {
        let file = staged(b"abc");
        let token = CancellationToken::new();
        let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

        let verified = verify_part(file.path(), Some(3), Some(expected), &token, || {
            StopReason::Cancel
        })
        .unwrap();

        assert!(verified.length_verified);
        assert_eq!(verified.checksum_matches, Some(true));
    }

    #[test]
    fn wrong_length_is_size_mismatch_and_unknown_length_is_unverified() {
        let file = staged(b"abc");
        let token = CancellationToken::new();

        let wrong = verify_part(file.path(), Some(4), None, &token, || StopReason::Cancel);
        assert_eq!(
            wrong,
            Err(TaskStop::Fail(Failure::new(
                DownloadErrorCode::SizeMismatch
            )))
        );
        let unknown = verify_part(file.path(), None, None, &token, || StopReason::Cancel).unwrap();
        assert!(!unknown.length_verified);
        assert_eq!(unknown.checksum_matches, None);
    }

    #[test]
    fn a_wrong_checksum_is_reported_not_raised() {
        let file = staged(b"abc");
        let token = CancellationToken::new();
        let wrong = "0".repeat(64);

        let verified = verify_part(file.path(), Some(3), Some(&wrong), &token, || {
            StopReason::Cancel
        })
        .unwrap();

        assert_eq!(verified.checksum_matches, Some(false));
    }

    #[test]
    fn file_ids_encode_serial_then_id() {
        let id = FileId {
            volume_serial: 0x0102_0304_0506_0708,
            id: [9; 16],
        };
        let bytes = encode_file_id(&id);
        assert_eq!(bytes.len(), 24);
        assert_eq!(&bytes[..8], &0x0102_0304_0506_0708u64.to_le_bytes());
        assert_eq!(&bytes[8..], &[9; 16]);
    }
}
