//! Security identifier wrappers.
//!
//! [`Sid`] owns a copy of the SID bytes in 8-byte-aligned storage, so it carries no raw
//! pointers and is `Send + Sync`. SID strings are validated before they can reach an SDDL
//! string (see [`validate_sid_string`]).
#![allow(unsafe_code)] // Win32 token and SID FFI; every block carries a SAFETY comment.

use std::fmt;

use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, HANDLE, HLOCAL};
use windows::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSidToSidW};
use windows::Win32::Security::{
    CopySid, EqualSid, GetLengthSid, GetTokenInformation, IsValidSid, PSID, TOKEN_QUERY,
    TOKEN_USER, TokenUser,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{Owned, PWSTR};

use crate::error::{PlatformError, win32_err};

/// Longest SID string accepted (a SID has at most 15 sub-authorities).
const MAX_SID_STRING_LEN: usize = 200;

/// Checks `s` against `^S-1-\d+(-\d+)+$` (ASCII digits only, no trailing newline).
///
/// This is the gate that keeps attacker-influenced text out of SDDL.
///
/// # Errors
/// [`PlatformError::InvalidInput`] when `s` does not match.
pub fn validate_sid_string(s: &str) -> Result<(), PlatformError> {
    const BAD: PlatformError = PlatformError::InvalidInput("malformed SID string");
    if s.len() > MAX_SID_STRING_LEN {
        return Err(BAD);
    }
    let rest = s.strip_prefix("S-1-").ok_or(BAD)?;
    let mut count = 0usize;
    for part in rest.split('-') {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return Err(BAD);
        }
        count += 1;
    }
    if count < 2 {
        return Err(BAD);
    }
    Ok(())
}

/// An owned security identifier.
#[derive(Clone)]
pub struct Sid {
    buf: Vec<u64>,
}

impl Sid {
    /// The SID of the user this process runs as (from the process token).
    ///
    /// # Errors
    /// [`PlatformError::Win32`] when the token cannot be opened or read.
    pub fn current_user() -> Result<Self, PlatformError> {
        let mut raw = HANDLE::default();
        // SAFETY: GetCurrentProcess returns a pseudo handle that needs no close; `raw` is a
        // valid out pointer for the token handle.
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut raw) }
            .map_err(win32_err("OpenProcessToken"))?;
        // SAFETY: OpenProcessToken succeeded, so `raw` is a token handle we own.
        let token = unsafe { Owned::new(raw) };
        token_user_sid(*token)
    }

    /// Parses a SID string such as `S-1-5-18`.
    ///
    /// # Errors
    /// [`PlatformError::InvalidInput`] for a malformed string, [`PlatformError::Win32`] if
    /// the system rejects it.
    pub fn parse(s: &str) -> Result<Self, PlatformError> {
        validate_sid_string(s)?;
        let wide = crate::wide::to_wide_nul(s)?;
        let mut psid = PSID::default();
        // SAFETY: `wide` is NUL-terminated and outlives the call; `psid` is a valid out pointer.
        unsafe { ConvertStringSidToSidW(windows::core::PCWSTR(wide.as_ptr()), &raw mut psid) }
            .map_err(win32_err("ConvertStringSidToSidW"))?;
        // SAFETY: the SID was allocated with LocalAlloc by the call above; the guard frees it.
        let _guard = unsafe { Owned::new(HLOCAL(psid.0)) };
        // SAFETY: `psid` points to a valid SID that stays alive until `_guard` drops.
        unsafe { Self::copy_from(psid) }
    }

    /// Copies the SID at `psid` into owned storage.
    ///
    /// # Safety
    /// `psid` must be null or point to a readable SID.
    pub(crate) unsafe fn copy_from(psid: PSID) -> Result<Self, PlatformError> {
        if psid.0.is_null() {
            return Err(PlatformError::InvalidInput("null SID"));
        }
        // SAFETY: the caller guarantees `psid` points to a readable SID.
        if !unsafe { IsValidSid(psid) }.as_bool() {
            return Err(PlatformError::InvalidInput("invalid SID"));
        }
        // SAFETY: `psid` was just validated.
        let len = unsafe { GetLengthSid(psid) };
        let bytes = usize::try_from(len).map_err(|_| PlatformError::InvalidInput("SID length"))?;
        let mut buf = vec![0u64; bytes.div_ceil(8)];
        let dest = PSID(buf.as_mut_ptr().cast());
        // SAFETY: `dest` has at least `len` writable bytes and `psid` is valid.
        unsafe { CopySid(len, dest, psid) }.map_err(win32_err("CopySid"))?;
        Ok(Self { buf })
    }

    /// Pointer to the SID for passing to Win32 calls; valid while `self` is alive.
    #[must_use]
    pub fn as_psid(&self) -> PSID {
        PSID(self.buf.as_ptr().cast_mut().cast())
    }

    /// The canonical `S-1-...` string form.
    ///
    /// # Errors
    /// [`PlatformError::Win32`] if the conversion fails.
    pub fn to_sid_string(&self) -> Result<String, PlatformError> {
        let mut out = PWSTR::null();
        // SAFETY: `as_psid` points to the valid SID owned by `self`; `out` is a valid out pointer.
        unsafe { ConvertSidToStringSidW(self.as_psid(), &raw mut out) }
            .map_err(win32_err("ConvertSidToStringSidW"))?;
        // SAFETY: the string was allocated with LocalAlloc by the call above; the guard frees it.
        let _guard = unsafe { Owned::new(HLOCAL(out.0.cast())) };
        // SAFETY: `out` is a NUL-terminated wide string that lives until `_guard` drops.
        unsafe { out.to_string() }
            .map_err(|_| PlatformError::InvalidInput("SID string is not valid UTF-16"))
    }
}

