//! Verifies a file against the generator and reports the first mismatching offset.

use crate::content;
use std::io::Read;
use std::path::Path;

const VERIFY_CHUNK: usize = 64 * 1024;

/// Why verification failed.
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    /// The file differs from the generated content at `offset` (a length difference reports the
    /// shorter length as the offset).
    #[error("content mismatch at byte offset {offset}")]
    Mismatch {
        /// Zero-based offset of the first differing byte.
        offset: u64,
    },
    /// The file could not be read.
    #[error("cannot read file: {0}")]
    Io(#[from] std::io::Error),
}

/// Streams `path` against the generator for `seed`/`size`.
///
/// # Errors
/// [`VerifyError::Mismatch`] with the first differing offset (including truncation or extra
/// trailing bytes), or [`VerifyError::Io`] when the file cannot be read.
pub fn file_matches(path: &Path, seed: u64, size: u64) -> Result<(), VerifyError> {
    let mut file = std::fs::File::open(path)?;
    let mut actual = vec![0u8; VERIFY_CHUNK];
    let mut expected = vec![0u8; VERIFY_CHUNK];
    let mut pos = 0_u64;
    while pos < size {
        let want = usize::try_from(size - pos).map_or(VERIFY_CHUNK, |left| left.min(VERIFY_CHUNK));
        let got = read_full(&mut file, &mut actual[..want])?;
        content::fill(seed, pos, &mut expected[..got]);
        if let Some(i) = first_diff(&actual[..got], &expected[..got]) {
            return Err(VerifyError::Mismatch {
                offset: pos + widen(i),
            });
        }
        pos += widen(got);
        if got < want {
            return Err(VerifyError::Mismatch { offset: pos });
        }
    }
    // Any byte beyond `size` is a mismatch at `size`.
    if read_full(&mut file, &mut actual[..1])? > 0 {
        return Err(VerifyError::Mismatch { offset: size });
    }
    Ok(())
}

/// Streams the first `len` bytes of `path` against the generator for `seed`. Bytes beyond
/// `len` are ignored: this checks a committed prefix of a partial file.
///
/// # Errors
/// [`VerifyError::Mismatch`] with the first differing offset (a file shorter than `len` reports
/// its length), or [`VerifyError::Io`] when the file cannot be read.
pub fn prefix_matches(path: &Path, seed: u64, len: u64) -> Result<(), VerifyError> {
    let mut file = std::fs::File::open(path)?;
    let mut actual = vec![0u8; VERIFY_CHUNK];
    let mut expected = vec![0u8; VERIFY_CHUNK];
    let mut pos = 0_u64;
    while pos < len {
        let want = usize::try_from(len - pos).map_or(VERIFY_CHUNK, |left| left.min(VERIFY_CHUNK));
        let got = read_full(&mut file, &mut actual[..want])?;
        content::fill(seed, pos, &mut expected[..got]);
        if let Some(i) = first_diff(&actual[..got], &expected[..got]) {
            return Err(VerifyError::Mismatch {
                offset: pos + widen(i),
            });
        }
        pos += widen(got);
        if got < want {
            return Err(VerifyError::Mismatch { offset: pos });
        }
    }
    Ok(())
}

fn widen(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

fn first_diff(a: &[u8], b: &[u8]) -> Option<usize> {
    a.iter().zip(b).position(|(x, y)| x != y)
}

/// Reads until `buf` is full or EOF; returns the byte count.
fn read_full(file: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        match file.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(total)
}
