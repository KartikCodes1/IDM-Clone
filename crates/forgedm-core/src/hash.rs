//! Streaming SHA-256 of a file on disk (ARCHITECTURE 5.5 step 3, ADR-015).
//!
//! The hash is always computed by reading the staged file back in [`HASH_READ`] pieces, so
//! it is correct across pause, kill and resume and verifies what is actually on disk.
//! Runs on a blocking thread; the cancellation token is checked between reads.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// Read size (A.9 `HASH_READ`).
pub const HASH_READ: usize = 1024 * 1024;

/// A computed digest and the number of bytes it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashResult {
    /// SHA-256, lowercase hex.
    pub sha256: String,
    /// Bytes read.
    pub length: u64,
}

/// Why hashing stopped early.
#[derive(Debug, Error)]
pub enum HashError {
    /// The file could not be opened or read.
    #[error("cannot read the file")]
    Io(#[from] io::Error),
    /// The token was cancelled between reads.
    #[error("hashing cancelled")]
    Cancelled,
}

/// Streams `path` through SHA-256.
///
/// # Errors
/// [`HashError::Io`] on any read failure, [`HashError::Cancelled`] when `cancel` fires.
pub fn sha256_file(path: &Path, cancel: &CancellationToken) -> Result<HashResult, HashError> {
    let file = File::open(path)?;
    sha256_reader(file, cancel)
}

/// Streams `reader` through SHA-256 in [`HASH_READ`] pieces.
///
/// # Errors
/// As [`sha256_file`].
pub fn sha256_reader(
    mut reader: impl Read,
    cancel: &CancellationToken,
) -> Result<HashResult, HashError> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; HASH_READ];
    let mut length = 0u64;
    loop {
        if cancel.is_cancelled() {
            return Err(HashError::Cancelled);
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err.into()),
        };
        hasher.update(&buf[..n]);
        length += n as u64;
    }
    Ok(HashResult {
        sha256: hex::encode(hasher.finalize().as_slice()),
        length,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        let cancel = CancellationToken::new();
        let empty = sha256_reader(io::empty(), &cancel).unwrap();
        assert_eq!(
            empty.sha256,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(empty.length, 0);
        let abc = sha256_reader(&b"abc"[..], &cancel).unwrap();
        assert_eq!(
            abc.sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(abc.length, 3);
    }

    #[test]
    fn multi_piece_input_matches_one_shot() {
        let cancel = CancellationToken::new();
        let data = vec![7u8; HASH_READ * 2 + 5];
        let streamed = sha256_reader(&data[..], &cancel).unwrap();
        let direct = hex::encode(Sha256::digest(&data).as_slice());
        assert_eq!(streamed.sha256, direct);
        assert_eq!(streamed.length, data.len() as u64);
    }

    #[test]
    fn a_cancelled_token_stops_before_reading() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(matches!(
            sha256_reader(&b"abc"[..], &cancel),
            Err(HashError::Cancelled)
        ));
    }
}
