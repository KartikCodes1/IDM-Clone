//! Known folder lookup.
//!
//! Resolved with `SHGetKnownFolderPath`, never environment variables (spoofable).
#![allow(unsafe_code)] // Shell FFI; every block has a SAFETY comment.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{
    FOLDERID_Downloads, FOLDERID_LocalAppData, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
};
use windows::core::GUID;

use crate::error::{PlatformError, win32_err};

/// The user's Downloads folder (`FOLDERID_Downloads`).
///
/// # Errors
/// [`PlatformError`] if the shell cannot resolve it or returns a non-absolute path.
pub fn downloads() -> Result<PathBuf, PlatformError> {
    known_folder(&FOLDERID_Downloads)
}

/// `%LOCALAPPDATA%` as the shell reports it (`FOLDERID_LocalAppData`).
///
/// # Errors
/// [`PlatformError`] if the shell cannot resolve it or returns a non-absolute path.
pub fn local_app_data() -> Result<PathBuf, PlatformError> {
    known_folder(&FOLDERID_LocalAppData)
}

fn known_folder(id: &GUID) -> Result<PathBuf, PlatformError> {
    // SAFETY: `id` is a valid GUID reference; a default token means the current user.
    let pwstr = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) }
        .map_err(win32_err("SHGetKnownFolderPath"))?;
    // SAFETY: on success `pwstr` is a NUL-terminated wide string; it is only read here.
    let units = unsafe { pwstr.as_wide() };
    let path = PathBuf::from(OsString::from_wide(units));
    // SAFETY: the string was allocated by the shell with CoTaskMemAlloc and is not used
    // after this point (`path` holds an independent copy).
    unsafe { CoTaskMemFree(Some(pwstr.0.cast_const().cast())) };
    if !path.is_absolute() {
        return Err(PlatformError::InvalidInput(
            "known folder path is not absolute",
        ));
    }
    Ok(path)
}
