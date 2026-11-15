//! Named pipe instances with DACL.
//!
//! The pipe is created with an explicit protected DACL (NETWORK denied first, only the
//! current user allowed, medium integrity label with no-write-up and no-read-up). A NULL
//! descriptor would grant Everyone read access and is never used.
//!
//! The returned handle is overlapped, byte-mode, remote-client-rejecting and limited to
//! [`PIPE_MAX_INSTANCES`] instances. The IPC crate adopts it into its async runtime.
#![allow(unsafe_code)] // Win32 pipe and security descriptor FFI; every block has a SAFETY comment.

use std::os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};

use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_SUCCESS, GetLastError, HANDLE, HLOCAL, INVALID_HANDLE_VALUE,
};
use windows::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SE_KERNEL_OBJECT,
};
use windows::Win32::Security::{
    DACL_SECURITY_INFORMATION, LABEL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    SECURITY_ATTRIBUTES,
};
use windows::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows::core::{Owned, PCWSTR, PWSTR};

use crate::error::{PlatformError, win32_err};
use crate::sid::{Sid, validate_sid_string};
use crate::wide::to_wide_nul;

/// Maximum pipe instances: `MAX_ACTIVE_CONNECTIONS (16) + 1` (PHASE_0_1_PLAN A.9).
pub const PIPE_MAX_INSTANCES: u32 = 17;
/// In and out buffer size in bytes (64 KiB, PHASE_0_1_PLAN A.9).
pub const PIPE_BUFFER_SIZE: u32 = 65_536;

const PIPE_NAMESPACE: &str = r"\\.\pipe\";
/// `SDDL_REVISION_1`.
const SDDL_REVISION_1: u32 = 1;

/// Failure to create a pipe instance.
#[derive(Debug, thiserror::Error)]
pub enum PipeError {
    /// `first_pipe_instance` was requested but the name already exists, or the DACL denies
    /// us the existing pipe: the name is taken (possibly squatted).
    #[error("pipe name is already in use")]
    NameInUse,
    /// Any other failure.
    #[error(transparent)]
    Platform(#[from] PlatformError),
}

/// Builds the pipe SDDL for `user_sid` after validating it.
///
/// Result: `D:P(D;;GA;;;NU)(A;;GA;;;<sid>)S:(ML;;NWNR;;;ME)`.
///
/// # Errors
/// [`PlatformError::InvalidInput`] when `user_sid` is not a well-formed SID string.
pub fn pipe_sddl(user_sid: &str) -> Result<String, PlatformError> {
    validate_sid_string(user_sid)?;
    Ok(format!(
        "D:P(D;;GA;;;NU)(A;;GA;;;{user_sid})S:(ML;;NWNR;;;ME)"
    ))
}

/// A security descriptor for pipe instances. Keep it alive for the server's lifetime.
pub struct PipeSecurity {
    descriptor: Owned<HLOCAL>,
}

// SAFETY: the descriptor is a LocalAlloc'd block that is never mutated after construction
// and is only read by `CreateNamedPipeW`; sharing the pointer between threads is sound.
unsafe impl Send for PipeSecurity {}
// SAFETY: see `Send`; all access is read-only.
unsafe impl Sync for PipeSecurity {}

impl PipeSecurity {
    /// Builds the descriptor for the given user.
    ///
    /// # Errors
    /// [`PlatformError`] if the SID or SDDL conversion fails.
    pub fn for_user(user: &Sid) -> Result<Self, PlatformError> {
        let sddl = pipe_sddl(&user.to_sid_string()?)?;
        let wide = to_wide_nul(&sddl)?;
        let mut psd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `wide` is NUL-terminated and outlives the call; `psd` is a valid out pointer.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(wide.as_ptr()),
                SDDL_REVISION_1,
                &raw mut psd,
                None,
            )
        }
        .map_err(win32_err(
            "ConvertStringSecurityDescriptorToSecurityDescriptorW",
        ))?;
        // SAFETY: the descriptor was allocated with LocalAlloc by the call above.
        let descriptor = unsafe { Owned::new(HLOCAL(psd.0)) };
        Ok(Self { descriptor })
    }

    /// Builds the descriptor for the user this process runs as.
    ///
    /// # Errors
    /// [`PlatformError`] if the current user cannot be determined or the SDDL is rejected.
    pub fn for_current_user() -> Result<Self, PlatformError> {
        Self::for_user(&Sid::current_user()?)
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: SECURITY_ATTRIBUTES_LEN,
            lpSecurityDescriptor: self.descriptor.0,
            bInheritHandle: false.into(),
        }
    }
}