impl PartialEq for Sid {
    fn eq(&self, other: &Self) -> bool {
        // SAFETY: both pointers reference valid SIDs owned by their `Sid`. In windows 0.62 a
        // "not equal" result is an `Err` (code 0), so only `is_ok()` means equal.
        unsafe { EqualSid(self.as_psid(), other.as_psid()) }.is_ok()
    }
}

impl Eq for Sid {}

impl fmt::Debug for Sid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.to_sid_string() {
            Ok(s) => write!(f, "Sid({s})"),
            Err(_) => f.write_str("Sid(<unprintable>)"),
        }
    }
}

/// Reads the `TokenUser` SID from an open token handle.
pub(crate) fn token_user_sid(token: HANDLE) -> Result<Sid, PlatformError> {
    let mut needed = 0u32;
    // SAFETY: a null buffer of length 0 is the documented size query; `needed` is a valid out pointer.
    let probe = unsafe { GetTokenInformation(token, TokenUser, None, 0, &raw mut needed) };
    match probe {
        Err(e) if e.code() == ERROR_INSUFFICIENT_BUFFER.to_hresult() => {}
        Err(e) => return Err(PlatformError::from_windows("GetTokenInformation", &e)),
        Ok(()) => return Err(PlatformError::InvalidInput("empty token information")),
    }
    let bytes = usize::try_from(needed).map_err(|_| PlatformError::InvalidInput("token size"))?;
    // u64 elements guarantee the 8-byte alignment TOKEN_USER requires.
    let mut buf = vec![0u64; bytes.div_ceil(8)];
    // SAFETY: `buf` provides at least `needed` writable, 8-aligned bytes.
    unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            needed,
            &raw mut needed,
        )
    }
    .map_err(win32_err("GetTokenInformation"))?;
    // SAFETY: the successful call filled `buf` with a TOKEN_USER; alignment is satisfied above.
    let user = unsafe { &*buf.as_ptr().cast::<TOKEN_USER>() };
    // SAFETY: the SID pointer lies inside `buf`, which is alive for this call.
    unsafe { Sid::copy_from(user.User.Sid) }
}
