//! Pipe peer identity.
//!
//! Resolves a process id (from `GetNamedPipeClientProcessId` or `GetNamedPipeServerProcessId`)
//! to its user SID and full image path, and decides whether the peer is acceptable. Every
//! failure rejects the peer (fail closed).
#![allow(unsafe_code)] // Win32 process and token FFI; every block has a SAFETY comment.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, BorrowedHandle};
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Security::TOKEN_QUERY;
use windows::Win32::System::Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId};
use windows::Win32::System::Threading::{
    OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows::core::{Owned, PWSTR};

use crate::error::{PlatformError, win32_err};
use crate::sid::{Sid, token_user_sid};

/// Capacity (in UTF-16 units) for the image path buffer: the NTFS long-path maximum.
const IMAGE_PATH_CAPACITY: usize = 32_768;

/// Who is on the other end of a pipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerIdentity {
    /// The peer process's token user.
    pub sid: Sid,
    /// The peer's full image path as reported by the kernel.
    pub image_path: PathBuf,
}

/// Why a peer was rejected.
#[derive(Debug, thiserror::Error)]
pub enum PeerError {
    /// The peer runs as a different user.
    #[error("peer runs as a different user")]
    SidMismatch,
    /// The peer's executable is not the expected one.
    #[error("peer image {actual:?} is not the expected image {expected:?}")]
    ImageMismatch {
        /// The image path found.
        actual: PathBuf,
        /// The image path required.
        expected: PathBuf,
    },
    /// The peer could not be inspected; treated as a rejection.
    #[error("peer could not be inspected: {0}")]
    Platform(#[from] PlatformError),
}

/// Process id of the client connected to a server pipe handle.
///
/// # Errors
/// [`PlatformError::Win32`] if the handle is not a connected server pipe.
pub fn pipe_client_pid(pipe: BorrowedHandle<'_>) -> Result<u32, PlatformError> {
    let mut pid = 0u32;
    // SAFETY: `pipe` is a live handle borrowed for the call; `pid` is a valid out pointer.
    unsafe { GetNamedPipeClientProcessId(HANDLE(pipe.as_raw_handle()), &raw mut pid) }
        .map_err(win32_err("GetNamedPipeClientProcessId"))?;
    Ok(pid)
}

/// Process id of the server end of a client pipe handle.
///
/// # Errors
/// [`PlatformError::Win32`] if the handle is not a client pipe.
pub fn pipe_server_pid(pipe: BorrowedHandle<'_>) -> Result<u32, PlatformError> {
    let mut pid = 0u32;
    // SAFETY: `pipe` is a live handle borrowed for the call; `pid` is a valid out pointer.
    unsafe { GetNamedPipeServerProcessId(HANDLE(pipe.as_raw_handle()), &raw mut pid) }
        .map_err(win32_err("GetNamedPipeServerProcessId"))?;
    Ok(pid)
}

/// Resolves `pid` to its user SID and full image path.
///
/// Opens the process with `PROCESS_QUERY_LIMITED_INFORMATION` only. Any failure (access
/// denied for protected or other-user processes, exited process, bad pid) is an error.
///
/// # Errors
/// [`PlatformError::Win32`] when the process, its token or its image name is unreadable.
pub fn peer_identity(pid: u32) -> Result<PeerIdentity, PlatformError> {
    // SAFETY: plain OpenProcess call; the result is checked and owned by the guard below.
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .map_err(win32_err("OpenProcess"))?;
    // SAFETY: OpenProcess succeeded, so `raw` is a process handle we own.
    let process = unsafe { Owned::new(raw) };

    let mut token_raw = HANDLE::default();
    // SAFETY: `process` is a live process handle; `token_raw` is a valid out pointer.
    unsafe { OpenProcessToken(*process, TOKEN_QUERY, &raw mut token_raw) }
        .map_err(win32_err("OpenProcessToken"))?;
    // SAFETY: OpenProcessToken succeeded, so `token_raw` is a token handle we own.
    let token = unsafe { Owned::new(token_raw) };
    let sid = token_user_sid(*token)?;

    Ok(PeerIdentity {
        sid,
        image_path: image_path(*process)?,
    })
}

fn image_path(process: HANDLE) -> Result<PathBuf, PlatformError> {
    let mut buf = vec![0u16; IMAGE_PATH_CAPACITY];
    let mut len = u32::try_from(buf.len()).map_err(|_| PlatformError::InvalidInput("buffer"))?;
    // SAFETY: `buf` has `len` writable UTF-16 units; `len` is a valid in/out pointer.
    unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &raw mut len,
        )
    }
    .map_err(win32_err("QueryFullProcessImageNameW"))?;
    let written = usize::try_from(len).map_err(|_| PlatformError::InvalidInput("length"))?;
    let units = buf
        .get(..written)
        .ok_or(PlatformError::InvalidInput("image path length"))?;
    Ok(PathBuf::from(OsString::from_wide(units)))
}

/// Accepts the peer at `pid` only if it runs as `expected_sid` and, when given, its image
/// equals `expected_image` (canonicalised, case-insensitive).
///
/// # Errors
/// [`PeerError::SidMismatch`], [`PeerError::ImageMismatch`], or [`PeerError::Platform`] when
/// the peer cannot be inspected (fail closed).
pub fn verify_peer(
    pid: u32,
    expected_sid: &Sid,
    expected_image: Option<&Path>,
) -> Result<PeerIdentity, PeerError> {
    let identity = peer_identity(pid)?;
    if identity.sid != *expected_sid {
        return Err(PeerError::SidMismatch);
    }
    if let Some(expected) = expected_image
        && comparable(&identity.image_path) != comparable(expected)
    {
        return Err(PeerError::ImageMismatch {
            actual: identity.image_path,
            expected: expected.to_path_buf(),
        });
    }
    Ok(identity)
}

/// Canonicalises (when the file exists), lowercases and strips the verbatim prefix.
fn comparable(path: &Path) -> String {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let lower = canonical.to_string_lossy().to_lowercase();
    if let Some(unc) = lower.strip_prefix(r"\\?\unc\") {
        return format!(r"\\{unc}");
    }
    lower
        .strip_prefix(r"\\?\")
        .map_or_else(|| lower.clone(), str::to_owned)
}
