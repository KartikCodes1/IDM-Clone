//! DPAPI protect and unprotect.
//!
//! `CryptProtectData` / `CryptUnprotectData` with the current-user scope only
//! (`CRYPTPROTECT_LOCAL_MACHINE` is never used), `CRYPTPROTECT_UI_FORBIDDEN`, no prompt
//! struct, and caller-supplied entropy that binds a blob to its context. DPAPI output is
//! released with `LocalFree`; recovered plaintext is wiped when dropped.
#![allow(unsafe_code)] // Crypt32 FFI; every block has a SAFETY comment.

use std::fmt;
use std::sync::atomic::{Ordering, compiler_fence};

use windows::Win32::Foundation::HLOCAL;
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows::core::{Owned, PCWSTR};

use crate::error::{PlatformError, win32_err};

/// Plaintext recovered from DPAPI. Wiped on drop; never printed.
pub struct SecretBytes(Vec<u8>);

impl SecretBytes {
    /// The plaintext bytes.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretBytes([redacted; {} bytes])", self.0.len())
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

/// Overwrites `buf` with zeros in a way the optimiser may not elide.
fn wipe(buf: &mut [u8]) {
    for byte in buf.iter_mut() {
        // SAFETY: `byte` is a valid, aligned, exclusive reference to a `u8`.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

fn blob_of(bytes: &[u8]) -> Result<CRYPT_INTEGER_BLOB, PlatformError> {
    let len =
        u32::try_from(bytes.len()).map_err(|_| PlatformError::InvalidInput("value too large"))?;
    Ok(CRYPT_INTEGER_BLOB {
        cbData: len,
        pbData: bytes.as_ptr().cast_mut(),
    })
}

/// Copies a DPAPI-allocated output blob, then releases it with `LocalFree`.
///
/// When `wipe_source` is set the DPAPI-owned memory is zeroed before it is freed.
fn take_output(out: &CRYPT_INTEGER_BLOB, wipe_source: bool) -> Result<Vec<u8>, PlatformError> {
    if out.pbData.is_null() {
        return Err(PlatformError::InvalidInput("DPAPI returned no data"));
    }
    // SAFETY: DPAPI allocated `pbData` with LocalAlloc; the guard frees it on every path.
    let _guard = unsafe { Owned::new(HLOCAL(out.pbData.cast())) };
    let len = usize::try_from(out.cbData).map_err(|_| PlatformError::InvalidInput("length"))?;
    // SAFETY: DPAPI guarantees `pbData` points to `cbData` readable and writable bytes.
    let region = unsafe { std::slice::from_raw_parts_mut(out.pbData, len) };
    let copy = region.to_vec();
    if wipe_source {
        wipe(region);
    }
    Ok(copy)
}

/// Encrypts `plaintext` for the current user, bound to `entropy`.
///
/// # Errors
/// [`PlatformError::InvalidInput`] for empty `entropy` or oversize values,
/// [`PlatformError::Win32`] if `CryptProtectData` fails.
pub fn protect(plaintext: &[u8], entropy: &[u8]) -> Result<Vec<u8>, PlatformError> {
    if entropy.is_empty() {
        return Err(PlatformError::InvalidInput("entropy must not be empty"));
    }
    let input = blob_of(plaintext)?;
    let entropy_blob = blob_of(entropy)?;
    let mut out = CRYPT_INTEGER_BLOB::default();
    // SAFETY: `input` and `entropy_blob` point into slices that outlive the call; no prompt
    // struct or reserved pointer is passed; `out` is a valid out pointer.
    unsafe {
        CryptProtectData(
            &raw const input,
            PCWSTR::null(),
            Some(&raw const entropy_blob),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &raw mut out,
        )
    }
    .map_err(win32_err("CryptProtectData"))?;
    take_output(&out, false)
}

/// Decrypts a blob produced by [`protect`] with the same `entropy`.
///
/// # Errors
/// [`PlatformError::InvalidInput`] for empty `blob` or `entropy`,
/// [`PlatformError::Win32`] when the blob is corrupt, the entropy differs, or it belongs to
/// another user.
pub fn unprotect(blob: &[u8], entropy: &[u8]) -> Result<SecretBytes, PlatformError> {
    if entropy.is_empty() {
        return Err(PlatformError::InvalidInput("entropy must not be empty"));
    }
    if blob.is_empty() {
        return Err(PlatformError::InvalidInput("blob must not be empty"));
    }
    let input = blob_of(blob)?;
    let entropy_blob = blob_of(entropy)?;
    let mut out = CRYPT_INTEGER_BLOB::default();
    // SAFETY: `input` and `entropy_blob` point into slices that outlive the call; no
    // description, prompt struct or reserved pointer is requested; `out` is a valid out pointer.
    unsafe {
        CryptUnprotectData(
            &raw const input,
            None,
            Some(&raw const entropy_blob),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &raw mut out,
        )
    }
    .map_err(win32_err("CryptUnprotectData"))?;
    take_output(&out, true).map(SecretBytes)
}