/// `size_of::<SECURITY_ATTRIBUTES>()` as the `u32` Win32 expects (checked at compile time).
const SECURITY_ATTRIBUTES_LEN: u32 = {
    let len = size_of::<SECURITY_ATTRIBUTES>();
    assert!(len <= u32::MAX as usize);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "asserted to fit just above"
    )]
    {
        len as u32
    }
};

/// Creates one pipe instance named `name` (`\\.\pipe\...`) protected by `security`.
///
/// Set `first` to claim the name (`FILE_FLAG_FIRST_PIPE_INSTANCE`); it fails with
/// [`PipeError::NameInUse`] if any instance exists. Replacement instances use
/// `first = false`. The handle is overlapped, byte mode, rejects remote clients and
/// allows at most [`PIPE_MAX_INSTANCES`] instances with 64 KiB buffers.
///
/// # Errors
/// [`PipeError::NameInUse`] on access denied when `first` is set, [`PipeError::Platform`]
/// otherwise
/// (including a name outside the pipe namespace).
pub fn create_pipe_instance(
    name: &str,
    first: bool,
    security: &PipeSecurity,
) -> Result<OwnedHandle, PipeError> {
    validate_pipe_name(name)?;
    let wide = to_wide_nul(name)?;
    let mut open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
    if first {
        open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    let mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
    let sa = security.attributes();
    // SAFETY: `wide` is NUL-terminated and `sa` (with its descriptor, kept alive by
    // `security`) outlives the call.
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(wide.as_ptr()),
            open_mode,
            mode,
            PIPE_MAX_INSTANCES,
            PIPE_BUFFER_SIZE,
            PIPE_BUFFER_SIZE,
            0,
            Some(&raw const sa),
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_invalid() {
        // SAFETY: GetLastError has no preconditions; called right after the failing call.
        let code = unsafe { GetLastError() };
        return Err(if first && code == ERROR_ACCESS_DENIED {
            PipeError::NameInUse
        } else {
            PlatformError::win32("CreateNamedPipeW", code.0).into()
        });
    }
    // SAFETY: CreateNamedPipeW succeeded, so `handle` is a valid handle we exclusively own.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle.0) })
}

fn validate_pipe_name(name: &str) -> Result<(), PlatformError> {
    let in_namespace = name
        .get(..PIPE_NAMESPACE.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(PIPE_NAMESPACE));
    if !in_namespace || name.len() == PIPE_NAMESPACE.len() {
        return Err(PlatformError::InvalidInput(
            "pipe name must be \\\\.\\pipe\\<name>",
        ));
    }
    Ok(())
}

/// Reads the DACL and mandatory label of a live kernel object back as SDDL.
///
/// Used to verify that the pipe really carries the intended ACEs, not just that the
/// descriptor we passed in was right.
///
/// # Errors
/// [`PlatformError::Win32`] if the security info cannot be read or converted.
pub fn readback_security_sddl(handle: BorrowedHandle<'_>) -> Result<String, PlatformError> {
    let info = DACL_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION;
    let mut psd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: `handle` is a live handle borrowed for the call; `psd` is a valid out pointer
    // and the other out pointers are optional.
    let status = unsafe {
        GetSecurityInfo(
            HANDLE(handle.as_raw_handle()),
            SE_KERNEL_OBJECT,
            info,
            None,
            None,
            None,
            None,
            Some(&raw mut psd),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(PlatformError::win32("GetSecurityInfo", status.0));
    }
    // SAFETY: GetSecurityInfo returned a LocalAlloc'd descriptor; the guard frees it.
    let _psd_guard = unsafe { Owned::new(HLOCAL(psd.0)) };
    let mut text = PWSTR::null();
    // SAFETY: `psd` is a valid descriptor alive until the guard drops; `text` is a valid out pointer.
    unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            psd,
            SDDL_REVISION_1,
            info,
            &raw mut text,
            None,
        )
    }
    .map_err(win32_err(
        "ConvertSecurityDescriptorToStringSecurityDescriptorW",
    ))?;
    // SAFETY: `text` was allocated with LocalAlloc by the call above; the guard frees it.
    let _text_guard = unsafe { Owned::new(HLOCAL(text.0.cast())) };
    // SAFETY: `text` is a NUL-terminated wide string that lives until the guard drops.
    unsafe { text.to_string() }
        .map_err(|_| PlatformError::InvalidInput("security descriptor is not valid UTF-16"))
}
