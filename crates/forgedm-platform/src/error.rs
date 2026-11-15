//! Platform error type.
//!
//! Every Phase 1 variant is defined here (including the filesystem and Mark-of-the-Web ones
//! that slices S1.5 and S1.6 will produce) so downstream crates can match exhaustively
//! without waiting on those slices. Nothing in this crate panics; all failures are values.

use std::io;

/// Failure of a platform primitive.
#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    /// A Win32 API returned a failure code.
    #[error("{api} failed with Win32 error {code}")]
    Win32 {
        /// Name of the failing API.
        api: &'static str,
        /// The Win32 error code (`GetLastError` value).
        code: u32,
    },
    /// The caller supplied a value the primitive refuses to process.
    #[error("invalid input: {0}")]
    InvalidInput(&'static str),
    /// A standard library I/O failure.
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
    /// The destination of a no-replace move already exists.
    #[error("destination already exists")]
    AlreadyExists,
    /// A file is open elsewhere without sharing (finalize retries on this).
    #[error("file is locked by another process")]
    SharingViolation,
    /// The volume has no free space.
    #[error("disk full")]
    DiskFull,
    /// The path exceeds what Windows accepts.
    #[error("path too long")]
    PathTooLong,
    /// The destination is unavailable (volume removed, share offline).
    #[error("destination unavailable")]
    DestinationUnavailable,
    /// The file system lacks a needed capability or hit a limit (for example no sparse files).
    #[error("file system limit or missing capability: {0}")]
    FileSystemLimit(&'static str),
    /// A path resolved outside the directory it must stay inside (reparse point, junction).
    #[error("path escapes its directory")]
    PathEscape,
    /// An operation is not supported on this volume or Windows version.
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    /// Mark-of-the-Web: group policy refused the file.
    #[error("blocked by policy")]
    MotwBlockedByPolicy,
    /// Mark-of-the-Web: an antivirus provider refused or removed the file.
    #[error("blocked by antivirus")]
    MotwBlockedByAntivirus,
    /// Mark-of-the-Web could not be applied for another reason.
    #[error("mark-of-the-web failed: {api} error {code}")]
    MotwFailed {
        /// Name of the failing API.
        api: &'static str,
        /// The Win32 or HRESULT code, reinterpreted as unsigned.
        code: u32,
    },
}

impl PlatformError {
    /// Builds a [`PlatformError::Win32`] from an explicit code.
    #[must_use]
    pub const fn win32(api: &'static str, code: u32) -> Self {
        Self::Win32 { api, code }
    }

    /// Converts a `windows` crate error raised by `api` into a [`PlatformError::Win32`].
    ///
    /// `HRESULT_FROM_WIN32` values (`0x8007xxxx`) are unwrapped to the plain Win32 code so
    /// callers can compare against `ERROR_*` constants.
    #[must_use]
    pub fn from_windows(api: &'static str, err: &windows::core::Error) -> Self {
        Self::Win32 {
            api,
            code: unwrap_hresult(err.code().0),
        }
    }
}

/// Reinterprets an `HRESULT` as unsigned and unwraps `HRESULT_FROM_WIN32` facility codes.
#[must_use]
pub fn unwrap_hresult(hresult: i32) -> u32 {
    let raw = u32::from_ne_bytes(hresult.to_ne_bytes());
    if raw & 0xFFFF_0000 == 0x8007_0000 {
        raw & 0xFFFF
    } else {
        raw
    }
}

/// Builds a closure mapping a `windows` error to [`PlatformError::Win32`] for `api`.
pub(crate) fn win32_err(api: &'static str) -> impl Fn(windows::core::Error) -> PlatformError {
    move |e| PlatformError::from_windows(api, &e)
}
